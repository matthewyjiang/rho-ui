//! The desktop UI's agent layer: launch an ACP agent, talk to it, and keep
//! each session's state. No GUI code lives here.

mod agent;
mod session;
mod timeline;

pub use agent::{
    Agent, AgentCommand, AgentError, AgentEvent, AgentEvents, AgentInfo, CommandInfo,
    PermissionChoice, PermissionKind, PermissionRequest, SessionId, SessionSetup, SessionSummary,
};
pub use session::{
    ConfigCategory, ConfigChoice, ConfigKind, ConfigOption, ConfigValue, Entry, Message, Notice,
    NoticeLevel, PlanItem, PlanStatus, RunState, Session, SessionUpdate, ToolCall, ToolContent,
    ToolKind, ToolStatus, Usage,
};
pub use timeline::{any_failed, summary, thought_line, Row, Step, Summary, Synced, Timeline};
