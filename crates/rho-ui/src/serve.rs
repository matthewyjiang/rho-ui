//! `rho-ui serve`: serves rho-ui's web build to a browser, and does on
//! this machine what a browser can't, for the page to reach this machine
//! as the desktop app reaches a remote one over ssh
//! (`rho_ui_core::server::wire` lists what it offers): run a script, relay
//! a Unix socket, the rho daemon's, run a login shell for the terminal,
//! and share the desktop app's settings and state database. Asked for a
//! remote machine (`wire::MACHINE`), it runs the script, relays the
//! socket and runs the shell there instead, over ssh, as the desktop app
//! here would reach it.
//!
//! Whoever reaches it can run anything as this user, so it listens on
//! 127.0.0.1 unless told otherwise, and answers only a page that holds the
//! token it prints at start (`auth.rs`). Reach it from another machine
//! through a tunnel, such as `ssh -L`.
//!
//! The page itself is the web build `cargo xtask web` makes: embedded in
//! a release that `cargo xtask dist` builds, else read from `target/web`
//! or `--web DIR` (`files.rs`). Unix only: it relays a Unix socket and
//! runs `sh`.

mod auth;
mod files;
mod script;
mod shell;
mod socket;
mod state;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
};

use axum::{routing::get, Router};
use rho_ui_core::{server::wire, DaemonError, SshLink, SshTarget};
use serde::Deserialize;

pub(crate) use self::{auth::Token, files::Web};

/// The port it listens on unless told: any free one would do, but one that
/// stays put keeps a bookmark or a tunnel working across restarts.
const DEFAULT_PORT: u16 = 8765;

const USAGE: &str = "usage: rho-ui serve [--port PORT] [--bind ADDRESS] [--web DIR]

Serves rho-ui in a browser at http://ADDRESS:PORT (127.0.0.1:8765 unless
told). Whoever reaches it can run commands as you: keep it on 127.0.0.1
and reach it from elsewhere through a tunnel such as `ssh -L`.

--web DIR serves the web build in DIR, as `cargo xtask web` makes it in
target/web, in place of the one built in.";

/// What `rho-ui serve` is asked.
#[derive(Debug)]
struct Options {
    address: SocketAddr,
    web: Web,
}

/// Parses the arguments after `serve`. Returns the usage text on bad input.
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT);
    let mut dir = None;
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{arg} needs a value\n\n{USAGE}"))
        };
        match arg.as_str() {
            "--port" => {
                let port = value()?;
                let port = port
                    .parse()
                    .map_err(|_| format!("--port {port}: not a port"))?;
                address.set_port(port);
            }
            "--bind" => {
                let ip = value()?;
                let ip = ip
                    .parse()
                    .map_err(|_| format!("--bind {ip}: not an IP address"))?;
                address.set_ip(ip);
            }
            "--web" => dir = Some(Web::dir(PathBuf::from(value()?))?),
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }
    let web = match dir {
        Some(web) => web,
        None => Web::built_in()?,
    };
    Ok(Options { address, web })
}

/// The machine an endpoint works on, from its query: `machine=HOST` for
/// the remote one at ssh host `HOST`, else this one.
#[derive(Deserialize)]
pub(crate) struct At {
    machine: Option<String>,
}

/// The ssh link to the remote machine at `host`, as the desktop app here
/// reaches it: a connection to each host, shared by every script, socket
/// and shell there.
async fn ssh(host: &str) -> Result<SshLink, DaemonError> {
    SshTarget::new(host).link().await
}

/// The routes: the page's files, open to anyone, and the endpoints the
/// page uses, behind the token.
pub(crate) fn router(token: Arc<Token>, web: Web) -> Router {
    let shared = Arc::new(state::Shared::open());
    Router::new()
        .route(wire::SCRIPT, get(script::open))
        .route(wire::SOCKET, get(socket::open))
        .route(wire::SHELL, get(shell::open))
        .route(
            wire::STATE,
            get(move |upgrade| {
                let shared = Arc::clone(&shared);
                async move { state::open(shared, upgrade) }
            }),
        )
        .fallback_service(files::serve(web))
        .layer(axum::middleware::from_fn_with_state(token, auth::check))
}

/// Runs `rho-ui serve` with `args`, those after `serve`, until Ctrl-C.
#[expect(
    clippy::print_stderr,
    reason = "a command-line program says what it does, and why it stops, on stderr"
)]
pub(crate) fn main(args: impl Iterator<Item = String>) -> ExitCode {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the OS lets this process start threads and an I/O driver");
    runtime.block_on(async {
        let token = Arc::new(Token::new());
        let listener = match tokio::net::TcpListener::bind(options.address).await {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("rho-ui serve: can't listen on {}: {error}", options.address);
                return ExitCode::FAILURE;
            }
        };
        // The port the OS picked, for `--port 0`.
        let address = listener.local_addr().unwrap_or(options.address);
        eprintln!(
            "rho-ui serve: open http://{address}/#token={}",
            token.as_str()
        );
        let served = axum::serve(listener, router(token, options.web))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await;
        match served {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("rho-ui serve: {error}");
                ExitCode::FAILURE
            }
        }
    })
}
