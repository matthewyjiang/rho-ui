//! The fake daemon's answer to each protocol method.

use std::{
    path::PathBuf,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use rho_protocol::{
    methods::{
        AuthChooseStoreParams, AuthChooseStoreResponse, AuthKeyParams, AuthKeyResponse,
        AuthListParams, AuthListResponse, AuthLoginCancelParams, AuthLoginCancelResponse,
        AuthLoginParams, AuthLoginResponse, AuthLogoutParams, AuthLogoutResponse, ConfigGetParams,
        ConfigRegisterParams, ConfigRegisterResponse, ConfigSchema, ConfigSetParams,
        ConfigSetResponse, JobId, JobKind, JobListResponse, JobLive, JobState, JobSummary,
        MessageId, Replay, Resolution, Seq, SessionConfigureParams, SessionConfigureResponse,
        SessionDeleteParams, SessionDeleteResponse, SessionId, SessionInspectParams,
        SessionInspectResponse, SessionListParams, SessionListResponse, SessionMcpCatalog,
        SessionMcpCatalogParams, SessionMcpCompleteParams, SessionMcpCompleteResponse,
        SessionMcpInventory, SessionMcpParams, SessionMcpResourceParams,
        SessionMcpResourceResponse, SessionObserveParams, SessionProcessesParams,
        SessionProcessesResponse, SessionRenameParams, SessionRenameResponse, SessionSummary,
        SessionUnsteerParams, SessionUnsteerResponse, SteerId, StreamId,
    },
    subscription, ApprovalAsk, ApprovalSource, Ask, BuildId, ClientInfo, ConfigWatch, ContentBlock,
    CredentialStoreKind, CredentialStoreStatus, DaemonStatus, DetachReason, ErrorCode, InputAnswer,
    InputAsker, InputRequest, InputRequestId, LoginStream, Operation, ProtocolVersion, RestartCost,
    RpcError, Service, SessionAnswerParams, SessionAnswerResponse, SessionAttachParams,
    SessionAttachResponse, SessionCancelParams, SessionCancelResponse, SessionChange, SessionEvent,
    SessionHoldParams, SessionHoldResponse, SessionNewParams, SessionNewResponse,
    SessionPromptParams, SessionPromptResponse, SessionSteerParams, SessionSteerResponse,
    SettingsService, StoreSelectedBy, Subscription, ToolKind, ToolStatus, TurnOutcome,
    UnsteerOutcome, PROTOCOL_VERSION,
};

use super::{text, user_message, FakeSession, State, Turn, BACKLOG};

pub(super) struct Fake {
    pub(super) state: Mutex<State>,
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
            turn_elapsed_ms: session.turn.as_ref().map(Turn::elapsed_ms),
            inputs: session
                .turn
                .iter()
                .filter_map(|turn| turn.input.clone())
                .collect(),
            title: session.title.clone(),
            options: session.settings.options(),
        };
        let (publisher, subscription) = subscription(BACKLOG, replay);
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
