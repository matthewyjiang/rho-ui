//! The fake daemon's session reports (`rho/session/report`): made up, but
//! shaped as rho's are, and grown with the session's conversation, so a
//! report read after a turn differs from one read before it.
//!
//! As rho's daemon, it reads a report between turns only, refusing one
//! while a turn runs (`SessionBusy`), and only of a session it has loaded:
//! here, one some client follows (`SessionNotLoaded` otherwise).
//! [`FakeDaemon::leave_out_native_context`] makes a session's model unable
//! to take some of its conversation, as after a switch of provider.
//! [`FakeDaemon::hold_reports`] holds its answers, as a busy daemon's.

use std::{collections::BTreeSet, path::PathBuf};

use rho_protocol::methods::{
    CompactionCheck, CompactionContextSize, CompactionExtent, CompactionRun, CompactionRunOutcome,
    CompactionRunTrigger, CompactionSize, CompactionSkipReason, CompactionThreshold,
    CompactionTier, CompletedCompactions, ContextBasis, ContextMessageKind, ContextMessagePart,
    ContextPromptPart, ContextToolResultPart, ContextToolSchemaPart, HookActivityRecord,
    HookContract, HookEvent, HookOutcome, HookStatus, HookTools, PromptSourceKind,
    ProviderContextBlockId, SessionCompactionReport, SessionContextReport, SessionHooksReport,
    SessionProviderContextReport, SessionReport, SessionReportData, SessionReportParams,
    SessionReportResponse, SummaryRequestPath, ToolSchemaOrigin, SESSION_REPORT,
};
use rho_protocol::{ErrorCode, ModelRef, RpcError};
use tokio::sync::watch;

use super::{FakeDaemon, FakeSession, State, UiSessionId};

/// Whether the fake daemon serves `rho/session/report`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReportMethod {
    /// It serves the four reports.
    #[default]
    Served,
    /// As a daemon from before typed reports: it answers `MethodNotFound`.
    Unserved,
}

/// The window of every fake session's model, in tokens.
const WINDOW: u64 = 200_000;

/// The tokens each event of a session's conversation adds to its context.
const EVENT_TOKENS: u64 = 1_200;

/// How the fake answers `rho/session/report`.
#[derive(Debug)]
pub(super) struct FakeReports {
    method: ReportMethod,
    /// Sessions whose model can't take some of their conversation.
    native_left_out: BTreeSet<String>,
    /// Whether answers wait; see [`FakeDaemon::hold_reports`]. Only an
    /// answer waiting subscribes, so its receivers count those waiting.
    pub(super) held: watch::Sender<bool>,
}

impl FakeReports {
    pub(super) fn new(method: ReportMethod) -> Self {
        Self {
            method,
            native_left_out: BTreeSet::new(),
            held: watch::Sender::new(false),
        }
    }
}

impl State {
    /// Answers `rho/session/report`.
    pub(super) fn report(
        &mut self,
        params: &SessionReportParams,
    ) -> Result<SessionReportResponse, RpcError> {
        if self.reports.method == ReportMethod::Unserved {
            return Err(RpcError::new(
                ErrorCode::MethodNotFound,
                format!("unknown method {SESSION_REPORT}"),
            ));
        }
        let left_out = self
            .reports
            .native_left_out
            .contains(params.session.as_str());
        let session = self.session(&params.session)?;
        if !session.publishers.iter().any(|client| !client.is_gone()) {
            return Err(RpcError::new(
                ErrorCode::SessionNotLoaded,
                "the daemon has not loaded the session; attach to it first",
            ));
        }
        if session.turn.is_some() {
            return Err(RpcError::new(
                ErrorCode::SessionBusy,
                "the session is in a turn; ask again once it ends",
            ));
        }
        let data = match params.report {
            SessionReport::Context => SessionReportData::Context(context(session)),
            SessionReport::Hooks => SessionReportData::Hooks(hooks(session)),
            SessionReport::Compaction => {
                SessionReportData::Compaction(Box::new(compaction(session)))
            }
            SessionReport::ProviderContext => {
                let writer = left_out.then(|| model("openai", "gpt-5"));
                SessionReportData::ProviderContext(provider_context(writer))
            }
            _ => {
                return Err(RpcError::new(
                    ErrorCode::InvalidParams,
                    "this daemon does not know the report",
                ))
            }
        };
        Ok(SessionReportResponse::new(data))
    }
}

impl FakeDaemon {
    /// Makes `session`'s model unable to take two blocks of its
    /// conversation, which another provider's model wrote.
    pub fn leave_out_native_context(&self, session: &UiSessionId) {
        let mut state = self.service.lock();
        state
            .reports
            .native_left_out
            .insert(session.as_str().to_owned());
    }

    /// Makes `rho/session/report` read its report as it is asked, but
    /// answer only once [`Self::release_reports`], as a busy daemon's
    /// answer comes late: it says what was so when it was asked.
    pub fn hold_reports(&self) {
        self.service.lock().reports.held.send_replace(true);
    }

    /// Sends the answers [`Self::hold_reports`] held, and answers those
    /// after at once.
    pub fn release_reports(&self) {
        self.service.lock().reports.held.send_replace(false);
    }

    /// How many answers [`Self::hold_reports`] holds.
    pub fn held_reports(&self) -> usize {
        self.service.lock().reports.held.receiver_count()
    }
}

/// The tokens `session`'s conversation takes.
fn conversation_tokens(session: &FakeSession) -> u64 {
    let events = u64::try_from(session.history.len()).unwrap_or(u64::MAX);
    events.saturating_mul(EVENT_TOKENS)
}

fn context(session: &FakeSession) -> SessionContextReport {
    let conversation = conversation_tokens(session);
    let system_prompt = vec![
        ContextPromptPart {
            source: PromptSourceKind::Base,
            path: None,
            tokens: 6_200,
        },
        ContextPromptPart {
            source: PromptSourceKind::InstructionFile,
            path: Some(session.cwd.join("AGENTS.md")),
            tokens: 1_400,
        },
        ContextPromptPart {
            source: PromptSourceKind::Skills,
            path: None,
            tokens: 900,
        },
    ];
    let tool_schemas = vec![
        ContextToolSchemaPart {
            origin: ToolSchemaOrigin::BuiltIn,
            schemas: 14,
            tokens: 9_800,
        },
        ContextToolSchemaPart {
            origin: ToolSchemaOrigin::Mcp {
                server: "github".to_owned(),
            },
            schemas: 6,
            tokens: 3_100,
        },
    ];
    let messages = vec![
        ContextMessagePart {
            kind: ContextMessageKind::Assistant,
            tokens: conversation / 2,
        },
        ContextMessagePart {
            kind: ContextMessageKind::User,
            tokens: conversation / 4,
        },
    ];
    let tool_results = vec![
        ContextToolResultPart {
            tool: Some("bash".to_owned()),
            tokens: conversation / 6,
        },
        ContextToolResultPart {
            tool: Some("read_file".to_owned()),
            tokens: conversation / 12,
        },
    ];
    let request_context_tokens = 300;
    let parts: u64 = system_prompt.iter().map(|part| part.tokens).sum::<u64>()
        + tool_schemas.iter().map(|part| part.tokens).sum::<u64>()
        + messages.iter().map(|part| part.tokens).sum::<u64>()
        + tool_results.iter().map(|part| part.tokens).sum::<u64>()
        + request_context_tokens;
    SessionContextReport {
        // Fixed request framing is in the total but in no part.
        tokens: parts + 200,
        basis: ContextBasis::ProviderCalibrated,
        window_tokens: Some(WINDOW),
        system_prompt,
        tool_schemas,
        messages,
        tool_results,
        request_context_tokens,
    }
}

fn hooks(session: &FakeSession) -> SessionHooksReport {
    let file = session.cwd.join(".rho/hooks.toml");
    SessionHooksReport {
        enabled: true,
        files: vec![PathBuf::from("/home/user/.rho/hooks.toml"), file],
        skipped_untrusted: None,
        hooks: vec![
            HookContract {
                id: "user:notify".to_owned(),
                status: HookStatus::Active,
                event: HookEvent::RunCompleted,
                tools: HookTools::All,
                command: vec!["notify-send".to_owned(), "rho".to_owned()],
                working_directory: session.cwd.clone(),
                timeout_ms: 5_000,
                environment: Vec::new(),
                warnings: Vec::new(),
            },
            HookContract {
                id: "project:fmt".to_owned(),
                status: HookStatus::Active,
                event: HookEvent::BeforeToolUse,
                tools: HookTools::Matching {
                    patterns: vec!["bash".to_owned()],
                },
                command: vec!["./scripts/check.sh".to_owned()],
                working_directory: session.cwd.clone(),
                timeout_ms: 30_000,
                environment: vec!["RHO_TOOL".to_owned()],
                warnings: Vec::new(),
            },
        ],
        recent_activity: vec![
            HookActivityRecord {
                hook: Some("project:fmt".to_owned()),
                event: HookEvent::BeforeToolUse,
                outcome: HookOutcome::Continued,
                duration_ms: Some(42),
                output_truncated: false,
            },
            HookActivityRecord {
                hook: Some("project:fmt".to_owned()),
                event: HookEvent::BeforeToolUse,
                outcome: HookOutcome::Denied {
                    reason: "rm -rf is not allowed".to_owned(),
                },
                duration_ms: Some(38),
                output_truncated: false,
            },
        ],
    }
}

fn compaction(session: &FakeSession) -> SessionCompactionReport {
    let tokens = context(session).tokens;
    SessionCompactionReport {
        auto_compact: true,
        context: CompactionContextSize {
            tokens,
            local_tokens: tokens - tokens / 20,
            provider_reported_tokens: Some(tokens),
            provider_request_local_tokens: Some(tokens - tokens / 20),
        },
        window_tokens: Some(WINDOW),
        threshold_tokens: Some(WINDOW / 10 * 8),
        target_tokens: Some(WINDOW / 10 * 4),
        local_target_tokens: Some(WINDOW / 10 * 4),
        completed: CompletedCompactions {
            count: 1,
            removed_local_tokens: 96_000,
            last: Some(CompactionSize {
                before_local_tokens: 162_000,
                after_local_tokens: 66_000,
            }),
        },
        last_run: Some(CompactionRun {
            trigger: CompactionRunTrigger::Automatic,
            outcome: CompactionRunOutcome::Completed,
            tier: Some(CompactionTier::TextSummary),
            summary_request: Some(SummaryRequestPath::SessionHistory),
            model: Some(model("anthropic", "claude-sonnet-4-5")),
            elided_tool_results: 12,
            context_tokens: 168_000,
            prompt_tokens: Some(170_000),
            output_tokens: Some(4_100),
            cache_read_tokens: Some(150_000),
            cost_usd_micros: Some(118_000),
            latency_ms: 21_400,
            next_prompt_tokens: Some(70_200),
            reread: None,
        }),
        last_check: Some(CompactionCheck {
            context_tokens: tokens,
            threshold: Some(CompactionThreshold::ContextTokens {
                tokens: WINDOW / 10 * 8,
            }),
            skipped: Some(CompactionSkipReason::BelowThreshold),
            extent: CompactionExtent::History,
        }),
    }
}

/// The provider context report, with two reasoning blocks `writer` wrote
/// left out if it is another model's.
fn provider_context(writer: Option<ModelRef>) -> SessionProviderContextReport {
    let left_out = writer.is_some();
    let (omitted_blocks, omitted_kinds, omitted_block_ids) = if left_out {
        (
            2,
            vec!["reasoning".to_owned()],
            vec![ProviderContextBlockId(7), ProviderContextBlockId(9)],
        )
    } else {
        (0, Vec::new(), Vec::new())
    };
    SessionProviderContextReport {
        target: model("anthropic", "claude-sonnet-4-5"),
        omitted_blocks,
        omitted_kinds,
        omitted_block_ids,
        source: writer,
        can_compact: left_out,
        source_selectable: left_out,
    }
}

fn model(provider: &str, model: &str) -> ModelRef {
    ModelRef {
        provider: provider.to_owned(),
        model: model.to_owned(),
    }
}
