//! The ssh connections to a host: one for the daemon's socket and every
//! script, and one more for the terminals' shells, made when the first
//! terminal opens ([`Purpose`]). A connection costs a key signature, which an agent
//! such as 1Password's asks the user to approve, so rho-ui makes one of
//! each per host, keeps it while anything uses it, and for [`LINGER`]
//! after, for the next user to take.

use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

use russh::{
    client::{self, Handle, Msg},
    ChannelOpenFailure, ChannelReadHalf, ChannelStream, ChannelWriteHalf, Preferred,
};
use tokio::{
    net::TcpStream,
    sync::{OwnedSemaphorePermit, Semaphore},
};

use super::{
    auth::{self, Identities},
    config::{self, Resolved},
    host_key::{self, KnownKey, Refusal, Verifier},
    read_files,
};
use crate::{daemon::DaemonError, exec::io};

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

/// The session channels sshd allows open at once on one connection: the
/// default of its `MaxSessions` (`sshd_config(5)`: "open shell, login or
/// subsystem (e.g. sftp) sessions"; the daemon's `direct-streamlocal`
/// channel isn't one). Each script and each terminal takes one.
const SSHD_SESSIONS: usize = 10;

/// The most terminals' shells open at once on one remote machine: all of
/// sshd's default `MaxSessions` on the terminals' own connection, so they
/// never take a session a script needs. A host whose `MaxSessions` is
/// lower refuses past it, and that error names it.
pub const MAX_SHELLS: usize = SSHD_SESSIONS;

/// The most scripts running at once on one connection; one past it waits
/// for another to end. Half of sshd's sessions, because sshd goes on
/// counting a session for a moment after its channel closes: it frees
/// the session on the next turn of its loop, after every packet it has
/// read, and the open of the script that took the slot may be among
/// them. So each script at once may count twice, and twice this must
/// fit. Handing scripts over to OpenSSH 8.9's sshd on one machine, 10 at
/// once had it refuse 104 of 1,200, 6 at once 2 of 10,200, and 5 at once
/// none of 10,200. A script given up on while it runs leaves its slot at
/// once, though sshd holds its session until the script's process ends,
/// so sshd may still refuse one, with an error that names `MaxSessions`.
const SCRIPTS_AT_ONCE: usize = SSHD_SESSIONS / 2;

/// What a connection to a host carries. Scripts burst to a handful of
/// sessions at once as a session's turn ends, and a terminal pane keeps a
/// shell for each session it has shown, so the two sharing sshd's 10
/// sessions would leave each too few.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Purpose {
    /// The daemon's socket and every script.
    Daemon,
    /// The terminals' shells.
    Terminals,
}

/// A signed-in ssh connection to a host.
pub(super) struct Connection {
    /// The destination the user named, for messages and `ssh` commands.
    host: String,
    /// How it was reached, to reach the host again so for another
    /// [`Purpose`].
    resolved: Resolved,
    handle: Handle<Verifier>,
    /// A permit for each terminal's shell open on it, of [`MAX_SHELLS`].
    shells: Arc<Semaphore>,
    /// A permit for each script's session open on it, of
    /// [`SCRIPTS_AT_ONCE`].
    scripts: Arc<Semaphore>,
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

/// A lease on the open connection to `host` for `purpose`, opening one if
/// none is: with `route`, else as `ssh -G` resolves the host. Callers for
/// one host and purpose wait on one another, so they share one
/// connection, and the user approves one signature; others don't wait.
/// Runs on the I/O runtime.
///
/// # Errors
///
/// Fails, saying why, if the host can't be reached, its key isn't the
/// one `known_hosts` lists, or it accepts none of the user's keys.
pub(super) async fn shared(
    host: &str,
    route: Option<&Resolved>,
    purpose: Purpose,
) -> Result<Lease, DaemonError> {
    let slot = slot(host, purpose);
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

/// Where one host's connection for one purpose is found while anything
/// holds it.
type Slot = Arc<tokio::sync::Mutex<Weak<Connection>>>;

/// `host`'s slot for `purpose`, one for the life of the process.
fn slot(host: &str, purpose: Purpose) -> Slot {
    static SLOTS: OnceLock<Mutex<HashMap<(String, Purpose), Slot>>> = OnceLock::new();
    let mut slots = SLOTS
        .get_or_init(Mutex::default)
        .lock()
        .expect("the connection slots' mutex is never held across a panic");
    Arc::clone(slots.entry((host.to_owned(), purpose)).or_default())
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
            resolved: resolved.clone(),
            handle,
            shells: Arc::new(Semaphore::new(MAX_SHELLS)),
            scripts: Arc::new(Semaphore::new(SCRIPTS_AT_ONCE)),
        })
    }

    /// The destination the user named.
    pub(super) fn host(&self) -> &str {
        &self.host
    }

    /// How it was reached.
    pub(super) fn resolved(&self) -> &Resolved {
        &self.resolved
    }

    /// Whether the connection is gone, as after a link died.
    pub(super) fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// Opens a session channel, for one command: its reading half, and its
    /// writing half, which closes the channel when dropped. The channel
    /// holds `slot`, from [`Self::script_slot`] or [`Self::shell_slot`],
    /// until it has been told to close.
    ///
    /// The open runs in a task of its own, to its end whether or not the
    /// caller still waits. russh can't take back a channel it has asked
    /// for, and closes none it is never told to, so one whose caller went
    /// before sshd confirmed it would stay open, holding one of sshd's
    /// [`SSHD_SESSIONS`] until the connection ends; ten of those and sshd
    /// refuses every script. The task closes a channel nobody took.
    ///
    /// # Errors
    ///
    /// Fails if the connection is gone, or the host refuses a session, as
    /// at sshd's `MaxSessions`.
    pub(super) async fn session(
        self: &Arc<Self>,
        slot: OwnedSemaphorePermit,
    ) -> Result<(ChannelReadHalf, CloseOnDrop), DaemonError> {
        let host = &self.host;
        if self.is_closed() {
            return Err(DaemonError::Unreachable(format!(
                "no ssh connection to {host}"
            )));
        }
        let connection = Arc::clone(self);
        let opening = io().spawn(async move {
            let (read, write) = connection.handle.channel_open_session().await?.split();
            Ok::<_, russh::Error>((read, CloseOnDrop(Some((write, slot)))))
        });
        opening
            .await
            .map_err(|_| DaemonError::Closed)?
            .map_err(|error| match error {
                russh::Error::ChannelOpenFailure(reason) => DaemonError::Unreachable(format!(
                    "{host} refused an ssh session ({reason:?}): sshd allows \
                     {SSHD_SESSIONS} at once on one connection unless its MaxSessions says \
                     otherwise"
                )),
                other => DaemonError::Unreachable(format!(
                    "couldn't open an ssh session on {host}: {other}"
                )),
            })
    }

    /// A slot for one more script, of [`SCRIPTS_AT_ONCE`], once one is
    /// free: a burst of scripts runs that many at a time, in the order
    /// they asked, rather than have sshd refuse those past its
    /// `MaxSessions`. Dropping the future gives up its place.
    pub(super) async fn script_slot(&self) -> OwnedSemaphorePermit {
        Arc::clone(&self.scripts)
            .acquire_owned()
            .await
            .expect("nothing closes the scripts' semaphore")
    }

    /// A slot for one more terminal's shell, of [`MAX_SHELLS`]; the shell
    /// holds it while it runs, and dropping it frees it. sshd frees the
    /// shell's session a moment later, once the shell has gone, so a
    /// terminal opened at once in its place may meet sshd's own refusal,
    /// whose error names `MaxSessions` too.
    ///
    /// # Errors
    ///
    /// Says why, for the user, when every slot is held.
    pub(super) fn shell_slot(&self) -> Result<OwnedSemaphorePermit, String> {
        Arc::clone(&self.shells).try_acquire_owned().map_err(|_| {
            format!(
                "rho-ui runs at most {MAX_SHELLS} terminals on {host}, the ssh sessions \
                 sshd allows on their connection by default (MaxSessions); this would be \
                 terminal {asked}. Exit a shell on {host}, or close a terminal pane (a pane \
                 keeps a shell for each of the machine's sessions it has shown), to open \
                 another.",
                host = self.host,
                asked = MAX_SHELLS + 1,
            )
        })
    }

    /// A stream to the Unix socket `socket` on the host, such as the Rho
    /// daemon's, over a `direct-streamlocal` channel, as `ssh -W` opens.
    ///
    /// # Errors
    ///
    /// The open runs to its end whether or not the caller still waits, as
    /// [`Self::session`]'s does: the stream closes its channel when
    /// dropped, so a connect given up on leaves no channel to the daemon
    /// open.
    ///
    /// # Errors
    ///
    /// Fails if sshd won't forward to the socket, or nothing listens there.
    pub(super) async fn daemon(
        self: &Arc<Self>,
        socket: &str,
    ) -> Result<ChannelStream<Msg>, DaemonError> {
        let host = &self.host;
        let opening = io().spawn({
            let (connection, socket) = (Arc::clone(self), socket.to_owned());
            async move {
                let channel = connection
                    .handle
                    .channel_open_direct_streamlocal(socket)
                    .await?;
                Ok::<_, russh::Error>(channel.into_stream())
            }
        });
        opening
            .await
            .map_err(|_| DaemonError::Closed)?
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
            })
    }
}

/// A channel's writing half that closes the channel when dropped, as when
/// a script's future is given up, a shell ends or is hung up on, or sshd
/// refused either. Closing one already closed does nothing. With it, the
/// channel's slot on its connection, freed once the close is sent.
pub(super) struct CloseOnDrop(Option<(ChannelWriteHalf<Msg>, OwnedSemaphorePermit)>);

impl CloseOnDrop {
    pub(super) fn channel(&self) -> &ChannelWriteHalf<Msg> {
        let (channel, _) = self.0.as_ref().expect("only dropping takes the channel");
        channel
    }
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if let Some((channel, slot)) = self.0.take() {
            // On the I/O runtime, as the future may drop on any thread.
            io().spawn(async move {
                let _ = channel.close().await;
                drop(slot);
            });
        }
    }
}
