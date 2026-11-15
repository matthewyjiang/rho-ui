//! rho's A2UI v1.0 core: the data model, the paths that address it, and the
//! expressions and basic catalog functions that read it.
//!
//! This crate has no GUI code. It is tested against the conformance suites
//! that Google publishes with A2UI, vendored under `vendor/a2ui`.
//!
//! [`Catalog`] loads a v1.0 catalog, the components and functions a surface
//! may use, and [`CatalogValidator`] checks component instances against one.

mod basic_catalog;
mod bundle;
mod catalog;
mod coerce;
mod data_context;
mod data_model;
mod expr;
mod functions;
mod ident;
mod message;
mod pointer;
mod regex_dialect;
mod schema;
mod shape;
mod template;
mod validate;

pub use catalog::{
    Callers, Catalog, CatalogError, ComponentDef, Derivation, Entry, EntryKind, FunctionDef,
    ReturnType,
};
pub use data_context::DataContext;
pub use data_model::{DataError, DataModel, WatchId, MAX_LIST_INDEX};
pub use expr::{Call, Expr, ExpressionError};
pub use functions::{Args, Effect, Function, FunctionError, Functions};
pub use ident::is_identifier;
pub use message::{
    parse_payload, ActionMessage, AgentMessage, CallRendererFunction, CreateSurface, ErrorCode,
    ErrorMessage, ErrorTarget, FunctionFailure, FunctionResponse, Metadata, MetadataError,
    RendererMessage, UpdateComponents, UpdateDataModel, WireComponent, PROTOCOL_VERSION,
};
pub use pointer::{resolve_path, Pointer, PointerError};
pub use regex_dialect::RegexError;
pub use schema::{agent_to_renderer, common_types};
pub use template::{parse_template, MAX_DEPTH, MAX_TEMPLATE_CHARS, MAX_TEMPLATE_PARTS};
pub use validate::{validate_common_type, CatalogValidator, ValidationError, Violation};
