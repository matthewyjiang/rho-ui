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
mod prompt;
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
    Detached, PendingUpgrade, PromptKey, ReportedWorktree, RestartCost, SessionId, SessionSummary,
    SurfaceActionError, Unsteered, VersionFix, VersionMismatch,
};
pub use input::{
    Answer, ChoiceKind, Form, InputChoice, InputId, InputRequest, Question, QuestionChoice,
    Questions, Suggestion, SuggestionStyle, TextBox, Timeout,
};
pub use jobs::{Job, JobActivity, Jobs, Run, RunStatus, Runs};
pub use link::Link;
pub use options::{
    Choices, CommandInfo, CommandKind, ModelChoice, ModelOptions, ModelRef, OptionChange,
    PermissionMode, ReasoningLevel, SessionOptions,
};
pub use prompt::{
    check_attachment_total, mebibytes, AttachedFile, Attachment, AttachmentsTooLarge, Image,
    ImageFormat, Prompt, MAX_ATTACHED_TEXT_CHARS, MAX_PROMPT_ATTACHMENT_BYTES,
};
pub use remote_shell::{
    RemoteShell, ShellControl, ShellEnded, ShellError, ShellInput, ShellOutput,
};
pub use rho_install::{RhoCheck, RhoInstall, Update};
/// The auth profiles, logins and credential store [`Daemon`]'s sign-in
/// methods speak of, as rho-protocol has them.
pub use rho_protocol::{
    AuthKind, AuthStatus, AuthSummary, CredentialStoreKind, CredentialStoreStatus, LoginId,
    LoginOutcome, LoginPrompt, StoreSelectedBy, UncheckedKey, UncheckedReason,
};
/// What [`Daemon::check_worktrees`] and [`Daemon::prune_worktrees`] report
/// of each session worktree, as rho-protocol has it.
pub use rho_protocol::{SessionWorktree, WorktreeVerdict};
pub use server::{ServerLink, ServerTarget};
pub use session::{
    Attached, Checkout, Continuity, Cursor, Delay, Entry, Message, Notice, NoticeLevel, PlanItem,
    PlanStatus, RetryReason, RunState, Session, SessionSetup, SessionUpdate, SteerId, ToolCall,
    ToolContent, ToolKind, ToolStatus, TurnId, Usage, UserMessage, Worktree,
};
#[cfg(not(target_family = "wasm"))]
pub use ssh::{SshLink, SshTarget, MAX_SHELLS};
pub use timeline::{any_failed, summary, thought_line, Row, Step, Summary, Synced, Timeline};
pub use tool_card::{SearchLine, ToolBody, ToolCard, ToolFact};
