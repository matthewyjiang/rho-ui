//! One session's conversation as the UI shows it, rebuilt from ACP v2
//! `session/update` notifications. Pure state: no I/O, no GUI.
//!
//! Messages are keyed by `messageId`: a chunk appends, a full message
//! replaces (that is how resume replay and edits arrive). Tool calls are keyed
//! by `toolCallId` and merge field by field.

use std::{collections::HashMap, path::PathBuf};

use agent_client_protocol::schema::{v2, MaybeUndefined};

use crate::agent::{commands, CommandInfo, SessionId, SessionSetup};

/// An opaque update for one thread. Only [`Thread::apply`] reads it.
#[derive(Debug)]
pub struct ThreadUpdate(pub(crate) v2::SessionUpdate);

/// Whether the agent is working on this thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    /// Waiting for a prompt.
    Idle,
    /// A turn is in progress.
    Running,
    /// Blocked on the user, such as an approval.
    NeedsAction,
    /// The user stopped the last turn.
    Cancelled,
    /// The last turn failed.
    Failed(String),
}

impl RunState {
    /// True while a turn is in progress.
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Running | Self::NeedsAction)
    }
}

/// Context fill and spend, as the agent last reported them.
#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    used: u64,
    size: u64,
    cost: Option<(f64, String)>,
}

impl Usage {
    /// Context tokens in use.
    pub fn used(&self) -> u64 {
        self.used
    }

    /// Context window size in tokens.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Cumulative session cost: amount and ISO currency code.
    pub fn cost(&self) -> Option<(f64, &str)> {
        self.cost
            .as_ref()
            .map(|(amount, currency)| (*amount, currency.as_str()))
    }
}

/// A value to set with [`crate::Agent::set_config`].
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigValue {
    /// A [`ConfigChoice::value`] for a [`ConfigKind::Select`] option.
    Select(String),
    /// The new state of a [`ConfigKind::Toggle`] option.
    Toggle(bool),
}

/// One choice in a [`ConfigKind::Select`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigChoice {
    value: String,
    name: String,
}

impl ConfigChoice {
    /// Pass to [`crate::Agent::set_config`] as [`ConfigValue::Select`].
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Label to show.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The shape of a [`ConfigOption`] and its current value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigKind {
    /// Pick one of several values.
    Select {
        /// The selected [`ConfigChoice::value`].
        current: String,
        /// Every choice, groups flattened in order.
        choices: Vec<ConfigChoice>,
    },
    /// On or off.
    Toggle(bool),
}

/// A per-session setting the agent exposes, such as mode or model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigOption {
    id: String,
    name: String,
    kind: ConfigKind,
}

impl ConfigOption {
    /// Stable ID such as `mode`, `model`, `thought_level`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Label to show, such as "Model".
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its shape and current value.
    pub fn kind(&self) -> &ConfigKind {
        &self.kind
    }
}

/// Converts ACP options, dropping kinds this UI cannot show.
pub(crate) fn config_options(options: Vec<v2::SessionConfigOption>) -> Vec<ConfigOption> {
    options
        .into_iter()
        .filter_map(|option| {
            let kind = match option.kind {
                v2::SessionConfigKind::Select(select) => {
                    let choices = match select.options {
                        v2::SessionConfigSelectOptions::Ungrouped(options) => options,
                        v2::SessionConfigSelectOptions::Grouped(groups) => {
                            groups.into_iter().flat_map(|group| group.options).collect()
                        }
                        _ => Vec::new(),
                    };
                    ConfigKind::Select {
                        current: select.current_value.0.to_string(),
                        choices: choices
                            .into_iter()
                            .map(|choice| ConfigChoice {
                                value: choice.value.0.to_string(),
                                name: choice.name,
                            })
                            .collect(),
                    }
                }
                v2::SessionConfigKind::Boolean(toggle) => ConfigKind::Toggle(toggle.current_value),
                _ => return None,
            };
            Some(ConfigOption {
                id: option.config_id.0.to_string(),
                name: option.name,
                kind,
            })
        })
        .collect()
}

/// Where a tool call is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    /// Announced, not started; may be waiting on approval.
    Pending,
    /// Running now.
    Running,
    /// Finished.
    Done,
    /// Finished with an error.
    Failed,
    /// Stopped before it finished.
    Cancelled,
}

/// One piece of a tool card's body.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolContent {
    /// Output text, such as a command's stdout.
    Text(String),
    /// Changed files, with a git patch when the agent sent one.
    Diff {
        /// Every file the change touches.
        paths: Vec<PathBuf>,
        /// Patch text in git format, when the agent sent one.
        patch: Option<String>,
    },
}

/// A tool the agent ran, shown as a card.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    id: String,
    title: String,
    status: ToolStatus,
    content: Vec<ToolContent>,
}

impl ToolCall {
    /// The agent's `toolCallId`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What it does, such as the command it runs. Empty until the agent says.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Where the call is in its life.
    pub fn status(&self) -> ToolStatus {
        self.status
    }

    /// Output and file changes, in order.
    pub fn content(&self) -> &[ToolContent] {
        &self.content
    }
}

/// Text from the user or the agent, as Markdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    id: String,
    text: String,
}

impl Message {
    /// The agent's `messageId`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Everything received so far.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// How loud a [`Notice`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    /// For information.
    Info,
    /// Something may need attention.
    Warning,
    /// Something failed.
    Error,
}

/// A message from the agent that is not part of the conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    level: NoticeLevel,
    title: String,
    description: Option<String>,
}

impl Notice {
    /// How loud it is.
    pub fn level(&self) -> NoticeLevel {
        self.level
    }

    /// One line.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// More detail, if any.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

/// One row of the transcript.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// What the user sent.
    User(Message),
    /// The agent's reply.
    Agent(Message),
    /// The agent's visible reasoning.
    Thought(Message),
    /// A tool call.
    Tool(ToolCall),
    /// A notice from the agent.
    Notice(Notice),
}

impl Entry {
    /// A stable key for this row, derived from the agent's IDs. Use it for
    /// element identity.
    pub fn key(&self) -> String {
        match self {
            Entry::User(message) | Entry::Agent(message) | Entry::Thought(message) => {
                format!("msg:{}", message.id)
            }
            Entry::Tool(tool) => format!("tool:{}", tool.id),
            Entry::Notice(_) => String::new(),
        }
    }
}

/// Progress on one [`PlanItem`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStatus {
    /// Not started.
    Pending,
    /// Being worked on.
    InProgress,
    /// Finished.
    Done,
}

/// One line of the agent's checklist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanItem {
    text: String,
    status: PlanStatus,
}

impl PlanItem {
    /// The task.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Its progress.
    pub fn status(&self) -> PlanStatus {
        self.status
    }
}

/// Index key for entries the agent may update later.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum EntryKey {
    Message(String),
    Tool(String),
}

/// A session's transcript and status.
#[derive(Clone, Debug)]
pub struct Thread {
    id: SessionId,
    title: Option<String>,
    entries: Vec<Entry>,
    index: HashMap<EntryKey, usize>,
    state: RunState,
    usage: Option<Usage>,
    config: Vec<ConfigOption>,
    commands: Vec<CommandInfo>,
    plan: Vec<PlanItem>,
    revision: u64,
}

impl Thread {
    /// An empty thread for `id`. Create it before resuming the session, so
    /// replayed history has somewhere to go.
    pub fn new(id: SessionId) -> Self {
        Self {
            id,
            title: None,
            entries: Vec::new(),
            index: HashMap::new(),
            state: RunState::Idle,
            usage: None,
            config: Vec::new(),
            commands: Vec::new(),
            plan: Vec::new(),
            revision: 0,
        }
    }

    /// Records what `session/new` or `session/resume` returned.
    pub fn set_setup(&mut self, setup: SessionSetup) {
        self.config = setup.config;
        self.commands = setup.commands;
        self.revision += 1;
    }

    /// Replaces the settings after [`crate::Agent::set_config`].
    pub fn set_config(&mut self, config: Vec<ConfigOption>) {
        self.config = config;
        self.revision += 1;
    }

    /// Sets the title, such as one from [`crate::SessionSummary::title`].
    pub fn set_title(&mut self, title: Option<String>) {
        self.title = title;
        self.revision += 1;
    }

    /// Marks a prompt that the agent refused before it started.
    pub fn fail(&mut self, error: String) {
        self.state = RunState::Failed(error);
        self.revision += 1;
    }

    /// The session this thread shows.
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// The agent's title for the session, once it has one.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Transcript rows in the order they first appeared.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Whether the agent is working.
    pub fn state(&self) -> &RunState {
        &self.state
    }

    /// Context fill and spend, once reported.
    pub fn usage(&self) -> Option<&Usage> {
        self.usage.as_ref()
    }

    /// Settings the user can change, such as mode and model.
    pub fn config(&self) -> &[ConfigOption] {
        &self.config
    }

    /// Slash commands and skills on offer.
    pub fn commands(&self) -> &[CommandInfo] {
        &self.commands
    }

    /// The agent's current checklist.
    pub fn plan(&self) -> &[PlanItem] {
        &self.plan
    }

    /// Increases on every change, for cheap "did anything change" checks.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Applies one update from [`crate::AgentEvent::Update`].
    pub fn apply(&mut self, update: ThreadUpdate) {
        self.revision += 1;
        match update.0 {
            v2::SessionUpdate::UserMessageChunk(chunk) => {
                self.append_message(&chunk, Entry::User);
            }
            v2::SessionUpdate::AgentMessageChunk(chunk) => {
                self.append_message(&chunk, Entry::Agent);
            }
            v2::SessionUpdate::AgentThoughtChunk(chunk) => {
                self.append_message(&chunk, Entry::Thought);
            }
            v2::SessionUpdate::UserMessage(message) => {
                self.replace_message(&message.message_id, message.content, Entry::User);
            }
            v2::SessionUpdate::AgentMessage(message) => {
                self.replace_message(&message.message_id, message.content, Entry::Agent);
            }
            v2::SessionUpdate::AgentThought(message) => {
                self.replace_message(&message.message_id, message.content, Entry::Thought);
            }
            v2::SessionUpdate::StateUpdate(state) => self.state = run_state(state),
            v2::SessionUpdate::ToolCallUpdate(update) => self.update_tool(update),
            v2::SessionUpdate::ToolCallContentChunk(chunk) => {
                let tool = self.tool_mut(&chunk.tool_call_id.0);
                if let Some(content) = tool_content(chunk.content) {
                    tool.content.push(content);
                }
            }
            v2::SessionUpdate::PlanUpdate(update) => {
                if let v2::PlanUpdateContent::Items(items) = update.plan {
                    self.plan = items
                        .entries
                        .into_iter()
                        .map(|entry| PlanItem {
                            text: entry.content,
                            status: match entry.status {
                                v2::PlanEntryStatus::InProgress => PlanStatus::InProgress,
                                v2::PlanEntryStatus::Completed => PlanStatus::Done,
                                _ => PlanStatus::Pending,
                            },
                        })
                        .collect();
                }
            }
            v2::SessionUpdate::AvailableCommandsUpdate(update) => {
                self.commands = commands(update.available_commands);
            }
            v2::SessionUpdate::ConfigOptionUpdate(update) => {
                self.config = config_options(update.config_options);
            }
            v2::SessionUpdate::SessionInfoUpdate(update) => match update.title {
                MaybeUndefined::Value(title) => self.title = Some(title),
                MaybeUndefined::Null => self.title = None,
                MaybeUndefined::Undefined => {}
            },
            v2::SessionUpdate::UsageUpdate(usage) => {
                self.usage = Some(Usage {
                    used: usage.used,
                    size: usage.size,
                    cost: usage.cost.map(|cost| (cost.amount, cost.currency)),
                });
            }
            v2::SessionUpdate::Notice(notice) => self.entries.push(Entry::Notice(Notice {
                level: match notice.severity {
                    v2::NoticeSeverity::Warning => NoticeLevel::Warning,
                    v2::NoticeSeverity::Error => NoticeLevel::Error,
                    _ => NoticeLevel::Info,
                },
                title: notice.title,
                description: notice.description,
            })),
            // Terminals and compaction progress are not shown yet.
            _ => {}
        }
    }

    /// Index of the entry under `key`, inserting `make()` at the end if new.
    fn upsert(&mut self, key: EntryKey, make: impl FnOnce() -> Entry) -> usize {
        let entries = &mut self.entries;
        *self.index.entry(key).or_insert_with(|| {
            entries.push(make());
            entries.len() - 1
        })
    }

    fn message_mut(&mut self, id: &str, make: fn(Message) -> Entry) -> &mut Message {
        let at = self.upsert(EntryKey::Message(id.to_owned()), || {
            make(Message {
                id: id.to_owned(),
                text: String::new(),
            })
        });
        match &mut self.entries[at] {
            Entry::User(message) | Entry::Agent(message) | Entry::Thought(message) => message,
            _ => unreachable!("message keys index message entries"),
        }
    }

    fn append_message(&mut self, chunk: &v2::ContentChunk, make: fn(Message) -> Entry) {
        let message = self.message_mut(&chunk.message_id.0, make);
        message.text.push_str(&block_text(&chunk.content));
    }

    fn replace_message(
        &mut self,
        id: &v2::MessageId,
        content: MaybeUndefined<Vec<v2::ContentBlock>>,
        make: fn(Message) -> Entry,
    ) {
        let message = self.message_mut(&id.0, make);
        match content {
            MaybeUndefined::Undefined => {}
            MaybeUndefined::Null => message.text.clear(),
            MaybeUndefined::Value(blocks) => message.text = blocks.iter().map(block_text).collect(),
        }
    }

    fn tool_mut(&mut self, id: &str) -> &mut ToolCall {
        let at = self.upsert(EntryKey::Tool(id.to_owned()), || {
            Entry::Tool(ToolCall {
                id: id.to_owned(),
                title: String::new(),
                status: ToolStatus::Pending,
                content: Vec::new(),
            })
        });
        match &mut self.entries[at] {
            Entry::Tool(tool) => tool,
            _ => unreachable!("tool keys index tool entries"),
        }
    }

    fn update_tool(&mut self, update: v2::ToolCallUpdate) {
        let tool = self.tool_mut(&update.tool_call_id.0);
        if let MaybeUndefined::Value(title) = update.title {
            tool.title = title;
        }
        if let MaybeUndefined::Value(status) = update.status {
            tool.status = match status {
                v2::ToolCallStatus::Pending => ToolStatus::Pending,
                v2::ToolCallStatus::InProgress => ToolStatus::Running,
                v2::ToolCallStatus::Completed => ToolStatus::Done,
                v2::ToolCallStatus::Failed => ToolStatus::Failed,
                v2::ToolCallStatus::Cancelled => ToolStatus::Cancelled,
                _ => tool.status,
            };
        }
        match update.content {
            MaybeUndefined::Undefined => {}
            MaybeUndefined::Null => tool.content.clear(),
            MaybeUndefined::Value(content) => {
                tool.content = content.into_iter().filter_map(tool_content).collect();
            }
        }
    }
}

fn run_state(state: v2::StateUpdate) -> RunState {
    match state {
        v2::StateUpdate::RequiresAction(_) => RunState::NeedsAction,
        v2::StateUpdate::Idle(idle) => match idle.stop_reason {
            Some(v2::StopReason::Cancelled) => RunState::Cancelled,
            Some(v2::StopReason::Error(stop)) => RunState::Failed(match stop.error {
                Some(error) => match error.data {
                    Some(serde_json::Value::String(detail)) => detail,
                    _ => error.message,
                },
                None => "the turn failed".to_owned(),
            }),
            Some(v2::StopReason::Refusal) => RunState::Failed("the model refused".to_owned()),
            Some(v2::StopReason::MaxTokens) => {
                RunState::Failed("hit the output token limit".to_owned())
            }
            _ => RunState::Idle,
        },
        // `running`, and draft states this UI does not know yet.
        _ => RunState::Running,
    }
}

/// Text for one content block. Non-text blocks become short placeholders.
fn block_text(block: &v2::ContentBlock) -> String {
    match block {
        v2::ContentBlock::Text(text) => text.text.clone(),
        v2::ContentBlock::Image(_) => "[image]".to_owned(),
        v2::ContentBlock::Audio(_) => "[audio]".to_owned(),
        v2::ContentBlock::ResourceLink(link) => format!("[{}]", link.name),
        v2::ContentBlock::Resource(_) => "[attachment]".to_owned(),
        _ => String::new(),
    }
}

fn tool_content(content: v2::ToolCallContent) -> Option<ToolContent> {
    match content {
        v2::ToolCallContent::Content(content) => {
            Some(ToolContent::Text(block_text(&content.content)))
        }
        v2::ToolCallContent::Diff(diff) => Some(ToolContent::Diff {
            paths: diff
                .changes
                .into_iter()
                .filter_map(|change| match change.operation {
                    v2::DiffChangeOperation::Add(change)
                    | v2::DiffChangeOperation::Delete(change)
                    | v2::DiffChangeOperation::Modify(change) => Some(change.path.0),
                    v2::DiffChangeOperation::Move(change)
                    | v2::DiffChangeOperation::Copy(change) => Some(change.path.0),
                    _ => None,
                })
                .collect(),
            patch: diff.patch.map(|patch| patch.text),
        }),
        _ => None,
    }
}

#[cfg(test)]
#[path = "thread_tests.rs"]
mod tests;
