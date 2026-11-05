//! The desktop UI's agent layer: launch an ACP agent, talk to it, and keep
//! each session's thread state. No GUI code lives here.

mod agent;
mod thread;
mod timeline;

pub use agent::{
    Agent, AgentCommand, AgentError, AgentEvent, AgentEvents, AgentInfo, CommandInfo,
    PermissionChoice, PermissionKind, PermissionRequest, SessionId, SessionSetup, SessionSummary,
};
pub use thread::{
    ConfigChoice, ConfigKind, ConfigOption, ConfigValue, Entry, Message, Notice, NoticeLevel,
    PlanItem, PlanStatus, RunState, Thread, ThreadUpdate, ToolCall, ToolContent, ToolKind,
    ToolStatus, Usage,
};
pub use timeline::{any_failed, summary, thought_line, Row, Step, Summary, Synced, Timeline};
