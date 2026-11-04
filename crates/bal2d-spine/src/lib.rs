//! Spine 4.2 skeletal animation: data model, binary reader, atlas binding and runtime.
//!
//! `no_std` + `alloc` so the same code runs in the phone-side compiler (wasm), the host tools
//! and on the watch. The runtime reproduces the Spine 4.2 pose semantics (bone inheritance,
//! IK / transform / path / physics constraints, timelines, AnimationState mixing); its output
//! is checked against the reference runtime by the golden tests in `tests/`.
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod atlas;
pub mod attachment;
pub mod binary;
pub mod data;
pub mod math;
pub mod skeleton;
pub mod state;
pub mod timeline;
pub mod writer;

pub use atlas::{Atlas, bind_atlas};
pub use binary::{ReadError, SkelSource, read_skeleton, read_skeleton_from};
pub use data::*;
pub use skeleton::{Bone, Physics, Skeleton, Slot};
