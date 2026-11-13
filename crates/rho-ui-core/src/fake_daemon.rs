//! A scripted rho daemon for tests: rho-protocol's own server side on a real
//! Unix socket, with no model behind it. rho-ui-core's daemon tests and
//! rho-ui's headless UI tests both run it.
//!
//! - `rho/session/new` returns "new-1", "new-2", ...
//! - `rho/session/list` returns pages of two: new sessions that have had a
//!   prompt, newest first, then "s1" (titled "first") and "old" (untitled).
//!   `rho/session/delete` drops one.
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
//!   next "busy" is taken like any prompt. The prompt "deleting" is
//!   refused as busy with no turn under way, as a session being deleted
//!   refuses it.
//! - With [`Script::tui_job`], the session "tui-1" has a turn under way, as
//!   one rho's terminal UI started: it is listed, its job runs, and a steer
//!   is read at once, answered, and ends the turn. The steer "wait" is
//!   never read: it stays queued until withdrawn or the turn is cancelled.
//! - Settings are shaped like rho's: four permission modes, three models,
//!   three reasoning levels, and fast mode. Every session offers the slash
//!   commands `compact` and `prompt:review` and the skills `pstack` and
//!   `prove-it-works`.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rho_protocol::{
    display::{
        CodeWindow, DiffRow, DiffRowKind, ToolBody, ToolCard, ToolCardStatus, ToolFact, ToolFamily,
        ToolHeader,
    },
    methods::{
        BackgroundItem, BackgroundStatus, BackgroundWork, FastModeOptions, JobState, MessageId,
        ModelChoice, ModelOptions, ModelRef, PermissionModeOptions, ReasoningLevel,
        ReasoningOptions, Seq, SessionCommand, SessionId, SessionOptions, SteerId, ToolCallId,
        ToolCallUpdate, TurnId,
    },
    serve_connection, AnswerStop, BuildId, CardTone, CommandKind, ContentBlock, DaemonInfo,
    ErrorCode, InputRequest, PermissionMode, Published, Publisher, RpcError, SessionEvent,
    ToolContent, ToolKind, ToolStatus, TurnOutcome,
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
    /// Whether [`TUI_SESSION`] exists, with a turn under way.
    pub tui_job: TuiJob,
    /// Whether "old"'s replay ends with an info card and a warning card.
    pub cards: bool,
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
            let info = DaemonInfo {
                version: "9.9.9".to_owned(),
                pid: std::process::id(),
                build: BuildId::of_this_crate(),
            };
            while let Ok((stream, _)) = listener.accept().await {
                let (service, info) = (Arc::clone(&serving), info.clone());
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

    /// How many worktrees the daemon has added for new sessions.
    pub fn worktrees(&self) -> usize {
        self.service.lock().worktrees
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
        if script.cards {
            cards(&mut old);
        }
        sessions.insert("old".to_owned(), old);
        let mut state = Self {
            sessions,
            new_sessions: 0,
            worktrees: 0,
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
