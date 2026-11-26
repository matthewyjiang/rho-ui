//! A scripted rho daemon for tests: rho-protocol's own server side on a real
//! Unix socket, or named pipe on Windows, with no model behind it.
//! rho-ui-core's daemon tests and rho-ui's headless UI tests both run it.
//!
//! - `rho/session/new` returns "new-1", "new-2", ..., each as the agent
//!   it names, else `default` ([`FakeDaemon::agent`]).
//! - `rho/session/fork` returns "fork-1", "fork-2", ...: a saved session
//!   in supervised mode that replays its parent's conversation. The list
//!   leaves forks out, and deleting a session deletes its forks. Forking a fork, a subagent's session or one that
//!   saved nothing is refused as rho refuses it.
//! - `rho/session/list` returns pages of two: new sessions that have had a
//!   prompt, newest first, then "s1" (titled "first") and "old" (untitled).
//!   `rho/session/delete` drops one, and `rho/session/rename` titles one.
//!   `rho/session/list_detailed` pages the same, each with its agent and
//!   the session that started it, which [`FakeDaemon::start_session`]
//!   records, as does that session's job in `rho/job/list`. With
//!   [`Script::session_lists`] [`SessionLists::Plain`], the daemon is from
//!   before `list_detailed`, which answers `MethodNotFound`.
//!   [`FakeDaemon::detailed_lists`] counts the `list_detailed` requests.
//! - `rho/session/mark` saves a session's mark, stamped with the time,
//!   which `rho/session/list_detailed` lists and its followers hear of.
//!   No session starts with a mark. [`FakeDaemon::set_mark`] marks one as
//!   another client, or the daemon itself, would. A daemon from before
//!   `list_detailed` ([`SessionLists::Plain`]) is from before marks too,
//!   and answers `MethodNotFound`.
//! - Attaching to "old" replays one agent message, "replayed", then
//!   [`Script::replay_turns`] turns, each a question, a thought, three tool
//!   calls, an edit, and a Markdown answer.
//! - A prompt asks to run `ls`, then answers "you chose" and the answer,
//!   with a denial's reason after a colon. The prompt "fail" is refused,
//!   and "slow" is taken half a second late.
//! - The prompt "questions" asks [`questions`]: a required single-choice
//!   question that takes the user's own text, and an optional
//!   multiple-choice one that suggests "test". It answers "you answered"
//!   and each question's values, or "you declined"; answers that give the
//!   text "refuse" are refused, leaving the request open. "questions timeout"
//!   asks the same with a five-minute timeout whose fallback picks Rust;
//!   holding it drops the timeout.
//! - The prompt "pictures" reads an image, whose call carries it, and
//!   answers with two images by path, `plot.png` and `missing.png`, from
//!   the session's folder.
//! - The prompt "plan" asks to carry out a plan, and answers "you chose"
//!   and the answer, with its feedback after a colon.
//! - The prompt "cards" says it looks, then sends [`CARDS`], then answers:
//!   a run of cards between two replies.
//! - The prompt "report" answers with [`report`], a long reply that
//!   streams in over several events.
//! - The prompt "background" starts a background process, `sleep 30`, and
//!   answers at once, leaving it running. Stopping it, or
//!   [`FakeDaemon::finish_background`], ends it, and the daemon starts a
//!   turn of its own that tells the model; that turn runs until a steer
//!   ends it, and drops the work it delivered from the list as it ends.
//!   `rho/session/processes` lists the session's processes while their
//!   work is listed, and reads what [`FakeDaemon::write_process`] had one
//!   write meanwhile, which the session retains whole; [`FakeDaemon::process_reads`]
//!   counts the reads.
//! - The prompt "subagent" starts a subagent, [`SUBAGENT_TASK`], and
//!   answers at once, leaving it running in a session of its own,
//!   "sub-N", as agent [`SUBAGENT_AGENT`] on [`SUBAGENT_MODEL`], which
//!   can be observed but not attached to. Every other session runs as the
//!   default agent, but for a new one that names its own.
//!   [`FakeDaemon::finish_subagent`] ends its run: the parent shows its
//!   whole result as a card, `subagent_result`, and the parent's daemon
//!   turn tells the model, as for a process.
//! - [`FakeDaemon::start_workflow`] starts the workflow [`WORKFLOW_NAME`]
//!   in a session. Its two agent nodes are in the job list only: one
//!   queued second in line, one failed with the error [`WORKFLOW_ERROR`].
//! - The prompt "/compact" compacts the context from [`COMPACTED`]'s
//!   before to its after, and answers saying so, as rho does. The prompt
//!   "long" reads a file, compacts the same automatically, reads it again,
//!   and answers.
//! - The prompt "flaky" sets a plan of three steps, one done, one under
//!   way and one to do, then streams half an answer that the provider
//!   discards, and waits 30 s to retry, rate limited. The turn runs on
//!   until [`FakeDaemon::finish_retry`], which retries: every step is done,
//!   and the turn answers "The whole answer".
//! - A session's first prompt "busy" is refused as busy: the daemon has
//!   just started a turn of its own, which answers and ends at once. The
//!   next "busy" is taken like any prompt. The prompt "deleting" is
//!   refused as busy with no turn under way, as a session being deleted
//!   refuses it.
//! - The prompt "ui" calls the `ui` tool, which shows [`surface::form`] and
//!   waits: the call stays in progress, carrying the surface. An action on
//!   the surface ([`FakeDaemon::surface_actions`] lists them) finishes the
//!   call and the turn answers "you sent" and the event's name; the event
//!   [`surface::REFUSED_EVENT`] is refused instead, as a session refuses
//!   an action on a surface it does not have. The prompt "gallery" shows
//!   [`surface::gallery`] without waiting, and answers at once; "widen"
//!   changes the form so ([`surface::widened`]).
//!   [`FakeDaemon::fall_behind`] detaches a session's clients as fallen
//!   behind.
//! - An attach with a cursor of the daemon's stream gets the events after
//!   it ([`Replay::Tail`]); any other attach gets the whole session
//!   replayed ([`Replay::Reset`]), which replays the calls with their
//!   surfaces. Each run of the daemon, from a restart on, has a stream of
//!   its own, as rho's does, and so does each load of a session after
//!   [`FakeDaemon::unload`].
//! - A prompt sent again with the idempotency key of the session's latest
//!   turn answers with that turn and starts none.
//!   [`FakeDaemon::prompts`] counts the prompts that started a turn.
//! - With [`Script::tui_job`], the session "tui-1" has a turn under way, as
//!   one rho's terminal UI started: it is listed, its job runs, and a steer
//!   is read at once, answered, and ends the turn. The steer "wait" is
//!   never read: it stays queued until withdrawn or the turn is cancelled.
//!   The steer "keep going" is read, and the turn runs on unanswered.
//! - Settings are shaped like rho's: four permission modes, three models,
//!   three reasoning levels, and fast mode. Every session offers the slash
//!   commands `compact` and `prompt:review` and the skills `pstack` and
//!   `prove-it-works`. A session adds a directory that is one on this
//!   machine, once, and refuses others as rho does; an add saves the
//!   session, and the list stays sorted. `rho/session/new` takes its
//!   setup's directories as the new session's, unsaved, and refuses one
//!   that is not a directory.
//!   [`FakeDaemon::followers`] counts the clients following a session.
//! - A session keeps the mode and model a client chose for the sessions it
//!   starts, and sends them after its agent in every reset replay and
//!   again whenever they change. It refuses `unknown` as a mode and a model
//!   it doesn't offer, but not `auto` without a permission-classifier
//!   model. [`FakeDaemon::save_unreadable_started_choice`] gives a session
//!   a saved choice the daemon can't read, under which any model is
//!   refused until a mode replaces it;
//!   [`FakeDaemon::save_unknown_started_mode`] one whose mode it doesn't
//!   know, with a model. A change to the choice sent while the session's
//!   turn runs is applied, and answered, when the turn ends, as rho does;
//!   a change to its own settings is not held.
//! - rho's own settings are two pages. "General" holds
//!   `general.auto_compact` (on/off, on by default), `general.permission_mode`
//!   (a choice of auto, plan or supervised) and, under "Models",
//!   `general.title_model` (a model). "Tools" holds `tools.shell_timeout`
//!   (seconds, 1 to 3600) and `tools.shell` (text). Revisions count the
//!   writes: "r0", "r1", ... A write against an older revision is refused
//!   as a conflict, unless only its own writer wrote since.
//!   [`FakeDaemon::edit_agent_setting`] changes one as a hand edit does.
//! - `rho/model/list` lists the models a session offers, "fake/small",
//!   "fake/large" and "fake/huge", and no Claude Code models.
//!   [`FakeDaemon::set_models`] changes that list, and sends it to each
//!   client that listed, as a sign-in does; sessions keep their models.
//! - Sessions run on [`KEY_AUTH`], which takes an API key and is signed in
//!   unless [`Script::signed_out`]; then a session opens only with
//!   `sign_in: later`. [`LOGIN_AUTH`] takes a login, whose prompt follows
//!   at once and which waits for [`FakeDaemon::approve_login`]. The key
//!   [`REFUSED_KEY`] is refused, and [`UNCHECKED_KEY`] saved unchecked. Credentials go to a file store the
//!   config chose.
//! - `rho/daemon/status` reports the upgrade [`FakeDaemon::replace_program`]
//!   set, if any, and what [`FakeDaemon::set_in_progress`] set.
//!   `rho/daemon/restart` restarts in place, as rho does: it counts
//!   ([`FakeDaemon::restarts`]), drops the upgrade, as the restarted daemon
//!   runs the new program, detaches every attached session as restarting,
//!   forgets new sessions no prompt saved, and closes every connection;
//!   new ones are served as before. [`FakeDaemon::restart`] restarts it so
//!   as rho does by itself to upgrade, and
//!   [`FakeDaemon::restart_at_next_prompt`] as a prompt arrives, which it
//!   answers `DaemonStopping`, having run nothing. `rho/daemon/stop`
//!   detaches every session as stopping and closes every connection
//!   ([`FakeDaemon::stops`]), and the next connection is served as a daemon
//!   started afresh would be. [`FakeDaemon::attaches`] counts each
//!   session's attaches.
//! - [`FakeDaemon::run_another_build`] has the daemon answer every hello
//!   as another build than the client's, until it restarts or stops.
//!   [`FakeDaemon::speak_older_protocol`] has it refuse every hello as a
//!   daemon of an older protocol, until a file appears, as a fake `rho
//!   daemon restart` makes it: the real one replaces the daemon.
//! - Every session's origin is `acme/widgets` on github.com, and its
//!   pull requests are those of a workspace in no git repository that
//!   links nothing, until [`FakeDaemon::open_pull_request`] gives its
//!   branch `feature` the pull request `acme/widgets#12` or
//!   [`FakeDaemon::edit_pull_requests`] changes them; each change reaches
//!   its followers as a `pull_requests` event. A link takes a github.com
//!   pull URL, `owner/name#12` or `#12`, and refuses other text as
//!   `InvalidParams`. [`FakeDaemon::pull_request_lists`] and
//!   [`FakeDaemon::pull_request_shows`] say what clients asked. With
//!   [`Script::pull_requests`] [`PullRequestMethods::Unserved`], the
//!   daemon is from before pull requests: every pull request method
//!   answers `MethodNotFound`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rho_protocol::{
    display::{
        CodeWindow, DiffRow, DiffRowKind, NotificationCard, NotificationDelivery,
        NotificationPreview, NotificationTone, NotificationVisibility, ToolBody, ToolCard,
        ToolCardStatus, ToolFact, ToolFamily, ToolHeader,
    },
    methods::{
        BackgroundItem, BackgroundStatus, BackgroundWork, Budget, FastModeOptions, IdempotencyKey,
        JobId, JobKind, JobState, JobSummary, MessageId, ModelChoice, ModelOptions, ModelRef,
        PendingUpgrade, PermissionModeOptions, QueuePlace, ReasoningLevel, ReasoningOptions,
        Replay, Seq, SessionCommand, SessionId, SessionOptions, SessionSurfaceActionParams,
        SessionWorktree, StartedSessionDefaults, SteerId, StreamId, ToolCallId, ToolCallUpdate,
        TurnId,
    },
    serve_connection, subscription, AnswerStop, BuildId, CardTone, Choice, ClientEnvironment,
    CommandKind, CompactionCounts, CompactionId, CompactionStatus, CompactionTrigger, ContentBlock,
    Cursor, DaemonEndpoint, DaemonInfo, DefaultAnswer, DefaultStyle, DetachReason, ErrorCode,
    InputRequest, ModelList, PermissionMode, PlanEntry, PlanStatus, ProcessStream, ProtocolVersion,
    Published, Publisher, Question, RestartCost, RetryReason, RpcError, Selection,
    SessionAttachResponse, SessionEvent, Subscription, ToolContent, ToolKind, ToolStatus,
    TurnOutcome, VersionMismatch, WorktreeReport, WorktreeVerdict, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader},
    runtime::Runtime,
    sync::watch,
};

use crate::{agent_settings::SettingValue, DaemonTarget, SessionId as UiSessionId, SessionMark};

mod auth;
mod config;
mod listener;
mod processes;
mod pull_requests;
mod service;
mod started;
pub mod surface;

use auth::FakeAuth;
pub use auth::{KEY_AUTH, LOGIN_AUTH, LOGIN_CODE, LOGIN_URL, REFUSED_KEY, UNCHECKED_KEY};
use config::FakeConfig;
use listener::Listener;
use processes::FakeProcesses;
use pull_requests::FakePullRequests;
pub use pull_requests::{PullRequestMethods, PULL_REQUEST_BODY};
use service::Fake;
use started::{HeldChange, StartedChoice};

/// The session [`Script::tui_job`] runs.
pub const TUI_SESSION: &str = "tui-1";

/// The task of the subagent the prompt "subagent" starts: its title in
/// the parent's background work, and its session's first message.
pub const SUBAGENT_TASK: &str = "Read the parser";

/// What the subagent [`SUBAGENT_TASK`] runs on, as its session's model and
/// reasoning say: not its parent's.
pub const SUBAGENT_MODEL: &str = "Huge · High";

/// The agent the subagent [`SUBAGENT_TASK`] runs as.
pub const SUBAGENT_AGENT: &str = "worker";

/// The name of the workflow [`FakeDaemon::start_workflow`] starts.
pub const WORKFLOW_NAME: &str = "review";

/// Why the workflow [`WORKFLOW_NAME`]'s second agent node failed.
pub const WORKFLOW_ERROR: &str = "provider refused the request";

/// The whole result the subagent [`SUBAGENT_TASK`] returns, in Markdown:
/// as long as an explorer's report, with headings, a list and code.
fn subagent_result() -> String {
    let parts = (1..=6).map(|n| {
        format!("\n## {n}. Part {n}\n\n- `src/parse.rs:{n}0`\n\n```rust\nfn part_{n}() {{}}\n```\n")
    });
    std::iter::once("Conclusion: the parser lives in `src/parse.rs`.\n".to_owned())
        .chain(parts)
        .collect()
}

/// The questionnaire the prompt "questions" asks.
pub fn questions() -> Vec<Question> {
    let choice = |value: &str, label: &str| Choice {
        value: value.to_owned(),
        label: label.to_owned(),
        description: None,
    };
    let described = |value: &str, label: &str, description: &str| Choice {
        description: Some(description.to_owned()),
        ..choice(value, label)
    };
    vec![
        Question {
            id: "lang".to_owned(),
            prompt: "Which language should the tool use?".to_owned(),
            header: Some("Language".to_owned()),
            // Rust's description is too long for one line of any window.
            choices: vec![
                described(
                    "rust",
                    "Rust",
                    &"What the rest of the repo is written in, so it shares the lints, \
                      the build and the release. "
                        .repeat(4),
                ),
                described("go", "Go", "Quicker to build."),
            ],
            selection: Selection::One,
            allow_other: true,
            help: None,
            default: None,
            required: true,
        },
        Question {
            id: "checks".to_owned(),
            prompt: "Which checks should run?".to_owned(),
            header: Some("Checks".to_owned()),
            choices: vec![
                choice("fmt", "Format"),
                choice("clippy", "Clippy"),
                choice("test", "Tests"),
            ],
            selection: Selection::Many,
            allow_other: false,
            help: Some("Pick any.".to_owned()),
            default: Some(DefaultAnswer {
                values: vec!["test".to_owned()],
                style: DefaultStyle::Selected,
            }),
            required: false,
        },
    ]
}

/// Writes the program `text` to `path`, runnable, from a child process,
/// for tests that stand in a fake `rho` or the like. A handle written through
/// this process would leak into the processes other tests spawn meanwhile,
/// until they exec, and running the file then fails with "Text file busy"
/// (ETXTBSY).
///
/// # Panics
///
/// If `sh` can't run or fails to write the file.
pub fn write_program(path: &Path, text: &str) {
    let status = std::process::Command::new("sh")
        .args([
            "-c",
            r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#,
            "sh",
            text,
        ])
        .arg(path)
        .status()
        .expect("sh runs");
    assert!(status.success(), "writing {} failed", path.display());
}

/// Where rho's daemon of the rho home `rho_home` listens when
/// `XDG_RUNTIME_DIR` is unset: a Unix socket, or on Windows a named pipe.
/// Makes the home and the daemon's runtime directory if missing.
fn endpoint_of(rho_home: &Path) -> PathBuf {
    std::fs::create_dir_all(rho_home).expect("the fake daemon makes its rho home");
    #[expect(
        clippy::disallowed_methods,
        reason = "rho names its daemon after the rho home as std resolves it, prefix and all"
    )]
    let canonical = std::fs::canonicalize(rho_home).expect("the rho home resolves");
    let endpoint = DaemonEndpoint::locate(&canonical, None).expect("a short rho home");
    std::fs::create_dir_all(&endpoint.runtime_dir).expect("the daemon's runtime directory");
    endpoint.socket
}

/// What the fake daemon holds when it starts.
#[derive(Clone, Copy, Debug, Default)]
pub struct Script {
    /// Turns that attaching to "old" replays after its one message.
    pub replay_turns: usize,
    /// Whether [`TUI_SESSION`] exists, with a turn under way.
    pub tui_job: TuiJob,
    /// Whether "old"'s replay ends with a reply of one paragraph over
    /// 4 KiB, which GPUI Kit parses off the UI thread.
    pub long_reply: bool,
    /// How many more saved sessions, `past-0` and on, each an hour older
    /// than the last and older than "old", to fill long lists.
    pub past_sessions: usize,
    /// Whether the daemon lists sessions with the session that started
    /// each, or is from before that.
    pub session_lists: SessionLists,
    /// Whether [`KEY_AUTH`], which every session runs on, starts signed
    /// out, so a session opens only with `sign_in: later`.
    pub signed_out: bool,
    /// Whether the daemon serves pull requests, or is from before them.
    pub pull_requests: PullRequestMethods,
}

/// Whether the fake daemon serves `rho/session/list_detailed`, and with it
/// `rho/session/mark`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionLists {
    /// It serves `list_detailed` as well as `list`.
    #[default]
    Detailed,
    /// As a daemon from before `list_detailed`: it serves `list` alone, and
    /// `list_detailed` and `mark` answer `MethodNotFound`.
    Plain,
}

/// What a session [`FakeDaemon::start_session`] starts is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Started {
    /// Its first turn ended.
    Idle,
    /// Its first turn runs, until a steer ends it.
    Working,
}

/// Whether the fake daemon runs a turn some other client started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TuiJob {
    /// No such session.
    #[default]
    Absent,
    /// [`TUI_SESSION`] is saved and its turn runs.
    Running,
}

/// A running fake daemon. Dropping it stops it, and removes its rho home
/// and the directory of its socket.
#[derive(Debug)]
pub struct FakeDaemon {
    socket: PathBuf,
    service: Arc<Fake>,
    /// Its rho home.
    _dir: tempfile::TempDir,
    _runtime: Runtime,
}

impl FakeDaemon {
    /// Starts a daemon holding `script`, listening where rho's daemon of a
    /// rho home of its own, a temporary directory, would.
    ///
    /// # Panics
    ///
    /// If the OS refuses a temporary directory, a socket in it, or a
    /// thread: a test cannot go on without them.
    pub fn start(script: Script) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory for the fake daemon");
        let socket = endpoint_of(dir.path());
        Self::serve(script, socket, dir)
    }

    fn serve(script: Script, socket: PathBuf, dir: tempfile::TempDir) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_io()
            .enable_time()
            .build()
            .expect("a runtime for the fake daemon");
        let mut listener = {
            let _guard = runtime.enter();
            Listener::bind(&socket).expect("the fake daemon binds its socket")
        };
        let service = Arc::new(Fake {
            state: Mutex::new(State::new(script)),
            restarted: tokio::sync::watch::Sender::new(0),
            closed: tokio::sync::watch::Sender::new(()),
        });
        let serving = Arc::clone(&service);
        runtime.spawn(async move {
            while let Ok(stream) = listener.accept().await {
                let service = Arc::clone(&serving);
                tokio::spawn(async move {
                    let greeting = service.lock().greet();
                    let build = match greeting {
                        Greeting::Refuse(cost) => {
                            refuse_as_older(stream, cost, &service).await;
                            return;
                        }
                        Greeting::Serve(build) => build,
                    };
                    let info = DaemonInfo {
                        version: "9.9.9".to_owned(),
                        pid: std::process::id(),
                        build,
                    };
                    // Runs until the client leaves, or the daemon restarts
                    // or stops.
                    let mut closed = service.closed.subscribe();
                    let close = async move {
                        let _ = closed.changed().await;
                    };
                    let _ = serve_connection(stream, &info, service, close).await;
                });
            }
        });
        Self {
            socket,
            service,
            _dir: dir,
            _runtime: runtime,
        }
    }

    /// How a client reaches this daemon.
    pub fn target(&self) -> DaemonTarget {
        DaemonTarget::Socket(self.socket.clone())
    }

    /// Where it listens: its Unix socket, or on Windows its named pipe.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Whether auth profile `auth` has credentials.
    pub fn signed_in(&self, auth: &str) -> bool {
        self.service.lock().auth.is_signed_in(auth)
    }

    /// The provider approves the login under way, which then succeeds.
    ///
    /// # Panics
    ///
    /// If no login is under way.
    pub fn approve_login(&self) {
        self.service.lock().auth.approve();
    }

    /// The value of variable `name` in the environment the latest
    /// `rho/session/new` carried, which rho makes the session's whole
    /// process environment.
    pub fn new_session_var(&self, name: &str) -> Option<String> {
        let state = self.service.lock();
        let (_, value) = state
            .new_session_environment
            .iter()
            .flat_map(ClientEnvironment::iter)
            .find(|(set, _)| *set == name)?;
        Some(value.to_owned())
    }

    /// How many open requests clients have held ([`rho_protocol::SESSION_HOLD`]).
    pub fn holds(&self) -> usize {
        self.service.lock().holds
    }

    /// Has the daemon find rho `version` installed over its program,
    /// waiting for `waiting_for` things to end before it upgrades.
    pub fn replace_program(&self, version: &str, waiting_for: u64) {
        self.service.lock().upgrade = Some(PendingUpgrade {
            version: version.to_owned(),
            waiting_for,
        });
    }

    /// How many times the daemon restarted.
    pub fn restarts(&self) -> usize {
        *self.service.restarted.borrow()
    }

    /// How many times a client asked the daemon to stop.
    pub fn stops(&self) -> usize {
        self.service.lock().stops
    }

    /// How many things `rho/daemon/status` says stopping or restarting
    /// the daemon would cut short.
    pub fn set_in_progress(&self, in_progress: u64) {
        self.service.lock().in_progress = in_progress;
    }

    /// Has the daemon answer each hello as another build than the
    /// client's, saying a restart costs `cost`, until it restarts or
    /// stops: then it runs the client's build. Connections open before
    /// heard nothing of it.
    pub fn run_another_build(&self, cost: RestartCost) {
        self.service.lock().skew = Some(cost);
    }

    /// Has the daemon refuse each hello as a daemon of an older protocol
    /// would, saying a restart costs `cost`, until the file `replaced`
    /// exists: a fake `rho daemon restart` makes it, as the real one
    /// replaces the daemon with the rho installed. Then it serves hellos
    /// again.
    pub fn speak_older_protocol(&self, cost: RestartCost, replaced: &Path) {
        self.service.lock().older = Some(Older {
            cost,
            replaced: replaced.to_owned(),
        });
    }

    /// How many hellos the daemon refused as older than the client.
    pub fn refusals(&self) -> usize {
        self.service.lock().refusals
    }

    /// How many hellos the daemon accepted: one per connection.
    pub fn hellos(&self) -> usize {
        self.service.lock().hellos
    }

    /// How many `rho/session/list_detailed` requests the daemon got, each
    /// page and each refusal one.
    pub fn detailed_lists(&self) -> usize {
        self.service.lock().detailed_lists
    }

    /// Restarts the daemon in place, as rho does by itself once the
    /// upgrade it waits for has nothing to cut short.
    pub fn restart(&self) {
        self.service.restart_now();
    }

    /// Has the daemon restart as the next prompt arrives, before the
    /// prompt's turn starts, so it answers `DaemonStopping`.
    pub fn restart_at_next_prompt(&self) {
        self.service.lock().restart_at_prompt = true;
    }

    /// How many prompts started a turn in session `id`; one sent again
    /// with its key counts once.
    pub fn prompts(&self, id: &str) -> usize {
        self.service
            .lock()
            .sessions
            .get(id)
            .map_or(0, |session| session.prompts)
    }

    /// Whether the daemon has session `id`, saved or loaded.
    pub fn has_session(&self, id: &str) -> bool {
        self.service.lock().sessions.contains_key(id)
    }

    /// How many times clients attached to session `id`.
    pub fn attaches(&self, id: &str) -> usize {
        self.service
            .lock()
            .sessions
            .get(id)
            .map_or(0, |session| session.attaches)
    }

    /// How many clients follow session `id` now, attached or observing:
    /// a detach, or a client that leaves, ends its following.
    pub fn followers(&self, id: &str) -> usize {
        self.service.lock().sessions.get(id).map_or(0, |session| {
            session
                .publishers
                .iter()
                .filter(|publisher| !publisher.is_gone())
                .count()
        })
    }

    /// The title session `id` has saved, if it has one.
    pub fn title(&self, id: &str) -> Option<String> {
        self.service.lock().sessions.get(id)?.title.clone()
    }

    /// The agent session `id` runs as: the one it was opened as, else
    /// `default`.
    pub fn agent(&self, id: &str) -> Option<String> {
        Some(self.service.lock().sessions.get(id)?.agent.clone())
    }

    /// The workspace session `id` runs in.
    pub fn cwd(&self, id: &str) -> Option<PathBuf> {
        Some(self.service.lock().sessions.get(id)?.cwd.clone())
    }

    /// The directories session `id`'s tools reach besides its workspace,
    /// sorted, as rho keeps them.
    pub fn added_dirs(&self, id: &str) -> Option<Vec<PathBuf>> {
        Some(
            self.service
                .lock()
                .sessions
                .get(id)?
                .settings
                .added_dirs
                .clone(),
        )
    }

    /// Has session `parent` start session `id` through session control, as
    /// rho's `sessions` tool does: saved now in `parent`'s workspace,
    /// titled `title`, and doing what `started` says. The list and its job
    /// name `parent` as the session that started it.
    ///
    /// # Panics
    ///
    /// If the daemon has no session `parent`: the test has a bug.
    pub fn start_session(&self, parent: &str, id: &str, title: &str, started: Started) {
        let mut state = self.service.lock();
        let date = state.next_date();
        let cwd = state
            .sessions
            .get(parent)
            .unwrap_or_else(|| panic!("the fake daemon has no session {parent}"))
            .cwd
            .to_string_lossy()
            .into_owned();
        let mut session = FakeSession::new(&cwd, Some(title), date);
        session.parent = Some(parent.to_owned());
        if started == Started::Working {
            session.turn = Some(state.next_turn());
            session.state(JobState::Running);
        }
        session.publish(user_message(&format!("{id}-u1"), title));
        if started == Started::Idle {
            session.say(&format!("{id}-m1"), "Started.");
        }
        state.sessions.insert(id.to_owned(), session);
    }

    /// Gives session `id` a saved choice for the sessions it starts that
    /// the daemon can't read, as one a newer rho saved: the next reset
    /// replay reports its mode as `unknown`.
    ///
    /// # Panics
    ///
    /// If the daemon has no session `id`: the test has a bug.
    pub fn save_unreadable_started_choice(&self, id: &UiSessionId) {
        self.save_started_choice(id, StartedChoice::Unreadable);
    }

    /// Gives session `id` a saved choice for the sessions it starts whose
    /// mode the daemon doesn't know, as one a newer rho saved, with the
    /// model `fake/large`: the next reset replay reports its mode as
    /// `unknown`, with the model, and a model change is taken.
    ///
    /// # Panics
    ///
    /// If the daemon has no session `id`: the test has a bug.
    pub fn save_unknown_started_mode(&self, id: &UiSessionId) {
        let chosen = StartedSessionDefaults {
            permission_mode: Some(PermissionMode::Unknown),
            model: Some(model("large")),
        };
        self.save_started_choice(id, StartedChoice::Chosen(chosen));
    }

    fn save_started_choice(&self, id: &UiSessionId, choice: StartedChoice) {
        let mut state = self.service.lock();
        let session = state
            .sessions
            .get_mut(id.as_str())
            .unwrap_or_else(|| panic!("the fake daemon has no session {id}"));
        session.started = choice;
    }

    /// The mark session `id` has saved, if it has one.
    pub fn mark(&self, id: &str) -> Option<SessionMark> {
        let mark = self.service.lock().sessions.get(id)?.mark.clone()?;
        SessionMark::from_wire(&mark)
    }

    /// Saves `mark` as session `id`'s, stamped with the time, as another
    /// client or the daemon itself does, and tells the session's followers.
    ///
    /// # Panics
    ///
    /// If the daemon has no session `id`, or doesn't know the mark's kind.
    pub fn set_mark(&self, id: &str, mark: &rho_protocol::NewSessionMark) {
        let mut state = self.service.lock();
        let session = state
            .sessions
            .get_mut(id)
            .unwrap_or_else(|| panic!("the fake daemon has no session {id}"));
        let mark = SessionMark::stamp(mark, unix_now()).expect("a mark the fake daemon knows");
        session.remark(Some(mark));
    }

    /// How many worktrees the daemon has added for new sessions.
    pub fn worktrees(&self) -> usize {
        self.service.lock().worktrees
    }

    /// Puts a session worktree of "/w/a" on `branch` on disk, added for
    /// `session` (one the daemon lists) or for a job it no longer lists,
    /// which `rho/worktree/prune` finds to be `verdict`. A prune that isn't
    /// a dry run removes it if that is [`WorktreeVerdict::Merged`].
    pub fn add_session_worktree(
        &self,
        session: Option<&str>,
        branch: &str,
        verdict: WorktreeVerdict,
    ) {
        let mut state = self.service.lock();
        // `rho/job/list` names each session's job so.
        let job = JobId::new(match session {
            Some(session) => format!("job-{session}"),
            None => format!("job-gone-{}", state.session_worktrees.len()),
        });
        state.session_worktrees.push(WorktreeReport {
            job: Some(job),
            worktree: SessionWorktree {
                branch: branch.to_owned(),
                // Spelled out, not joined, so it reads the same on Windows.
                path: PathBuf::from(format!("/rho/worktrees/{}", branch.replace('/', "-"))),
                origin: PathBuf::from("/w/a"),
            },
            detached: false,
            verdict,
            stale: false,
            bytes: None,
        });
    }

    /// The branches of the session worktrees still on disk.
    pub fn session_worktrees(&self) -> Vec<String> {
        self.service
            .lock()
            .session_worktrees
            .iter()
            .map(|report| report.worktree.branch.clone())
            .collect()
    }

    /// The value of agent setting `key` now: `None` for one that is unset
    /// or that the fake doesn't have.
    pub fn agent_setting(&self, key: &str) -> Option<SettingValue> {
        self.service.lock().config.value(key)
    }

    /// Sets agent setting `key` to `value` as a hand edit of `config.toml`
    /// does: no client wrote it, so a client's write against the revision
    /// before it is refused, and every client that read the settings hears
    /// of it.
    pub fn edit_agent_setting(&self, key: &str, value: SettingValue) {
        self.service.lock().config.edit(key, value);
    }

    /// Lists `models`, each a `(model, label)` of provider `fake`, as the
    /// models a model setting may take, as a sign-in to another provider
    /// changes them: every client that read them hears of it.
    pub fn set_models(&self, models: &[(&str, &str)]) {
        self.service
            .lock()
            .models
            .send_replace(Arc::new(model_list(models)));
    }

    /// Ends `session`'s oldest running background process by itself, with
    /// exit code `code`, as `sleep` exiting does; the daemon then starts a
    /// turn of its own to tell the model.
    ///
    /// # Panics
    ///
    /// If the session runs no background process: the test has a bug.
    pub fn finish_background(&self, session: &UiSessionId, code: i32) {
        let mut state = self.service.lock();
        let turn = state.next_turn();
        let session = state
            .sessions
            .get_mut(session.as_str())
            .unwrap_or_else(|| panic!("the fake daemon has no session {session}"));
        let process = session
            .background
            .iter_mut()
            .find(|work| work.status == BackgroundStatus::Running)
            .unwrap_or_else(|| panic!("session runs no background process to finish"));
        process.status = BackgroundStatus::Completed;
        process.exit_code = Some(code);
        let told = format!("`{}` exited with {code}.", process.title);
        session.report_background();
        if session.turn.is_none() {
            session.wake(turn, &told);
        }
    }

    /// Has `session`'s oldest running background process write `text` to
    /// `stream`, as one chunk of its output.
    ///
    /// # Panics
    ///
    /// If the session runs no background process: the test has a bug.
    pub fn write_process(&self, session: &UiSessionId, stream: ProcessStream, text: &str) {
        self.with_running_process(session, |processes, id| {
            processes.write(id, stream, text);
        });
    }

    /// Has `session`'s oldest running background process have run for
    /// `ran` so far, and last written `quiet` ago.
    ///
    /// # Panics
    ///
    /// If the session runs no background process: the test has a bug.
    pub fn age_process(&self, session: &UiSessionId, ran: Duration, quiet: Duration) {
        self.with_running_process(session, |processes, id| processes.age(id, ran, quiet));
    }

    /// Runs `act` on `session`'s processes with the ID of its oldest
    /// running one.
    fn with_running_process(
        &self,
        session: &UiSessionId,
        act: impl FnOnce(&mut FakeProcesses, &str),
    ) {
        let mut state = self.service.lock();
        let session = state
            .sessions
            .get_mut(session.as_str())
            .unwrap_or_else(|| panic!("the fake daemon has no session {session}"));
        let id = session
            .background
            .iter()
            .find_map(|work| match &work.item {
                BackgroundItem::Process { id } if work.status.is_running() => Some(id.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("session runs no background process"));
        act(&mut session.processes, &id);
    }

    /// How many `rho/session/processes` requests the daemon got.
    pub fn process_reads(&self) -> usize {
        self.service.lock().process_reads
    }

    /// Starts the workflow [`WORKFLOW_NAME`] in `session`, as the
    /// session's background work, with its two agent nodes in the job
    /// list: one queued second in line, and one failed with
    /// [`WORKFLOW_ERROR`].
    ///
    /// # Panics
    ///
    /// If the daemon has no session `session`: the test has a bug.
    pub fn start_workflow(&self, session: &UiSessionId) {
        let mut state = self.service.lock();
        let run = format!("wf-{}", state.extra_jobs.len());
        let parent = state
            .sessions
            .get_mut(session.as_str())
            .unwrap_or_else(|| panic!("the fake daemon has no session {session}"));
        parent.background.push(BackgroundWork {
            item: BackgroundItem::Workflow { run: run.clone() },
            title: WORKFLOW_NAME.to_owned(),
            status: BackgroundStatus::Running,
            exit_code: None,
        });
        parent.report_background();
        let cwd = parent.cwd.clone();
        let job = |id: String, kind, state, parent: String| JobSummary {
            id: JobId::new(id),
            kind,
            state,
            workspace: cwd.clone(),
            parent: Some(JobId::new(parent)),
            grant: None,
            grant_limit: None,
            session: None,
            run: None,
            workflow_run: None,
            result_pending: false,
            created_at: 0,
            updated_at: 0,
            error: None,
            queue: None,
            live: None,
            worktree: None,
            worktree_stale: false,
        };
        let drive = format!("job-{run}");
        let workflow = JobSummary {
            workflow_run: Some(run.clone()),
            ..job(
                drive.clone(),
                JobKind::Workflow,
                JobState::Running,
                format!("job-{session}"),
            )
        };
        let queued = JobSummary {
            run: Some(format!("{run}-a")),
            queue: Some(QueuePlace {
                budget: Budget::AgentConcurrency,
                limit: 10,
                position: 2,
            }),
            ..job(
                format!("{drive}-a"),
                JobKind::Subagent,
                JobState::Queued,
                drive.clone(),
            )
        };
        let failed = JobSummary {
            run: Some(format!("{run}-b")),
            error: Some(WORKFLOW_ERROR.to_owned()),
            ..job(
                format!("{drive}-b"),
                JobKind::Subagent,
                JobState::Failed,
                drive,
            )
        };
        state.extra_jobs.extend([workflow, queued, failed]);
    }

    /// Retries the request the prompt "flaky" left waiting, and answers.
    ///
    /// # Panics
    ///
    /// If `session` runs no turn: the test has a bug.
    pub fn finish_retry(&self, session: &UiSessionId) {
        let mut state = self.service.lock();
        let session = state
            .sessions
            .get_mut(session.as_str())
            .unwrap_or_else(|| panic!("the fake daemon has no session {session}"));
        let turn = session.turn.as_ref().map_or_else(
            || panic!("session runs no turn to retry"),
            |turn| turn.id.clone(),
        );
        session.publish(SessionEvent::ProviderRequest);
        session.plan([PlanStatus::Completed; 3]);
        session.answer(&format!("m-{turn}-2"), "The whole answer");
    }

    /// Every action clients sent `session` with
    /// `rho/session/surface_action`, the refused ones too, oldest first.
    pub fn surface_actions(&self, session: &UiSessionId) -> Vec<SessionSurfaceActionParams> {
        self.service
            .lock()
            .sessions
            .get(session.as_str())
            .map(|session| session.surface_actions.clone())
            .unwrap_or_default()
    }

    /// Unloads `session`, as rho's daemon does a while after nothing follows
    /// it: it loads again in a stream of its own, so attaching with a
    /// cursor from before replays all of it.
    ///
    /// # Panics
    ///
    /// If a client still follows it, which keeps rho's loaded.
    pub fn unload(&self, session: &UiSessionId) {
        let mut state = self.service.lock();
        let session = state
            .sessions
            .get_mut(session.as_str())
            .expect("the session to unload exists");
        let followers = session.publishers.iter().filter(|p| !p.is_gone()).count();
        assert_eq!(followers, 0, "a followed session stays loaded");
        session.unloads += 1;
    }

    /// Detaches every client of `session` as fallen behind, as rho's
    /// daemon does to one that left too many events unread. Attaching
    /// again replays the session.
    pub fn fall_behind(&self, session: &UiSessionId) {
        let mut state = self.service.lock();
        if let Some(session) = state.sessions.get_mut(session.as_str()) {
            for publisher in session.publishers.drain(..) {
                publisher.close(DetachReason::Lagged);
            }
        }
    }

    /// Ends `session`'s oldest running subagent: its own session answers
    /// and ends its run, which detaches whoever observes it, and the
    /// daemon starts a turn of its own in `session` to tell the model.
    ///
    /// # Panics
    ///
    /// If the session runs no subagent: the test has a bug.
    pub fn finish_subagent(&self, session: &UiSessionId) {
        let mut state = self.service.lock();
        let turn = state.next_turn();
        let parent = state
            .sessions
            .get_mut(session.as_str())
            .unwrap_or_else(|| panic!("the fake daemon has no session {session}"));
        let work = parent
            .background
            .iter_mut()
            .find(|work| {
                work.status == BackgroundStatus::Running
                    && matches!(work.item, BackgroundItem::Subagent { .. })
            })
            .unwrap_or_else(|| panic!("session runs no subagent to finish"));
        work.status = BackgroundStatus::Completed;
        let BackgroundItem::Subagent {
            session: Some(child),
            ..
        } = work.item.clone()
        else {
            panic!("the fake's subagents open their session as they start");
        };
        parent.report_background();
        parent.publish(SessionEvent::Card {
            title: format!("Completed · {SUBAGENT_TASK}"),
            markdown: subagent_result(),
            plain: subagent_result(),
            tone: CardTone::Neutral,
            display: None,
        });
        if parent.turn.is_none() {
            parent.wake(turn, "The subagent found the parser.");
        }
        let child = state
            .sessions
            .get_mut(child.as_str())
            .expect("a subagent's session outlives its run");
        child.answer("sub-m2", "Found the parser.");
        for publisher in child.publishers.drain(..) {
            publisher.close(DetachReason::NotLoaded);
        }
    }
}

/// How many live events a client may leave unread before it is detached:
/// rho's own daemon allows 4096. A scripted turn publishes under ten, and a
/// replay does not count, so a test that reaches this has stopped reading;
/// the fake then panics, naming the number, since no test expects the
/// detach.
const BACKLOG: usize = 4096;

const MODELS: [(&str, &str); 3] = [("small", "Small"), ("large", "Large"), ("huge", "Huge")];

fn model(name: &str) -> ModelRef {
    ModelRef {
        provider: "fake".to_owned(),
        model: name.to_owned(),
    }
}

/// `models`, each a `(model, label)` of provider `fake`, as a session and
/// a model setting list them.
fn model_choices(models: &[(&str, &str)]) -> Vec<ModelChoice> {
    models
        .iter()
        .map(|(name, label)| ModelChoice {
            model: model(name),
            label: (*label).to_owned(),
        })
        .collect()
}

/// `models` as `rho/model/list` answers: no Claude Code models, since the
/// fake's model setting runs on rho alone.
fn model_list(models: &[(&str, &str)]) -> ModelList {
    ModelList {
        models: model_choices(models),
        claude_cli: Vec::new(),
    }
}

/// A session's settings, as `rho/session/configure` changes them.
#[derive(Clone, Debug)]
struct Settings {
    mode: PermissionMode,
    model: ModelRef,
    reasoning: ReasoningLevel,
    fast: bool,
    added_dirs: Vec<PathBuf>,
}

impl Settings {
    fn new() -> Self {
        Self {
            mode: PermissionMode::Auto,
            model: model("small"),
            reasoning: ReasoningLevel::Medium,
            fast: false,
            added_dirs: Vec::new(),
        }
    }

    /// Adds `path` where it sorts, as rho does: refuses one that is not a
    /// directory, or that the session reaches already.
    fn add_directory(&mut self, path: PathBuf) -> Result<(), RpcError> {
        if !path.is_dir() {
            return Err(not_a_directory(&path));
        }
        match self.added_dirs.binary_search(&path) {
            Ok(_) => Err(RpcError::new(
                ErrorCode::InvalidParams,
                format!("the session already reaches {}", path.display()),
            )),
            Err(ix) => {
                self.added_dirs.insert(ix, path);
                Ok(())
            }
        }
    }

    /// The settings of a session `rho/session/new` opens with `dirs`, as
    /// rho takes them: sorted, a repeat folded in, and one that is not a
    /// directory refused.
    fn opened_with(dirs: Vec<PathBuf>) -> Result<Self, RpcError> {
        if let Some(path) = dirs.iter().find(|path| !path.is_dir()) {
            return Err(not_a_directory(path));
        }
        let mut settings = Self::new();
        settings.added_dirs = dirs;
        settings.added_dirs.sort();
        settings.added_dirs.dedup();
        Ok(settings)
    }

    fn options(&self) -> SessionOptions {
        SessionOptions {
            model: ModelOptions {
                current: self.model.clone(),
                auth: KEY_AUTH.to_owned(),
                available: model_choices(&MODELS),
            },
            reasoning: Some(ReasoningOptions {
                current: self.reasoning,
                available: vec![
                    ReasoningLevel::Low,
                    ReasoningLevel::Medium,
                    ReasoningLevel::High,
                ],
            }),
            fast_mode: Some(FastModeOptions { enabled: self.fast }),
            permission_mode: PermissionModeOptions {
                current: self.mode,
                available: vec![
                    PermissionMode::Auto,
                    PermissionMode::Plan,
                    PermissionMode::AllowEdits,
                    PermissionMode::Supervised,
                ],
            },
            commands: vec![
                SessionCommand {
                    name: "compact".to_owned(),
                    kind: CommandKind::Slash,
                    description: "Compact".to_owned(),
                    input_hint: Some("[instructions]".to_owned()),
                },
                SessionCommand {
                    name: "prompt:review".to_owned(),
                    kind: CommandKind::Slash,
                    description: "Review the change".to_owned(),
                    input_hint: None,
                },
                SessionCommand {
                    name: "pstack".to_owned(),
                    kind: CommandKind::Skill,
                    description: "Route engineering work".to_owned(),
                    input_hint: None,
                },
                SessionCommand {
                    name: "prove-it-works".to_owned(),
                    kind: CommandKind::Skill,
                    description: "Verify through the real path".to_owned(),
                    input_hint: None,
                },
            ],
            added_dirs: self.added_dirs.clone(),
        }
    }
}

/// rho's refusal of `path` as a directory a session reaches.
fn not_a_directory(path: &Path) -> RpcError {
    RpcError::new(
        ErrorCode::InvalidParams,
        format!("{} is not a directory", path.display()),
    )
}

/// The turn a session runs.
#[derive(Debug)]
struct Turn {
    /// Counts the daemon's turns, so it also names the turn's one request.
    number: u64,
    id: TurnId,
    tool: ToolCallId,
    /// The approval it waits on, if it asked.
    input: Option<InputRequest>,
    /// Steers it accepted and has not read.
    queued: Vec<SteerId>,
    began: Instant,
}

impl Turn {
    /// How long the turn has run, as the daemon reports it: it grows from
    /// one answer to the next.
    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.began.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

#[derive(Debug)]
struct FakeSession {
    cwd: PathBuf,
    title: Option<String>,
    /// Unix seconds, for the list's order.
    updated_at: u64,
    /// What `rho/session/mark` saved; a fork starts with none.
    mark: Option<rho_protocol::SessionMark>,
    /// Listed by `rho/session/list`: a new session is once it has a prompt
    /// or an added directory.
    saved: bool,
    /// The id of the agent it runs as, which a reset replay starts with.
    agent: String,
    settings: Settings,
    /// What a client chose for the sessions it starts, kept apart from its
    /// own settings.
    started: StartedChoice,
    /// Changes to `started` sent while a turn ran, waiting for its end.
    held_started: Vec<HeldChange>,
    state: JobState,
    turn: Option<Turn>,
    /// Every event so far, which an attach replays.
    history: Vec<SessionEvent>,
    publishers: Vec<Publisher>,
    /// How many times clients attached to it.
    attaches: usize,
    /// How many prompts started a turn.
    prompts: usize,
    /// The idempotency key of the prompt that started the latest turn, and
    /// that turn.
    last_prompt: Option<(IdempotencyKey, TurnId)>,
    live_seq: u64,
    /// How many times the daemon unloaded it ([`FakeDaemon::unload`]): each
    /// load has a stream of its own.
    unloads: u64,
    /// Background work, as the last background event listed it.
    background: Vec<BackgroundWork>,
    /// What its background processes wrote.
    processes: FakeProcesses,
    /// Whether a prompt "busy" was already refused.
    refused_busy: bool,
    /// A subagent's session, which only its run writes: observed, never
    /// attached to.
    subagent: bool,
    /// For a fork, the session it forked.
    fork_of: Option<String>,
    /// The session that started it through session control
    /// ([`FakeDaemon::start_session`]).
    parent: Option<String>,
    /// The actions clients sent on its surfaces.
    surface_actions: Vec<SessionSurfaceActionParams>,
}

impl FakeSession {
    fn new(cwd: &str, title: Option<&str>, updated_at: u64) -> Self {
        Self {
            cwd: PathBuf::from(cwd),
            title: title.map(str::to_owned),
            updated_at,
            mark: None,
            saved: true,
            agent: "default".to_owned(),
            settings: Settings::new(),
            started: StartedChoice::Chosen(StartedSessionDefaults::default()),
            held_started: Vec::new(),
            state: JobState::Idle,
            turn: None,
            history: Vec::new(),
            publishers: Vec::new(),
            attaches: 0,
            prompts: 0,
            last_prompt: None,
            live_seq: 0,
            unloads: 0,
            background: Vec::new(),
            processes: FakeProcesses::default(),
            refused_busy: false,
            subagent: false,
            fork_of: None,
            parent: None,
            surface_actions: Vec::new(),
        }
    }

    /// A fork of `self`: its conversation, in supervised mode, as
    /// `rho/session/fork` opens one. The tests fork only between turns.
    fn fork(&self, parent: &str) -> Self {
        let mut fork = Self::new(
            &self.cwd.to_string_lossy(),
            self.title.as_deref(),
            self.updated_at,
        );
        fork.history.clone_from(&self.history);
        fork.agent.clone_from(&self.agent);
        fork.settings.model = self.settings.model.clone();
        fork.settings.reasoning = self.settings.reasoning;
        fork.settings.mode = PermissionMode::Supervised;
        fork.fork_of = Some(parent.to_owned());
        fork
    }

    /// The session of a subagent in `cwd` whose run, `turn`, is under way
    /// on [`SUBAGENT_TASK`], with [`SUBAGENT_MODEL`].
    fn subagent(cwd: &Path, turn: Turn) -> Self {
        let mut session = Self::new(&cwd.to_string_lossy(), None, 0);
        session.saved = false;
        session.subagent = true;
        SUBAGENT_AGENT.clone_into(&mut session.agent);
        session.settings.model = model("huge");
        session.settings.reasoning = ReasoningLevel::High;
        session.turn = Some(turn);
        session.state(JobState::Running);
        session.publish(user_message("sub-u1", SUBAGENT_TASK));
        session.say("sub-m1", "Reading the parser.");
        session
    }

    /// Follows the session from here, in the daemon's `stream`: the events
    /// after `after`, if it is a place in that stream, or else what the
    /// session holds now; then what it publishes until the client leaves.
    fn follow(
        &mut self,
        stream: StreamId,
        after: Option<&Cursor>,
    ) -> (SessionAttachResponse, Publisher, Subscription) {
        let tail = after
            .filter(|after| after.stream == stream && after.seq.0 <= self.live_seq)
            .map(|after| after.seq.0);
        let (replay, replayed) = match tail {
            Some(after) => (Replay::Tail, self.tail(after)),
            None => (Replay::Reset, self.transcript()),
        };
        let response = SessionAttachResponse {
            stream,
            workspace: self.cwd.clone(),
            replay,
            replay_events: u64::try_from(replayed.len()).unwrap_or(u64::MAX),
            state: self.state,
            turn: self.turn.as_ref().map(|turn| turn.id.clone()),
            turn_elapsed_ms: self.turn.as_ref().map(Turn::elapsed_ms),
            inputs: self
                .turn
                .iter()
                .filter_map(|turn| turn.input.clone())
                .collect(),
            title: self.title.clone(),
            options: self.settings.options(),
            background: self.background.clone(),
        };
        let (publisher, subscription) = subscription(BACKLOG, replayed);
        (response, publisher, subscription)
    }

    /// The events after seq `after`, as they were published: the `n`th
    /// event published has seq `n`.
    fn tail(&self, after: u64) -> Vec<(Seq, SessionEvent)> {
        debug_assert_eq!(
            u64::try_from(self.history.len()).ok(),
            Some(self.live_seq),
            "every event published is in the history, in order"
        );
        let skip = usize::try_from(after).expect("a seq the fake published fits in usize");
        (after + 1..)
            .map(Seq)
            .zip(self.history[skip..].iter().cloned())
            .collect()
    }

    /// The whole session, as a reset replays it, after the agent it runs
    /// as and the choice for the sessions it starts, which no saved
    /// transcript holds. It leaves out background events, as rho's saved
    /// transcripts hold none (rho replays them only for a turn in
    /// progress, which the fake doesn't keep apart): the attach response
    /// lists the work as it is now, without work whose result the model
    /// has. Nor do they hold pull requests, which a client asks for,
    /// marks, which the list reports, or changes to the choice for started
    /// sessions. Nor does it mark steers, as rho's doesn't.
    fn transcript(&self) -> Vec<(Seq, SessionEvent)> {
        let agent = SessionEvent::Agent {
            id: self.agent.clone(),
        };
        let started = SessionEvent::StartedSessionDefaults(self.started.reported());
        let history = self
            .history
            .iter()
            .filter(|event| {
                !matches!(
                    event,
                    SessionEvent::Background { .. }
                        | SessionEvent::PullRequests { .. }
                        | SessionEvent::StartedSessionDefaults(_)
                        | SessionEvent::MarkChanged { .. }
                )
            })
            .cloned();
        let replay: Vec<SessionEvent> = [agent, started]
            .into_iter()
            .chain(history)
            .map(|mut event| {
                if let SessionEvent::UserMessage { steer, .. } = &mut event {
                    *steer = None;
                }
                event
            })
            .collect();
        // As the daemon numbers a reset's replay: 0 for all but the last.
        let last = replay.len().saturating_sub(1);
        replay
            .into_iter()
            .enumerate()
            .map(|(ix, event)| (Seq(if ix == last { self.live_seq } else { 0 }), event))
            .collect()
    }

    /// Saves `mark` in place of the session's own, and tells its followers
    /// if that changed it, as rho does.
    fn remark(&mut self, mark: Option<rho_protocol::SessionMark>) {
        if self.mark != mark {
            self.mark.clone_from(&mark);
            self.publish(SessionEvent::MarkChanged { mark });
        }
    }

    /// Records `event` and sends it to every attached client.
    fn publish(&mut self, event: SessionEvent) {
        if let SessionEvent::State { state, .. } = &event {
            self.state = *state;
        }
        self.live_seq += 1;
        let seq = Seq(self.live_seq);
        self.publishers.retain(|publisher| {
            match publisher.try_publish(seq, event.clone()) {
                Ok(()) => true,
                // The client detached or left.
                Err(Published::Gone) => false,
                Err(Published::Lagged) => panic!(
                    "a fake daemon client fell {BACKLOG} events behind at seq {}",
                    self.live_seq
                ),
            }
        });
        self.history.push(event);
    }

    fn state(&mut self, state: JobState) {
        let turn = self.turn.as_ref().map(|turn| turn.id.clone());
        self.publish(SessionEvent::State { state, turn });
    }

    fn say(&mut self, message: &str, text: &str) {
        self.publish(SessionEvent::AgentMessageChunk {
            message: MessageId::new(message),
            text: text.to_owned(),
        });
    }

    /// Publishes tool call `id` as a rho 3 daemon does: its title, kind and
    /// status, `content` as the model read it, and `card`, what rho's
    /// terminal UI draws for it.
    fn tool(
        &mut self,
        id: &ToolCallId,
        (title, kind): (&str, ToolKind),
        status: ToolStatus,
        content: Vec<ToolContent>,
        card: ToolCard,
    ) {
        self.publish(SessionEvent::tool_call(ToolCallUpdate {
            id: id.clone(),
            title: Some(title.to_owned()),
            kind: Some(kind),
            status: Some(status),
            raw_input: None,
            content: (!content.is_empty()).then_some(content),
            locations: None,
            display: Some(card.into()),
        }));
    }

    /// Sets the plan of the prompt "flaky", its steps at `statuses`.
    fn plan(&mut self, statuses: [PlanStatus; 3]) {
        let steps = ["Read the parser", "Split it in two", "Run the tests"];
        self.publish(SessionEvent::Plan {
            entries: steps
                .iter()
                .zip(statuses)
                .map(|(step, status)| PlanEntry {
                    content: (*step).to_owned(),
                    status,
                })
                .collect(),
        });
    }

    /// Runs the turn of the prompt "/compact" as rho does: the compaction,
    /// then a reply that says again what it did.
    fn compact(&mut self, turn: Turn, echo: SessionEvent) {
        let id = turn.id.clone();
        self.turn = Some(turn);
        self.state(JobState::Running);
        self.publish(echo);
        self.compacted(&format!("c-{id}"), CompactionTrigger::Command);
        let CompactionCounts {
            previous_messages,
            current_messages,
            previous_tokens,
            current_tokens,
            ..
        } = COMPACTED;
        self.answer(
            &format!("m-{id}"),
            &format!(
                "context compacted: {previous_messages} → {current_messages} messages, \
                 {previous_tokens} → {current_tokens} tokens"
            ),
        );
    }

    /// Runs the turn of the prompt "long", one whose context fills: a read,
    /// an automatic compaction, another read, and an answer.
    fn long(&mut self, turn: Turn, echo: SessionEvent) {
        let id = turn.id.clone();
        self.turn = Some(turn);
        self.state(JobState::Running);
        self.publish(echo);
        let read = |session: &mut Self, n: u8| {
            session.tool(
                &ToolCallId::new(format!("r-{id}-{n}")),
                ("read_file(src/lib.rs)", ToolKind::Read),
                ToolStatus::Completed,
                vec![text("fn main() {}")],
                card(
                    ToolStatus::Completed,
                    ToolFamily::FileCommand,
                    ToolHeader::call("read_file", Some("src/lib.rs".to_owned())),
                ),
            );
        };
        read(self, 1);
        self.compacted(&format!("c-{id}"), CompactionTrigger::Automatic);
        read(self, 2);
        self.answer(&format!("m-{id}"), "Read it twice.");
    }

    /// Compaction `id`, started by `trigger`, from [`COMPACTED`]'s before
    /// to its after.
    fn compacted(&mut self, id: &str, trigger: CompactionTrigger) {
        let id = CompactionId::new(id);
        self.publish(SessionEvent::Compaction {
            id: id.clone(),
            status: CompactionStatus::Started {
                trigger: Some(trigger),
            },
        });
        self.publish(SessionEvent::Compaction {
            id,
            status: CompactionStatus::Completed { counts: COMPACTED },
        });
    }

    /// Starts the turn of the prompt "flaky": the provider discards what
    /// it streamed, and the turn waits to retry.
    fn flaky(&mut self, turn: Turn, echo: SessionEvent) {
        let id = turn.id.clone();
        self.turn = Some(turn);
        self.state(JobState::Running);
        self.publish(echo);
        self.plan([
            PlanStatus::Completed,
            PlanStatus::InProgress,
            PlanStatus::Pending,
        ]);
        self.publish(SessionEvent::ProviderRequest);
        self.say(&format!("m-{id}-1"), "Half an ans");
        self.publish(SessionEvent::ResponseDiscarded);
        self.publish(SessionEvent::ProviderRetry {
            reason: RetryReason::RateLimit,
            retry_after_ms: Some(30_000),
        });
    }

    /// Tells clients the session's background work as it is now.
    fn report_background(&mut self) {
        self.processes.sync(&self.background);
        self.publish(SessionEvent::Background {
            work: self.background.clone(),
        });
    }

    /// Starts a background process `sleep 30` with ID `id`.
    fn start_background(&mut self, id: String) {
        self.processes.start(&id, "sleep 30");
        self.background.push(BackgroundWork {
            item: BackgroundItem::Process { id },
            title: "sleep 30".to_owned(),
            status: BackgroundStatus::Running,
            exit_code: None,
        });
        self.report_background();
    }

    /// Starts subagent run `run`, [`SUBAGENT_TASK`], writing session `child`.
    fn start_subagent(&mut self, run: String, child: &str) {
        self.background.push(BackgroundWork {
            item: BackgroundItem::Subagent {
                run,
                session: Some(SessionId::new(child)),
            },
            title: SUBAGENT_TASK.to_owned(),
            status: BackgroundStatus::Running,
            exit_code: None,
        });
        self.report_background();
    }

    /// Starts `turn`, one of the daemon's own that tells the model `told`
    /// about background work that ended. It runs until a steer ends it.
    fn wake(&mut self, turn: Turn, told: &str) {
        // A prompt's key names only the session's latest turn.
        self.last_prompt = None;
        let id = turn.id.clone();
        self.turn = Some(turn);
        self.state(JobState::Running);
        self.publish(SessionEvent::ThoughtChunk {
            message: MessageId::new(format!("th-{id}")),
            text: "Reading what ended.".to_owned(),
        });
        self.say(&format!("m-{id}"), told);
    }

    /// Ends the running turn with `outcome`. The model had the results of
    /// background work that ended, which leaves the list first.
    fn end_turn(&mut self, outcome: TurnOutcome, state: JobState) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        let listed = self.background.len();
        self.background.retain(|work| work.status.is_running());
        if self.background.len() != listed {
            self.report_background();
        }
        self.publish(SessionEvent::TurnEnded {
            turn: turn.id.clone(),
            outcome,
        });
        self.publish(SessionEvent::State {
            state,
            turn: Some(turn.id),
        });
        self.apply_held_started();
    }

    /// Reads an image with `read_file`, whose card is text and whose
    /// content carries the image, as a rho 3 daemon's does.
    fn read_image(&mut self, id: &str) {
        let read = card(
            ToolStatus::Completed,
            ToolFamily::FileCommand,
            ToolHeader::call("read_file", Some("plot.png".to_owned())),
        );
        self.tool(
            &ToolCallId::new(id),
            ("read_file(plot.png)", ToolKind::Read),
            ToolStatus::Completed,
            vec![
                text("image/png image (173 bytes)"),
                ToolContent::Image {
                    mime_type: "image/png".to_owned(),
                    data: TOOL_IMAGE.to_owned(),
                },
            ],
            read,
        );
    }

    /// Publishes `ui` call `id` in `status`, carrying the `messages` that
    /// show `surface`, as every update of a real daemon's call does.
    fn ui(&mut self, id: &ToolCallId, status: ToolStatus, surface: &str, messages: Vec<Value>) {
        let header = ToolHeader::call("ui", Some(surface.to_owned()));
        let mut content = vec![ToolContent::Surface {
            surface: surface.to_owned(),
            messages,
        }];
        if status == ToolStatus::Completed {
            content.insert(0, text(&json!({"surfaceId": surface}).to_string()));
        }
        self.tool(
            id,
            (&format!("ui({surface})"), ToolKind::Other),
            status,
            content,
            card(status, ToolFamily::Form, header),
        );
    }

    /// Finishes the turn as answered with `text`.
    fn answer(&mut self, message: &str, text: &str) {
        self.say(message, text);
        self.answered(text);
    }

    /// Finishes the turn as answered with `text`, which is ASCII and
    /// streams in [`STREAM_CHUNK`] bytes at a time.
    fn stream_answer(&mut self, message: &str, text: &str) {
        for chunk in text.as_bytes().chunks(STREAM_CHUNK) {
            let chunk = std::str::from_utf8(chunk).expect("a streamed reply is ASCII");
            self.say(message, chunk);
        }
        self.answered(text);
    }

    /// Ends the turn, whose reply `text` is already said.
    fn answered(&mut self, text: &str) {
        self.publish(SessionEvent::Usage {
            used_tokens: 2500,
            window_tokens: Some(10_000),
            cost_usd_micros: Some(10_000),
        });
        self.end_turn(
            TurnOutcome::Answered {
                text: text.to_owned(),
                stop: AnswerStop::EndTurn,
            },
            JobState::Completed,
        );
    }
}

/// The image the prompt "pictures" reads: a 96x48 PNG, base64.
const TOOL_IMAGE: &str = "iVBORw0KGgoAAAANSUhEUgAAAGAAAAAwCAIAAABhdOiYAAAAdElEQVR42u3aMQ0AIRAAQSCIeQlooUIBYlCAHGS9AkpCQmbbK0gmdx3xqytoX0IACBAgQIAAvVreDVZvp98uY9ogJwYIkAABAgQIECBAgAQIECBAgAABAiRAgAABAgQIECBAAgQI0L2ij+Q2CBAgQIAAvdoPYAgF9XG26YoAAAAASUVORK5CYII=";

fn text(text: &str) -> ToolContent {
    ToolContent::Text {
        text: text.to_owned(),
    }
}

/// The card of a call that is in `status`, headed `header`.
fn card(status: ToolStatus, family: ToolFamily, header: ToolHeader) -> ToolCard {
    let status = match status {
        ToolStatus::Completed => ToolCardStatus::Ok,
        ToolStatus::Failed => ToolCardStatus::Error,
        _ => ToolCardStatus::Running,
    };
    ToolCard::new(status, family, header)
}

/// The card of shell command `command`, which printed `output` once done.
/// The time as the fake daemon's clock says, in Unix seconds.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is past 1970")
        .as_secs()
}

fn shell_card(command: &str, status: ToolStatus, output: Option<&str>) -> ToolCard {
    let header = ToolHeader::shell("$", Some(command.to_owned()));
    let mut card = card(status, ToolFamily::FileCommand, header);
    card.push_fact(ToolFact::Timeout { seconds: None });
    if let Some(output) = output {
        card.push_fact(ToolFact::Exit {
            code: 0,
            duration_ms: Some(100),
        });
        card.body = ToolBody::Lines(output.lines().map(str::to_owned).collect());
    }
    card
}

/// What a shell call's content holds for the model: its output, wrapped.
fn shell_output(output: &str) -> ToolContent {
    text(&format!("stdout:\n{output}\n\ntime: 0.1s"))
}

fn user_message(id: &str, text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        message: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        steer: None,
    }
}

/// How much of a streamed reply one event carries, in bytes: less than a
/// line of [`report`]'s prose, so its lines arrive unfinished.
const STREAM_CHUNK: usize = 53;

/// The context before and after the prompt "/compact" compacts it.
pub const COMPACTED: CompactionCounts = CompactionCounts {
    previous_messages: 263,
    current_messages: 65,
    previous_tokens: 247_276,
    current_tokens: 54_798,
    cost_usd_micros: None,
};

/// The reply to the prompt "report", 6.4 KiB: sections of a heading, a
/// paragraph, a list, a code block that holds a blank line, and a table.
pub fn report() -> String {
    let sentence = "The walker visits each node once, and reports the ones it could not reach. ";
    let sections: Vec<String> = (0..6)
        .map(|n| {
            format!(
                "## Section {n}\n\n{}\n\n- first of {n}\n- second of {n}\n\n\
                 ```rust\nfn section_{n}() {{\n    let found = walk(&tree);\n\n    report(found);\n}}\n```\n\n\
                 | Node | Reached |\n| --- | --- |\n| `a{n}` | yes |\n| `b{n}` | no |\n\n",
                sentence.repeat(12)
            )
        })
        .collect();
    sections.concat()
}

/// The replayed turns of "old", as a saved conversation holds them.
fn replay(session: &mut FakeSession, turns: usize) {
    session.say("m0", "replayed");
    for n in 0..turns {
        session.publish(user_message(
            &format!("u{n}"),
            &format!("Question {n}: what does this do?"),
        ));
        session.publish(SessionEvent::ThoughtChunk {
            message: MessageId::new(format!("th{n}")),
            text: format!(
                "**Finding the walker**\n\nThe tree walk for {n} is probably in src/{n}.rs."
            ),
        });
        tool_calls(session, n);
        let items: Vec<String> = (0..6)
            .map(|i| format!("- item {i} with `code` and *emphasis*"))
            .collect();
        session.say(
            &format!("r{n}"),
            &format!(
                "## Answer {n}\n\nThis **function** walks the `tree` and returns a list:\n\n{}\n\n```rust\nfn main() {{\n    println!(\"hi\");\n}}\n```\n",
                items.join("\n")
            ),
        );
    }
}

/// Turn `n`'s run of tool calls, as agents make them: a read, a search, a
/// command and an edit.
fn tool_calls(session: &mut FakeSession, n: usize) {
    let matches: Vec<String> = (0..60).map(|i| format!("src/{n}.rs:{i}: match")).collect();
    let found = matches.join("\n");
    let read = card(
        ToolStatus::Completed,
        ToolFamily::FileCommand,
        ToolHeader::call("read_file", Some(format!("src/{n}.rs"))),
    )
    .with_facts(vec![ToolFact::Count {
        label: "lines".to_owned(),
        value: 60,
        detail: None,
    }]);
    session.tool(
        &ToolCallId::new(format!("t{n}-0")),
        (&format!("read_file(src/{n}.rs)"), ToolKind::Read),
        ToolStatus::Completed,
        vec![text(&found)],
        read,
    );
    let search = card(
        ToolStatus::Completed,
        ToolFamily::FileCommand,
        ToolHeader::call("grep", Some(format!("pattern, src/{n}.rs"))),
    )
    .with_facts(vec![ToolFact::Count {
        label: "matches".to_owned(),
        value: 60,
        detail: Some("in 2 files".to_owned()),
    }])
    .with_body(ToolBody::Search {
        pattern: "pattern".to_owned(),
        literal: false,
        case_sensitive: true,
        lines: grep_lines(n),
    });
    session.tool(
        &ToolCallId::new(format!("t{n}-1")),
        (&format!("grep(pattern, src/{n}.rs)"), ToolKind::Search),
        ToolStatus::Completed,
        vec![text(&grep_lines(n).join("\n"))],
        search,
    );
    // A heredoc, so the command runs over several lines.
    let command = format!("python3 - <<'EOF'\nprint(walk({n}))\nEOF");
    session.tool(
        &ToolCallId::new(format!("t{n}-2")),
        (&command, ToolKind::Execute),
        ToolStatus::Completed,
        vec![shell_output(&found)],
        shell_card(&command, ToolStatus::Completed, Some(&found)),
    );
    let (old, new) = (
        "fn walk(tree: &Tree) -> Vec<Node> {\n    let mut out = vec![];\n    out\n}\n",
        "fn walk(tree: &Tree) -> Vec<Node> {\n    let mut out = Vec::with_capacity(tree.len());\n    out.extend(tree.roots());\n    out\n}\n",
    );
    let edited = card(
        ToolStatus::Completed,
        ToolFamily::FileDiff,
        ToolHeader::call("str_replace", Some(format!("src/{n}.rs"))),
    )
    .with_facts(vec![ToolFact::DiffStat {
        added: 2,
        removed: 1,
        path: None,
    }])
    .with_body(ToolBody::Diff(vec![
        DiffRow::new(DiffRowKind::Removed, Some(2), "    let mut out = vec![];"),
        DiffRow::new(
            DiffRowKind::Added,
            Some(2),
            "    let mut out = Vec::with_capacity(tree.len());",
        ),
        DiffRow::new(DiffRowKind::Added, Some(3), "    out.extend(tree.roots());"),
    ]));
    session.tool(
        &ToolCallId::new(format!("t{n}-edit")),
        (&format!("str_replace(src/{n}.rs)"), ToolKind::Edit),
        ToolStatus::Completed,
        // The receipt the model reads: what changed, then the file.
        vec![
            text(&format!(
                "edited src/{n}.rs; replaced 1 occurrence(s)\n\nsrc/{n}.rs\n1:{}",
                new.lines().next().unwrap_or_default()
            )),
            ToolContent::Diff {
                path: PathBuf::from(format!("/w/b/src/{n}.rs")),
                old_text: Some(old.to_owned()),
                new_text: new.to_owned(),
                deleted: false,
            },
        ],
        edited,
    );
    // A script, which rho's card shows as code.
    let script = format!("for path in glob(\"src/{n}/*.rs\"):\n    print(read_file(path)[:80])");
    let scripted = card(
        ToolStatus::Completed,
        ToolFamily::Default,
        ToolHeader::call("codemode", Some("2 calls".to_owned())),
    )
    .with_body(ToolBody::Code {
        language: "python".to_owned(),
        window: CodeWindow::Head,
        lines: script.lines().map(str::to_owned).collect(),
    });
    session.tool(
        &ToolCallId::new(format!("t{n}-script")),
        ("codemode", ToolKind::Other),
        ToolStatus::Completed,
        vec![text("fn walk(tree: &Tree) -> Vec<Node> {")],
        scripted,
    );
}

/// What rho's grep prints for turn `n`'s search, as its card carries it: each
/// file, then `N | text` for each line that matched, then how many.
fn grep_lines(n: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for file in [format!("src/{n}.rs"), format!("src/{n}/walk.rs")] {
        lines.push(file);
        lines.extend(
            (1..=30).map(|line| format!("{line} |     let found = pattern(tree, {line});")),
        );
    }
    lines.extend([String::new(), "60 matches in 2 files".to_owned()]);
    lines
}

/// The cards the prompt "cards" sends between its two replies, as a lead
/// gets them from its workers, processes and pull requests: `(title,
/// tone, reference, body)`. A failure, three routine cards, then the
/// comments, which ask for a look, then three more routine cards: with the
/// rail's first four always showing, the last three fold, and the comments
/// show though they come after the fourth.
pub const CARDS: [(&str, CardTone, &str, &str); 8] = [
    (
        "Failed (exit 101) · cargo test -j 6 -p rho-ui",
        CardTone::Error,
        "p-3",
        "test ui_tests::transcript_keeps_scroll_on_resize ... FAILED",
    ),
    (
        "Checks passed · matthewyjiang/rho-ui#99",
        CardTone::Success,
        "matthewyjiang/rho-ui#99",
        "- All 9 checks passed.",
    ),
    (
        "Approved · matthewyjiang/rho-ui#100",
        CardTone::Success,
        "matthewyjiang/rho-ui#100",
        "",
    ),
    (
        "Approved · matthewyjiang/rho-ui#102",
        CardTone::Success,
        "matthewyjiang/rho-ui#102",
        "",
    ),
    (
        "New comments · matthewyjiang/rho-ui#101",
        CardTone::Accent,
        "matthewyjiang/rho-ui#101",
        "- 1 new comment:\n  - coderabbitai: \"Split the status view out of #10?\"",
    ),
    (
        "Completed · brief rereads",
        CardTone::Success,
        "run-7",
        "Fixed and pushed to `feat/lead-brief-after-compaction`.",
    ),
    (
        "New commits · matthewyjiang/rho#2040",
        CardTone::Neutral,
        "matthewyjiang/rho#2040",
        "",
    ),
    (
        "Changes requested · matthewyjiang/rho#2040",
        CardTone::Warning,
        "matthewyjiang/rho#2040",
        "- coderabbitai: \"Reread the brief before any write to it.\"",
    ),
];

/// Card `(title, tone, reference, body)` as rho's daemon sends a
/// notification: the bodies, and the card rho's terminal UI draws.
fn notification((title, tone, reference, body): (&str, CardTone, &str, &str)) -> SessionEvent {
    let display_tone = match tone {
        CardTone::Accent => NotificationTone::Accent,
        CardTone::Success => NotificationTone::Success,
        CardTone::Warning => NotificationTone::Warning,
        CardTone::Error => NotificationTone::Error,
        _ => NotificationTone::Neutral,
    };
    SessionEvent::Card {
        title: title.to_owned(),
        markdown: body.to_owned(),
        plain: body.to_owned(),
        tone,
        display: Some(Box::new(NotificationCard {
            title: title.to_owned(),
            sender: "github".to_owned(),
            recipient: "session".to_owned(),
            delivery: NotificationDelivery::Received,
            tone: display_tone,
            preview: NotificationPreview::Truncated,
            visibility: NotificationVisibility::Conversation,
            reference: Some(reference.to_owned()),
            subtitle: None,
            body: body.to_owned(),
            output: None,
            details: Vec::new(),
        })),
    }
}

#[derive(Debug)]
struct State {
    sessions: BTreeMap<String, FakeSession>,
    /// Whether it serves `rho/session/list_detailed`.
    session_lists: SessionLists,
    /// How many `rho/session/list_detailed` requests it got, each page and
    /// each refusal one.
    detailed_lists: usize,
    /// How many `rho/session/processes` requests it got.
    process_reads: usize,
    /// How many of each the daemon has made, for their IDs.
    new_sessions: u64,
    forks: u64,
    /// How many sessions asked for a new worktree.
    worktrees: usize,
    /// Session worktrees on disk, for `rho/worktree/prune`.
    session_worktrees: Vec<WorktreeReport>,
    /// How many requests clients held.
    holds: usize,
    /// The upgrade `rho/daemon/status` reports.
    upgrade: Option<PendingUpgrade>,
    /// What `rho/daemon/status` says stopping or restarting would cut short.
    in_progress: u64,
    /// While the daemon answers hellos as another build, what it says a
    /// restart costs.
    skew: Option<RestartCost>,
    /// While the daemon refuses hellos as older than its clients.
    older: Option<Older>,
    /// How many hellos it refused so.
    refusals: usize,
    /// How many hellos it accepted.
    hellos: usize,
    stops: usize,
    /// The environment the latest `rho/session/new` carried.
    new_session_environment: Option<ClientEnvironment>,
    turns: u64,
    steers: u64,
    /// Jobs of runs no session writes, listed after the sessions' own.
    extra_jobs: Vec<JobSummary>,
    /// rho's own settings.
    config: FakeConfig,
    /// The models a model setting may take, which every client that read
    /// them follows.
    models: watch::Sender<Arc<ModelList>>,
    auth: FakeAuth,
    pull_requests: FakePullRequests,
    /// Restart as the next prompt arrives; see
    /// [`FakeDaemon::restart_at_next_prompt`].
    restart_at_prompt: bool,
}

/// A daemon of an older protocol than its clients', until it is replaced.
#[derive(Debug)]
struct Older {
    /// What its refusals say a restart costs.
    cost: RestartCost,
    /// The file whose appearance replaces it.
    replaced: PathBuf,
}

impl Older {
    fn is_replaced(&self) -> bool {
        self.replaced.exists()
    }
}

/// A protocol older than any rho-ui speaks.
const OLDER_PROTOCOL: ProtocolVersion = ProtocolVersion::new(0, 0);

/// How the daemon answers a new connection's hello.
enum Greeting {
    /// As older than the client, saying a restart costs this.
    Refuse(RestartCost),
    /// Accepted, as a daemon of this build.
    Serve(BuildId),
}

/// Reads the hello on `stream` and refuses it as a daemon of
/// [`OLDER_PROTOCOL`] would, saying a restart costs `cost`, as rho-protocol's
/// own server does a client newer than itself; its server speaks only
/// [`PROTOCOL_VERSION`], so this writes the refusal itself. Counts it in
/// `service`'s refusals.
async fn refuse_as_older(stream: impl AsyncRead + AsyncWrite, cost: RestartCost, service: &Fake) {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut hello = String::new();
    if BufReader::new(reader).read_line(&mut hello).await.is_err() {
        return;
    }
    service.lock().refusals += 1;
    let id = serde_json::from_str::<Value>(&hello)
        .ok()
        .and_then(|hello| hello.get("id").cloned())
        .unwrap_or(Value::Null);
    let mismatch = VersionMismatch {
        restart: Some(cost),
        ..VersionMismatch::between(OLDER_PROTOCOL, PROTOCOL_VERSION)
            .expect("rho-ui speaks a protocol newer than 0.0")
    };
    let data = serde_json::to_value(mismatch).expect("a mismatch is JSON");
    let error = RpcError::new(ErrorCode::VersionMismatch, mismatch.to_string()).with_data(data);
    let mut answer = json!({ "jsonrpc": "2.0", "id": id, "error": error }).to_string();
    answer.push('\n');
    let _ = writer.write_all(answer.as_bytes()).await;
    let _ = writer.shutdown().await;
}

impl State {
    /// How to answer a new connection's hello: refused while the daemon is
    /// older than its clients and not yet replaced, else served as another
    /// build or this one.
    fn greet(&mut self) -> Greeting {
        if self.older.as_ref().is_some_and(Older::is_replaced) {
            self.older = None;
        }
        match (&self.older, self.skew) {
            (Some(older), _) => Greeting::Refuse(older.cost),
            (None, Some(_)) => Greeting::Serve(BuildId::new("another-build")),
            (None, None) => Greeting::Serve(BuildId::of_this_crate()),
        }
    }

    fn new(script: Script) -> Self {
        let mut sessions = BTreeMap::new();
        sessions.insert(
            "s1".to_owned(),
            FakeSession::new("/w/a", Some("first"), 1_767_312_000),
        );
        let mut old = FakeSession::new("/w/b", None, 1_767_225_600);
        replay(&mut old, script.replay_turns);
        if script.long_reply {
            // One paragraph: rho-ui draws a reply of several blocks in
            // parts, each parsed as a text of its own.
            let sentences: Vec<String> = (0..60)
                .map(|n| format!("Sentence {n} of a long reply, which says a good deal about very little, and then says it again."))
                .collect();
            old.say("long", &sentences.join(" "));
        }
        sessions.insert("old".to_owned(), old);
        let mut updated = 1_767_225_600;
        for n in 0..script.past_sessions {
            updated -= 3600;
            let title = format!("past {n}");
            sessions.insert(
                format!("past-{n}"),
                FakeSession::new("/w/b", Some(&title), updated),
            );
        }
        let mut state = Self {
            sessions,
            session_lists: script.session_lists,
            detailed_lists: 0,
            process_reads: 0,
            new_sessions: 0,
            forks: 0,
            worktrees: 0,
            session_worktrees: Vec::new(),
            holds: 0,
            upgrade: None,
            in_progress: 0,
            skew: None,
            older: None,
            refusals: 0,
            hellos: 0,
            stops: 0,
            new_session_environment: None,
            turns: 0,
            steers: 0,
            extra_jobs: Vec::new(),
            config: FakeConfig::new(),
            models: watch::Sender::new(Arc::new(model_list(&MODELS))),
            auth: FakeAuth::new(&script),
            pull_requests: FakePullRequests::new(script.pull_requests),
            restart_at_prompt: false,
        };
        if script.tui_job == TuiJob::Running {
            let mut session = FakeSession::new("/w/a", Some("from the tui"), 1_767_398_400);
            let mut turn = state.next_turn();
            // Under way for a minute and a half when the test starts.
            turn.began = turn
                .began
                .checked_sub(Duration::from_secs(90))
                .expect("the machine has been up for 90 s");
            session.turn = Some(turn);
            session.state(JobState::Running);
            session.publish(user_message("tui-u1", "refactor the parser"));
            session.say("tui-m1", "Reading the parser.");
            state.sessions.insert(TUI_SESSION.to_owned(), session);
        }
        state
    }

    /// The date of a session that saves something now: the time, as a
    /// daemon's clock says, and after every session's date, so sessions
    /// saved within one second keep their order.
    fn next_date(&self) -> u64 {
        let newest = self.sessions.values().map(|session| session.updated_at);
        let now = unix_now();
        newest.max().map_or(now, |newest| now.max(newest + 1))
    }

    fn next_turn(&mut self) -> Turn {
        self.turns += 1;
        Turn {
            number: self.turns,
            id: TurnId::new(format!("turn-{}", self.turns)),
            tool: ToolCallId::new(format!("tool-{}", self.turns)),
            input: None,
            queued: Vec::new(),
            began: Instant::now(),
        }
    }

    fn session(&mut self, id: &SessionId) -> Result<&mut FakeSession, RpcError> {
        self.sessions
            .get_mut(id.as_str())
            .ok_or_else(|| RpcError::new(ErrorCode::SessionNotFound, format!("no session {id}")))
    }
}
