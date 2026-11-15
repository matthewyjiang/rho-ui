//! rho's A2UI v1.0 core: the data model, the paths that address it, and the
//! expressions and basic catalog functions that read it.
//!
//! This crate has no GUI code. It is tested against the conformance suites
//! that Google publishes with A2UI, vendored under `vendor/a2ui`.

mod basic_catalog;
mod coerce;
mod data_context;
mod data_model;
mod expr;
mod functions;
mod pointer;
mod regex_dialect;
mod template;

pub use data_context::DataContext;
pub use data_model::{DataError, DataModel, WatchId, MAX_LIST_INDEX};
pub use expr::{Call, Expr, ExpressionError};
pub use functions::{Args, Effect, Function, FunctionError, Functions};
pub use pointer::{resolve_path, Pointer, PointerError};
pub use regex_dialect::RegexError;
pub use template::{parse_template, MAX_DEPTH, MAX_TEMPLATE_CHARS, MAX_TEMPLATE_PARTS};
