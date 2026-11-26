//! The desktop UI's rho layer: connect to the rho daemon, talk to it, and
//! keep each session's state. No GUI code lives here.

pub mod agent_settings;
mod background;
mod completion;
mod daemon;
mod exec;
#[cfg(all(feature = "test-support", not(target_family = "wasm")))]
pub mod fake_daemon;
#[cfg(all(feature = "test-support", not(target_family = "wasm")))]
pub mod fake_sshd;
mod input;
mod jobs;
mod link;
mod options;
pub mod platform;
mod process_log;
mod prompt;
#[cfg(not(target_family = "wasm"))]
pub mod release;
mod remote_shell;
mod rho_install;
pub mod server;
mod session;
mod setup;
#[cfg(not(target_family = "wasm"))]
mod ssh;
pub mod surface;
mod timeline;
mod tool_card;

pub use agent_settings::{AgentSettings, AgentSettingsChanged};
pub use background::{BackgroundId, BackgroundKind, BackgroundStatus, BackgroundWork};
pub use completion::{Completion, Edit};
pub use daemon::{
    Daemon, DaemonError, DaemonEvent, DaemonEvents, DaemonInfo, DaemonStatus, DaemonTarget,
    Detached, PendingUpgrade, PromptKey, PullRequestError, ReportedWorktree, RestartCost,
    SessionId, SessionMark, SessionSummary, SurfaceActionError, Unsteered, VersionFix,
    VersionMismatch, LEAD_AGENT,
};
pub use input::{
    Answer, ChoiceKind, Form, InputChoice, InputId, InputRequest, Question, QuestionChoice,
    Questions, Suggestion, SuggestionStyle, TextBox, Timeout,
};
pub use jobs::{Job, JobActivity, Jobs, Run, RunStatus, Runs};
pub use link::Link;
pub use options::{
    Choices, CommandInfo, CommandKind, ModelChoice, ModelOptions, ModelRef, OptionChange,
    PermissionMode, ReasoningLevel, SessionOptions, StartedMode, StartedSessionChange,
    StartedSessionDefaults,
};
pub use process_log::{LogChange, OutputLine, ProcessLog};
pub use prompt::{
    check_attachment_total, mebibytes, AttachedFile, Attachment, AttachmentsTooLarge, Image,
    ImageFormat, Prompt, MAX_ATTACHED_TEXT_CHARS, MAX_PROMPT_ATTACHMENT_BYTES,
};
pub use remote_shell::{
    RemoteShell, ShellControl, ShellEnded, ShellError, ShellInput, ShellOutput,
};
pub use rho_install::{RhoCheck, RhoInstall, Update};
/// The mark [`Daemon::mark_session`] sets, as rho-protocol has it.
pub use rho_protocol::NewSessionMark;
/// The auth profiles, logins and credential store [`Daemon`]'s sign-in
/// methods speak of, as rho-protocol has them.
pub use rho_protocol::{
    AuthKind, AuthStatus, AuthSummary, CredentialStoreKind, CredentialStoreStatus, LoginId,
    LoginOutcome, LoginPrompt, StoreSelectedBy, UncheckedKey, UncheckedReason,
};
/// A session's pull requests, as [`Daemon::list_pull_requests`] and its
/// siblings answer and [`SessionUpdate::pull_requests`] carries them, as
/// rho-protocol has them.
pub use rho_protocol::{
    BranchLookup, BranchPullRequest, LinkedPullRequest, PullRequestCheck, PullRequestCheckStatus,
    PullRequestDetail, PullRequestId, PullRequestLinkOrigin, PullRequestMergeMethod,
    PullRequestMergeable, PullRequestPost, PullRequestRead, PullRequestReview, PullRequestRollup,
    PullRequestState, PullRequestSummary, PullRequestTarget, PullRequestThread,
    PullRequestThreadState, PullRequestTrouble, PullRequestVerdict, SessionPullRequestLinkResponse,
    SessionPullRequestShowResponse, SessionPullRequests,
};
/// A session's background processes and their output, as
/// [`Daemon::processes`] reads them, as rho-protocol has them.
pub use rho_protocol::{
    ProcessChunk, ProcessOutput, ProcessOutputParams, ProcessState, ProcessStream, SessionProcess,
    SessionProcessesResponse,
};
/// What [`Daemon::check_worktrees`] and [`Daemon::prune_worktrees`] report
/// of each session worktree, as rho-protocol has it.
pub use rho_protocol::{SessionWorktree, WorktreeVerdict};
pub use server::{ServerLink, ServerTarget};
pub use session::{
    Attached, Checkout, Compaction, ContextSize, Continuity, Cursor, Delay, Entry, Message, Notice,
    NoticeLevel, PlanItem, PlanStatus, RetryReason, RunState, Session, SessionSetup, SessionUpdate,
    SteerId, ToolCall, ToolContent, ToolKind, ToolStatus, TurnId, Usage, UserMessage, Worktree,
};
#[cfg(not(target_family = "wasm"))]
pub use ssh::{SshLink, SshTarget, MAX_SHELLS};
pub use timeline::{any_failed, summary, thought_line, Row, Step, Summary, Synced, Timeline};
pub use tool_card::{SearchLine, ToolBody, ToolCard, ToolFact};
