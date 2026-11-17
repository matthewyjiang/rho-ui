//! Reaching a machine through `rho-ui serve`, as rho-ui's web build does: the
//! browser can't run a program or reach a Unix socket, so the server, on
//! that machine, does both for it, each over a WebSocket of its own
//! ([`wire`] lists them). It is the same work an ssh server does for the
//! desktop app's remote machines (`ssh.rs`), and rho-ui asks the same of
//! it: the setup script ([`crate::setup`]) starts the daemon and says
//! where it listens, the server relays that socket, and every script after
//! runs as `sh -s` there.

mod shell;
mod state;
mod stream;
pub mod wire;
mod ws;

use std::{sync::Arc, time::Duration};

use rho_protocol::{Client, ClientHello, HelloResponse, Notifications};

pub use self::state::{SharedState, StateDone, StateWriter};
pub(crate) use self::stream::WsStream;
use self::ws::Frame;
use crate::{
    daemon::{handshake, DaemonError, Machine},
    exec::{on_io, within},
    link::{Ending, Link, Output, ScriptError},
    setup,
};

/// A machine reached through `rho-ui serve`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerTarget {
    /// Where the server listens, as `http://127.0.0.1:8765`.
    address: String,
    /// The token the server printed, which each WebSocket offers.
    token: String,
    rho: String,
    /// The ssh host of the machine, when the server reaches a remote one
    /// for the browser; `None` for the server's own.
    machine: Option<String>,
}

impl ServerTarget {
    /// The machine of the server at `address`, an `http://` URL with no
    /// path, as `http://127.0.0.1:8765`, which printed `token`. It runs the
    /// program `rho` there.
    pub fn new(address: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            token: token.into(),
            rho: "rho".to_owned(),
            machine: None,
        }
    }

    /// The remote machine at ssh host `host` instead, which the server
    /// reaches over ssh, as the desktop app on its machine would.
    #[must_use]
    pub fn with_machine(mut self, host: impl Into<String>) -> Self {
        self.machine = Some(host.into());
        self
    }

    /// Runs `rho` on the machine to start its daemon: a name looked up on
    /// the server's PATH, or a path.
    #[must_use]
    pub fn with_rho(mut self, rho: impl Into<String>) -> Self {
        self.rho = rho.into();
        self
    }

    /// The `rho` program it runs on the machine; see [`Self::with_rho`].
    pub(crate) fn rho(&self) -> &str {
        &self.rho
    }

    /// A link to the server, for scripts to run there whether or not its
    /// daemon answers. Opening one costs nothing: each script opens a
    /// WebSocket of its own.
    pub(crate) fn link(&self) -> ServerLink {
        ServerLink {
            target: Arc::new(self.clone()),
        }
    }
}

/// The server one [`ServerTarget`]'s connection reached. Links are equal
/// when they came from one connection.
#[derive(Clone, Debug)]
pub struct ServerLink {
    target: Arc<ServerTarget>,
}

impl PartialEq for ServerLink {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.target, &other.target)
    }
}

impl Eq for ServerLink {}

impl ServerLink {
    /// The machine, as messages name it: its ssh host, or the server's
    /// address.
    pub(crate) fn name(&self) -> &str {
        self.target
            .machine
            .as_deref()
            .unwrap_or(&self.target.address)
    }

    /// The WebSocket URL of `endpoint`, one of [`wire`]'s, with `query`
    /// and the machine's `machine=` ([`wire::MACHINE`]).
    fn url(&self, endpoint: &str, query: &[(&str, &str)]) -> String {
        let address = &self.target.address;
        let address = address
            .strip_prefix("http")
            .map_or_else(|| address.clone(), |rest| format!("ws{rest}"));
        let machine = self
            .target
            .machine
            .as_deref()
            .map(|host| (wire::MACHINE, host));
        let query: Vec<String> = query
            .iter()
            .copied()
            .chain(machine)
            .map(|(key, value)| format!("{key}={}", percent_encode(value)))
            .collect();
        match query.as_slice() {
            [] => format!("{address}{endpoint}"),
            _ => format!("{address}{endpoint}?{}", query.join("&")),
        }
    }

    /// Runs `input` with `sh -s` there, within `deadline` if any, and
    /// returns what it printed. Dropping the future closes the WebSocket,
    /// and the server kills the script.
    pub(crate) async fn script(
        &self,
        input: Vec<u8>,
        deadline: Option<Duration>,
    ) -> Result<Vec<u8>, ScriptError> {
        let url = self.url(wire::SCRIPT, &[]);
        let token = self.target.token.clone();
        let ran = async move {
            let (mut send, mut receive) = ws::open(&url, &token)
                .await
                .map_err(|why| ScriptError::Session(DaemonError::Unreachable(why)))?;
            if send.send(input).await.is_err() {
                return Err(ScriptError::Cut);
            }
            let mut stdout = Vec::new();
            while let Some(frame) = receive.recv().await {
                match frame {
                    Frame::Binary(bytes) => stdout.extend(bytes),
                    Frame::Text(json) => {
                        let end: wire::ScriptEnd =
                            serde_json::from_str(&json).map_err(|error| {
                                ScriptError::Failed(format!(
                                    "rho-ui serve said something else: {error}"
                                ))
                            })?;
                        let ending = match end.ending {
                            wire::Ending::Exited(code) => Ending::Exited(code),
                            wire::Ending::Killed(signal) => Ending::Killed(signal),
                            wire::Ending::NotStarted(why) => Ending::NotStarted(why),
                        };
                        let output = Output {
                            stdout,
                            stderr: end.stderr.into_bytes(),
                            ending,
                        };
                        return output.result();
                    }
                }
            }
            Err(ScriptError::Cut)
        };
        let ran = async move {
            match deadline {
                Some(deadline) => within(deadline, ran)
                    .await
                    .unwrap_or(Err(ScriptError::TimedOut(deadline))),
                None => ran.await,
            }
        };
        on_io(ran).await.map_err(ScriptError::Session)?
    }

    /// A stream to the Unix socket `socket` there, the daemon's.
    async fn socket(&self, socket: &str) -> Result<WsStream, DaemonError> {
        let url = self.url(wire::SOCKET, &[("path", socket)]);
        let (send, receive) = ws::open(&url, &self.target.token).await.map_err(|why| {
            DaemonError::Unreachable(format!(
                "couldn't reach the Rho daemon's socket {socket} through {}: {why}",
                self.name()
            ))
        })?;
        Ok(WsStream::new(send, receive))
    }
}

/// How `rho-ui serve` says a script it ran over ssh ended, `ran`, on the
/// remote machine at `host`: the inverse of what [`ServerLink::script`]
/// reads. `None` if the connection went with no word of how.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn script_end(
    ran: Result<Output, DaemonError>,
    host: &str,
) -> Option<(Vec<u8>, wire::ScriptEnd)> {
    let not_started = |why: String| {
        let end = wire::ScriptEnd {
            ending: wire::Ending::NotStarted(why),
            stderr: String::new(),
        };
        Some((Vec::new(), end))
    };
    let output = match ran {
        Ok(output) => output,
        Err(error) => return not_started(error.to_string()),
    };
    let ending = match output.ending {
        Ending::Exited(code) => wire::Ending::Exited(code),
        Ending::Killed(signal) => wire::Ending::Killed(signal),
        Ending::NotStarted(why) => wire::Ending::NotStarted(why),
        Ending::Refused => return not_started(format!("{host}'s sshd wouldn't run sh")),
        Ending::Cut => return None,
    };
    let end = wire::ScriptEnd {
        ending,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    Some((output.stdout, end))
}

/// Connects to the daemon on `target`'s machine, starting it there first
/// if none runs, and completes the handshake as `hello`. Runs on the I/O
/// runtime.
pub(crate) async fn connect(
    target: &ServerTarget,
    hello: ClientHello,
) -> Result<(Client, Notifications, HelloResponse, Machine), DaemonError> {
    let server = target.link();
    let link = Link::Server(server.clone());
    let setup = setup::run(&link, target.rho()).await?;
    let stream = server.socket(&setup.socket).await?;
    let where_ = format!("at {} through {}", setup.socket, server.name());
    let (client, notifications, hello) = handshake(stream, hello, &where_).await?;
    let machine = Machine {
        environment: setup.environment,
        home: Some(setup.home),
        link: Some(link),
    };
    Ok((client, notifications, hello, machine))
}

/// `text` for a URL's query: bytes other than letters, digits and `-._~/`
/// as `%XX`.
fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}
