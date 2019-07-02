// Author: Tom Olsson <mail@tomolsson.se>
// Copyright © 2019, Embark Studios, all rights reserved.
// Created: 10 April 2019

#![warn(clippy::all)]
#![warn(rust_2018_idioms)]

/*!
Wrapper for PhysX Scene
 */

use super::{
    articulation_reduced_coordinate::*,
    body::*,
    physics::Physics,
    rigid_actor::RigidActor,
    rigid_static::*,
    traits::*,
    transform::{na_to_px_v3, px_to_na_v3},
    user_data::UserData,
    visual_debugger::*,
};
use nalgebra_glm as glm;
use enumflags2_derive::EnumFlags;

use physx_sys::*;
use std::ops::{Deref, DerefMut};
use std::ptr::{null, null_mut};
use std::sync::RwLock;

pub struct Scene {
    px_scene: RwLock<Option<*mut PxScene>>,
    bodies: Vec<ArticulationReducedCoordinate>,
    statics: Vec<RigidStatic>,
    simulation_callback: Option<*mut PxSimulationEventCallback>,
}

////////////////////////////////////////////////////////////////////////////////
// #[physx_type]
impl Scene {
    pub fn new(scene: *mut PxScene) -> Self {
        let mut _self = Self {
            px_scene: RwLock::new(Some(scene)),
            bodies: Vec::new(),
            statics: Vec::new(),
            simulation_callback: None,
        };
        _self.allocate_user_data();
        _self
    }

    fn allocate_user_data(&mut self) {
        let userdata = Box::new(UserData::new_scene());
        let scene = self.px_scene.write().unwrap().unwrap();
        unsafe {
            (*scene).userData = Box::into_raw(userdata) as *mut std::ffi::c_void;
        }
    }

    pub fn release(&mut self) {
        unsafe {
            // destroy simulation callback if we have one
            if let Some(callback) = self.simulation_callback.take() {
                destroy_contact_callback(callback);
            }

            // Release the scene object
            let mut scene = self.px_scene.write().unwrap();
            let scene = scene.take().expect("scene already released");
            let b: Box<UserData> = Box::from_raw((*scene).userData as *mut _);
            drop(b);
            PxScene_release_mut(scene);
        }
    }

    /// Get the visual debugger client
    pub fn get_pvd_client(&self) -> PvdSceneClient {
        PvdSceneClient::from_ptr(unsafe {
            PxScene_getScenePvdClient_mut(
                self.px_scene.write().unwrap().expect("accessing null ptr"),
            )
        })
    }

    pub fn add_actor(&mut self, mut actor: RigidStatic) -> BodyHandle {
        unsafe {
            PxScene_addActor_mut(
                self.px_scene.write().unwrap().expect("accessing null ptr"),
                actor.get_raw_mut() as *mut PxActor,
                null(),
            );
        }

        let handle = BodyHandle(actor.get_raw() as usize);
        self.statics.push(actor);
        handle
    }

    /// Add a multibody articulation to the world
    /// TODO: Make this take a boxed-trait object or similar instead
    pub fn add_articulation<T: FnOnce(&mut ArticulationReducedCoordinate)>(
        &mut self,
        mut mb: ArticulationReducedCoordinate,
        func: T,
    ) -> PartHandle {
        let handle = mb.root_handle();
        func(&mut mb);

        let scene = self.px_scene.write().unwrap();
        unsafe {
            PxScene_addArticulation_mut(
                scene.expect("accessing null ptr"),
                mb.deref_mut().get_raw_mut(),
            );
        }
        mb.common_init();
        self.bodies.push(mb);
        handle
    }

    /// Remove an articulation from the world
    pub fn remove_articulation(&mut self, handle: BodyHandle) {
        unsafe {
            let articulation = handle.0 as *const PxArticulationBase;
            PxScene_removeArticulation_mut(
                self.px_scene.write().unwrap().expect("accessing null ptr"),
                articulation as *mut PxArticulationBase,
                false,
            );

            if let Some(idx) = self
                .bodies
                .iter()
                .position(|b| b.deref().get_raw() == articulation)
            {
                let mut body = self.bodies.swap_remove(idx);
                body.release();
            }
        }
    }

    ////////////////////////////////////////////////////////////////////////////////

    pub fn simulate(&mut self, duration: f32) {
        let scene = self.px_scene.write().unwrap();
        unsafe {
            PxScene_simulate_mut(
                scene.expect("accessing null ptr"),
                duration,
                null_mut(),
                null_mut(),
                0,
                true,
            );
        }
    }

    ////////////////////////////////////////////////////////////////////////////////

    pub fn fetch_results(&mut self, block: bool) {
        let scene = self.px_scene.write().unwrap();
        unsafe {
            PxScene_fetchResults_mut(scene.expect("accessing null ptr"), block, null_mut());
        }
    }
    //    fn read(self) -> std::sync::RwLockReadGuard<Px>

    pub fn get_multibody(&self, handle: BodyHandle) -> Option<&ArticulationReducedCoordinate> {
        self.bodies.iter().find(|bod| bod.handle() == handle)
    }

    pub fn get_multibody_mut(
        &mut self,
        handle: BodyHandle,
    ) -> Option<&mut ArticulationReducedCoordinate> {
        self.bodies.iter_mut().find(|bod| bod.handle() == handle)
    }

    pub fn sample_height(
        &self,
        position: glm::Vec3,
        ignored: Option<&RigidActor>,
        ignore_dynamic: bool,
    ) -> Option<glm::Vec3> {
        // todo[tolsson]: clean this up
        let ignored_body = if let Some(body) = ignored {
            body.get_raw()
        } else {
            null_mut()
        };

        let down = -glm::Vec3::y_axis();
        let max_dist = 1e5;

        unsafe {
            let mut filter_data = PxQueryFilterData_new_2(PxQueryFlags {
                mBits: (PxQueryFlag::ePREFILTER | PxQueryFlag::eSTATIC) as u16,
            });

            if !ignore_dynamic {
                filter_data.flags.mBits |= PxQueryFlag::eDYNAMIC as u16;
            }

            let filter_callback = create_raycast_filter_callback(ignored_body);
            let mut hit: PxRaycastHit = std::mem::uninitialized();

            let hit_anything = PxSceneQueryExt_raycastSingle_mut(
                self.px_scene
                    .read()
                    .expect("failed reading from scene")
                    .expect("accessing null ptr"),
                &na_to_px_v3(position),
                &na_to_px_v3(down.into_inner()),
                max_dist,
                PxSceneQueryFlags {
                    mBits: PxHitFlag::ePOSITION as u16,
                },
                &mut hit,
                &filter_data as *const PxQueryFilterData as *const PxSceneQueryFilterData,
                filter_callback as *mut PxQueryFilterCallback as *mut PxSceneQueryFilterCallback,
                null_mut(),
            );

            PxQueryFilterCallback_delete(filter_callback);

            if hit_anything {
                Some(px_to_na_v3(hit.position))
            } else {
                None
            }
        }
    }

    pub fn set_simulation_event_callback<T>(
        &mut self,
        callback: physx_sys::CollisionCallback,
        userdata: *mut T,
    ) {
        unsafe {
            let callback =
                physx_sys::create_contact_callback(callback, userdata as *mut std::ffi::c_void);
            self.simulation_callback = Some(callback);
            PxScene_setSimulationEventCallback_mut(
                self.px_scene
                    .write()
                    .expect("failed reading scene")
                    .expect("accessing null ptr"),
                callback,
            );
        }
    }

    pub fn find_matching_rigid_actor_mut(
        &mut self,
        actor: *const PxRigidActor,
    ) -> Option<&mut RigidActor> {
        if let Some(rigid) = self.statics.iter_mut().find_map(|elem| {
            let actor_ptr = elem.get_raw() as *const PxRigidActor;
            if actor == actor_ptr {
                Some(elem)
            } else {
                None
            }
        }) {
            return Some(rigid);
        };

        for body in &mut self.bodies {
            let handle = body.handle();
            let handle = PartHandle(handle.0, actor as usize);
            let part = body.part_from_handle_mut(handle);
            if part.is_some() {
                return part.map(|link| link.deref_mut().deref_mut());
            }
        }

        None
    }

    // pub fn find_matching_rigid_actor(&self, actor: *const PxRigidActor) -> Option<&RigidActor> {
    //     if let Some(rigid) = self.statics.iter().find_map(|elem| {
    //         let actor_ptr = elem.get_raw() as *const PxRigidActor;
    //         if actor == actor_ptr {
    //             Some(elem)
    //         } else {
    //             None
    //         }
    //     }) {
    //         return Some(rigid);
    //     };

    //     for body in &self.bodies {
    //         let handle = body.handle();
    //         let handle = PartHandle(handle.0, actor as usize);
    //         let part = body.part_from_handle(handle);
    //         if part.is_some() {
    //             return part.map(|link| link.deref().deref());
    //         }
    //     }

    //     None
    // }

    /// Looking
    pub fn collide_raw_pair(
        &mut self,
        first_px_actor: *mut PxRigidActor,
        second_px_actor: *mut PxRigidActor,
        pairs: &[PxContactPair],
    ) {
        // This function is really unrustic, but I can't find a nice way to do it
        // since the lookup of either collision member will take a mutable borrow on the scene...
        // However doing it with two pointers bypasses this, disgustingly enough. The NonNull
        // probably adds a bit of overhead, but in comparison to the search it's likely fine.
        if first_px_actor != second_px_actor {
            unsafe {
                // Since each lookup might traverse a lot of pointers we verify each lookup directly.
                let first_actor = self
                    .find_matching_rigid_actor_mut(first_px_actor)
                    .map(|val| val as *mut RigidActor);

                if let Some(first) = first_actor {
                    #[allow(clippy::transmute_ptr_to_ref)]
                    let first: &mut RigidActor = std::mem::transmute(first);
                    let second_actor = self
                        .find_matching_rigid_actor_mut(second_px_actor)
                        .map(|val| val as *mut RigidActor);

                    if let Some(second) = second_actor {
                        #[allow(clippy::transmute_ptr_to_ref)]
                        let second: &mut RigidActor = std::mem::transmute(second);
                        first.on_collide(second, pairs);
                        second.on_collide(first, pairs);
                    }
                }
            }
        }
    }

    pub fn get_bodies(&self) -> &Vec<ArticulationReducedCoordinate> {
        &self.bodies
    }

    ////////////////////////////////////////////////////////////////////////////////

    pub fn step(&mut self, step: f32, block: bool) {
        let scene = self.px_scene.write().unwrap();
        unsafe {
            PxScene_simulate_mut(
                scene.expect("accessing null ptr"),
                step,
                null_mut(),
                null_mut(),
                0,
                true,
            );
            PxScene_fetchResults_mut(scene.expect("accessing null ptr"), block, null_mut());
        }
    }
}

#[derive(Debug, Clone, Copy, EnumFlags)]
#[repr(u32)]
pub enum BroadPhaseType {
    SweepAndPrune = 1,
    MultiBoxPruning = 2,
    AutomaticBoxPruning = 4,
    GPU = 8,
}

impl Into<PxBroadPhaseType::Enum> for BroadPhaseType {
    fn into(self) -> PxBroadPhaseType::Enum {
        match self {
            BroadPhaseType::SweepAndPrune => 0,
            BroadPhaseType::MultiBoxPruning => 1,
            BroadPhaseType::AutomaticBoxPruning => 2,
            BroadPhaseType::GPU => 3,
        }
    }
}

pub enum SimulationThreadType {
    Dedicated(u32),
    Shared(*mut PxCpuDispatcher),
    Default,
}

pub struct SceneBuilder {
    pub(crate) gravity: glm::Vec3,
    pub(crate) simulation_filter_shader: Option<SimulationFilterShader>,
    pub(crate) simulation_threading: Option<SimulationThreadType>,
    pub(crate) broad_phase_type: BroadPhaseType,
}

impl Default for SceneBuilder {
    fn default() -> Self {
        Self {
            gravity: glm::vec3(0.0, -9.80665, 0.0), // standard gravity value
            simulation_filter_shader: None,
            simulation_threading: None,
            broad_phase_type: BroadPhaseType::SweepAndPrune,
        }
    }
}

impl SceneBuilder {
    /// Set the gravity for the scene.
    ///
    /// Default: [0.0, -9.80665, 0.0] (standard gravity)
    pub fn set_gravity(&mut self, gravity: glm::Vec3) -> &mut Self {
        self.gravity = gravity;
        self
    }

    /// Set a callback to be invoked on various simulation events. Note:
    /// Currently only handles collision events
    ///
    /// Default: not set
    pub fn set_simulation_filter_shader(
        &mut self,
        simulation_filter_shader: SimulationFilterShader,
    ) -> &mut Self {
        self.simulation_filter_shader = Some(simulation_filter_shader);
        self
    }

    /// Set the number of threads to use for simulation
    ///
    /// Default: 1
    pub fn set_simulation_threading(&mut self, _type: SimulationThreadType) -> &mut Self {
        self.simulation_threading = Some(_type);
        self
    }

    /// Set the number of threads to use for simulation
    ///
    /// Default: 1
    pub fn set_broad_phase_type(&mut self, _type: BroadPhaseType) -> &mut Self {
        self.broad_phase_type = _type;
        self
    }

    /// Build a new Scene from the provided parameters
    pub(super) fn build(&self, physics: &mut Physics) -> PxSceneDesc {
        unsafe {
            let tolerances = physics.get_tolerances_scale();
            let mut scene_desc = PxSceneDesc_new(tolerances);

            let dispatcher = match self.simulation_threading.as_ref().expect("foo") {
                SimulationThreadType::Default => {
                    phys_PxDefaultCpuDispatcherCreate(1, null_mut()) as *mut _
                }
                SimulationThreadType::Dedicated(count) => {
                    phys_PxDefaultCpuDispatcherCreate(*count, null_mut()) as *mut _
                }
                SimulationThreadType::Shared(dispatcher) => *dispatcher as *mut _,
            };

            scene_desc.cpuDispatcher = dispatcher;
            scene_desc.gravity = na_to_px_v3(self.gravity);

            if let Some(filter_shader) = self.simulation_filter_shader {
                physx_sys::enable_custom_filter_shader(
                    &mut scene_desc as *mut PxSceneDesc,
                    filter_shader,
                );
            } else {
                scene_desc.filterShader = get_default_simulation_filter_shader();
            }
            scene_desc
        }
    }
}
