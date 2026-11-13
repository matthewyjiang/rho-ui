//! The desktop UI's rho layer: connect to the rho daemon, talk to it, and
//! keep each session's state. No GUI code lives here.

mod background;
mod daemon;
#[cfg(feature = "test-support")]
pub mod fake_daemon;
mod input;
mod options;
mod session;
mod timeline;

pub use background::{BackgroundId, BackgroundKind, BackgroundStatus, BackgroundWork};
pub use daemon::{
    Daemon, DaemonError, DaemonEvent, DaemonEvents, DaemonInfo, DaemonTarget, Detached, Job,
    JobActivity, SessionId, SessionSummary, Unsteered,
};
pub use input::{ChoiceKind, InputChoice, InputId, InputRequest};
pub use options::{
    CommandInfo, ConfigCategory, ConfigChoice, ConfigKind, ConfigOption, ConfigValue, FAST_MODE_ID,
};
pub use session::{
    Attached, Checkout, Entry, Message, Notice, NoticeLevel, PlanItem, PlanStatus, RunState,
    Session, SessionSetup, SessionUpdate, SteerId, ToolCall, ToolContent, ToolKind, ToolStatus,
    TurnId, Usage, Worktree,
};
pub use timeline::{any_failed, summary, thought_line, Row, Step, Summary, Synced, Timeline};
