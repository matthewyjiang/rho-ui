//! A scripted rho daemon for tests: rho-protocol's own server side on a real
//! Unix socket, with no model behind it. rho-ui-core's daemon tests and
//! rho-ui's headless UI tests both run it.
//!
//! - `rho/session/new` returns "new-1", "new-2", ...
//! - `rho/session/list` returns pages of two: new sessions that have had a
//!   prompt, newest first, then "s1" (titled "first") and "old" (untitled),
//!   then [`Script::sessions`] more. `rho/session/delete` drops one.
//! - Attaching to "old" replays one agent message, "replayed", then
//!   [`Script::replay_turns`] turns, each a question, a thought, three tool
//!   calls, an edit, and a Markdown answer.
//! - A prompt asks to run `ls`, then answers "you chose" and the answer. The
//!   prompt "fail" is refused, and "slow" is taken half a second late.
//! - The prompt "background" starts a background process, `sleep 30`, and
//!   answers at once, leaving it running. Stopping it, or
//!   [`FakeDaemon::finish_background`], ends it, and the daemon starts a
//!   turn of its own that tells the model; that turn runs until a steer
//!   ends it, and drops the work it delivered from the list as it ends.
//! - A session's first prompt "busy" is refused as busy: the daemon has
//!   just started a turn of its own, which answers and ends at once. The
//!   next "busy" is taken like any prompt.
//! - With [`Script::tui_job`], the session "tui-1" has a turn under way, as
//!   one rho's terminal UI started: it is listed, its job runs, and a steer
//!   is read at once, answered, and ends the turn. The steer "wait" is
//!   never read: it stays queued until withdrawn or the turn is cancelled.
//! - Settings are shaped like rho's: four permission modes, three models,
//!   three reasoning levels, and fast mode.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rho_protocol::{
    methods::{
        BackgroundItem, BackgroundStatus, BackgroundWork, FastModeOptions, JobState, MessageId,
        ModelChoice, ModelOptions, ModelRef, PermissionModeOptions, ReasoningLevel,
        ReasoningOptions, Seq, SessionCommand, SessionId, SessionOptions, SteerId, ToolCallId,
        ToolCallUpdate, TurnId,
    },
    serve_connection, AnswerStop, BuildId, CommandKind, ContentBlock, DaemonInfo, ErrorCode,
    InputRequest, PermissionMode, Published, Publisher, RpcError, SessionEvent, ToolContent,
    ToolKind, ToolStatus, TurnOutcome,
};
use tokio::{net::UnixListener, runtime::Runtime};

use crate::{DaemonTarget, SessionId as UiSessionId};

mod service;

use service::Fake;

/// The session [`Script::tui_job`] runs.
pub const TUI_SESSION: &str = "tui-1";

/// What the fake daemon holds when it starts.
#[derive(Clone, Copy, Debug, Default)]
pub struct Script {
    /// Turns that attaching to "old" replays after its one message.
    pub replay_turns: usize,
    /// More saved sessions, "bulk-0" onwards, listed after "old".
    pub sessions: usize,
    /// Whether [`TUI_SESSION`] exists, with a turn under way.
    pub tui_job: TuiJob,
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

/// A running fake daemon. Dropping it stops it and removes its socket.
#[derive(Debug)]
pub struct FakeDaemon {
    socket: PathBuf,
    service: Arc<Fake>,
    _dir: tempfile::TempDir,
    _runtime: Runtime,
}

impl FakeDaemon {
    /// Starts a daemon holding `script`, listening on a socket in a
    /// directory of its own.
    ///
    /// # Panics
    ///
    /// If the OS refuses a temporary directory, a socket in it, or a
    /// thread: a test cannot go on without them.
    pub fn start(script: Script) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory for the fake daemon");
        let socket = dir.path().join("daemon.sock");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_io()
            .enable_time()
            .build()
            .expect("a runtime for the fake daemon");
        let listener = {
            let _guard = runtime.enter();
            UnixListener::bind(&socket).expect("the fake daemon binds its socket")
        };
        let service = Arc::new(Fake {
            state: Mutex::new(State::new(script)),
        });
        let serving = Arc::clone(&service);
        runtime.spawn(async move {
            let service = serving;
            let info = DaemonInfo {
                version: "9.9.9".to_owned(),
                pid: std::process::id(),
                build: BuildId::of_this_crate(),
            };
            while let Ok((stream, _)) = listener.accept().await {
                let (service, info) = (Arc::clone(&service), info.clone());
                tokio::spawn(async move {
                    // Runs until the client leaves; the fake never stops
                    // a connection itself.
                    let _ = serve_connection(stream, &info, service, std::future::pending()).await;
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
}

impl Settings {
    fn new() -> Self {
        Self {
            mode: PermissionMode::Auto,
            model: model("small"),
            reasoning: ReasoningLevel::Medium,
            fast: false,
        }
    }

    fn options(&self) -> SessionOptions {
        SessionOptions {
            model: ModelOptions {
                current: self.model.clone(),
                auth: "fake-api-key".to_owned(),
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
            commands: vec![SessionCommand {
                name: "compact".to_owned(),
                kind: CommandKind::Slash,
                description: "Compact".to_owned(),
                input_hint: None,
            }],
            added_dirs: Vec::new(),
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
    live_seq: u64,
    /// Background work, as the last background event listed it.
    background: Vec<BackgroundWork>,
    /// Whether a prompt "busy" was already refused.
    refused_busy: bool,
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
            live_seq: 0,
            background: Vec::new(),
            refused_busy: false,
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

    fn tool(
        &mut self,
        id: &ToolCallId,
        title: &str,
        kind: ToolKind,
        status: ToolStatus,
        output: Option<ToolContent>,
    ) {
        self.publish(SessionEvent::tool_call(ToolCallUpdate {
            id: id.clone(),
            title: Some(title.to_owned()),
            kind: Some(kind),
            status: Some(status),
            raw_input: None,
            content: output.map(|output| vec![output]),
            locations: None,
            display: None,
        }));
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

    /// Finishes the turn as answered with `text`.
    fn answer(&mut self, message: &str, text: &str) {
        self.say(message, text);
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

fn text(text: &str) -> ToolContent {
    ToolContent::Text {
        text: text.to_owned(),
    }
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
        let matches: Vec<String> = (0..20).map(|i| format!("src/{n}.rs:{i}: match")).collect();
        // A run of tool calls, as agents make them.
        for (k, (kind, title)) in [
            (ToolKind::Read, format!("Read src/{n}.rs")),
            (ToolKind::Search, format!("rg -n pattern src/{n}.rs")),
            (ToolKind::Execute, format!("cargo test walk_{n}")),
        ]
        .into_iter()
        .enumerate()
        {
            session.tool(
                &ToolCallId::new(format!("t{n}-{k}")),
                &title,
                kind,
                ToolStatus::Completed,
                Some(text(&matches.join("\n"))),
            );
        }
        session.tool(
            &ToolCallId::new(format!("t{n}-edit")),
            &format!("Edit src/{n}.rs"),
            ToolKind::Edit,
            ToolStatus::Completed,
            Some(ToolContent::Diff {
                path: PathBuf::from(format!("/w/b/src/{n}.rs")),
                old_text: Some(
                    "fn walk(tree: &Tree) -> Vec<Node> {\n    let mut out = vec![];\n    out\n}\n"
                        .to_owned(),
                ),
                new_text: "fn walk(tree: &Tree) -> Vec<Node> {\n    let mut out = Vec::with_capacity(tree.len());\n    out.extend(tree.roots());\n    out\n}\n"
                    .to_owned(),
                deleted: false,
            }),
        );
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

#[derive(Debug)]
struct State {
    sessions: BTreeMap<String, FakeSession>,
    /// How many of each the daemon has made, for their IDs.
    new_sessions: u64,
    turns: u64,
    steers: u64,
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
        sessions.insert("old".to_owned(), old);
        for n in 0..script.sessions {
            let title = format!("Bulk session {n} with a longer title that truncates");
            let cwd = format!("/w/project-{}", n % 7);
            sessions.insert(
                format!("bulk-{n}"),
                FakeSession::new(&cwd, Some(&title), 1_764_547_200),
            );
        }
        let mut state = Self {
            sessions,
            new_sessions: 0,
            turns: 0,
            steers: 0,
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
