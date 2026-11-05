//! A running ACP agent process (normally `rho acp`) and the requests the UI
//! sends it. One process serves many sessions.
//!
//! ```ignore
//! let (agent, events) = Agent::start(AgentCommand::rho(), |driver| spawn(driver)).await?;
//! let setup = agent.new_session(&cwd).await?;
//! agent.prompt(setup.id(), "hello").await?;
//! while let Some(event) = events.recv().await { /* route by session id */ }
//! ```

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use agent_client_protocol::{
    self as acp,
    schema::{v2, ProtocolVersion},
    AcpAgent, AcpAgentConfig, Client, Responder, V2ConnectionTo,
};
use futures::{channel::oneshot, future::BoxFuture, FutureExt};

use crate::thread::{config_options, ConfigOption, ConfigValue, ThreadUpdate};

/// How to launch the agent process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCommand {
    program: PathBuf,
    args: Vec<String>,
}

impl AgentCommand {
    /// `rho acp`, found on `PATH`.
    pub fn rho() -> Self {
        Self::new("rho", ["acp"])
    }

    /// Any program that serves ACP v2 on stdio.
    pub fn new(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}

/// Why a request to the agent failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    /// The agent answered with an error, or the connection failed. Holds the
    /// agent's message, which is meant for people.
    #[error("{0}")]
    Protocol(String),
    /// The agent process ended before it finished starting.
    #[error("agent exited during startup")]
    Exited,
}

impl From<acp::Error> for AgentError {
    fn from(error: acp::Error) -> Self {
        Self::Protocol(describe_error(&error))
    }
}

/// Error text for people: the message plus any string detail the agent sent.
fn describe_error(error: &acp::Error) -> String {
    let detail = match error.data.as_ref() {
        Some(serde_json::Value::String(detail)) => Some(detail),
        Some(serde_json::Value::Object(map)) => match map.get("error") {
            Some(serde_json::Value::String(detail)) => Some(detail),
            _ => None,
        },
        _ => None,
    };
    match detail {
        Some(detail) => format!("{}: {detail}", error.message),
        None => error.message.clone(),
    }
}

/// A session ID. Only the agent issues them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(Arc<str>);

impl SessionId {
    /// The ID as the agent sent it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn to_acp(&self) -> v2::SessionId {
        v2::SessionId::new(Arc::clone(&self.0))
    }
}

impl From<v2::SessionId> for SessionId {
    fn from(id: v2::SessionId) -> Self {
        Self(id.0)
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The agent's own name and version, from `initialize`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentInfo {
    name: String,
    version: String,
}

impl AgentInfo {
    /// Machine name, such as `rho`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Version string, such as `2.32.3`.
    pub fn version(&self) -> &str {
        &self.version
    }
}

/// A saved session from `session/list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    id: SessionId,
    cwd: PathBuf,
    title: Option<String>,
    updated_at: Option<String>,
}

impl SessionSummary {
    /// Pass to [`Agent::resume_session`] with [`Self::cwd`].
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// The workspace the session runs in.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The agent's generated title, once it has one.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Last activity as an RFC 3339 timestamp, as the agent sent it.
    pub fn updated_at(&self) -> Option<&str> {
        self.updated_at.as_deref()
    }
}

/// What `session/new` and `session/resume` return. Hand it to
/// [`crate::Thread::set_setup`].
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSetup {
    pub(crate) id: SessionId,
    pub(crate) config: Vec<ConfigOption>,
    pub(crate) commands: Vec<CommandInfo>,
}

impl SessionSetup {
    /// The session this setup belongs to.
    pub fn id(&self) -> &SessionId {
        &self.id
    }
}

/// A slash command or skill the agent offers in this session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInfo {
    name: String,
    description: String,
}

impl CommandInfo {
    /// What the user types after `/`, such as `compact` or `$skill-name`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// One line on what it does.
    pub fn description(&self) -> &str {
        &self.description
    }
}

pub(crate) fn commands(list: Vec<v2::AvailableCommand>) -> Vec<CommandInfo> {
    list.into_iter()
        .map(|command| CommandInfo {
            name: command.name,
            description: command.description,
        })
        .collect()
}

/// Everything the agent pushes without being asked.
#[derive(Debug)]
pub enum AgentEvent {
    /// A change to one session's thread; apply it with [`crate::Thread::apply`].
    Update {
        /// The session the change belongs to.
        session: SessionId,
        /// The change itself.
        update: ThreadUpdate,
    },
    /// The agent wants approval before running a tool.
    Permission(PermissionRequest),
    /// The connection ended.
    Exited {
        /// Why, unless it closed normally.
        error: Option<String>,
    },
}

/// The agent's event stream. It ends after [`AgentEvent::Exited`].
#[derive(Clone, Debug)]
pub struct AgentEvents(async_channel::Receiver<AgentEvent>);

impl AgentEvents {
    /// Waits for the next event; `None` once the stream has ended.
    pub async fn recv(&self) -> Option<AgentEvent> {
        self.0.recv().await.ok()
    }
}

/// A connected agent. Cheap to clone; the process exits when the last clone
/// is dropped.
#[derive(Clone, Debug)]
pub struct Agent {
    connection: V2ConnectionTo<acp::Agent>,
    info: Arc<AgentInfo>,
    _shutdown: Arc<oneshot::Sender<()>>,
}

// UI code hands these to background tasks; keep them thread-safe.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Agent>();
    assert_send_sync::<AgentEvents>();
};

type Startup = Result<(V2ConnectionTo<acp::Agent>, v2::InitializeResponse), AgentError>;

impl Agent {
    /// Launches the agent and runs `initialize`.
    ///
    /// `spawn` must poll the given future to completion on some executor. The
    /// future owns the process and its stdio, and ends when the agent exits or
    /// the last `Agent` clone is dropped.
    ///
    /// # Errors
    ///
    /// Fails if the program cannot start, exits early, or rejects
    /// `initialize`.
    pub async fn start(
        command: AgentCommand,
        spawn: impl FnOnce(BoxFuture<'static, ()>),
    ) -> Result<(Agent, AgentEvents), AgentError> {
        let (event_tx, event_rx) = async_channel::unbounded();
        // Room for one outcome: whichever of `initialize` or the connection
        // ending reports first wins; a later report finds it full.
        let (startup_tx, startup_rx) = async_channel::bounded::<Startup>(1);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let process = AcpAgent::new(AcpAgentConfig::new(command.program).args(command.args));
        let updates = event_tx.clone();
        let permissions = event_tx.clone();
        let startup = startup_tx.clone();

        let driver = async move {
            let result = Client
                .v2()
                .name("rho-ui")
                .on_receive_notification(
                    async move |notification: v2::UpdateSessionNotification,
                                _connection: V2ConnectionTo<acp::Agent>| {
                        // Unbounded, so this never waits.
                        let _ = updates.try_send(AgentEvent::Update {
                            session: notification.session_id.into(),
                            update: ThreadUpdate(notification.update),
                        });
                        Ok(())
                    },
                    acp::on_receive_notification!(),
                )
                .on_receive_request(
                    async move |request: v2::RequestPermissionRequest,
                                responder: Responder<v2::RequestPermissionResponse>,
                                _connection: V2ConnectionTo<acp::Agent>| {
                        // If nobody receives it, dropping it answers "cancelled".
                        let _ = permissions.try_send(AgentEvent::Permission(
                            PermissionRequest::new(request, responder),
                        ));
                        Ok(())
                    },
                    acp::on_receive_request!(),
                )
                .connect_with(
                    process,
                    async move |connection: V2ConnectionTo<acp::Agent>| {
                        let initialized = connection
                            .send_request(v2::InitializeRequest::new(
                                ProtocolVersion::V2,
                                v2::Implementation::new("rho-ui", env!("CARGO_PKG_VERSION")),
                            ))
                            .block_task()
                            .await;
                        match initialized {
                            Ok(response) => {
                                let _ = startup.try_send(Ok((connection.clone(), response)));
                            }
                            Err(error) => {
                                let _ = startup.try_send(Err(error.clone().into()));
                                return Err(error);
                            }
                        }
                        let _ = shutdown_rx.await;
                        Ok(())
                    },
                )
                .await;
            let error = result.err().map(|error| describe_error(&error));
            let _ = startup_tx.try_send(Err(error
                .clone()
                .map_or(AgentError::Exited, AgentError::Protocol)));
            let _ = event_tx.try_send(AgentEvent::Exited { error });
        };
        spawn(driver.boxed());

        let (connection, initialized) =
            startup_rx.recv().await.map_err(|_| AgentError::Exited)??;
        let info = AgentInfo {
            name: initialized.info.name,
            version: initialized.info.version,
        };
        let agent = Agent {
            connection,
            info: Arc::new(info),
            _shutdown: Arc::new(shutdown_tx),
        };
        Ok((agent, AgentEvents(event_rx)))
    }

    /// Who answered `initialize`.
    pub fn info(&self) -> &AgentInfo {
        &self.info
    }

    /// Saved sessions in every workspace, newest first.
    ///
    /// # Errors
    ///
    /// Fails if the agent rejects the request or the connection is gone.
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, AgentError> {
        let mut sessions = Vec::new();
        let mut cursor = None;
        loop {
            let mut request = v2::ListSessionsRequest::new();
            request.cursor = cursor;
            let page = self.connection.send_request(request).block_task().await?;
            sessions.extend(page.sessions.into_iter().map(|session| SessionSummary {
                id: session.session_id.into(),
                cwd: session.cwd.0,
                title: session.title,
                updated_at: session.updated_at,
            }));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(sessions),
            }
        }
    }

    /// Starts a session in `cwd`.
    ///
    /// # Errors
    ///
    /// Fails if the agent rejects the request or the connection is gone. rho
    /// also fails here when the selected provider needs sign-in.
    pub async fn new_session(&self, cwd: &Path) -> Result<SessionSetup, AgentError> {
        let response = self
            .connection
            .send_request(v2::NewSessionRequest::new(cwd))
            .block_task()
            .await?;
        Ok(SessionSetup {
            id: response.session_id.into(),
            config: config_options(response.config_options),
            commands: commands(response.available_commands),
        })
    }

    /// Reopens a saved session. Its history replays as [`AgentEvent::Update`]s
    /// before this returns, so create the session's thread first.
    ///
    /// # Errors
    ///
    /// Fails if the session does not exist, the agent rejects the request, or
    /// the connection is gone.
    pub async fn resume_session(
        &self,
        id: &SessionId,
        cwd: &Path,
    ) -> Result<SessionSetup, AgentError> {
        let mut request = v2::ResumeSessionRequest::new(id.to_acp(), cwd);
        request.replay_from = Some(v2::ReplayFrom::Start(v2::ReplayFromStart::new()));
        let response = self.connection.send_request(request).block_task().await?;
        Ok(SessionSetup {
            id: id.clone(),
            config: config_options(response.config_options),
            commands: commands(response.available_commands),
        })
    }

    /// Sends a prompt. Returns once the agent accepts it; the turn itself
    /// streams as events.
    ///
    /// # Errors
    ///
    /// Fails if the agent refuses the prompt, for example because the session
    /// is busy or unknown. Nothing was added to the thread in that case.
    pub async fn prompt(&self, id: &SessionId, text: &str) -> Result<(), AgentError> {
        let request = v2::PromptRequest::new(id.to_acp(), vec![text.into()]);
        self.connection.send_request(request).block_task().await?;
        Ok(())
    }

    /// Stops the running turn, if any.
    ///
    /// # Errors
    ///
    /// Fails only if the connection is gone.
    pub fn cancel(&self, id: &SessionId) -> Result<(), AgentError> {
        self.connection
            .send_notification(v2::CancelSessionNotification::new(id.to_acp()))?;
        Ok(())
    }

    /// Changes a session setting. Returns every option, since one change can
    /// move others (a new model has different reasoning levels).
    ///
    /// # Errors
    ///
    /// Fails if the agent rejects the value or the connection is gone.
    pub async fn set_config(
        &self,
        id: &SessionId,
        config_id: &str,
        value: ConfigValue,
    ) -> Result<Vec<ConfigOption>, AgentError> {
        let value = match value {
            ConfigValue::Select(choice) => v2::SessionConfigOptionValue::id(choice),
            ConfigValue::Toggle(on) => v2::SessionConfigOptionValue::boolean(on),
        };
        let request =
            v2::SetSessionConfigOptionRequest::new(id.to_acp(), config_id.to_owned(), value);
        let response = self.connection.send_request(request).block_task().await?;
        Ok(config_options(response.config_options))
    }

    /// Frees a live session in the agent. Its history stays saved.
    ///
    /// # Errors
    ///
    /// Fails if the agent rejects the request or the connection is gone.
    pub async fn close_session(&self, id: &SessionId) -> Result<(), AgentError> {
        self.connection
            .send_request(v2::CloseSessionRequest::new(id.to_acp()))
            .block_task()
            .await?;
        Ok(())
    }

    /// Deletes a saved session for good.
    ///
    /// # Errors
    ///
    /// Fails if the agent rejects the request or the connection is gone.
    pub async fn delete_session(&self, id: &SessionId) -> Result<(), AgentError> {
        self.connection
            .send_request(v2::DeleteSessionRequest::new(id.to_acp()))
            .block_task()
            .await?;
        Ok(())
    }
}

/// How an approval choice behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionKind {
    /// Run this one call.
    AllowOnce,
    /// Run this call and similar ones without asking again.
    AllowAlways,
    /// Skip this one call.
    RejectOnce,
    /// Skip this call and refuse similar ones.
    RejectAlways,
    /// A kind this UI does not know.
    Other,
}

/// One answer the agent offers for a [`PermissionRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionChoice {
    id: String,
    label: String,
    kind: PermissionKind,
}

impl PermissionChoice {
    /// Pass to [`PermissionRequest::choose`].
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Button text, such as "Allow once".
    pub fn label(&self) -> &str {
        &self.label
    }

    /// What choosing it does.
    pub fn kind(&self) -> PermissionKind {
        self.kind
    }
}

/// A pending approval. Answer it with [`Self::choose`]; dropping it answers
/// "cancelled" so the agent never waits forever.
pub struct PermissionRequest {
    session: SessionId,
    title: String,
    description: Option<String>,
    choices: Vec<PermissionChoice>,
    responder: Option<Responder<v2::RequestPermissionResponse>>,
}

impl std::fmt::Debug for PermissionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionRequest")
            .field("session", &self.session)
            .field("title", &self.title)
            .field("choices", &self.choices)
            .finish_non_exhaustive()
    }
}

impl PermissionRequest {
    fn new(
        request: v2::RequestPermissionRequest,
        responder: Responder<v2::RequestPermissionResponse>,
    ) -> Self {
        let choices = request
            .options
            .into_iter()
            .map(|option| PermissionChoice {
                id: option.option_id.0.to_string(),
                label: option.name,
                kind: match option.kind {
                    v2::PermissionOptionKind::AllowOnce => PermissionKind::AllowOnce,
                    v2::PermissionOptionKind::AllowAlways => PermissionKind::AllowAlways,
                    v2::PermissionOptionKind::RejectOnce => PermissionKind::RejectOnce,
                    v2::PermissionOptionKind::RejectAlways => PermissionKind::RejectAlways,
                    _ => PermissionKind::Other,
                },
            })
            .collect();
        Self {
            session: request.session_id.into(),
            title: request.title,
            description: request.description,
            choices,
            responder: Some(responder),
        }
    }

    /// The session whose turn is waiting.
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    /// The question, such as "Run `cargo test`?".
    pub fn title(&self) -> &str {
        &self.title
    }

    /// More detail, when the agent sends any.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The answers on offer, in the agent's order.
    pub fn choices(&self) -> &[PermissionChoice] {
        &self.choices
    }

    /// Answers with the choice whose [`PermissionChoice::id`] is `choice`.
    pub fn choose(mut self, choice: &str) {
        let outcome = v2::RequestPermissionOutcome::Selected(v2::SelectedPermissionOutcome::new(
            v2::PermissionOptionId::new(choice.to_owned()),
        ));
        self.respond(outcome);
    }

    fn respond(&mut self, outcome: v2::RequestPermissionOutcome) {
        if let Some(responder) = self.responder.take() {
            let _ = responder.respond(v2::RequestPermissionResponse::new(outcome));
        }
    }
}

impl Drop for PermissionRequest {
    fn drop(&mut self) {
        self.respond(v2::RequestPermissionOutcome::Cancelled);
    }
}
