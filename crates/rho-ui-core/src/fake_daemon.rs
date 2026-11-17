//! A scripted rho daemon for tests: rho-protocol's own server side on a real
//! Unix socket, or named pipe on Windows, with no model behind it.
//! rho-ui-core's daemon tests and rho-ui's headless UI tests both run it.
//!
//! - `rho/session/new` returns "new-1", "new-2", ...
//! - `rho/session/list` returns pages of two: new sessions that have had a
//!   prompt, newest first, then "s1" (titled "first") and "old" (untitled).
//!   `rho/session/delete` drops one, and `rho/session/rename` titles one.
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
//! - The prompt "report" answers with [`report`], a long reply that
//!   streams in over several events.
//! - The prompt "background" starts a background process, `sleep 30`, and
//!   answers at once, leaving it running. Stopping it, or
//!   [`FakeDaemon::finish_background`], ends it, and the daemon starts a
//!   turn of its own that tells the model; that turn runs until a steer
//!   ends it, and drops the work it delivered from the list as it ends.
//! - The prompt "subagent" starts a subagent, [`SUBAGENT_TASK`], and
//!   answers at once, leaving it running in a session of its own,
//!   "sub-N", which can be observed but not attached to.
//!   [`FakeDaemon::finish_subagent`] ends its run: the parent shows its
//!   whole result as a card, [`subagent_result`], and the parent's daemon
//!   turn tells the model, as for a process.
//! - [`FakeDaemon::start_workflow`] starts the workflow [`WORKFLOW_NAME`]
//!   in a session. Its two agent nodes are in the job list only: one
//!   queued second in line, one failed with the error [`WORKFLOW_ERROR`].
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
//!   behind; attaching again replays the calls with their surfaces.
//! - With [`Script::tui_job`], the session "tui-1" has a turn under way, as
//!   one rho's terminal UI started: it is listed, its job runs, and a steer
//!   is read at once, answered, and ends the turn. The steer "wait" is
//!   never read: it stays queued until withdrawn or the turn is cancelled.
//!   The steer "keep going" is read, and the turn runs on unanswered.
//! - Settings are shaped like rho's: four permission modes, three models,
//!   three reasoning levels, and fast mode. Every session offers the slash
//!   commands `compact` and `prompt:review` and the skills `pstack` and
//!   `prove-it-works`. A session adds a directory that is one on this
//!   machine, once, and refuses others as rho does.
//!   [`FakeDaemon::followers`] counts the clients following a session.
//! - rho's own settings are two pages. "General" holds
//!   `general.auto_compact` (on/off, on by default), `general.permission_mode`
//!   (a choice of auto, plan or supervised) and, under "Models",
//!   `general.title_model` (a model). "Tools" holds `tools.shell_timeout`
//!   (seconds, 1 to 3600) and `tools.shell` (text). Revisions count the
//!   writes: "r0", "r1", ... A write against an older revision is refused
//!   as a conflict, unless only its own writer wrote since.
//!   [`FakeDaemon::edit_agent_setting`] changes one as a hand edit does.
//! - Sessions run on [`KEY_AUTH`], which takes an API key and is signed in
//!   unless [`Script::signed_out`]; then a session opens only with
//!   `sign_in: later`. [`LOGIN_AUTH`] takes a login, whose prompt follows
//!   at once and which waits for [`FakeDaemon::approve_login`]. The key
//!   [`REFUSED_KEY`] is refused, and [`UNCHECKED_KEY`] saved unchecked. Credentials go to a file store the
//!   config chose.
//! - `rho/daemon/status` reports the upgrade [`FakeDaemon::replace_program`]
//!   set, if any. `rho/daemon/restart` restarts in place, as rho does: it
//!   counts ([`FakeDaemon::restarts`]), drops the upgrade, as the restarted
//!   daemon runs the new program, detaches every attached session as
//!   restarting, and closes every connection; new ones are served as
//!   before. [`FakeDaemon::attaches`] counts each session's attaches.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rho_protocol::{
    display::{
        CodeWindow, DiffRow, DiffRowKind, ToolBody, ToolCard, ToolCardStatus, ToolFact, ToolFamily,
        ToolHeader,
    },
    methods::{
        BackgroundItem, BackgroundStatus, BackgroundWork, Budget, FastModeOptions, JobId, JobKind,
        JobState, JobSummary, MessageId, ModelChoice, ModelOptions, ModelRef, PendingUpgrade,
        PermissionModeOptions, QueuePlace, ReasoningLevel, ReasoningOptions, Replay, Seq,
        SessionCommand, SessionId, SessionOptions, SessionSurfaceActionParams, SteerId, StreamId,
        ToolCallId, ToolCallUpdate, TurnId,
    },
    serve_connection, subscription, AnswerStop, BuildId, CardTone, Choice, ClientEnvironment,
    CommandKind, ContentBlock, DaemonEndpoint, DaemonInfo, DefaultAnswer, DefaultStyle,
    DetachReason, ErrorCode, InputRequest, PermissionMode, PlanEntry, PlanStatus, Published,
    Publisher, Question, RetryReason, RpcError, Selection, SessionAttachResponse, SessionEvent,
    Subscription, ToolContent, ToolKind, ToolStatus, TurnOutcome,
};
use serde_json::{json, Value};
use tokio::runtime::Runtime;

use crate::{agent_settings::SettingValue, DaemonTarget, SessionId as UiSessionId};

mod auth;
mod config;
mod listener;
mod service;
pub mod surface;

use auth::FakeAuth;
pub use auth::{KEY_AUTH, LOGIN_AUTH, LOGIN_CODE, LOGIN_URL, REFUSED_KEY, UNCHECKED_KEY};
use config::FakeConfig;
use listener::Listener;
use service::Fake;

/// The session [`Script::tui_job`] runs.
pub const TUI_SESSION: &str = "tui-1";

/// The task of the subagent the prompt "subagent" starts: its title in
/// the parent's background work, and its session's first message.
pub const SUBAGENT_TASK: &str = "Read the parser";

/// The name of the workflow [`FakeDaemon::start_workflow`] starts.
pub const WORKFLOW_NAME: &str = "review";

/// Why the workflow [`WORKFLOW_NAME`]'s second agent node failed.
pub const WORKFLOW_ERROR: &str = "provider refused the request";

/// The whole result the subagent [`SUBAGENT_TASK`] returns, in Markdown:
/// as long as an explorer's report, with headings, a list and code.
pub fn subagent_result() -> String {
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
    /// Whether "old"'s replay ends with an info card and a warning card.
    pub cards: bool,
    /// Whether "old"'s replay ends with a reply of one paragraph over
    /// 4 KiB, which GPUI Kit parses off the UI thread.
    pub long_reply: bool,
    /// How many more saved sessions, `past-0` and on, each an hour older
    /// than the last and older than "old", to fill long lists.
    pub past_sessions: usize,
    /// Whether [`KEY_AUTH`], which every session runs on, starts signed
    /// out, so a session opens only with `sign_in: later`.
    pub signed_out: bool,
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
        });
        let serving = Arc::clone(&service);
        runtime.spawn(async move {
            let info = DaemonInfo {
                version: "9.9.9".to_owned(),
                pid: std::process::id(),
                build: BuildId::of_this_crate(),
            };
            while let Ok(stream) = listener.accept().await {
                let (service, info) = (Arc::clone(&serving), info.clone());
                tokio::spawn(async move {
                    // Runs until the client leaves, or the daemon restarts.
                    let mut restarted = service.restarted.subscribe();
                    let restart = async move {
                        let _ = restarted.changed().await;
                    };
                    let _ = serve_connection(stream, &info, service, restart).await;
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

    /// How many times a client asked the daemon to restart.
    pub fn restarts(&self) -> usize {
        *self.service.restarted.borrow()
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

    /// How many worktrees the daemon has added for new sessions.
    pub fn worktrees(&self) -> usize {
        self.service.lock().worktrees
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

    /// Adds `path` as rho does: refuses one that is not a directory, or
    /// that the session reaches already.
    fn add_directory(&mut self, path: PathBuf) -> Result<(), RpcError> {
        let refuse = |why: String| Err(RpcError::new(ErrorCode::InvalidParams, why));
        if !path.is_dir() {
            return refuse(format!("{} is not a directory", path.display()));
        }
        if self.added_dirs.contains(&path) {
            return refuse(format!("the session already reaches {}", path.display()));
        }
        self.added_dirs.push(path);
        Ok(())
    }

    fn options(&self) -> SessionOptions {
        SessionOptions {
            model: ModelOptions {
                current: self.model.clone(),
                auth: KEY_AUTH.to_owned(),
                available: MODELS
                    .iter()
                    .map(|(name, label)| ModelChoice {
                        model: model(name),
                        label: (*label).to_owned(),
                    })
                    .collect(),
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
    /// Listed by `rho/session/list`: a new session is once it has a prompt.
    saved: bool,
    settings: Settings,
    state: JobState,
    turn: Option<Turn>,
    /// Every event so far, which an attach replays.
    history: Vec<SessionEvent>,
    publishers: Vec<Publisher>,
    /// How many times clients attached to it.
    attaches: usize,
    live_seq: u64,
    /// Background work, as the last background event listed it.
    background: Vec<BackgroundWork>,
    /// Whether a prompt "busy" was already refused.
    refused_busy: bool,
    /// A subagent's session, which only its run writes: observed, never
    /// attached to.
    subagent: bool,
    /// The actions clients sent on its surfaces.
    surface_actions: Vec<SessionSurfaceActionParams>,
}

impl FakeSession {
    fn new(cwd: &str, title: Option<&str>, updated_at: u64) -> Self {
        Self {
            cwd: PathBuf::from(cwd),
            title: title.map(str::to_owned),
            updated_at,
            saved: true,
            settings: Settings::new(),
            state: JobState::Idle,
            turn: None,
            history: Vec::new(),
            publishers: Vec::new(),
            attaches: 0,
            live_seq: 0,
            background: Vec::new(),
            refused_busy: false,
            subagent: false,
            surface_actions: Vec::new(),
        }
    }

    /// The session of a subagent in `cwd` whose run, `turn`, is under way
    /// on [`SUBAGENT_TASK`].
    fn subagent(cwd: &Path, turn: Turn) -> Self {
        let mut session = Self::new(&cwd.to_string_lossy(), None, 0);
        session.saved = false;
        session.subagent = true;
        session.turn = Some(turn);
        session.state(JobState::Running);
        session.publish(user_message("sub-u1", SUBAGENT_TASK));
        session.say("sub-m1", "Reading the parser.");
        session
    }

    /// Follows the session from here: what it holds now, then what it
    /// publishes until the client leaves. The replay leaves out background
    /// events, as rho's saved transcripts hold none (rho replays them only
    /// for a turn in progress, which the fake doesn't keep apart): the
    /// attach response lists the work as it is now, without work whose
    /// result the model has. Nor does it mark steers, as rho's doesn't.
    fn follow(&mut self) -> (SessionAttachResponse, Publisher, Subscription) {
        let replay: Vec<SessionEvent> = self
            .history
            .iter()
            .filter(|event| !matches!(event, SessionEvent::Background { .. }))
            .cloned()
            .map(|mut event| {
                if let SessionEvent::UserMessage { steer, .. } = &mut event {
                    *steer = None;
                }
                event
            })
            .collect();
        // As the daemon numbers a reset's replay: 0 for all but the last.
        let last = replay.len().saturating_sub(1);
        let replay: Vec<(Seq, SessionEvent)> = replay
            .into_iter()
            .enumerate()
            .map(|(ix, event)| (Seq(if ix == last { self.live_seq } else { 0 }), event))
            .collect();
        let response = SessionAttachResponse {
            stream: StreamId::new("stream"),
            workspace: self.cwd.clone(),
            replay: Replay::Reset,
            replay_events: u64::try_from(replay.len()).unwrap_or(u64::MAX),
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
        let (publisher, subscription) = subscription(BACKLOG, replay);
        (response, publisher, subscription)
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
        self.publish(SessionEvent::Background {
            work: self.background.clone(),
        });
    }

    /// Starts a background process `sleep 30` with ID `id`.
    fn start_background(&mut self, id: String) {
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

/// An info card and a warning card, as background work and failing hooks
/// send them.
fn cards(session: &mut FakeSession) {
    for (title, plain, tone) in [
        (
            "Completed · cargo test",
            "ran 2m 04s\n142 passed",
            CardTone::Neutral,
        ),
        (
            "Hook failed · pre-commit",
            "exit status 1",
            CardTone::Warning,
        ),
    ] {
        session.publish(SessionEvent::Card {
            title: title.to_owned(),
            markdown: plain.to_owned(),
            plain: plain.to_owned(),
            tone,
            display: None,
        });
    }
}

#[derive(Debug)]
struct State {
    sessions: BTreeMap<String, FakeSession>,
    /// How many of each the daemon has made, for their IDs.
    new_sessions: u64,
    /// How many sessions asked for a new worktree.
    worktrees: usize,
    /// How many requests clients held.
    holds: usize,
    /// The upgrade `rho/daemon/status` reports.
    upgrade: Option<PendingUpgrade>,
    /// The environment the latest `rho/session/new` carried.
    new_session_environment: Option<ClientEnvironment>,
    turns: u64,
    steers: u64,
    /// Jobs of runs no session writes, listed after the sessions' own.
    extra_jobs: Vec<JobSummary>,
    /// rho's own settings.
    config: FakeConfig,
    auth: FakeAuth,
}

impl State {
    fn new(script: Script) -> Self {
        let mut sessions = BTreeMap::new();
        sessions.insert(
            "s1".to_owned(),
            FakeSession::new("/w/a", Some("first"), 1_767_312_000),
        );
        let mut old = FakeSession::new("/w/b", None, 1_767_225_600);
        replay(&mut old, script.replay_turns);
        if script.cards {
            cards(&mut old);
        }
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
            new_sessions: 0,
            worktrees: 0,
            holds: 0,
            upgrade: None,
            new_session_environment: None,
            turns: 0,
            steers: 0,
            extra_jobs: Vec::new(),
            config: FakeConfig::new(),
            auth: FakeAuth::new(&script),
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
