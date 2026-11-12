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
//! - With [`Script::tui_job`], the session "tui-1" has a turn under way, as
//!   one rho's terminal UI started: it is listed, its job runs, and a steer
//!   is read at once, answered, and ends the turn. The steer "wait" is
//!   never read: it stays queued until withdrawn or the turn is cancelled.
//! - Settings are shaped like rho's: four permission modes, three models,
//!   three reasoning levels, and fast mode.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use rho_protocol::{
    methods::{
        AuthChooseStoreParams, AuthChooseStoreResponse, AuthKeyParams, AuthKeyResponse,
        AuthListParams, AuthListResponse, AuthLoginCancelParams, AuthLoginCancelResponse,
        AuthLoginParams, AuthLoginResponse, AuthLogoutParams, AuthLogoutResponse, ConfigGetParams,
        ConfigRegisterParams, ConfigRegisterResponse, ConfigSchema, ConfigSetParams,
        ConfigSetResponse, FastModeOptions, JobId, JobKind, JobListResponse, JobLive, JobState,
        JobSummary, MessageId, ModelChoice, ModelOptions, ModelRef, PermissionModeOptions,
        ReasoningLevel, ReasoningOptions, Replay, Resolution, Seq, SessionCommand,
        SessionConfigureParams, SessionConfigureResponse, SessionDeleteParams,
        SessionDeleteResponse, SessionId, SessionInspectParams, SessionInspectResponse,
        SessionListParams, SessionListResponse, SessionMcpCatalog, SessionMcpCatalogParams,
        SessionMcpCompleteParams, SessionMcpCompleteResponse, SessionMcpInventory,
        SessionMcpParams, SessionMcpResourceParams, SessionMcpResourceResponse,
        SessionObserveParams, SessionOptions, SessionProcessesParams, SessionProcessesResponse,
        SessionRenameParams, SessionRenameResponse, SessionSummary, SessionUnsteerParams,
        SessionUnsteerResponse, SteerId, StreamId, ToolCallId, ToolCallUpdate, TurnId,
    },
    serve_connection, subscription, AnswerStop, ApprovalAsk, ApprovalSource, Ask, BuildId,
    ClientInfo, CommandKind, ConfigWatch, ContentBlock, CredentialStoreKind, CredentialStoreStatus,
    DaemonInfo, DaemonStatus, DetachReason, ErrorCode, InputAnswer, InputAsker, InputRequest,
    InputRequestId, LoginStream, Operation, PermissionMode, ProtocolVersion, Publisher,
    RestartCost, RpcError, Service, SessionAnswerParams, SessionAnswerResponse,
    SessionAttachParams, SessionAttachResponse, SessionCancelParams, SessionCancelResponse,
    SessionChange, SessionEvent, SessionHoldParams, SessionHoldResponse, SessionNewParams,
    SessionNewResponse, SessionPromptParams, SessionPromptResponse, SessionSteerParams,
    SessionSteerResponse, SettingsService, StoreSelectedBy, Subscription, ToolContent, ToolKind,
    ToolStatus, TurnOutcome, UnsteerOutcome, PROTOCOL_VERSION,
};
use tokio::{net::UnixListener, runtime::Runtime};

use crate::DaemonTarget;

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
        runtime.spawn(async move {
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
            _dir: dir,
            _runtime: runtime,
        }
    }

    /// How a client reaches this daemon.
    pub fn target(&self) -> DaemonTarget {
        DaemonTarget::Socket(self.socket.clone())
    }
}

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
        }
    }

    /// Records `event` and sends it to every attached client.
    fn publish(&mut self, event: SessionEvent) {
        if let SessionEvent::State { state, .. } = &event {
            self.state = *state;
        }
        self.live_seq += 1;
        let seq = Seq(self.live_seq);
        self.publishers
            .retain(|publisher| publisher.try_publish(seq, event.clone()).is_ok());
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

    /// Ends the running turn with `outcome`.
    fn end_turn(&mut self, outcome: TurnOutcome, state: JobState) {
        let Some(turn) = self.turn.take() else {
            return;
        };
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
            session.turn = Some(state.next_turn());
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
        }
    }

    fn session(&mut self, id: &SessionId) -> Result<&mut FakeSession, RpcError> {
        self.sessions
            .get_mut(id.as_str())
            .ok_or_else(|| RpcError::new(ErrorCode::SessionNotFound, format!("no session {id}")))
    }
}

struct Fake {
    state: Mutex<State>,
}

impl Fake {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("no test panics holding the fake daemon's state")
    }
}

fn not_steerable() -> RpcError {
    RpcError::new(ErrorCode::NotSteerable, "no model request is taking input")
}

fn unsupported(method: &str) -> RpcError {
    RpcError::new(
        ErrorCode::MethodNotFound,
        format!("the fake daemon does not serve {method}"),
    )
}

/// How an approval's answer reads in the turn's reply.
fn chosen(answer: &InputAnswer) -> &'static str {
    match answer {
        InputAnswer::AllowOnce => "allow_once",
        InputAnswer::AllowForSession => "allow_for_session",
        InputAnswer::Deny { .. } => "deny",
        _ => "something else",
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the trait's methods return futures; a scripted answer has nothing to await"
)]
impl Service for Fake {
    fn refused_newer(&self, _: ProtocolVersion) -> RestartCost {
        RestartCost {
            in_progress: 0,
            clients: 0,
        }
    }

    fn skewed(&self, _: &ClientInfo, _: &BuildId) -> RestartCost {
        RestartCost {
            in_progress: 0,
            clients: 0,
        }
    }

    async fn status(&self) -> DaemonStatus {
        DaemonStatus {
            pid: std::process::id(),
            version: "9.9.9".to_owned(),
            protocol: PROTOCOL_VERSION,
            started_at: 0,
            uptime_secs: 0,
            jobs: 0,
            socket: PathBuf::new(),
            credential_store: CredentialStoreStatus {
                store: CredentialStoreKind::File,
                selected_by: StoreSelectedBy::Config,
                unreachable: None,
            },
            upgrade: None,
        }
    }

    async fn stop(&self) {}

    async fn restart(&self) {}

    async fn new_session(&self, params: SessionNewParams) -> Result<SessionNewResponse, RpcError> {
        let mut state = self.lock();
        state.new_sessions += 1;
        let id = format!("new-{}", state.new_sessions);
        let mut session = FakeSession::new(&params.cwd.to_string_lossy(), None, 0);
        session.saved = false;
        let options = session.settings.options();
        state.sessions.insert(id.clone(), session);
        Ok(SessionNewResponse {
            session: SessionId::new(id),
            options,
        })
    }

    async fn prompt_session(
        &self,
        params: SessionPromptParams,
    ) -> Result<SessionPromptResponse, RpcError> {
        let prompt = match params.content.first() {
            Some(ContentBlock::Text { text }) => text.clone(),
            _ => String::new(),
        };
        if prompt == "fail" {
            return Err(RpcError::new(ErrorCode::SessionBusy, "the session is busy"));
        }
        if prompt == "slow" {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let mut state = self.lock();
        let newest = state
            .sessions
            .values()
            .map(|session| session.updated_at)
            .max()
            .unwrap_or_default();
        let turn = state.next_turn();
        let turn_id = turn.id.clone();
        let session = state.session(&params.session)?;
        if session.turn.is_some() {
            return Err(RpcError::new(ErrorCode::SessionBusy, "the session is busy"));
        }
        session.saved = true;
        session.updated_at = newest + 1;
        let request = InputRequest {
            id: InputRequestId(turn.number),
            ask: Ask::Approval(ApprovalAsk {
                source: ApprovalSource::BuiltInTool {
                    name: "bash".to_owned(),
                },
                operation: Operation::RunProcess {
                    program: PathBuf::from("ls"),
                    args: Vec::new(),
                    shell_command: Some("ls".to_owned()),
                    cwd: session.cwd.clone(),
                },
                reason: "the fake asks before every command".to_owned(),
            }),
            tool_call: Some(turn.tool.clone()),
            timeout: None,
            asker: InputAsker::Turn,
        };
        let tool = turn.tool.clone();
        session.turn = Some(Turn {
            input: Some(request.clone()),
            ..turn
        });
        session.state(JobState::Running);
        session.publish(user_message(&format!("u-{turn_id}"), &prompt));
        session.tool(&tool, "ls", ToolKind::Execute, ToolStatus::Pending, None);
        session.publish(SessionEvent::InputRequested {
            request: Box::new(request),
        });
        session.state(JobState::WaitingForInput);
        Ok(SessionPromptResponse { turn: turn_id })
    }

    async fn cancel_session(
        &self,
        params: SessionCancelParams,
    ) -> Result<SessionCancelResponse, RpcError> {
        let mut state = self.lock();
        let session = state.session(&params.session)?;
        if let Some(turn) = &mut session.turn {
            let tool = turn.tool.clone();
            if let Some(request) = turn.input.take() {
                session.publish(SessionEvent::InputResolved {
                    id: request.id,
                    resolution: Resolution::Withdrawn,
                });
                session.tool(&tool, "ls", ToolKind::Execute, ToolStatus::Failed, None);
            }
            session.end_turn(TurnOutcome::Cancelled, JobState::Cancelled);
        }
        Ok(SessionCancelResponse {})
    }

    async fn steer_session(
        &self,
        params: SessionSteerParams,
    ) -> Result<SessionSteerResponse, RpcError> {
        let mut state = self.lock();
        state.steers += 1;
        let id = SteerId::new(format!("steer-{}", state.steers));
        let session = state.session(&params.session)?;
        let Some(turn) = session.turn.as_mut().filter(|turn| turn.input.is_none()) else {
            return Err(not_steerable());
        };
        if params.text == "wait" {
            turn.queued.push(id.clone());
            return Ok(SessionSteerResponse { id });
        }
        session.publish(SessionEvent::UserMessage {
            message: MessageId::new(format!("u-{id}")),
            content: vec![ContentBlock::Text {
                text: params.text.clone(),
            }],
            steer: Some(id.clone()),
        });
        session.answer(&format!("m-{id}"), &format!("steered: {}", params.text));
        Ok(SessionSteerResponse { id })
    }

    async fn unsteer_session(
        &self,
        params: SessionUnsteerParams,
    ) -> Result<SessionUnsteerResponse, RpcError> {
        let mut state = self.lock();
        let Some(turn) = &mut state.session(&params.session)?.turn else {
            return Err(not_steerable());
        };
        let queued = turn.queued.iter().position(|id| *id == params.id);
        let outcome = match queued {
            Some(at) => {
                turn.queued.remove(at);
                UnsteerOutcome::Withdrawn
            }
            None => UnsteerOutcome::NotFound,
        };
        Ok(SessionUnsteerResponse { outcome })
    }

    async fn answer_session(
        &self,
        params: SessionAnswerParams,
    ) -> Result<SessionAnswerResponse, RpcError> {
        let mut state = self.lock();
        let session = state.session(&params.session)?;
        let open = session
            .turn
            .as_mut()
            .and_then(|turn| turn.input.take_if(|request| request.id == params.request));
        let (Some(request), Some(turn)) = (open, &session.turn) else {
            return Err(RpcError::new(
                ErrorCode::InputNotOpen,
                "the input request is not open",
            ));
        };
        let (tool, turn_id) = (turn.tool.clone(), turn.id.clone());
        session.publish(SessionEvent::InputResolved {
            id: request.id,
            resolution: Resolution::Answered,
        });
        session.state(JobState::Running);
        if matches!(params.answer, InputAnswer::Deny { .. }) {
            session.tool(&tool, "ls", ToolKind::Execute, ToolStatus::Failed, None);
        } else {
            let output = Some(text("a.rs\nb.rs"));
            session.tool(
                &tool,
                "ls",
                ToolKind::Execute,
                ToolStatus::Completed,
                output,
            );
        }
        let reply = format!("you chose {}", chosen(&params.answer));
        session.answer(&format!("m-{turn_id}"), &reply);
        Ok(SessionAnswerResponse {})
    }

    async fn hold_session(&self, _: SessionHoldParams) -> Result<SessionHoldResponse, RpcError> {
        Err(unsupported("rho/session/hold"))
    }

    async fn configure_session(
        &self,
        params: SessionConfigureParams,
    ) -> Result<SessionConfigureResponse, RpcError> {
        let mut state = self.lock();
        let session = state.session(&params.session)?;
        match params.change {
            SessionChange::PermissionMode { mode } => session.settings.mode = mode,
            SessionChange::Model { model, .. } => session.settings.model = model,
            SessionChange::Reasoning { level } => session.settings.reasoning = level,
            SessionChange::FastMode { enabled } => session.settings.fast = enabled,
            _ => return Err(unsupported("that session change")),
        }
        let options = session.settings.options();
        session.publish(SessionEvent::OptionsChanged {
            options: Box::new(options.clone()),
        });
        Ok(SessionConfigureResponse { options })
    }

    async fn session_mcp(&self, _: SessionMcpParams) -> Result<SessionMcpInventory, RpcError> {
        Err(unsupported("rho/session/mcp"))
    }

    async fn session_mcp_catalog(
        &self,
        _: SessionMcpCatalogParams,
    ) -> Result<SessionMcpCatalog, RpcError> {
        Err(unsupported("rho/session/mcp/catalog"))
    }

    async fn session_mcp_resource(
        &self,
        _: SessionMcpResourceParams,
    ) -> Result<SessionMcpResourceResponse, RpcError> {
        Err(unsupported("rho/session/mcp/resource"))
    }

    async fn session_mcp_complete(
        &self,
        _: SessionMcpCompleteParams,
    ) -> Result<SessionMcpCompleteResponse, RpcError> {
        Err(unsupported("rho/session/mcp/complete"))
    }

    async fn session_processes(
        &self,
        _: SessionProcessesParams,
    ) -> Result<SessionProcessesResponse, RpcError> {
        Err(unsupported("rho/session/processes"))
    }

    async fn inspect_session(
        &self,
        _: SessionInspectParams,
    ) -> Result<SessionInspectResponse, RpcError> {
        Err(unsupported("rho/session/inspect"))
    }

    async fn attach_session(
        &self,
        params: SessionAttachParams,
    ) -> Result<(SessionAttachResponse, Subscription), RpcError> {
        let mut state = self.lock();
        let session = state.session(&params.session)?;
        // As the daemon numbers a reset's replay: 0 for all but the last.
        let last = session.history.len().saturating_sub(1);
        let replay = session.history.iter().cloned().enumerate();
        let replay: Vec<(Seq, SessionEvent)> = replay
            .map(|(ix, event)| (Seq(if ix == last { session.live_seq } else { 0 }), event))
            .collect();
        let response = SessionAttachResponse {
            stream: StreamId::new("stream"),
            workspace: session.cwd.clone(),
            replay: Replay::Reset,
            replay_events: u64::try_from(replay.len()).unwrap_or(u64::MAX),
            state: session.state,
            turn: session.turn.as_ref().map(|turn| turn.id.clone()),
            turn_elapsed_ms: session.turn.as_ref().map(|_| 90_000),
            inputs: session
                .turn
                .iter()
                .filter_map(|turn| turn.input.clone())
                .collect(),
            title: session.title.clone(),
            options: session.settings.options(),
        };
        // Room for a whole scripted turn; a test client never falls behind.
        let (publisher, subscription) = subscription(4096, replay);
        session.publishers.push(publisher);
        Ok((response, subscription))
    }

    async fn observe_session(
        &self,
        _: SessionObserveParams,
    ) -> Result<(SessionAttachResponse, Subscription), RpcError> {
        Err(unsupported("rho/session/observe"))
    }

    async fn list_sessions(
        &self,
        params: SessionListParams,
    ) -> Result<SessionListResponse, RpcError> {
        /// Small, so two sessions already exercise the cursor.
        const PAGE: usize = 2;
        let state = self.lock();
        let mut saved: Vec<(&String, &FakeSession)> = state
            .sessions
            .iter()
            .filter(|(_, session)| session.saved)
            .collect();
        saved.sort_by_key(|(_, session)| std::cmp::Reverse(session.updated_at));
        let start = match params.cursor.as_deref().map(str::parse::<usize>) {
            None => 0,
            Some(Ok(start)) => start,
            Some(Err(_)) => {
                return Err(RpcError::new(ErrorCode::InvalidParams, "not a list cursor"));
            }
        };
        let sessions = saved
            .iter()
            .skip(start)
            .take(PAGE)
            .map(|(id, session)| SessionSummary {
                session: SessionId::new(id.as_str()),
                cwd: session.cwd.clone(),
                added_dirs: Vec::new(),
                title: session.title.clone(),
                first_user_message: None,
                last_user_message: None,
                updated_at: session.updated_at,
            });
        Ok(SessionListResponse {
            sessions: sessions.collect(),
            next_cursor: (start + PAGE < saved.len()).then(|| (start + PAGE).to_string()),
        })
    }

    async fn rename_session(
        &self,
        _: SessionRenameParams,
    ) -> Result<SessionRenameResponse, RpcError> {
        Err(unsupported("rho/session/rename"))
    }

    async fn delete_session(
        &self,
        params: SessionDeleteParams,
    ) -> Result<SessionDeleteResponse, RpcError> {
        let mut state = self.lock();
        state.session(&params.session)?;
        if let Some(session) = state.sessions.remove(params.session.as_str()) {
            for publisher in session.publishers {
                publisher.close(DetachReason::Deleted);
            }
        }
        Ok(SessionDeleteResponse {})
    }

    async fn list_jobs(&self) -> JobListResponse {
        let state = self.lock();
        let jobs = state.sessions.iter().map(|(id, session)| JobSummary {
            id: JobId::new(format!("job-{id}")),
            kind: JobKind::Session,
            state: session.state,
            workspace: session.cwd.clone(),
            parent: None,
            grant: None,
            grant_limit: None,
            session: Some(SessionId::new(id.as_str())),
            run: None,
            workflow_run: None,
            result_pending: false,
            created_at: 0,
            updated_at: session.updated_at,
            error: None,
            queue: None,
            live: Some(JobLive {
                title: session.title.clone(),
                model: session.settings.model.clone(),
                turn_elapsed_ms: session.turn.as_ref().map(|_| 90_000),
                usage: None,
                inputs: session
                    .turn
                    .iter()
                    .filter_map(|turn| turn.input.clone())
                    .collect(),
            }),
        });
        JobListResponse {
            jobs: jobs.collect(),
        }
    }

    async fn save_auth_key(&self, _: AuthKeyParams) -> Result<AuthKeyResponse, RpcError> {
        Err(unsupported("rho/auth/key"))
    }

    async fn logout_auth(&self, _: AuthLogoutParams) -> Result<AuthLogoutResponse, RpcError> {
        Err(unsupported("rho/auth/logout"))
    }

    async fn list_auths(&self, _: AuthListParams) -> Result<AuthListResponse, RpcError> {
        Err(unsupported("rho/auth/list"))
    }

    async fn login(
        &self,
        _: AuthLoginParams,
    ) -> Result<(AuthLoginResponse, LoginStream), RpcError> {
        Err(unsupported("rho/auth/login"))
    }

    async fn cancel_login(
        &self,
        _: AuthLoginCancelParams,
    ) -> Result<AuthLoginCancelResponse, RpcError> {
        Err(unsupported("rho/auth/login_cancel"))
    }

    async fn choose_store(
        &self,
        _: AuthChooseStoreParams,
    ) -> Result<AuthChooseStoreResponse, RpcError> {
        Err(unsupported("rho/auth/choose_store"))
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the trait's methods return futures; a scripted answer has nothing to await"
)]
impl SettingsService for Fake {
    async fn config_schema(&self) -> Result<ConfigSchema, RpcError> {
        Err(unsupported("rho/config/schema"))
    }

    async fn register_config(
        &self,
        _: ConfigRegisterParams,
    ) -> Result<ConfigRegisterResponse, RpcError> {
        Err(unsupported("rho/config/register"))
    }

    async fn get_config(&self, _: ConfigGetParams) -> Result<ConfigWatch, RpcError> {
        Err(unsupported("rho/config/get"))
    }

    async fn set_config(&self, _: ConfigSetParams) -> Result<ConfigSetResponse, RpcError> {
        Err(unsupported("rho/config/set"))
    }
}
