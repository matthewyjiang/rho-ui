//! The desktop UI's rho layer: connect to the rho daemon, talk to it, and
//! keep each session's state. No GUI code lives here.

pub mod agent_settings;
mod background;
mod completion;
mod daemon;
#[cfg(feature = "test-support")]
pub mod fake_daemon;
#[cfg(feature = "test-support")]
pub mod fake_sshd;
mod input;
mod options;
pub mod platform;
mod prompt;
mod rho_install;
mod session;
mod ssh;
mod timeline;
mod tool_card;

pub use agent_settings::{AgentSettings, AgentSettingsChanged};
pub use background::{BackgroundId, BackgroundKind, BackgroundStatus, BackgroundWork};
pub use completion::{Completion, Edit};
pub use daemon::{
    Daemon, DaemonError, DaemonEvent, DaemonEvents, DaemonInfo, DaemonTarget, Detached, Job,
    JobActivity, PendingUpgrade, SessionId, SessionSummary, Unsteered,
};
pub use input::{
    Answer, ChoiceKind, Form, InputChoice, InputId, InputRequest, Question, QuestionChoice,
    Questions, Suggestion, SuggestionStyle, TextBox, Timeout,
};
pub use options::{
    CommandInfo, CommandKind, ConfigCategory, ConfigChoice, ConfigKind, ConfigOption, ConfigValue,
    FAST_MODE_ID,
};
pub use prompt::{
    check_attachment_total, mebibytes, AttachedFile, Attachment, AttachmentsTooLarge, Image,
    ImageFormat, Prompt, MAX_ATTACHED_TEXT_CHARS, MAX_PROMPT_ATTACHMENT_BYTES,
};
pub use rho_install::{RhoCheck, RhoInstall, Update};
pub use session::{
    Attached, Checkout, Delay, Entry, Message, Notice, NoticeLevel, PlanItem, PlanStatus,
    RetryReason, RunState, Session, SessionSetup, SessionUpdate, SteerId, ToolCall, ToolContent,
    ToolKind, ToolStatus, TurnId, Usage, UserMessage, Worktree,
};
pub use ssh::{RemoteShell, ShellEnded, ShellInput, ShellOutput, SshLink, SshTarget};
pub use timeline::{any_failed, summary, thought_line, Row, Step, Summary, Synced, Timeline};
pub use tool_card::{SearchLine, ToolBody, ToolCard, ToolFact};
