//! rho's A2UI v1.0 core: the data model and the paths that address it.
//!
//! This crate has no GUI code. It is tested against the conformance suites
//! that Google publishes with A2UI, vendored under `vendor/a2ui`.

mod data_model;
mod pointer;

pub use data_model::{DataError, DataModel, WatchId, MAX_LIST_INDEX};
pub use pointer::{resolve_path, Pointer, PointerError};
