//! One ssh connection per host, shared by everything rho-ui does there:
//! the daemon's socket, and every script. A connection costs a key
//! signature, which an agent such as 1Password's asks the user to
//! approve, so rho-ui makes one per host and keeps it while it lives.

use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use rho_protocol::{Client, ClientError, ClientHello, HelloResponse, Notifications};
use russh::{
    client::{self, Handle, Msg},
    keys::PublicKeyOrCertificate,
    Channel, ChannelOpenFailure, Preferred,
};
use tokio::net::TcpStream;

use super::{
    auth,
    config::{self, Resolved},
    host_key::{self, KnownKey},
};
use crate::daemon::{DaemonError, HANDSHAKE_DEADLINE};

/// How long reaching a host may take: the TCP connection and ssh's key
/// exchange, not signing in, which may wait on the user to approve a key
/// agent's signature. A host that doesn't answer would otherwise wait out
/// the kernel's SYN retries (`tcp_syn_retries` = 6 here, about 127 s).
/// OpenSSH's whole first connection to localhost, signing in included,
/// took 2.6 s, so this is far past any host that answers.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How often to check a quiet connection is alive, and how many checks
/// may go unanswered: a link that died, as when the laptop sleeps or
/// changes networks, closes within 15 s × 3, and with it the daemon's
/// channel, so the app sees the connection end and offers Retry instead of
/// TCP retrying for many minutes.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_MAX: usize = 3;

/// A signed-in ssh connection to a host.
pub(super) struct Connection {
    /// The destination the user named, for messages and `ssh` commands.
    host: String,
    handle: Handle<Verifier>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// The live connection to `host`, opening one if none is: with `route`,
/// else as `ssh -G` resolves the host. Callers for one host wait on one
/// another, so they share one connection, and the user approves one
/// signature; other hosts don't wait. Runs on the I/O runtime.
///
/// # Errors
///
/// Fails, saying why, if the host can't be reached, its key isn't the
/// one `known_hosts` lists, or it accepts none of the user's keys.
pub(super) async fn shared(
    host: &str,
    route: Option<&Resolved>,
) -> Result<Arc<Connection>, DaemonError> {
    let slot = slot(host);
    let mut held = slot.lock().await;
    if let Some(live) = held.as_ref().filter(|connection| !connection.is_closed()) {
        return Ok(Arc::clone(live));
    }
    let resolved = match route {
        Some(route) => route.clone(),
        None => config::resolve(host).await?,
    };
    let connection = Arc::new(Connection::open(host, &resolved).await?);
    *held = Some(Arc::clone(&connection));
    Ok(connection)
}

/// Where one host's connection is kept, while it is open.
type Slot = Arc<tokio::sync::Mutex<Option<Arc<Connection>>>>;

/// `host`'s slot, one for the life of the process.
fn slot(host: &str) -> Slot {
    static SLOTS: OnceLock<Mutex<HashMap<String, Slot>>> = OnceLock::new();
    let mut slots = SLOTS
        .get_or_init(Mutex::default)
        .lock()
        .expect("the connection slots' mutex is never held across a panic");
    Arc::clone(slots.entry(host.to_owned()).or_default())
}

impl Connection {
    /// Connects to `host` as `resolved` says, checks its key, and signs in.
    async fn open(host: &str, resolved: &Resolved) -> Result<Self, DaemonError> {
        let known = host_key::read(resolved);
        let config = client::Config {
            preferred: Preferred {
                key: Cow::Owned(host_key::preferred(&known)),
                ..Preferred::DEFAULT
            },
            keepalive_interval: Some(KEEPALIVE_INTERVAL),
            keepalive_max: KEEPALIVE_MAX,
            ..client::Config::default()
        };
        let verifier = Verifier {
            host: host.to_owned(),
            name: host_key::lookup_name(resolved),
            known,
        };
        let (hostname, port) = (resolved.hostname.as_str(), resolved.port);
        let reach = async {
            let stream = TcpStream::connect((hostname, port))
                .await
                .map_err(|error| {
                    DaemonError::Unreachable(format!(
                        "couldn't reach {host} ({hostname} port {port}): {error}"
                    ))
                })?;
            // Small writes, such as a keystroke's or a request's, go at once.
            let _ = stream.set_nodelay(true);
            client::connect_stream(Arc::new(config), stream, verifier)
                .await
                .map_err(|refusal| match refusal {
                    Refusal::HostKey(why) => DaemonError::Unreachable(why),
                    Refusal::Ssh(error) => DaemonError::Unreachable(format!(
                        "couldn't open an ssh connection to {host}: {error}"
                    )),
                })
        };
        let mut handle = tokio::time::timeout(CONNECT_TIMEOUT, reach)
            .await
            .map_err(|_| {
                DaemonError::Unreachable(format!(
                    "{host} ({hostname} port {port}) didn't answer within {} s",
                    CONNECT_TIMEOUT.as_secs()
                ))
            })??;
        auth::sign_in(&mut handle, host, resolved).await?;
        Ok(Self {
            host: host.to_owned(),
            handle,
        })
    }

    /// The destination the user named.
    pub(super) fn host(&self) -> &str {
        &self.host
    }

    /// Whether the connection is gone, as after a link died.
    pub(super) fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// Opens a session channel, for one command.
    ///
    /// # Errors
    ///
    /// Fails if the host refuses it, as at sshd's `MaxSessions`, or the
    /// connection is gone.
    pub(super) async fn session(&self) -> Result<Channel<Msg>, DaemonError> {
        let host = &self.host;
        self.handle
            .channel_open_session()
            .await
            .map_err(|error| match error {
                russh::Error::ChannelOpenFailure(reason) => DaemonError::Unreachable(format!(
                    "{host} refused an ssh session ({reason:?}): sshd allows 10 at once on one \
                     connection unless its MaxSessions says otherwise"
                )),
                other => DaemonError::Unreachable(format!(
                    "couldn't open an ssh session on {host}: {other}"
                )),
            })
    }

    /// Connects to the Rho daemon at the Unix socket `socket` on the host,
    /// over a `direct-streamlocal` channel, as `ssh -W` does, and completes
    /// the handshake as `hello` within [`HANDSHAKE_DEADLINE`]. The channel
    /// lives as long as the client.
    ///
    /// # Errors
    ///
    /// Fails if sshd won't forward to the socket, nothing listens there,
    /// or the daemon doesn't answer; a daemon that answers with another
    /// protocol version says which side to update.
    pub(super) async fn daemon(
        &self,
        socket: &str,
        hello: ClientHello,
    ) -> Result<(Client, Notifications, HelloResponse), DaemonError> {
        let host = &self.host;
        let channel = self
            .handle
            .channel_open_direct_streamlocal(socket)
            .await
            .map_err(|error| {
                DaemonError::Unreachable(match error {
                    russh::Error::ChannelOpenFailure(
                        ChannelOpenFailure::AdministrativelyProhibited,
                    ) => format!(
                        "{host} refused to forward to the Rho daemon's socket {socket}: check \
                         that sshd_config there allows it (AllowStreamLocalForwarding, \
                         DisableForwarding)"
                    ),
                    russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed) => {
                        format!("nothing listens at the Rho daemon's socket {socket} on {host}")
                    }
                    other => {
                        format!("couldn't reach the Rho daemon on {host} at {socket}: {other}")
                    }
                })
            })?;
        let handshake = Client::handshake(channel.into_stream(), hello);
        match tokio::time::timeout(HANDSHAKE_DEADLINE, handshake).await {
            Err(_) => Err(DaemonError::Unreachable(format!(
                "the Rho daemon on {host} at {socket} didn't answer the handshake within {} s",
                HANDSHAKE_DEADLINE.as_secs()
            ))),
            Ok(Ok(connected)) => Ok(connected),
            // The daemon answered; its message names the side to update.
            Ok(Err(error @ (ClientError::VersionMismatch(_) | ClientError::Rpc { .. }))) => {
                Err(error.into())
            }
            Ok(Err(error)) => Err(DaemonError::Unreachable(format!(
                "couldn't reach the Rho daemon on {host} at {socket}: {error}"
            ))),
        }
    }
}

/// Checks the host's key against `known_hosts` while the connection
/// opens. russh's client handler.
pub(super) struct Verifier {
    host: String,
    /// The name the host is filed under in `known_hosts`.
    name: String,
    known: Vec<KnownKey>,
}

/// Why a connection didn't open.
#[derive(Debug)]
pub(super) enum Refusal {
    /// ssh failed, as russh says.
    Ssh(russh::Error),
    /// The host's key isn't the one `known_hosts` lists; why, for people.
    HostKey(String),
}

impl From<russh::Error> for Refusal {
    fn from(error: russh::Error) -> Self {
        Self::Ssh(error)
    }
}

impl Verifier {
    /// Whether the host's key is one `known_hosts` names for it.
    fn check(&self, server_public_key: &PublicKeyOrCertificate) -> Result<bool, Refusal> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_public_key else {
            return Err(Refusal::HostKey(format!(
                "{} offered a host certificate, and rho-ui checks only plain host keys",
                self.host
            )));
        };
        match host_key::verdict(&self.known, key).refusal(&self.host, &self.name) {
            None => Ok(true),
            Some(why) => Err(Refusal::HostKey(why)),
        }
    }
}

impl client::Handler for Verifier {
    type Error = Refusal;

    fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        std::future::ready(self.check(server_public_key))
    }
}
