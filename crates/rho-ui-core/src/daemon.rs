//! A connection to the rho daemon and the requests the UI sends it. One
//! connection follows many sessions.
//!
//! ```ignore
//! let target = DaemonTarget::rho("rho").expect("a rho home");
//! let (daemon, mut events) = Daemon::connect(target).await?;
//! let mut session = Session::from_setup(daemon.new_session(&cwd, Checkout::Local).await?);
//! session.attached(daemon.attach(session.id()).await?);
//! session.turn_began(&daemon.prompt(session.id(), &"hello".into()).await?);
//! while let Some(event) = events.recv().await { /* route by session id */ }
//! ```
//!
//! rho-protocol's `Client` reads and writes the socket in Tokio tasks, and
//! the ssh client that carries a remote daemon's socket (`ssh.rs`) runs on
//! Tokio too, so this module keeps one small Tokio runtime for that I/O.
//! Nothing else runs on it: every method here returns a plain future that
//! any executor can await, and replies cross over on channels or task
//! handles.

use std::{
    future::Future,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rho_protocol::{
    methods, Client, ClientEnvironment, ClientError, ClientHello, ClientInfo, ConfigGetParams,
    ConfigLayer, ConfigNamespace, ConfigRevision, ConfigSchemaParams, ConfigSetParams,
    ConfigWriter, DaemonNotification, DetachReason, ErrorCode, FrameError, IdleWake, JobListParams,
    LoginId, LoginOutcome, LoginPrompt, Notifications, RestartParams, SessionAnswerParams,
    SessionAttachParams, SessionBackgroundStopParams, SessionCancelParams, SessionConfigureParams,
    SessionDeleteParams, SessionDetachParams, SessionHoldParams, SessionListParams,
    SessionNewParams, SessionObserveParams, SessionPromptParams, SessionRenameParams,
    SessionSteerParams, SessionUnsteerParams, SettingChange, SignIn, StatusParams, TurnOutcome,
    UnsteerOutcome,
};
use tokio::{runtime::Runtime, task::JoinHandle};

use crate::{
    agent_settings::{AgentSettings, AgentSettingsChanged},
    background::BackgroundId,
    input::{Answer, InputId},
    jobs::Jobs,
    options::{change, config_options, ConfigCategory, ConfigOption, ConfigValue},
    platform,
    prompt::{mebibytes, Prompt},
    session::{Attached, Checkout, SessionSetup, SessionUpdate, SteerId, TurnId, Worktree},
    ssh::{self, SshLink, SshTarget},
};

mod auth;

/// How to reach the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonTarget {
    /// The daemon of the rho home `home`, found where rho-protocol's
    /// `LocalDaemon` says it listens. When none runs there, `rho daemon
    /// start` through the program `rho` starts it.
    Home {
        /// The rho home, such as `~/.rho`.
        home: PathBuf,
        /// The `rho` program that starts the daemon.
        rho: PathBuf,
    },
    /// A daemon already listening on this socket.
    Socket(PathBuf),
    /// The daemon of the default rho home on another machine, reached over
    /// ssh and started there when none runs. Its sessions run in that
    /// machine's login environment.
    Ssh(SshTarget),
}

impl DaemonTarget {
    /// The daemon of the rho home `rho` itself uses
    /// ([`platform::rho_home`]), started through the program `rho`; `None`
    /// when no variable places the rho home.
    pub fn rho(rho: impl Into<PathBuf>) -> Option<Self> {
        Some(Self::Home {
            home: platform::rho_home()?,
            rho: rho.into(),
        })
    }
}

/// Why the daemon could not be reached, or refused a request.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DaemonError {
    /// The daemon could not be started or connected to. Holds the reason,
    /// which is meant for people.
    #[error("{0}")]
    Unreachable(String),
    /// The daemon answered a request with an error. Holds its message,
    /// which is meant for people.
    #[error("{0}")]
    Refused(String),
    /// The session's turn could not take the request now: a prompt found
    /// a turn under way, as one the daemon starts itself when background
    /// work finishes, or a steer found no model request taking input.
    /// Holds the daemon's message.
    #[error("{0}")]
    Busy(String),
    /// A settings write found the settings changed since they were read,
    /// by another client or a hand edit of the file, and wrote nothing.
    /// Holds the daemon's message.
    #[error("{0}")]
    Conflict(String),
    /// The connection is gone.
    #[error("the Rho daemon closed the connection")]
    Closed,
}

impl From<ClientError> for DaemonError {
    fn from(error: ClientError) -> Self {
        // A prompt whose turn ended before it began says why in its outcome.
        if let Some(TurnOutcome::Failed { error }) = error.turn_ended() {
            return Self::Refused(error.message);
        }
        match error {
            ClientError::Closed => Self::Closed,
            ClientError::Rpc { error, .. } => match error.error_code() {
                Some(ErrorCode::SessionBusy | ErrorCode::NotSteerable) => Self::Busy(error.message),
                Some(ErrorCode::ConfigConflict) => Self::Conflict(error.message),
                _ => Self::Refused(error.message),
            },
            // Each of these names the daemon and what to do, as rho's own
            // frontends show them.
            other => Self::Unreachable(other.to_string()),
        }
    }
}

/// A session ID. Only the daemon issues them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(Arc<str>);

impl SessionId {
    /// The ID as the daemon sent it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn to_wire(&self) -> rho_protocol::SessionId {
        rho_protocol::SessionId::new(&*self.0)
    }
}

/// For IDs read back from storage, such as rho-ui's settled list.
impl From<&str> for SessionId {
    fn from(id: &str) -> Self {
        Self(id.into())
    }
}

impl From<rho_protocol::SessionId> for SessionId {
    fn from(id: rho_protocol::SessionId) -> Self {
        Self(id.as_str().into())
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The daemon that answered `rho/hello`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonInfo {
    version: String,
    notice: Option<String>,
    home: Option<PathBuf>,
}

impl DaemonInfo {
    /// rho's version, such as `3.0.0`.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Set when the daemon speaks this client's protocol version but is
    /// another build, so a request may fail: what to tell the user, in the
    /// daemon's words.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The home directory of the user the daemon runs as, on its machine:
    /// what `~` means in that machine's paths. `None` when this process has
    /// no `HOME`, for a daemon on this machine.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }
}

/// A saved session from `rho/session/list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    id: SessionId,
    cwd: PathBuf,
    title: Option<String>,
    updated_at: u64,
}

impl SessionSummary {
    /// Pass to [`Daemon::attach`].
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// The workspace the session runs in.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// rho's title for the session, or until it has one, its first prompt
    /// on one line.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// When the session was last saved, in Unix seconds.
    pub fn updated_at(&self) -> u64 {
        self.updated_at
    }
}

/// What became of a steer [`Daemon::unsteer`] asked to withdraw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsteered {
    /// It is gone before the model read it; forget it with
    /// [`crate::Session::withdraw_steer`].
    Withdrawn,
    /// Too late: the model has it, or will at its next request.
    Kept,
}

/// A newer rho the daemon has found installed over its own program, and
/// runs once nothing would be cut short.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingUpgrade {
    /// The newer rho's version, such as `3.2.0`.
    pub version: String,
    /// How many things it waits for: turns under way or waiting for a
    /// person, logins, and sessions with background work.
    pub waiting_for: u64,
}

/// Why the daemon stopped sending a session's events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detached {
    /// The session's events moved past what this client had read: it fell
    /// too far behind, or the conversation moved to another branch. Attach
    /// again to read it afresh.
    Behind,
    /// The session was deleted.
    Deleted,
    /// An observed session is not loaded: its replay was all of it, or the
    /// subagent run that wrote it ended. Nothing more follows; observe it
    /// again to read it afresh.
    Unloaded,
    /// The daemon is restarting in place, as it does to upgrade. The
    /// connection closes next; connect again.
    Restarting,
}

/// Everything the daemon pushes without being asked.
#[derive(Debug)]
pub enum DaemonEvent {
    /// A change to one session; apply it with [`crate::Session::apply`].
    Update {
        /// The session the change belongs to.
        session: SessionId,
        /// The change itself.
        update: SessionUpdate,
    },
    /// The daemon no longer sends this session's events.
    Detached {
        /// The session that was followed.
        session: SessionId,
        /// Why.
        reason: Detached,
    },
    /// rho's own settings on the daemon's machine changed, by any client
    /// or a hand edit of `config.toml`. Sent once
    /// [`Daemon::agent_settings`] has read them; apply it with
    /// [`AgentSettings::apply`].
    AgentSettingsChanged(AgentSettingsChanged),
    /// What the user must do to finish a login [`Daemon::login`] started:
    /// open a page, and for a device code enter the code there.
    LoginPrompt {
        /// The login, as [`Daemon::login`] returned it.
        login: LoginId,
        /// The page to open, any code to enter there, and what to do.
        prompt: LoginPrompt,
    },
    /// A login [`Daemon::login`] started ended, its last event. One that
    /// succeeded signed the daemon in for every session.
    LoginFinished {
        /// The login, as [`Daemon::login`] returned it.
        login: LoginId,
        /// Whether it succeeded, failed (and why) or was cancelled.
        outcome: LoginOutcome,
    },
}

/// The daemon's event stream. It ends when the connection does.
pub struct DaemonEvents {
    notifications: Notifications,
}

impl std::fmt::Debug for DaemonEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonEvents").finish_non_exhaustive()
    }
}

impl DaemonEvents {
    /// Waits for the next event; `None` once the connection has closed.
    pub async fn recv(&mut self) -> Option<DaemonEvent> {
        loop {
            match self.notifications.recv().await? {
                DaemonNotification::SessionEvent(event) => {
                    return Some(DaemonEvent::Update {
                        session: event.session.into(),
                        update: SessionUpdate(event.event),
                    });
                }
                DaemonNotification::SessionDetached(detached) => {
                    let reason = match detached.reason {
                        DetachReason::Lagged => Detached::Behind,
                        DetachReason::Deleted => Detached::Deleted,
                        DetachReason::Restarting => Detached::Restarting,
                        DetachReason::NotLoaded => Detached::Unloaded,
                        // `daemon_stopping` is followed by the connection
                        // closing, which says as much; the rest a newer
                        // daemon added.
                        _ => continue,
                    };
                    return Some(DaemonEvent::Detached {
                        session: detached.session.into(),
                        reason,
                    });
                }
                DaemonNotification::ConfigChanged(changed)
                    if changed.namespace == ConfigNamespace::Agent =>
                {
                    return Some(DaemonEvent::AgentSettingsChanged(AgentSettingsChanged(
                        changed,
                    )));
                }
                DaemonNotification::LoginPrompt(prompt) => {
                    return Some(DaemonEvent::LoginPrompt {
                        login: prompt.login,
                        prompt: prompt.prompt,
                    });
                }
                DaemonNotification::LoginFinished(finished) => {
                    return Some(DaemonEvent::LoginFinished {
                        login: finished.login,
                        outcome: finished.outcome,
                    });
                }
                // Other clients' settings, which this client does not use.
                _ => {}
            }
        }
    }
}

/// A connected daemon. Cheap to clone; the connection closes when the last
/// clone is dropped, and the daemon keeps running, with its sessions.
#[derive(Clone)]
pub struct Daemon {
    client: Arc<Client>,
    info: Arc<DaemonInfo>,
    /// What every request that carries an environment sends, which rho
    /// makes the session's whole process environment.
    environment: Arc<ClientEnvironment>,
    ssh: Option<SshLink>,
}

/// The machine the daemon runs on, as the sessions this client opens see it.
#[derive(Debug)]
pub(crate) struct Machine {
    /// The environment sessions run in.
    pub(crate) environment: ClientEnvironment,
    /// The home directory there.
    pub(crate) home: Option<PathBuf>,
    /// How to run commands there, for a remote machine.
    pub(crate) ssh: Option<SshLink>,
}

impl Machine {
    /// This machine, as this process sees it.
    fn this_process() -> Self {
        Self {
            environment: ClientEnvironment::of_this_process(),
            home: platform::home_dir(),
            ssh: None,
        }
    }
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

// UI code hands these to background tasks; keep them thread-safe.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    const fn assert_send<T: Send>() {}
    assert_send_sync::<Daemon>();
    assert_send::<DaemonEvents>();
};

/// The runtime the connection's socket I/O runs on, made on first use and
/// kept for the life of the process. One worker: the work is the daemon
/// sockets' reads and writes, `LocalDaemon` waiting on `rho daemon start`,
/// and for a daemon on another machine, the ssh connection that carries
/// its socket and the scripts run there.
pub(crate) fn io() -> &'static Runtime {
    static IO: OnceLock<Runtime> = OnceLock::new();
    IO.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("rho-ui-daemon-io")
            .enable_io()
            // The handshake's deadline, those on a remote machine's
            // scripts, and the ssh connection's keepalives; see `ssh.rs`.
            .enable_time()
            .build()
            .expect("the OS lets this process start a thread and an I/O driver")
    })
}

/// Runs `future` on the I/O runtime and waits for it. Dropping the future
/// this returns stops the task, and drops what the task holds, such as a
/// half-open connection or a script's ssh channel, so a caller that stops
/// waiting leaves nothing running.
///
/// # Errors
///
/// [`DaemonError::Closed`] if the task panicked.
pub(crate) async fn on_io<F>(future: F) -> Result<F::Output, DaemonError>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut task = AbortOnDrop(io().spawn(future));
    (&mut task.0).await.map_err(|_| DaemonError::Closed)
}

/// A task on the I/O runtime that stops when this is dropped.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        // Does nothing to a task that has finished.
        self.0.abort();
    }
}

/// How long a daemon may take to answer the handshake once its socket
/// connected, at a socket target or over an ssh channel. A daemon that accepts
/// and never answers would otherwise hold the connection attempt forever.
/// The default local target has none: rho-protocol's `LocalDaemon` does
/// that handshake itself, with no deadline, until rho-protocol adds one.
/// The handshake is one round trip over a socket that already connected,
/// so this is a tripwire, not a budget: it matches the ssh setup's
/// deadline, which covers a whole login and `rho daemon start`.
pub(crate) const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(30);

async fn open(target: DaemonTarget) -> Result<(Daemon, DaemonEvents), DaemonError> {
    let hello = ClientHello::frontend(ClientInfo {
        name: "rho-ui".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    });
    let (client, notifications, hello, machine) = match target {
        DaemonTarget::Home { home, rho } => {
            let rho = Command::new(crate::rho_install::find_rho(&rho));
            let (client, notifications, hello) =
                platform::endpoint::connect_or_start(home, rho, hello).await?;
            (client, notifications, hello, Machine::this_process())
        }
        DaemonTarget::Socket(socket) => {
            let stream = platform::endpoint::connect(&socket)
                .await
                .map_err(|error| {
                    DaemonError::Unreachable(format!(
                        "could not connect to the Rho daemon at {}: {error}",
                        socket.display()
                    ))
                })?;
            let (client, notifications, hello) =
                tokio::time::timeout(HANDSHAKE_DEADLINE, Client::handshake(stream, hello))
                    .await
                    .map_err(|_| {
                        DaemonError::Unreachable(format!(
                            "the Rho daemon at {} didn't answer the handshake within {} s",
                            socket.display(),
                            HANDSHAKE_DEADLINE.as_secs()
                        ))
                    })??;
            (client, notifications, hello, Machine::this_process())
        }
        DaemonTarget::Ssh(target) => ssh::connect(&target, hello).await?,
    };
    let info = DaemonInfo {
        version: hello.daemon.version,
        notice: hello.skew.map(|skew| skew.to_string()),
        home: machine.home,
    };
    let daemon = Daemon {
        client: Arc::new(client),
        info: Arc::new(info),
        environment: Arc::new(machine.environment),
        ssh: machine.ssh,
    };
    let events = DaemonEvents { notifications };
    Ok((daemon, events))
}

/// Names this process's settings writes (see `ConfigSetParams::writer`):
/// a revision stays good for a writer while only its own writes followed
/// it, so two quick changes from this app never conflict with each other.
/// Unique to this run of the app: its process ID and when it first wrote.
fn writer() -> &'static ConfigWriter {
    static WRITER: OnceLock<ConfigWriter> = OnceLock::new();
    WRITER.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        ConfigWriter::new(format!("rho-ui-{}-{nanos}", std::process::id()))
    })
}

/// What every session this client opens gets, as rho's terminal UI asks:
/// `idle_wake: turn`, so a turn ends while the background work it started
/// runs on, and the daemon starts a turn of its own when that work
/// finishes; and `sign_in: later`, so a session whose model has no
/// credentials opens signed out, for the user to sign in from it.
fn session_setup() -> rho_protocol::SessionSetup {
    rho_protocol::SessionSetup {
        idle_wake: IdleWake::Turn,
        sign_in: SignIn::Later,
        ..Default::default()
    }
}

impl Daemon {
    /// Connects to the daemon, starting it first if `target` says how.
    /// Dropping the future gives up the attempt: whatever it started to
    /// reach the daemon, such as an ssh connection still signing in or the
    /// setup script on the machine, stops with it. An ssh connection that
    /// signed in stays for 10 minutes, for the next attempt to reach that
    /// machine.
    ///
    /// # Errors
    ///
    /// Fails if no daemon runs and rho cannot be run or its daemon does not
    /// start, the socket refuses the connection, ssh cannot reach the
    /// machine, the daemon doesn't answer the handshake in time, or it
    /// speaks another protocol version; the message then says which side
    /// to update.
    pub async fn connect(target: DaemonTarget) -> Result<(Daemon, DaemonEvents), DaemonError> {
        on_io(open(target)).await?
    }

    /// Who answered `rho/hello`.
    pub fn info(&self) -> &DaemonInfo {
        &self.info
    }

    /// The ssh link to the daemon's machine, for a remote one.
    pub fn ssh(&self) -> Option<&SshLink> {
        self.ssh.as_ref()
    }

    /// Saved sessions in every workspace, newest first.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects the request or the connection is gone.
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, DaemonError> {
        let mut sessions = Vec::new();
        let mut cursor = None;
        loop {
            let params = SessionListParams {
                cwd: None,
                cursor,
                limit: None,
            };
            let page = self.client.request::<methods::SessionList>(&params).await?;
            sessions.extend(page.sessions.into_iter().map(|session| SessionSummary {
                id: session.session.into(),
                cwd: session.cwd,
                title: session.title.or(session.first_user_message),
                updated_at: session.updated_at,
            }));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(sessions),
            }
        }
    }

    /// The upgrade the daemon waits to run, if its program was replaced.
    /// Nothing pushes it; ask again to refresh.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects the request or the connection is gone.
    pub async fn pending_upgrade(&self) -> Result<Option<PendingUpgrade>, DaemonError> {
        let status = self
            .client
            .request::<methods::Status>(&StatusParams {})
            .await?;
        Ok(status.upgrade.map(|upgrade| PendingUpgrade {
            version: upgrade.version,
            waiting_for: upgrade.waiting_for,
        }))
    }

    /// Restarts the daemon in place now, into the program installed: every
    /// followed session gets [`Detached::Restarting`] and the connection
    /// closes, so connect again. Turns under way are cut short; a rho that
    /// resumes them carries them on once it is back.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects the request or the connection is gone.
    pub async fn restart(&self) -> Result<(), DaemonError> {
        self.client
            .request::<methods::Restart>(&RestartParams {})
            .await?;
        Ok(())
    }

    /// Lists the daemon's jobs: the sessions with work under way,
    /// whichever client started them (a turn running or queued, a request
    /// waiting for a person, or subagents or workflow runs still under way
    /// after the turn that started them), and the runs each session
    /// started. Nothing pushes changes to this list; ask again to refresh.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects the request or the connection is gone.
    pub async fn jobs(&self) -> Result<Jobs, DaemonError> {
        let list = self
            .client
            .request::<methods::JobList>(&JobListParams {})
            .await?;
        Ok(Jobs::from_list(list.jobs))
    }

    /// Opens a new session in `cwd`, or in a new worktree of the
    /// repository `cwd` is in, as `checkout` says. Nothing follows it yet:
    /// create its [`crate::Session`], then [`Self::attach`]. Until its
    /// first prompt the session is saved nowhere, and the daemon discards
    /// it once no client follows it, with its worktree when that is still
    /// clean.
    ///
    /// # Errors
    ///
    /// Fails if the daemon cannot open the session, as when `cwd` is in no
    /// git repository for a new worktree, or the connection is gone. A
    /// model with no credentials opens signed out, and its turns fail until
    /// a sign-in; see [`crate::Session::auth`].
    pub async fn new_session(
        &self,
        cwd: &Path,
        checkout: Checkout,
    ) -> Result<SessionSetup, DaemonError> {
        let params = SessionNewParams {
            cwd: cwd.to_path_buf(),
            environment: ClientEnvironment::clone(&self.environment),
            agent: None,
            setup: session_setup(),
            parent: None,
            grant: None,
            worktree: checkout == Checkout::NewWorktree,
        };
        let response = self.client.request::<methods::SessionNew>(&params).await?;
        Ok(SessionSetup {
            id: response.session.into(),
            options: response.options,
            worktree: response.worktree.map(|worktree| Worktree {
                branch: worktree.branch,
                path: worktree.path,
            }),
        })
    }

    /// Follows session `id`, reopening it in the daemon if it was unloaded.
    /// The daemon replays the conversation as [`DaemonEvent::Update`]s,
    /// then sends what happens next. The replay may arrive before this
    /// returns, so create the session's [`crate::Session`] first; it keeps
    /// the events until [`crate::Session::attached`] gets this answer.
    ///
    /// # Errors
    ///
    /// Fails if the session does not exist, another process such as a 2.x
    /// rho has it open, or the connection is gone.
    pub async fn attach(&self, id: &SessionId) -> Result<Attached, DaemonError> {
        let params = SessionAttachParams {
            session: id.to_wire(),
            after: None,
            environment: ClientEnvironment::clone(&self.environment),
            setup: session_setup(),
        };
        let response = self
            .client
            .request::<methods::SessionAttach>(&params)
            .await?;
        Ok(Attached(Box::new(response)))
    }

    /// Attaches to session `id` and lets it go at once, dropping the events
    /// that arrive meanwhile, which no [`crate::Session`] reads. After the
    /// daemon restarted in place, a rho that resumes turns carries on the
    /// one the restart cut short when a client attaches, as
    /// `idle_wake: turn` (see [`session_setup`]) lets it start turns of
    /// its own.
    ///
    /// # Errors
    ///
    /// Fails if the session does not exist or the connection is gone.
    pub async fn resume(&self, id: &SessionId) -> Result<(), DaemonError> {
        self.attach(id).await?;
        self.detach(id).await
    }

    /// Follows session `id` without taking part in it, as for a subagent's
    /// session, which only the subagent writes and which can't be attached
    /// to. A loaded session is followed as [`Self::attach`] follows it; an
    /// unloaded one replays as saved, then [`Detached::Unloaded`] ends it,
    /// as it ends a subagent's run. Events arrive as for an attach, so
    /// create the session's [`crate::Session`] first.
    ///
    /// # Errors
    ///
    /// Fails if the session does not exist, a subagent's run has not opened
    /// it yet, or the connection is gone.
    pub async fn observe(&self, id: &SessionId) -> Result<Attached, DaemonError> {
        let params = SessionObserveParams {
            session: id.to_wire(),
            after: None,
        };
        let response = self
            .client
            .request::<methods::SessionObserve>(&params)
            .await?;
        Ok(Attached(Box::new(response)))
    }

    /// Stops following session `id`. The session and any turn it runs stay
    /// in the daemon.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects the request or the connection is gone.
    pub async fn detach(&self, id: &SessionId) -> Result<(), DaemonError> {
        let params = SessionDetachParams {
            session: id.to_wire(),
        };
        self.client
            .request::<methods::SessionDetach>(&params)
            .await?;
        Ok(())
    }

    /// Sends a prompt. Returns once its turn began; pass the turn to
    /// [`crate::Session::turn_began`]. The turn itself streams as events.
    ///
    /// # Errors
    ///
    /// Fails if the daemon refuses the prompt, or the turn ended before it
    /// began; [`DaemonError::Busy`] when the session already runs a turn.
    /// Nothing was added to the session in that case. A prompt too large
    /// for one message to the daemon (see [`crate::MAX_PROMPT_ATTACHMENT_BYTES`])
    /// is [`DaemonError::Refused`], and the connection stays up.
    pub async fn prompt(&self, id: &SessionId, prompt: &Prompt) -> Result<TurnId, DaemonError> {
        let params = SessionPromptParams {
            session: id.to_wire(),
            content: prompt.to_wire(),
            timeout_ms: None,
            environment: ClientEnvironment::clone(&self.environment),
            grant: None,
            idempotency_key: None,
            author: rho_protocol::PromptAuthor::Person,
        };
        let response = self
            .client
            .request::<methods::SessionPrompt>(&params)
            .await
            .map_err(|error| match error {
                // Refused before any of it was written.
                ClientError::Frame(FrameError::LineTooLong { limit, observed }) => {
                    DaemonError::Refused(format!(
                        "it is {} encoded, over the {} the daemon reads in one message; \
                         send fewer or smaller attachments",
                        mebibytes(observed),
                        mebibytes(limit)
                    ))
                }
                other => other.into(),
            })?;
        Ok(TurnId::from_wire(&response.turn))
    }

    /// Adds `text` to the running turn's next model request, whichever
    /// client started the turn. Returns once the turn accepted it; record
    /// it with [`crate::Session::steer_sent`]. It reaches the conversation
    /// as a user message when the model reads it, and a turn that ends
    /// first drops it.
    ///
    /// # Errors
    ///
    /// Fails with [`DaemonError::Busy`] if no model request is taking
    /// input, as between turns.
    pub async fn steer(&self, id: &SessionId, text: &str) -> Result<SteerId, DaemonError> {
        let params = SessionSteerParams {
            session: id.to_wire(),
            text: text.to_owned(),
        };
        let response = self
            .client
            .request::<methods::SessionSteer>(&params)
            .await?;
        Ok(SteerId(response.id.as_str().to_owned()))
    }

    /// Withdraws steer `steer` of session `id` before the model reads it.
    ///
    /// # Errors
    ///
    /// Fails if no model request is taking input, as between turns, or the
    /// connection is gone.
    pub async fn unsteer(&self, id: &SessionId, steer: &SteerId) -> Result<Unsteered, DaemonError> {
        let params = SessionUnsteerParams {
            session: id.to_wire(),
            id: rho_protocol::SteerId::new(steer.0.as_str()),
        };
        let response = self
            .client
            .request::<methods::SessionUnsteer>(&params)
            .await?;
        Ok(match response.outcome {
            // `not_found`: the turn that held it ended, which dropped it.
            UnsteerOutcome::Withdrawn | UnsteerOutcome::NotFound => Unsteered::Withdrawn,
            _ => Unsteered::Kept,
        })
    }

    /// Stops the running turn, if any.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects the request or the connection is gone.
    pub async fn cancel(&self, id: &SessionId) -> Result<(), DaemonError> {
        let params = SessionCancelParams {
            session: id.to_wire(),
        };
        self.client
            .request::<methods::SessionCancel>(&params)
            .await?;
        Ok(())
    }

    /// Asks the daemon to stop background work `work` of session `id`.
    /// Returns once the stop is asked for: the work ends shortly after,
    /// and the session's background list then says how.
    ///
    /// # Errors
    ///
    /// Fails if the work already ended or the session does not know it,
    /// the session is not loaded, or the connection is gone.
    pub async fn stop_background(
        &self,
        id: &SessionId,
        work: &BackgroundId,
    ) -> Result<(), DaemonError> {
        let params = SessionBackgroundStopParams {
            session: id.to_wire(),
            work: work.0.clone(),
        };
        self.client
            .request::<methods::SessionBackgroundStop>(&params)
            .await?;
        Ok(())
    }

    /// Answers open request `request` of session `id` with `answer`. A
    /// request that already closed, as when another client answered first,
    /// counts as answered.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects the answer or the connection is gone;
    /// the request is then still open.
    pub async fn answer(
        &self,
        id: &SessionId,
        request: InputId,
        answer: &Answer,
    ) -> Result<(), DaemonError> {
        let params = SessionAnswerParams {
            session: id.to_wire(),
            request: rho_protocol::InputRequestId(request.0),
            answer: answer.0.clone(),
        };
        match self.client.request::<methods::SessionAnswer>(&params).await {
            Ok(_) => Ok(()),
            Err(error) if error.error_code() == Some(ErrorCode::InputNotOpen) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Stops open request `request`'s timeout, so it waits for an answer,
    /// as once the user starts on it. The session then hears the request
    /// was held. A request that closed meanwhile needs no holding.
    ///
    /// # Errors
    ///
    /// Fails if the daemon refuses or the connection is gone; the request
    /// may then still time out.
    pub async fn hold(&self, id: &SessionId, request: InputId) -> Result<(), DaemonError> {
        let params = SessionHoldParams {
            session: id.to_wire(),
            request: rho_protocol::InputRequestId(request.0),
        };
        match self.client.request::<methods::SessionHold>(&params).await {
            Ok(_) => Ok(()),
            Err(error) if error.error_code() == Some(ErrorCode::InputNotOpen) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Changes a session setting. Returns every option, since one change can
    /// move others (a new model has different reasoning levels). While a
    /// turn runs, the daemon applies the change, and answers, once the turn
    /// ends.
    ///
    /// # Errors
    ///
    /// Fails if the session has no such option, the daemon rejects the
    /// value, or the connection is gone.
    pub async fn set_config(
        &self,
        id: &SessionId,
        config_id: &str,
        value: ConfigValue,
    ) -> Result<Vec<ConfigOption>, DaemonError> {
        let change = change(config_id, &value)
            .ok_or_else(|| DaemonError::Refused(format!("{config_id} doesn't take {value:?}")))?;
        let params = SessionConfigureParams {
            session: id.to_wire(),
            change,
        };
        let response = self
            .client
            .request::<methods::SessionConfigure>(&params)
            .await?;
        Ok(config_options(&response.options))
    }

    /// Sets session `id`'s settings, now `current`, to those of `wanted`,
    /// such as another session's: the model first, since a model offers
    /// its own reasoning levels, then the rest. A setting the session lacks
    /// is skipped. Returns every option as the last change left them.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects a value or the connection is gone; the
    /// changes before it stay.
    pub async fn match_config(
        &self,
        id: &SessionId,
        wanted: &[ConfigOption],
        mut current: Vec<ConfigOption>,
    ) -> Result<Vec<ConfigOption>, DaemonError> {
        let order = [
            ConfigCategory::Model,
            ConfigCategory::ThoughtLevel,
            ConfigCategory::Other,
            ConfigCategory::Mode,
        ];
        for category in order {
            for want in wanted.iter().filter(|want| want.category() == category) {
                let differs = current
                    .iter()
                    .find(|option| option.id() == want.id())
                    .is_some_and(|option| option.value() != want.value());
                if differs {
                    current = self.set_config(id, want.id(), want.value()).await?;
                }
            }
        }
        Ok(current)
    }

    /// rho's own settings on the daemon's machine: every setting the daemon
    /// describes, with its value. From then on the connection sends
    /// [`DaemonEvent::AgentSettingsChanged`] whenever a value or the file's
    /// problems change, whoever changed them; reading again replaces that.
    ///
    /// # Errors
    ///
    /// Fails if the daemon has no settings to describe, as an older rho, or
    /// the connection is gone.
    pub async fn agent_settings(&self) -> Result<AgentSettings, DaemonError> {
        let schema = self
            .client
            .request::<methods::ConfigSchemaMethod>(&ConfigSchemaParams {})
            .await?;
        let params = ConfigGetParams {
            namespace: ConfigNamespace::Agent,
        };
        let read = self.client.request::<methods::ConfigGet>(&params).await?;
        Ok(AgentSettings::new(schema, read))
    }

    /// Writes `changes` to the user's own `config.toml`, all of them or
    /// none, against `revision`, the one [`AgentSettings::revision`] held
    /// when the user made them. Send the next write only once this
    /// returns: writes in flight together may land in either order. The
    /// new values arrive as [`DaemonEvent::AgentSettingsChanged`].
    ///
    /// # Errors
    ///
    /// [`DaemonError::Conflict`] if another client or a hand edit changed
    /// the settings since `revision`: read them again. Also fails if the
    /// daemon refuses a value, the file doesn't load, or the connection is
    /// gone. Nothing was written then.
    pub async fn set_agent_settings(
        &self,
        revision: ConfigRevision,
        changes: Vec<SettingChange>,
    ) -> Result<(), DaemonError> {
        let params = ConfigSetParams {
            namespace: ConfigNamespace::Agent,
            layer: ConfigLayer::Global,
            revision,
            writer: Some(writer().clone()),
            changes,
        };
        self.client.request::<methods::ConfigSet>(&params).await?;
        Ok(())
    }

    /// Saves `title` as the session's title, which a generated title never
    /// replaces. Clients attached to it get the title as a
    /// `TitleChanged` event.
    ///
    /// # Errors
    ///
    /// Fails if the daemon refuses the title (empty, a control character,
    /// or over its 500 characters), doesn't know the session, or the
    /// connection is gone.
    pub async fn rename_session(&self, id: &SessionId, title: &str) -> Result<(), DaemonError> {
        let params = SessionRenameParams {
            session: id.to_wire(),
            title: title.to_owned(),
        };
        self.client
            .request::<methods::SessionRename>(&params)
            .await?;
        Ok(())
    }

    /// Deletes a saved session for good, unless a client follows it: the
    /// daemon then refuses, so a session open in rho's terminal UI is not
    /// deleted under it. [`Self::detach`] this client's own attachment
    /// first.
    ///
    /// # Errors
    ///
    /// Fails if the session is busy or followed, the daemon rejects the
    /// request, or the connection is gone.
    pub async fn delete_session(&self, id: &SessionId) -> Result<(), DaemonError> {
        let params = SessionDeleteParams {
            session: id.to_wire(),
            unless_attached: true,
        };
        self.client
            .request::<methods::SessionDelete>(&params)
            .await?;
        Ok(())
    }
}
