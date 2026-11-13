//! The desktop UI's rho layer: connect to the rho daemon, talk to it, and
//! keep each session's state. No GUI code lives here.

mod background;
mod daemon;
#[cfg(feature = "test-support")]
pub mod fake_daemon;
mod input;
mod options;
mod prompt;
mod session;
mod timeline;
mod tool_card;

pub use background::{BackgroundId, BackgroundKind, BackgroundStatus, BackgroundWork};
pub use daemon::{
    Daemon, DaemonError, DaemonEvent, DaemonEvents, DaemonInfo, DaemonTarget, Detached, Job,
    JobActivity, SessionId, SessionSummary, Unsteered,
};
pub use input::{ChoiceKind, InputChoice, InputId, InputRequest};
pub use options::{
    CommandInfo, ConfigCategory, ConfigChoice, ConfigKind, ConfigOption, ConfigValue, FAST_MODE_ID,
};
pub use prompt::{
    check_attachment_total, AttachedFile, Attachment, AttachmentsTooLarge, Image, ImageFormat,
    Prompt, MAX_ATTACHED_TEXT_CHARS, MAX_PROMPT_ATTACHMENT_BYTES,
};
pub use session::{
    Attached, Checkout, Entry, Message, Notice, NoticeLevel, PlanItem, PlanStatus, RunState,
    Session, SessionSetup, SessionUpdate, SteerId, ToolCall, ToolContent, ToolKind, ToolStatus,
    TurnId, Usage, UserMessage, Worktree,
};
pub use timeline::{any_failed, summary, thought_line, Row, Step, Summary, Synced, Timeline};
pub use tool_card::{SearchLine, ToolBody, ToolCard, ToolFact};
