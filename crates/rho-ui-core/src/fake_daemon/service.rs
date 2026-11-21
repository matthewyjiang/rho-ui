//! The fake daemon's answer to each protocol method.

use std::{
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rho_protocol::{
    methods::{
        JobId, JobKind, JobListResponse, JobLive, JobState, JobSummary, MessageId, Resolution,
        SessionConfigureParams, SessionConfigureResponse, SessionDeleteParams,
        SessionDeleteResponse, SessionId, SessionListParams, SessionListResponse,
        SessionObserveParams, SessionRenameParams, SessionRenameResponse, SessionSummary,
        SessionUnsteerParams, SessionUnsteerResponse, SteerId, StreamId,
    },
    ApprovalAsk, ApprovalSource, Ask, AuthChooseStoreParams, AuthChooseStoreResponse,
    AuthKeyParams, AuthKeyResponse, AuthListParams, AuthListResponse, AuthLoginCancelParams,
    AuthLoginCancelResponse, AuthLoginParams, AuthLoginResponse, AuthLogoutParams,
    AuthLogoutResponse, BackgroundStatus, BuildId, ClientInfo, ConfigGetParams, ConfigSchema,
    ConfigSetParams, ConfigSetResponse, ConfigWatch, ContentBlock, DaemonStatus, DetachReason,
    ErrorCode, InputAnswer, InputAsker, InputRequest, InputRequestId, InputTimeout, LoginStream,
    Operation, PermissionMode, PlanApprovalAsk, ProtocolVersion, QuestionAsk, RestartCost,
    RpcError, Service, SessionAnswerParams, SessionAnswerResponse, SessionAttachParams,
    SessionAttachResponse, SessionBackgroundStopParams, SessionBackgroundStopResponse,
    SessionCancelParams, SessionCancelResponse, SessionChange, SessionEvent, SessionHoldParams,
    SessionHoldResponse, SessionNewParams, SessionNewResponse, SessionPromptParams,
    SessionPromptResponse, SessionSteerParams, SessionSteerResponse, SessionSurfaceActionParams,
    SessionSurfaceActionResponse, SessionWorktree, SettingsService, SignIn, Subscription, ToolKind,
    ToolStatus, TurnOutcome, UnsteerOutcome, WorktreePruneParams, WorktreePruneResponse,
    WorktreeVerdict, PROTOCOL_VERSION,
};

use super::{
    report, shell_card, shell_output, surface, FakeSession, State, Turn, KEY_AUTH, TOOL_IMAGE,
};

#[derive(Debug)]
pub(super) struct Fake {
    pub(super) state: Mutex<State>,
    /// Counts restarts, which each start another stream.
    pub(super) restarted: tokio::sync::watch::Sender<usize>,
    /// Changes at each restart and stop; every connection ends then, as
    /// a daemon restarting or stopping closes them.
    pub(super) closed: tokio::sync::watch::Sender<()>,
}

impl Fake {
    pub(super) fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("no test panics holding the fake daemon's state")
    }

    /// Restarts in place, as rho does: the restarted daemon runs the new
    /// program, so no upgrade waits; every attached session is detached as
    /// restarting, new sessions no prompt saved are gone, then every
    /// connection closes.
    pub(super) fn restart_now(&self) {
        let mut state = self.lock();
        state.upgrade = None;
        // It runs the program installed, rho-ui's own build.
        state.skew = None;
        state
            .sessions
            .retain(|_, session| session.saved || session.subagent);
        for session in state.sessions.values_mut() {
            for publisher in session.publishers.drain(..) {
                publisher.close(DetachReason::Restarting);
            }
        }
        drop(state);
        self.restarted.send_modify(|restarts| *restarts += 1);
        self.closed.send_replace(());
    }

    /// The stream of this run of the daemon: a restart starts another.
    fn stream(&self) -> StreamId {
        StreamId::new(format!("stream-{}", *self.restarted.borrow()))
    }
}

/// Starts the turn of the prompt `params`, as each prompt scripts it.
fn start_prompt(
    state: &mut State,
    params: &SessionPromptParams,
) -> Result<SessionPromptResponse, RpcError> {
    let prompt = match params.content.first() {
        Some(ContentBlock::Text { text }) => text.clone(),
        _ => String::new(),
    };
    if prompt == "fail" {
        return Err(RpcError::new(
            ErrorCode::InvalidParams,
            "the fake daemon refuses this prompt",
        ));
    }
    if prompt == "deleting" {
        return Err(RpcError::new(
            ErrorCode::SessionBusy,
            "the session is being deleted",
        ));
    }
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
        return Err(busy());
    }
    session.saved = true;
    session.updated_at = newest + 1;
    if prompt == "busy" && !session.refused_busy {
        session.refused_busy = true;
        session.wake(turn, "Nothing new from the background.");
        session.answer(&format!("m-{turn_id}-done"), "Back to you.");
        return Err(busy());
    }
    if prompt == "subagent" {
        return subagent_turn(state, params, turn);
    }
    if prompt == "background" {
        session.turn = Some(turn);
        session.state(JobState::Running);
        session.publish(echo(&turn_id, params));
        session.start_background(format!("p-{turn_id}"));
        session.answer(&format!("m-{turn_id}"), "Started `sleep 30`.");
        return Ok(SessionPromptResponse { turn: turn_id });
    }
    if prompt == "flaky" {
        session.flaky(turn, echo(&turn_id, params));
        return Ok(SessionPromptResponse { turn: turn_id });
    }
    if prompt == "figures" {
        session.turn = Some(turn);
        session.state(JobState::Running);
        session.publish(echo(&turn_id, params));
        session.answer(&format!("m-{turn_id}"), FIGURES);
        return Ok(SessionPromptResponse { turn: turn_id });
    }
    if prompt == "report" {
        session.turn = Some(turn);
        session.state(JobState::Running);
        session.publish(echo(&turn_id, params));
        session.stream_answer(&format!("m-{turn_id}"), &report());
        return Ok(SessionPromptResponse { turn: turn_id });
    }
    if let Some(shown) = Shown::by(&prompt) {
        session.show_surface(turn, echo(&turn_id, params), shown);
        return Ok(SessionPromptResponse { turn: turn_id });
    }
    if prompt == "pictures" {
        session.turn = Some(turn);
        session.state(JobState::Running);
        session.publish(echo(&turn_id, params));
        session.read_image(&format!("img-{turn_id}"));
        session.answer(&format!("m-{turn_id}"), PICTURES);
        return Ok(SessionPromptResponse { turn: turn_id });
    }
    let (ask, timeout) = other_ask(&prompt).unwrap_or_else(|| (ls_approval(&session.cwd), None));
    session.ask(params, turn, ask, timeout);
    Ok(SessionPromptResponse { turn: turn_id })
}

fn busy() -> RpcError {
    RpcError::new(
        ErrorCode::SessionBusy,
        "session already has a turn in progress",
    )
}

fn not_steerable() -> RpcError {
    RpcError::new(ErrorCode::NotSteerable, "no model request is taking input")
}

/// The user message a prompt for turn `turn` becomes, with all its content
/// blocks, as the daemon echoes it.
fn echo(turn: &rho_protocol::TurnId, prompt: &SessionPromptParams) -> SessionEvent {
    SessionEvent::UserMessage {
        message: MessageId::new(format!("u-{turn}")),
        content: prompt.content.clone(),
        steer: None,
    }
}

/// Runs `turn` for the prompt "subagent": it starts a subagent in a
/// session of its own, then ends while the subagent runs on.
fn subagent_turn(
    state: &mut State,
    params: &SessionPromptParams,
    turn: Turn,
) -> Result<SessionPromptResponse, RpcError> {
    let turn_id = turn.id.clone();
    let child = format!("sub-{}", turn.number);
    let run = state.next_turn();
    let session = state.session(&params.session)?;
    let cwd = session.cwd.clone();
    session.turn = Some(turn);
    session.state(JobState::Running);
    session.publish(echo(&turn_id, params));
    session.start_subagent(format!("run-{turn_id}"), &child);
    session.answer(&format!("m-{turn_id}"), "Started a subagent.");
    state
        .sessions
        .insert(child, FakeSession::subagent(&cwd, run));
    Ok(SessionPromptResponse { turn: turn_id })
}

/// The reply to the prompt "figures": a diagram, one wider than the
/// transcript, a display formula, an inline one, and prices that stay prose.
const FIGURES: &str = r"Here is the connection:

```mermaid
sequenceDiagram
    participant UI as rho-ui
    participant D as daemon
    UI->>D: handshake
    D-->>UI: session events
    Note over UI: draws the transcript
```

A turn, end to end:

```mermaid
flowchart LR
    Prompt --> Daemon --> Model --> Tools --> Approval --> Model2[Model again] --> Reply --> Transcript
```

The roots of $ax^2 + bx + c$ are

$$
x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}
$$

It cost $5 and $10 to run.
";

/// The reply to the prompt "pictures": an image in the session's folder,
/// and one that isn't there.
const PICTURES: &str = "Here is the plot:\n\n![The plot, a square on blue](plot.png)\n\nAnd the chart:\n\n![A missing chart](missing.png)\n";

/// The turn's reply to `answer`.
fn reply(answer: &InputAnswer) -> String {
    let chose = |name: &str, text: &Option<String>| match text {
        Some(text) => format!("you chose {name}: {text}"),
        None => format!("you chose {name}"),
    };
    match answer {
        InputAnswer::AllowOnce => chose("allow_once", &None),
        InputAnswer::AllowForSession => chose("allow_for_session", &None),
        InputAnswer::Deny { reason } => chose("deny", reason),
        InputAnswer::ApprovePlan { feedback, .. } => chose("approve_plan", feedback),
        InputAnswer::KeepPlanning { feedback } => chose("keep_planning", feedback),
        InputAnswer::Answers { answers } => {
            let answers = answers
                .iter()
                .map(|(id, values)| format!("{id}={}", values.join(",")));
            format!("you answered {}", answers.collect::<Vec<_>>().join("; "))
        }
        InputAnswer::Decline => "you declined".to_owned(),
        _ => "you chose something else".to_owned(),
    }
}

/// What the prompt `prompt` asks, if it asks something other than to run
/// `ls`: [`super::questions`], with a timeout for "questions timeout", or a
/// plan approval.
fn other_ask(prompt: &str) -> Option<(Ask, Option<InputTimeout>)> {
    match prompt {
        "questions" | "questions timeout" => {
            let deadline = SystemTime::now() + Duration::from_secs(300);
            let deadline_ms = deadline
                .duration_since(UNIX_EPOCH)
                .expect("the clock is past 1970")
                .as_millis();
            let timeout = (prompt == "questions timeout").then(|| InputTimeout {
                deadline_ms: u64::try_from(deadline_ms).expect("a deadline before 2554"),
                answer: InputAnswer::Answers {
                    answers: [("lang".to_owned(), vec!["rust".to_owned()])].into(),
                },
                reason: "Rust is what the repo uses".to_owned(),
            });
            let ask = Ask::Question(QuestionAsk {
                title: "Set up the tool".to_owned(),
                questions: super::questions(),
            });
            Some((ask, timeout))
        }
        "plan" => Some((
            Ask::PlanApproval(PlanApprovalAsk {
                plan: "1. Read the parser\n2. Split it in two".to_owned(),
                modes: vec![PermissionMode::AllowEdits, PermissionMode::Auto],
            }),
            None,
        )),
        _ => None,
    }
}

/// The approval a prompt asks for: to run `ls` in `cwd`.
fn ls_approval(cwd: &Path) -> Ask {
    Ask::Approval(ApprovalAsk {
        source: ApprovalSource::BuiltInTool {
            name: "bash".to_owned(),
        },
        operation: Operation::RunProcess {
            program: PathBuf::from("ls"),
            args: Vec::new(),
            shell_command: Some("ls".to_owned()),
            cwd: cwd.to_owned(),
        },
        reason: "the fake asks before every command".to_owned(),
    })
}

impl FakeSession {
    /// Starts `turn` for the prompt `params`, which waits on `ask`. An
    /// approval asks for the turn's tool call, `ls`, which shows pending.
    fn ask(
        &mut self,
        params: &SessionPromptParams,
        turn: Turn,
        ask: Ask,
        timeout: Option<InputTimeout>,
    ) {
        let (turn_id, tool) = (turn.id.clone(), turn.tool.clone());
        let runs_ls = matches!(ask, Ask::Approval(_));
        let request = InputRequest {
            id: InputRequestId(turn.number),
            ask,
            tool_call: runs_ls.then(|| tool.clone()),
            timeout,
            asker: InputAsker::Turn,
        };
        self.turn = Some(Turn {
            input: Some(request.clone()),
            ..turn
        });
        self.state(JobState::Running);
        self.publish(echo(&turn_id, params));
        if runs_ls {
            self.tool(
                &tool,
                ("ls", ToolKind::Execute),
                ToolStatus::Pending,
                Vec::new(),
                shell_card("ls", ToolStatus::Pending, None),
            );
        }
        self.publish(SessionEvent::InputRequested {
            request: Box::new(request),
        });
        self.state(JobState::WaitingForInput);
    }
}

/// What a prompt that calls the `ui` tool shows.
#[derive(Clone, Copy)]
enum Shown {
    /// The form, which the call waits on.
    Form,
    /// The gallery, which the call shows without waiting.
    Gallery,
    /// The form, changed, which the call shows without waiting.
    Widened,
}

impl Shown {
    /// What `prompt` shows, if it calls the `ui` tool.
    fn by(prompt: &str) -> Option<Self> {
        match prompt {
            "ui" => Some(Self::Form),
            "gallery" => Some(Self::Gallery),
            "widen" => Some(Self::Widened),
            _ => None,
        }
    }
}

impl FakeSession {
    /// Runs `turn`, which calls the `ui` tool to show `shown`: the form's
    /// call stays in progress until an action comes; any other is done,
    /// and the turn answers at once.
    fn show_surface(&mut self, turn: Turn, echo: SessionEvent, shown: Shown) {
        let (call, id) = (turn.tool.clone(), turn.id.clone());
        self.turn = Some(turn);
        self.state(JobState::Running);
        self.publish(echo);
        match shown {
            Shown::Form => self.ui(
                &call,
                ToolStatus::InProgress,
                surface::FORM_SURFACE,
                surface::form(),
            ),
            Shown::Gallery => {
                self.ui(
                    &call,
                    ToolStatus::Completed,
                    surface::GALLERY_SURFACE,
                    surface::gallery(TOOL_IMAGE),
                );
                self.answer(&format!("m-{id}"), "Here is every component.");
            }
            Shown::Widened => {
                self.ui(
                    &call,
                    ToolStatus::Completed,
                    surface::FORM_SURFACE,
                    surface::widened(),
                );
                self.answer(&format!("m-{id}"), "The form takes more now.");
            }
        }
    }
}

/// The methods rho-ui sends, plus those every `Service` must answer (the
/// handshake hooks, status, stop, restart, and the job list). Every other
/// one answers rho-protocol's default, not served.
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

    fn hello(&self, _: &ClientInfo) {
        self.lock().hellos += 1;
    }

    fn skewed(&self, _: &ClientInfo, _: &BuildId) -> RestartCost {
        self.lock().skew.unwrap_or(RestartCost {
            in_progress: 0,
            clients: 0,
        })
    }

    async fn status(&self) -> DaemonStatus {
        let state = self.lock();
        DaemonStatus {
            pid: std::process::id(),
            version: "9.9.9".to_owned(),
            protocol: PROTOCOL_VERSION,
            started_at: 0,
            uptime_secs: 0,
            jobs: 0,
            in_progress: state.in_progress,
            socket: PathBuf::new(),
            credential_store: state.auth.store(),
            upgrade: state.upgrade.clone(),
        }
    }

    /// The next client to start the daemon starts the program installed,
    /// so it serves that one as rho-ui's own build.
    async fn stop(&self) {
        let mut state = self.lock();
        state.stops += 1;
        state.upgrade = None;
        state.skew = None;
        for session in state.sessions.values_mut() {
            for publisher in session.publishers.drain(..) {
                publisher.close(DetachReason::DaemonStopping);
            }
        }
        drop(state);
        self.closed.send_replace(());
    }

    async fn restart(&self) {
        self.restart_now();
    }

    async fn new_session(&self, params: SessionNewParams) -> Result<SessionNewResponse, RpcError> {
        let mut state = self.lock();
        if params.setup.sign_in == SignIn::Required && !state.auth.is_signed_in(KEY_AUTH) {
            return Err(RpcError::new(
                ErrorCode::SignInRequired,
                format!("sign in to {KEY_AUTH} first"),
            ));
        }
        state.new_sessions += 1;
        state.new_session_environment = Some(params.environment.clone());
        let id = format!("new-{}", state.new_sessions);
        // A worktree as the daemon names one, for a `cwd` at the top of
        // its repository; the fake makes nothing on disk.
        state.worktrees += usize::from(params.worktree);
        let worktree = params.worktree.then(|| {
            let name = params.cwd.file_name().unwrap_or_default().to_string_lossy();
            let name = format!("{name}-{:08}", state.new_sessions);
            SessionWorktree {
                branch: format!("rho/{name}"),
                path: Path::new("/rho/worktrees").join(&name),
                origin: params.cwd.clone(),
            }
        });
        let cwd = worktree
            .as_ref()
            .map_or(&params.cwd, |worktree| &worktree.path);
        let mut session = FakeSession::new(&cwd.to_string_lossy(), None, 0);
        session.saved = false;
        let options = session.settings.options();
        state.sessions.insert(id.clone(), session);
        Ok(SessionNewResponse {
            session: SessionId::new(id),
            options,
            worktree,
        })
    }

    async fn prompt_session(
        &self,
        params: SessionPromptParams,
    ) -> Result<SessionPromptResponse, RpcError> {
        if std::mem::take(&mut self.lock().restart_at_prompt) {
            self.restart_now();
            return Err(RpcError::new(
                ErrorCode::DaemonStopping,
                "the daemon is restarting",
            ));
        }
        let slow =
            matches!(params.content.first(), Some(ContentBlock::Text { text }) if text == "slow");
        if slow {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // One lock from the key's check to its record, so two tries of one
        // prompt can't both start a turn.
        let mut state = self.lock();
        let session = state.session(&params.session)?;
        let again = session
            .last_prompt
            .as_ref()
            .filter(|(key, _)| params.idempotency_key.as_ref() == Some(key));
        if let Some((_, turn)) = again {
            return Ok(SessionPromptResponse { turn: turn.clone() });
        }
        let started = start_prompt(&mut state, &params)?;
        let session = state.session(&params.session)?;
        session.prompts += 1;
        session.last_prompt = params
            .idempotency_key
            .map(|key| (key, started.turn.clone()));
        Ok(started)
    }

    async fn cancel_session(
        &self,
        params: SessionCancelParams,
    ) -> Result<SessionCancelResponse, RpcError> {
        let mut state = self.lock();
        let session = state.session(&params.session)?;
        if let Some(turn) = &mut session.turn {
            let tool = turn.tool.clone();
            let request_was_ls = turn
                .input
                .as_ref()
                .is_some_and(|request| request.tool_call.is_some());
            if let Some(request) = turn.input.take() {
                session.publish(SessionEvent::InputResolved {
                    id: request.id,
                    resolution: Resolution::Withdrawn,
                });
            }
            if request_was_ls {
                session.tool(
                    &tool,
                    ("ls", ToolKind::Execute),
                    ToolStatus::Failed,
                    Vec::new(),
                    shell_card("ls", ToolStatus::Failed, None),
                );
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
        if params.text == "keep going" {
            return Ok(SessionSteerResponse { id });
        }
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
        if let InputAnswer::Answers { answers } = &params.answer {
            if answers.values().flatten().any(|value| value == "refuse") {
                return Err(RpcError::new(
                    ErrorCode::InvalidParams,
                    "the fake daemon refuses this answer",
                ));
            }
        }
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
        // Only the approval asks for a tool call, `ls`.
        if request.tool_call.is_some() {
            if matches!(params.answer, InputAnswer::Deny { .. }) {
                session.tool(
                    &tool,
                    ("ls", ToolKind::Execute),
                    ToolStatus::Failed,
                    Vec::new(),
                    shell_card("ls", ToolStatus::Failed, None),
                );
            } else {
                let output = "a.rs\nb.rs";
                session.tool(
                    &tool,
                    ("ls", ToolKind::Execute),
                    ToolStatus::Completed,
                    vec![shell_output(output)],
                    shell_card("ls", ToolStatus::Completed, Some(output)),
                );
            }
        }
        session.answer(&format!("m-{turn_id}"), &reply(&params.answer));
        Ok(SessionAnswerResponse {})
    }

    async fn surface_action(
        &self,
        params: SessionSurfaceActionParams,
    ) -> Result<SessionSurfaceActionResponse, RpcError> {
        let mut state = self.lock();
        let session = state.session(&params.session)?;
        let name = params.action["action"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let shown = params.surface.clone();
        session.surface_actions.push(params);
        if name == surface::REFUSED_EVENT {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                format!("the session has no surface {shown}"),
            ));
        }
        // The prompt "ui" left its call waiting on the form.
        let waiting = session
            .turn
            .as_ref()
            .filter(|_| shown == surface::FORM_SURFACE)
            .map(|turn| (turn.tool.clone(), turn.id.clone()));
        if let Some((call, turn)) = waiting {
            session.ui(
                &call,
                ToolStatus::Completed,
                surface::FORM_SURFACE,
                surface::form(),
            );
            session.answer(&format!("m-{turn}"), &format!("you sent {name}"));
        }
        Ok(SessionSurfaceActionResponse {})
    }

    async fn hold_session(
        &self,
        params: SessionHoldParams,
    ) -> Result<SessionHoldResponse, RpcError> {
        let mut state = self.lock();
        state.holds += 1;
        let session = state.session(&params.session)?;
        let open = session
            .turn
            .as_mut()
            .and_then(|turn| turn.input.as_mut())
            .filter(|request| request.id == params.request);
        let Some(request) = open else {
            return Err(RpcError::new(
                ErrorCode::InputNotOpen,
                "the input request is not open",
            ));
        };
        if request.timeout.take().is_some() {
            session.publish(SessionEvent::InputHeld { id: params.request });
        }
        Ok(SessionHoldResponse {})
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
            SessionChange::AddDirectory { path } => session.settings.add_directory(path)?,
            _ => {
                return Err(RpcError::new(
                    ErrorCode::MethodNotFound,
                    "the fake daemon does not serve that session change",
                ))
            }
        }
        let options = session.settings.options();
        session.publish(SessionEvent::OptionsChanged {
            options: Box::new(options.clone()),
        });
        Ok(SessionConfigureResponse { options })
    }

    async fn attach_session(
        &self,
        params: SessionAttachParams,
    ) -> Result<(SessionAttachResponse, Subscription), RpcError> {
        let mut state = self.lock();
        let session = state.session(&params.session)?;
        if session.subagent {
            return Err(RpcError::new(
                ErrorCode::SessionOpenFailed,
                "a subagent's session is continued only by its parent",
            ));
        }
        session.attaches += 1;
        let (response, publisher, subscription) =
            session.follow(self.stream(), params.after.as_ref());
        session.publishers.push(publisher);
        Ok((response, subscription))
    }

    /// Follows a subagent's session while its run is under way; any other
    /// session, and one whose run ended, counts as unloaded: it replays,
    /// then the observe ends.
    async fn observe_session(
        &self,
        params: SessionObserveParams,
    ) -> Result<(SessionAttachResponse, Subscription), RpcError> {
        let mut state = self.lock();
        let session = state.session(&params.session)?;
        let loaded = session.subagent && session.turn.is_some();
        // A session observed unloaded always replays whole.
        let after = params.after.as_ref().filter(|_| loaded);
        let (response, publisher, subscription) = session.follow(self.stream(), after);
        if loaded {
            session.publishers.push(publisher);
        } else {
            publisher.close(DetachReason::NotLoaded);
        }
        Ok((response, subscription))
    }

    async fn session_background_stop(
        &self,
        params: SessionBackgroundStopParams,
    ) -> Result<SessionBackgroundStopResponse, RpcError> {
        let mut state = self.lock();
        let turn = state.next_turn();
        let session = state.session(&params.session)?;
        let Some(work) = session
            .background
            .iter_mut()
            .find(|work| work.item == params.work && work.status.is_running())
        else {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "could not stop background work: it is not running",
            ));
        };
        work.status = BackgroundStatus::Cancelled;
        let told = format!("`{}` was stopped.", work.title);
        session.report_background();
        if session.turn.is_none() {
            session.wake(turn, &told);
        }
        Ok(SessionBackgroundStopResponse {})
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

    /// Refuses a blank title or one with a control character, as rho does.
    async fn rename_session(
        &self,
        params: SessionRenameParams,
    ) -> Result<SessionRenameResponse, RpcError> {
        let title = params.title.trim();
        if title.is_empty() || title.chars().any(char::is_control) {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "a title must be one line of text",
            ));
        }
        let mut state = self.lock();
        let session = state.session(&params.session)?;
        session.title = Some(title.to_owned());
        session.publish(SessionEvent::TitleChanged {
            title: title.to_owned(),
        });
        Ok(SessionRenameResponse {})
    }

    async fn delete_session(
        &self,
        params: SessionDeleteParams,
    ) -> Result<SessionDeleteResponse, RpcError> {
        let mut state = self.lock();
        let session = state.session(&params.session)?;
        let followed = session.publishers.iter().any(|client| !client.is_gone());
        if params.unless_attached && followed {
            return Err(RpcError::new(
                ErrorCode::SessionInUse,
                "a client is attached to the session",
            ));
        }
        if let Some(session) = state.sessions.remove(params.session.as_str()) {
            for publisher in session.publishers {
                publisher.close(DetachReason::Deleted);
            }
        }
        Ok(SessionDeleteResponse {})
    }

    async fn list_auths(&self, _: AuthListParams) -> Result<AuthListResponse, RpcError> {
        Ok(AuthListResponse {
            auths: self.lock().auth.list(),
        })
    }

    async fn save_auth_key(&self, params: AuthKeyParams) -> Result<AuthKeyResponse, RpcError> {
        self.lock().auth.save_key(&params)
    }

    async fn logout_auth(&self, params: AuthLogoutParams) -> Result<AuthLogoutResponse, RpcError> {
        Ok(self.lock().auth.logout(&params))
    }

    async fn login(
        &self,
        params: AuthLoginParams,
    ) -> Result<(AuthLoginResponse, LoginStream), RpcError> {
        self.lock().auth.login(&params)
    }

    async fn cancel_login(
        &self,
        params: AuthLoginCancelParams,
    ) -> Result<AuthLoginCancelResponse, RpcError> {
        self.lock().auth.cancel_login(&params)
    }

    async fn choose_store(
        &self,
        params: AuthChooseStoreParams,
    ) -> Result<AuthChooseStoreResponse, RpcError> {
        Ok(AuthChooseStoreResponse {
            credential_store: self.lock().auth.choose_store(params.store),
        })
    }

    async fn prune_worktrees(
        &self,
        params: WorktreePruneParams,
    ) -> Result<WorktreePruneResponse, RpcError> {
        let mut state = self.lock();
        let worktrees = state.session_worktrees.clone();
        if !params.dry_run {
            state
                .session_worktrees
                .retain(|report| !matches!(report.verdict, WorktreeVerdict::Merged { .. }));
        }
        Ok(WorktreePruneResponse { worktrees })
    }

    async fn list_jobs(&self) -> JobListResponse {
        let state = self.lock();
        let jobs = state.sessions.iter().map(|(id, session)| JobSummary {
            id: JobId::new(format!("job-{id}")),
            kind: if session.subagent {
                JobKind::Subagent
            } else {
                JobKind::Session
            },
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
            worktree: None,
            worktree_stale: false,
            live: Some(JobLive {
                title: session.title.clone(),
                model: session.settings.model.clone(),
                turn_elapsed_ms: session.turn.as_ref().map(Turn::elapsed_ms),
                usage: None,
                inputs: session
                    .turn
                    .iter()
                    .filter_map(|turn| turn.input.clone())
                    .collect(),
            }),
        });
        JobListResponse {
            jobs: jobs.chain(state.extra_jobs.iter().cloned()).collect(),
        }
    }
}

/// No settings: every `rho/config/*` method answers rho-protocol's default,
/// not served.
/// rho's own settings; see `config.rs`.
#[expect(
    clippy::unused_async_trait_impl,
    reason = "the trait's methods return futures; a scripted answer has nothing to await"
)]
impl SettingsService for Fake {
    async fn config_schema(&self) -> Result<ConfigSchema, RpcError> {
        Ok(self.lock().config.schema())
    }

    async fn get_config(&self, params: ConfigGetParams) -> Result<ConfigWatch, RpcError> {
        self.lock().config.watch(&params.namespace)
    }

    async fn set_config(&self, params: ConfigSetParams) -> Result<ConfigSetResponse, RpcError> {
        self.lock().config.set(params)
    }
}
