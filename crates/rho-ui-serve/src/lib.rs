//! rho-ui-serve: serves rho-ui's web build to a browser, and does on this
//! machine what a browser can't, for the page to reach this machine as the
//! desktop app reaches a remote one over ssh (`rho_ui_core::server::wire`
//! lists what it offers): run a script, relay a Unix socket, the rho
//! daemon's, and run a login shell for the terminal.
//!
//! Whoever reaches it can run anything as this user, so it listens on
//! 127.0.0.1 unless told otherwise, and answers only a browser that holds
//! the token it prints at start (`auth.rs`). Reach it from another machine
//! through a tunnel, such as `ssh -L`.
//!
//! Unix only: it relays a Unix socket and runs `sh`.
#![cfg(unix)]

mod auth;
mod files;
mod script;
mod shell;
mod socket;

use std::{path::PathBuf, sync::Arc};

use axum::{routing::get, Router};
use rho_ui_core::server::wire;

pub use crate::auth::Token;

/// The routes: the page's files, and the endpoints the page uses, all
/// behind the token.
pub fn router(token: Arc<Token>, dist: PathBuf) -> Router {
    Router::new()
        .route(wire::SCRIPT, get(script::open))
        .route(wire::SOCKET, get(socket::open))
        .route(wire::SHELL, get(shell::open))
        .fallback_service(files::serve(dist))
        .layer(axum::middleware::from_fn_with_state(token, auth::check))
}
