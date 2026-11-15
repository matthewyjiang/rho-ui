//! rho's A2UI v1.0 core: the data model, the paths that address it, and the
//! expressions that read it.
//!
//! This crate has no GUI code. It is tested against the conformance suites
//! that Google publishes with A2UI, vendored under `vendor/a2ui`.

mod coerce;
mod data_model;
mod expr;
mod pointer;
mod template;

pub use data_model::{DataError, DataModel, WatchId, MAX_LIST_INDEX};
pub use expr::{Call, Expr, ExpressionError};
pub use pointer::{resolve_path, Pointer, PointerError};
pub use template::{parse_template, MAX_DEPTH, MAX_TEMPLATE_CHARS, MAX_TEMPLATE_PARTS};
