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
//! rho-protocol's `Client` reads and writes the socket in Tokio tasks, so
//! this module keeps one small Tokio runtime for that I/O. Nothing else runs
//! on it: every method here returns a plain future that any executor can
//! await, and replies cross over on channels.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::Duration,
};

use rho_protocol::{
    methods, Client, ClientEnvironment, ClientError, ClientHello, ClientInfo, DaemonNotification,
    DetachReason, ErrorCode, FrameError, IdleWake, JobKind, JobListParams, JobState, JobSummary,
    LocalDaemon, LocalDaemonError, Notifications, SessionAnswerParams, SessionAttachParams,
    SessionBackgroundStopParams, SessionCancelParams, SessionConfigureParams, SessionDeleteParams,
    SessionDetachParams, SessionHoldParams, SessionListParams, SessionNewParams,
    SessionObserveParams, SessionPromptParams, SessionSteerParams, SessionUnsteerParams,
    TurnOutcome, UnsteerOutcome,
};
use tokio::{net::UnixStream, runtime::Runtime, sync::oneshot};

use crate::{
    background::BackgroundId,
    input::{Answer, InputId},
    options::{change, config_options, ConfigCategory, ConfigOption, ConfigValue},
    prompt::{mebibytes, Prompt},
    session::{Attached, Checkout, SessionSetup, SessionUpdate, SteerId, TurnId, Worktree},
    ssh::{self, SshTarget},
};

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
    /// The daemon of the rho home `rho` itself uses, started through the
    /// program `rho`; `None` when neither `RHO_HOME` nor `HOME` is set.
    pub fn rho(rho: impl Into<PathBuf>) -> Option<Self> {
        Some(Self::Home {
            home: rho_home(|name| std::env::var_os(name))?,
            rho: rho.into(),
        })
    }
}

/// The rho home as `rho` finds it: `$RHO_HOME`, else `$HOME/.rho`, with an
/// empty variable taken as unset. rho-protocol does not export this rule,
/// so it is copied from rho's `paths::rho_dir`, without its Windows
/// fallbacks (rho-ui does not build there).
fn rho_home(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |name| var(name).filter(|value| !value.is_empty());
    set("RHO_HOME")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".rho")))
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
    /// The connection is gone.
    #[error("the rho daemon closed the connection")]
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
                _ => Self::Refused(error.message),
            },
            // Each of these names the daemon and what to do, as rho's own
            // frontends show them.
            other => Self::Unreachable(other.to_string()),
        }
    }
}

impl From<LocalDaemonError> for DaemonError {
    fn from(error: LocalDaemonError) -> Self {
        match error {
            LocalDaemonError::Handshake(error) => error.into(),
            // Each names the command or socket that failed, and why.
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

/// What a session's job is doing, from `rho/job/list`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobActivity {
    /// A turn runs, or waits for a slot.
    Working,
    /// The session waits for a person: an approval or a question.
    Waiting,
}

/// A session with work under way in the daemon, whichever client started it.
#[derive(Clone, Debug)]
pub struct Job {
    session: SessionId,
    cwd: PathBuf,
    title: Option<String>,
    activity: JobActivity,
    turn_elapsed: Option<Duration>,
}

impl Job {
    /// The session the job runs. Pass to [`Daemon::attach`] to follow it.
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    /// The workspace the job runs in.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The session's title, once it has one.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// What the job is doing.
    pub fn activity(&self) -> JobActivity {
        self.activity
    }

    /// How long the turn under way has run, when the daemon says. It grows
    /// between two listings of the same job.
    pub fn turn_elapsed(&self) -> Option<Duration> {
        self.turn_elapsed
    }

    /// Whether `other` is this job as an earlier or later listing would
    /// show it: everything but [`Self::turn_elapsed`] is the same. Two
    /// listings that differ only in elapsed time changed nothing to show.
    pub fn is_same_work(&self, other: &Job) -> bool {
        self.session == other.session
            && self.cwd == other.cwd
            && self.title == other.title
            && self.activity == other.activity
    }

    /// The job as the sidebar needs it, or `None` for one that is not a
    /// session with work under way.
    fn from_wire(job: JobSummary) -> Option<Self> {
        // A subagent's or workflow's session belongs to the session that
        // started it, and can't be attached to.
        if job.kind != JobKind::Session {
            return None;
        }
        let live = job.live.as_ref();
        let asks = live.is_some_and(|live| !live.inputs.is_empty());
        let activity = match job.state {
            JobState::WaitingForInput => JobActivity::Waiting,
            // A subagent's question waits whether or not a turn runs.
            _ if asks => JobActivity::Waiting,
            JobState::Running | JobState::Queued => JobActivity::Working,
            _ => return None,
        };
        Some(Self {
            session: job.session?.into(),
            cwd: job.workspace,
            turn_elapsed: live
                .and_then(|live| live.turn_elapsed_ms)
                .map(Duration::from_millis),
            title: job.live.and_then(|live| live.title),
            activity,
        })
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
                // Logins and settings, which this client does not use.
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
}

/// The machine the daemon runs on, as the sessions this client opens see it.
#[derive(Debug)]
pub(crate) struct Machine {
    /// The environment sessions run in.
    pub(crate) environment: ClientEnvironment,
    /// The home directory there.
    pub(crate) home: Option<PathBuf>,
}

impl Machine {
    /// This machine, as this process sees it.
    fn this_process() -> Self {
        Self {
            environment: ClientEnvironment::of_this_process(),
            home: std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from),
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
/// kept for the life of the process. One worker: the work is one socket's
/// reads and writes, and `LocalDaemon` waiting on `rho daemon start`, or
/// the ssh commands that reach a daemon on another machine.
fn io() -> &'static Runtime {
    static IO: OnceLock<Runtime> = OnceLock::new();
    IO.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("rho-ui-daemon-io")
            .enable_io()
            // The deadline on a remote machine's setup; see `ssh.rs`.
            .enable_time()
            .build()
            .expect("the OS lets this process start a thread and an I/O driver")
    })
}

async fn open(target: DaemonTarget) -> Result<(Daemon, DaemonEvents), DaemonError> {
    let hello = ClientHello::frontend(ClientInfo {
        name: "rho-ui".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    });
    let (client, notifications, hello, machine) = match target {
        DaemonTarget::Home { home, rho } => {
            let (client, notifications, hello) = LocalDaemon::new(home)
                .connect_or_start(Command::new(rho), hello)
                .await?;
            (client, notifications, hello, Machine::this_process())
        }
        DaemonTarget::Socket(socket) => {
            let stream = UnixStream::connect(&socket).await.map_err(|error| {
                DaemonError::Unreachable(format!(
                    "could not connect to the rho daemon at {}: {error}",
                    socket.display()
                ))
            })?;
            let (client, notifications, hello) = Client::handshake(stream, hello).await?;
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
    };
    let events = DaemonEvents { notifications };
    Ok((daemon, events))
}

/// What every session this client opens gets: `idle_wake: turn`, as rho's
/// terminal UI asks: a turn ends while the background work it started runs
/// on, and the daemon starts a turn of its own when that work finishes.
fn session_setup() -> rho_protocol::SessionSetup {
    rho_protocol::SessionSetup {
        idle_wake: IdleWake::Turn,
        ..Default::default()
    }
}

impl Daemon {
    /// Connects to the daemon, starting it first if `target` says how.
    ///
    /// # Errors
    ///
    /// Fails if no daemon runs and rho cannot be run or its daemon does not
    /// start, the socket refuses the connection, ssh cannot reach the
    /// machine, or the daemon speaks another protocol version; the message
    /// then says which side to update.
    pub async fn connect(target: DaemonTarget) -> Result<(Daemon, DaemonEvents), DaemonError> {
        let (reply, opened) = oneshot::channel();
        io().spawn(async move {
            // The caller may have stopped waiting; the connection then
            // closes as it drops here.
            let _ = reply.send(open(target).await);
        });
        opened.await.map_err(|_| DaemonError::Closed)?
    }

    /// Who answered `rho/hello`.
    pub fn info(&self) -> &DaemonInfo {
        &self.info
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

    /// The sessions with work under way in the daemon, whichever client
    /// started them: a turn running or queued, or a request waiting for a
    /// person. Nothing pushes changes to this list; ask again to refresh.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects the request or the connection is gone.
    pub async fn active_jobs(&self) -> Result<Vec<Job>, DaemonError> {
        let list = self
            .client
            .request::<methods::JobList>(&JobListParams {})
            .await?;
        Ok(list.jobs.into_iter().filter_map(Job::from_wire).collect())
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
    /// Fails if the daemon cannot open the session, as when its model's
    /// provider needs a sign-in, `cwd` is in no git repository for a new
    /// worktree, or the connection is gone.
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn job(kind: &str, state: &str, live: &serde_json::Value) -> Option<Job> {
        let summary = serde_json::from_value(json!({"id": "j1", "kind": kind, "state": state,
            "workspace": "/w", "session": "s1", "created_at": 0, "updated_at": 0, "live": live}));
        Job::from_wire(summary.expect("a valid job summary"))
    }

    #[test]
    fn the_rho_home_is_rho_home_else_dot_rho_in_home() {
        let home = |vars: &[(&str, &str)]| {
            rho_home(|name| {
                vars.iter()
                    .find(|(set, _)| *set == name)
                    .map(|(_, value)| OsString::from(value))
            })
        };
        let both = [("RHO_HOME", "/srv/rho"), ("HOME", "/home/me")];
        assert_eq!(home(&both), Some(PathBuf::from("/srv/rho")));
        let empty = [("RHO_HOME", ""), ("HOME", "/home/me")];
        assert_eq!(home(&empty), Some(PathBuf::from("/home/me/.rho")));
        assert_eq!(home(&[("HOME", "")]), None);
    }

    fn live(elapsed_ms: u64, inputs: &serde_json::Value) -> serde_json::Value {
        json!({"title": "t", "model": {"provider": "p", "model": "m"},
            "turn_elapsed_ms": elapsed_ms, "inputs": inputs})
    }

    #[test]
    fn only_sessions_with_work_under_way_are_jobs_to_show() {
        let none = json!([]);
        let activity = |job: Option<Job>| job.map(|job| job.activity());
        assert_eq!(
            activity(job("session", "running", &live(5, &none))),
            Some(JobActivity::Working)
        );
        assert_eq!(
            activity(job("session", "queued", &live(5, &none))),
            Some(JobActivity::Working)
        );
        assert_eq!(
            activity(job("session", "waiting_for_input", &live(5, &none))),
            Some(JobActivity::Waiting)
        );
        assert_eq!(activity(job("session", "completed", &live(5, &none))), None);
        assert_eq!(activity(job("session", "idle", &json!(null))), None);
        assert_eq!(
            activity(job("subagent", "running", &live(5, &none))),
            None,
            "a subagent's session can't be attached to"
        );

        // A subagent's question keeps its parent waiting between turns.
        let asks = json!([{"id": 1, "asker": "subagent", "ask": {"kind": "question",
            "title": "Which?", "questions": []}}]);
        assert_eq!(
            activity(job("session", "completed", &live(5, &asks))),
            Some(JobActivity::Waiting)
        );
    }

    /// The daemon reports a running turn's elapsed time anew at each
    /// listing. If that alone made a job look changed, the sidebar would
    /// reload the session list at every poll while anything runs.
    #[test]
    fn elapsed_time_alone_does_not_change_a_job() {
        let at = |ms| job("session", "running", &live(ms, &json!([]))).expect("a running job");
        let (earlier, later) = (at(1_000), at(2_000));
        assert_ne!(earlier.turn_elapsed(), later.turn_elapsed());
        assert!(earlier.is_same_work(&later));

        let waiting = job("session", "waiting_for_input", &live(2_000, &json!([])));
        assert!(!earlier.is_same_work(&waiting.expect("a waiting job")));
    }
}
