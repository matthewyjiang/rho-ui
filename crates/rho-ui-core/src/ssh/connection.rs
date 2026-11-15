//! One ssh connection per host, shared by everything rho-ui does there:
//! the daemon's socket, and every script. A connection costs a key
//! signature, which an agent such as 1Password's asks the user to
//! approve, so rho-ui makes one per host, keeps it while anything uses
//! it, and for [`LINGER`] after, for the next user to take.

use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

use russh::{
    client::{self, Handle, Msg},
    Channel, ChannelOpenFailure, ChannelStream, Preferred,
};
use tokio::net::TcpStream;

use super::{
    auth::{self, Identities},
    config::{self, Resolved},
    host_key::{self, KnownKey, Refusal, Verifier},
    read_files,
};
use crate::daemon::{io, DaemonError};

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

/// How long a connection stays open after its last user lets it go, so a
/// reconnect or retry soon after signs in no more: parity with the
/// `ControlPersist 600` of the OpenSSH master it replaces. A machine
/// removed in Settings, or no longer reached, lets its login session on
/// the host go after this.
const LINGER: Duration = Duration::from_secs(600);

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

/// A use of a host's shared connection. The connection stays open while
/// any lease on it does, and for [`LINGER`] after the last goes.
#[derive(Debug)]
pub(super) struct Lease {
    connection: Arc<Connection>,
}

impl Lease {
    pub(super) fn connection(&self) -> &Arc<Connection> {
        &self.connection
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.connection.is_closed() {
            return;
        }
        let connection = Arc::clone(&self.connection);
        // On the I/O runtime, as a lease may drop on any thread. Each
        // lease that goes keeps the connection that long, so the last
        // one's keeps it longest.
        io().spawn(async move {
            tokio::time::sleep(LINGER).await;
            drop(connection);
        });
    }
}

/// A lease on the open connection to `host`, opening one if none is:
/// with `route`, else as `ssh -G` resolves the host. Callers for one host
/// wait on one another, so they share one connection, and the user
/// approves one signature; other hosts don't wait. Runs on the I/O
/// runtime.
///
/// # Errors
///
/// Fails, saying why, if the host can't be reached, its key isn't the
/// one `known_hosts` lists, or it accepts none of the user's keys.
pub(super) async fn shared(host: &str, route: Option<&Resolved>) -> Result<Lease, DaemonError> {
    let slot = slot(host);
    let mut held = slot.lock().await;
    if let Some(open) = held.upgrade().filter(|connection| !connection.is_closed()) {
        return Ok(Lease { connection: open });
    }
    let resolved = match route {
        Some(route) => route.clone(),
        None => config::resolve(host).await?,
    };
    let credentials = {
        let resolved = resolved.clone();
        read_files(move || Credentials::read(&resolved))
            .await?
            .map_err(DaemonError::Unreachable)?
    };
    let connection = Arc::new(Connection::open(host, &resolved, credentials).await?);
    *held = Arc::downgrade(&connection);
    Ok(Lease { connection })
}

/// Where one host's connection is found while anything holds it.
type Slot = Arc<tokio::sync::Mutex<Weak<Connection>>>;

/// `host`'s slot, one for the life of the process.
fn slot(host: &str) -> Slot {
    static SLOTS: OnceLock<Mutex<HashMap<String, Slot>>> = OnceLock::new();
    let mut slots = SLOTS
        .get_or_init(Mutex::default)
        .lock()
        .expect("the connection slots' mutex is never held across a panic");
    Arc::clone(slots.entry(host.to_owned()).or_default())
}

/// What connecting reads from disk, read before it starts so the I/O
/// runtime's one worker, which every daemon socket shares, never waits on
/// the disk.
struct Credentials {
    /// What the `known_hosts` and `RevokedHostKeys` files list for the
    /// host.
    known: Vec<KnownKey>,
    /// The key and certificate files to sign in with.
    identities: Identities,
}

impl Credentials {
    /// Reads what `resolved` names.
    ///
    /// # Errors
    ///
    /// Fails, saying why, if the `RevokedHostKeys` file can't be checked.
    fn read(resolved: &Resolved) -> Result<Self, String> {
        Ok(Self {
            known: host_key::read(resolved)?,
            identities: Identities::read(resolved),
        })
    }
}

impl Connection {
    /// Connects to `host` as `resolved` says, checks its key, and signs in
    /// with `credentials`.
    async fn open(
        host: &str,
        resolved: &Resolved,
        credentials: Credentials,
    ) -> Result<Self, DaemonError> {
        let Credentials { known, identities } = credentials;
        let config = client::Config {
            preferred: Preferred {
                key: Cow::Owned(host_key::preferred(&known)),
                ..Preferred::DEFAULT
            },
            keepalive_interval: Some(KEEPALIVE_INTERVAL),
            keepalive_max: KEEPALIVE_MAX,
            ..client::Config::default()
        };
        let verifier = Verifier::new(host, resolved, known);
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
        auth::sign_in(&mut handle, host, resolved, &identities).await?;
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

    /// A stream to the Unix socket `socket` on the host, such as the Rho
    /// daemon's, over a `direct-streamlocal` channel, as `ssh -W` opens.
    ///
    /// # Errors
    ///
    /// Fails if sshd won't forward to the socket, or nothing listens there.
    pub(super) async fn daemon(&self, socket: &str) -> Result<ChannelStream<Msg>, DaemonError> {
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
        Ok(channel.into_stream())
    }
}
