//! One session's conversation as the UI shows it, rebuilt from the daemon's
//! session events. Pure state: no I/O, no GUI.
//!
//! Messages are keyed by their message ID: a chunk appends, a whole user
//! message replaces. Tool calls are keyed by their call ID and merge field by
//! field.
//!
//! A session starts out holding its events: the daemon sends an attachment's
//! replay right after the attach response, and the two reach the UI on
//! separate paths. [`Session::attached`] takes the response, then the events
//! held so far, so the response always lands first.

use std::{path::PathBuf, time::Duration};

use rho_protocol::{
    AnswerStop, CardTone, ContentBlock, JobState, SessionAttachResponse, SessionEvent,
    SessionOptions, ToolCallUpdate, TurnOutcome,
};

use crate::{
    daemon::SessionId,
    input::{InputId, InputRequest},
    options::{commands, config_options, CommandInfo, ConfigOption},
};

/// An opaque event for one session. Only [`Session::apply`] reads it.
#[derive(Debug)]
pub struct SessionUpdate(pub(crate) SessionEvent);

/// What `rho/session/new` returned. Hand it to [`Session::set_setup`].
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSetup {
    pub(crate) id: SessionId,
    pub(crate) options: SessionOptions,
}

impl SessionSetup {
    /// The session this setup belongs to.
    pub fn id(&self) -> &SessionId {
        &self.id
    }
}

/// What `rho/session/attach` returned. Hand it to [`Session::attached`].
#[derive(Debug)]
pub struct Attached(pub(crate) Box<SessionAttachResponse>);

impl Attached {
    /// How long the turn under way had run when the attachment began.
    pub fn turn_elapsed(&self) -> Option<Duration> {
        self.0.turn_elapsed_ms.map(Duration::from_millis)
    }
}

/// Whether rho is working on this session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    /// Waiting for a prompt.
    Idle,
    /// A turn is in progress, or waits for a slot to run in.
    Running,
    /// The turn waits for a person, such as an approval.
    NeedsAction,
    /// Someone stopped the last turn.
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

/// Context fill and spend, as the daemon last reported them.
#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    used: u64,
    size: Option<u64>,
    cost_usd: Option<f64>,
}

impl Usage {
    /// Context tokens in use.
    pub fn used(&self) -> u64 {
        self.used
    }

    /// Context window size in tokens, when the model's is known.
    pub fn size(&self) -> Option<u64> {
        self.size
    }

    /// What the session has cost so far, in US dollars.
    pub fn cost_usd(&self) -> Option<f64> {
        self.cost_usd
    }
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
    /// Finished with an error, or was cut off.
    Failed,
}

/// What a tool call does, as rho classes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolKind {
    /// Reads files.
    Read,
    /// Edits, deletes or moves files.
    Edit,
    /// Searches files or code.
    Search,
    /// Runs a command.
    Execute,
    /// Planning the agent shows as a tool call.
    Think,
    /// Fetches something from the web.
    Fetch,
    /// Anything else.
    #[default]
    Other,
}

/// One piece of a tool card's body.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolContent {
    /// Output text, such as a command's stdout.
    Text(String),
    /// One changed file, with the change as a patch in git format.
    Diff {
        /// The file the change touches.
        path: PathBuf,
        /// The change, from the texts the daemon sent.
        patch: String,
    },
}

/// A tool rho ran, shown as a card.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    id: String,
    title: String,
    kind: ToolKind,
    status: ToolStatus,
    content: Vec<ToolContent>,
}

impl ToolCall {
    /// The daemon's ID for the call.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What it does, such as the command it runs. Empty until the daemon says.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// What the call does; [`ToolKind::Other`] until the daemon says.
    pub fn kind(&self) -> ToolKind {
        self.kind
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
    /// The daemon's ID for the message.
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

/// A message from rho that is not part of the conversation.
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

/// A steer the turn accepted that has not reached the conversation yet.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Steer {
    id: SteerId,
    text: String,
}

/// A session's transcript and status.
#[derive(Clone, Debug)]
pub struct Session {
    id: SessionId,
    title: Option<String>,
    /// Only ever appended to or changed in place, so an index names one
    /// entry for the session's lifetime.
    entries: Vec<Entry>,
    state: RunState,
    usage: Option<Usage>,
    config: Vec<ConfigOption>,
    commands: Vec<CommandInfo>,
    plan: Vec<PlanItem>,
    /// Open requests, oldest first.
    inputs: Vec<InputRequest>,
    /// Steers waiting to apply, oldest first.
    steers: Vec<Steer>,
    /// Steers a turn dropped when it ended; see [`Self::take_dropped_steers`].
    dropped_steers: Vec<String>,
    /// IDs of steers that reached the conversation before
    /// [`Self::steer_sent`] recorded them: the daemon's answer to a steer
    /// and the steer's own event race each other here.
    early_steers: Vec<SteerId>,
    /// Events that arrived before the attach response; `None` once
    /// [`Self::attached`] has run.
    held: Option<Vec<SessionEvent>>,
    /// Events of the attachment's replay still to come.
    replay_left: u64,
}

impl Session {
    /// An empty session for `id`, holding its events until
    /// [`Self::attached`]. Create it before attaching, so the replay has
    /// somewhere to go.
    pub fn new(id: SessionId) -> Self {
        Self {
            id,
            title: None,
            entries: Vec::new(),
            state: RunState::Idle,
            usage: None,
            config: Vec::new(),
            commands: Vec::new(),
            plan: Vec::new(),
            inputs: Vec::new(),
            steers: Vec::new(),
            dropped_steers: Vec::new(),
            early_steers: Vec::new(),
            held: Some(Vec::new()),
            replay_left: 0,
        }
    }

    /// Records what `rho/session/new` returned, so a new session's settings
    /// show before its attachment answers.
    pub fn set_setup(&mut self, setup: SessionSetup) {
        self.set_options(setup.options);
    }

    /// Records the attach response, then applies the events held for it.
    /// The transcript may have grown; nothing in it changed before this.
    pub fn attached(&mut self, attached: Attached) {
        let response = *attached.0;
        if response.title.is_some() {
            self.title = response.title;
        }
        self.set_options(response.options);
        self.settle(response.state);
        for request in response.inputs {
            self.open_input(InputRequest::from_wire(request));
        }
        self.replay_left = response.replay_events;
        for event in self.held.take().unwrap_or_default() {
            self.apply_event(event);
        }
    }

    /// Replaces the settings after [`crate::Daemon::set_config`].
    pub fn set_config(&mut self, config: Vec<ConfigOption>) {
        self.config = config;
    }

    /// Sets the title, such as one from [`crate::SessionSummary::title`].
    pub fn set_title(&mut self, title: Option<String>) {
        self.title = title;
    }

    /// Marks a prompt that the daemon refused before its turn began.
    pub fn fail(&mut self, error: String) {
        self.state = RunState::Failed(error);
    }

    /// The ID rho knows this session by.
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// rho's title for the session, once it has one.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Transcript rows in the order they first appeared.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Whether rho is working.
    pub fn state(&self) -> &RunState {
        &self.state
    }

    /// Whether the attachment is still catching up: its response or part of
    /// its replay has yet to arrive. State changes until then are history,
    /// not something happening now.
    pub fn is_replaying(&self) -> bool {
        self.held.is_some() || self.replay_left > 0
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

    /// What the session waits on a person for, oldest first.
    pub fn inputs(&self) -> &[InputRequest] {
        &self.inputs
    }

    /// Takes request `id` out, as the UI does once it sends an answer, so
    /// it can't be answered twice. The daemon's own word that it closed is
    /// then a no-op; [`Self::open_input`] puts it back if the answer failed.
    pub fn close_input(&mut self, id: InputId) -> Option<InputRequest> {
        let at = self.inputs.iter().position(|request| request.id() == id)?;
        Some(self.inputs.remove(at))
    }

    /// Adds an open request, replacing one of the same ID: an attach
    /// response and its replay may both carry it.
    pub fn open_input(&mut self, request: InputRequest) {
        match self
            .inputs
            .iter_mut()
            .find(|open| open.id() == request.id())
        {
            Some(open) => *open = request,
            None => self.inputs.push(request),
        }
    }

    /// Records a steer the running turn accepted, as `id`. It shows in
    /// [`Self::pending_steers`] until it reaches the conversation.
    pub fn steer_sent(&mut self, id: SteerId, text: String) {
        match self.early_steers.iter().position(|early| *early == id) {
            Some(at) => {
                self.early_steers.swap_remove(at);
            }
            None => self.steers.push(Steer { id, text }),
        }
    }

    /// Steers the turn accepted that the model has not read yet, oldest
    /// first: each one's ID, for [`crate::Daemon::unsteer`], and its text.
    pub fn pending_steers(&self) -> impl Iterator<Item = (&SteerId, &str)> {
        self.steers
            .iter()
            .map(|steer| (&steer.id, steer.text.as_str()))
    }

    /// Forgets pending steer `id`, once the daemon withdrew it, and returns
    /// its text.
    pub fn withdraw_steer(&mut self, id: &SteerId) -> Option<String> {
        let at = self.steers.iter().position(|steer| steer.id == *id)?;
        Some(self.steers.remove(at).text)
    }

    /// Steers a turn dropped because it ended before reading them, so the
    /// UI can hand the text back. Each is returned once.
    pub fn take_dropped_steers(&mut self) -> Vec<String> {
        std::mem::take(&mut self.dropped_steers)
    }

    /// Applies one event from [`crate::DaemonEvent::Update`]. Returns the
    /// index of the entry it added or changed, so a view can redraw just
    /// that row. Before [`Self::attached`] it keeps the event and returns
    /// `None`.
    pub fn apply(&mut self, update: SessionUpdate) -> Option<usize> {
        match &mut self.held {
            Some(held) => {
                held.push(update.0);
                None
            }
            None => self.apply_event(update.0),
        }
    }

    fn apply_event(&mut self, event: SessionEvent) -> Option<usize> {
        self.replay_left = self.replay_left.saturating_sub(1);
        match event {
            SessionEvent::UserMessage {
                message,
                content,
                steer,
            } => {
                if let Some(steer) = steer {
                    let steer = SteerId(steer.as_str().to_owned());
                    if self.withdraw_steer(&steer).is_none() {
                        self.early_steers.push(steer);
                    }
                }
                let (at, message) = self.message_mut(message.as_str(), Entry::User);
                message.text = content.iter().map(block_text).collect();
                Some(at)
            }
            SessionEvent::AgentMessageChunk { message, text } => {
                Some(self.append_message(message.as_str(), &text, Entry::Agent))
            }
            SessionEvent::ThoughtChunk { message, text } => {
                Some(self.append_message(message.as_str(), &text, Entry::Thought))
            }
            SessionEvent::ToolCall(update) => Some(self.update_tool(*update)),
            SessionEvent::Notice { text } => Some(self.push_notice(Notice {
                level: NoticeLevel::Info,
                title: text,
                description: None,
            })),
            SessionEvent::Card {
                title, plain, tone, ..
            } => Some(self.push_notice(Notice {
                level: match tone {
                    CardTone::Warning => NoticeLevel::Warning,
                    CardTone::Error => NoticeLevel::Error,
                    _ => NoticeLevel::Info,
                },
                title,
                description: Some(plain).filter(|text| !text.is_empty()),
            })),
            SessionEvent::State { state, .. } => {
                self.settle(state);
                None
            }
            SessionEvent::TurnEnded { outcome, .. } => {
                self.end_turn(outcome);
                None
            }
            SessionEvent::Plan { entries } => {
                self.plan = entries
                    .into_iter()
                    .map(|entry| PlanItem {
                        text: entry.content,
                        status: match entry.status {
                            rho_protocol::PlanStatus::InProgress => PlanStatus::InProgress,
                            rho_protocol::PlanStatus::Completed => PlanStatus::Done,
                            _ => PlanStatus::Pending,
                        },
                    })
                    .collect();
                None
            }
            SessionEvent::OptionsChanged { options } => {
                self.set_options(*options);
                None
            }
            SessionEvent::TitleChanged { title } => {
                self.title = Some(title);
                None
            }
            SessionEvent::Usage {
                used_tokens,
                window_tokens,
                cost_usd_micros,
            } => {
                self.usage = Some(Usage {
                    used: used_tokens,
                    size: window_tokens,
                    cost_usd: cost_usd_micros.map(usd),
                });
                None
            }
            SessionEvent::InputRequested { request } => {
                self.open_input(InputRequest::from_wire(*request));
                None
            }
            SessionEvent::InputResolved { id, .. } => {
                self.close_input(InputId(id.0));
                None
            }
            // Compaction and provider progress, held timeouts, and cache
            // warming are not shown yet.
            _ => None,
        }
    }

    fn set_options(&mut self, options: SessionOptions) {
        self.config = config_options(&options);
        self.commands = commands(options.commands);
    }

    /// Follows the job's state for whether a turn runs. How a turn ended
    /// comes from its own event ([`Self::end_turn`]), which arrives first
    /// and says why; a state alone only clears a turn that left none, as
    /// one the daemon's restart cut short does.
    fn settle(&mut self, state: JobState) {
        match state {
            JobState::Running | JobState::Queued => self.state = RunState::Running,
            JobState::WaitingForInput => self.state = RunState::NeedsAction,
            JobState::Idle
            | JobState::Completed
            | JobState::Failed
            | JobState::Cancelled
            | JobState::TimedOut
            | JobState::Interrupted
                if self.state.is_busy() =>
            {
                self.state = RunState::Idle;
            }
            // Already settled, or a state a newer daemon added.
            _ => {}
        }
    }

    fn end_turn(&mut self, outcome: TurnOutcome) {
        self.state = match outcome {
            TurnOutcome::Answered { stop, .. } => match stop {
                AnswerStop::Refusal => RunState::Failed("the model refused".to_owned()),
                AnswerStop::MaxTokens => RunState::Failed("hit the output token limit".to_owned()),
                _ => RunState::Idle,
            },
            TurnOutcome::MaxSteps { .. } => {
                RunState::Failed("stopped at the model-step limit".to_owned())
            }
            TurnOutcome::Cancelled => RunState::Cancelled,
            TurnOutcome::TimedOut { timeout_ms } => RunState::Failed(format!(
                "timed out after {:?}",
                Duration::from_millis(timeout_ms)
            )),
            TurnOutcome::Failed { error } => RunState::Failed(error.message),
            _ => RunState::Idle,
        };
        // The daemon drops a steer its turn never read.
        let dropped = self.steers.drain(..).map(|steer| steer.text);
        self.dropped_steers.extend(dropped);
        self.early_steers.clear();
    }

    fn push_notice(&mut self, notice: Notice) -> usize {
        self.entries.push(Entry::Notice(notice));
        self.entries.len() - 1
    }

    /// The message `id` and its index, appending `make(empty)` if it is new.
    /// Searches from the end: updates almost always touch the newest rows.
    fn message_mut(&mut self, id: &str, make: fn(Message) -> Entry) -> (usize, &mut Message) {
        let found = self.entries.iter().rposition(|entry| {
            matches!(entry, Entry::User(message) | Entry::Agent(message) | Entry::Thought(message) if message.id == id)
        });
        let at = found.unwrap_or_else(|| {
            self.entries.push(make(Message {
                id: id.to_owned(),
                text: String::new(),
            }));
            self.entries.len() - 1
        });
        match &mut self.entries[at] {
            Entry::User(message) | Entry::Agent(message) | Entry::Thought(message) => (at, message),
            _ => unreachable!("entry {at} was found or made as a message"),
        }
    }

    fn append_message(&mut self, id: &str, text: &str, make: fn(Message) -> Entry) -> usize {
        let (at, message) = self.message_mut(id, make);
        message.text.push_str(text);
        at
    }

    /// The tool call `id` and its index, appending a pending one if it is new.
    fn tool_mut(&mut self, id: &str) -> (usize, &mut ToolCall) {
        let found = self
            .entries
            .iter()
            .rposition(|entry| matches!(entry, Entry::Tool(tool) if tool.id == id));
        let at = found.unwrap_or_else(|| {
            self.entries.push(Entry::Tool(ToolCall {
                id: id.to_owned(),
                title: String::new(),
                kind: ToolKind::Other,
                status: ToolStatus::Pending,
                content: Vec::new(),
            }));
            self.entries.len() - 1
        });
        match &mut self.entries[at] {
            Entry::Tool(tool) => (at, tool),
            _ => unreachable!("entry {at} was found or made as a tool call"),
        }
    }

    fn update_tool(&mut self, update: ToolCallUpdate) -> usize {
        use rho_protocol::{ToolKind as Kind, ToolStatus as Status};

        let (at, tool) = self.tool_mut(update.id.as_str());
        if let Some(title) = update.title {
            tool.title = title;
        }
        if let Some(kind) = update.kind {
            tool.kind = match kind {
                Kind::Read => ToolKind::Read,
                Kind::Edit | Kind::Delete | Kind::Move => ToolKind::Edit,
                Kind::Search => ToolKind::Search,
                Kind::Execute => ToolKind::Execute,
                Kind::Think => ToolKind::Think,
                Kind::Fetch => ToolKind::Fetch,
                _ => ToolKind::Other,
            };
        }
        if let Some(status) = update.status {
            tool.status = match status {
                Status::Pending => ToolStatus::Pending,
                Status::InProgress => ToolStatus::Running,
                Status::Completed => ToolStatus::Done,
                Status::Failed => ToolStatus::Failed,
                _ => tool.status,
            };
        }
        if let Some(content) = update.content {
            tool.content = content.into_iter().filter_map(tool_content).collect();
        }
        at
    }
}

#[cfg(test)]
impl Session {
    /// A session past its attach response, for tests that only feed events.
    pub(crate) fn following(id: SessionId) -> Self {
        let mut session = Self::new(id);
        session.held = None;
        session
    }
}

/// A steer's ID, from [`crate::Daemon::steer`]; pass it to
/// [`Session::steer_sent`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SteerId(pub(crate) String);

/// Millionths of a US dollar as dollars, for display.
fn usd(micros: u64) -> f64 {
    #[expect(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "a displayed amount: f64 holds every whole micro-dollar up to $9 billion"
    )]
    let micros = micros as f64;
    micros / 1e6
}

/// Text for one content block. Non-text blocks become short placeholders.
fn block_text(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text { text } | ContentBlock::Resource { text, .. } => text.clone(),
        ContentBlock::Image { .. } => "[image]".to_owned(),
        ContentBlock::ResourceLink { name, .. } => format!("[{name}]"),
        _ => String::new(),
    }
}

fn tool_content(content: rho_protocol::ToolContent) -> Option<ToolContent> {
    match content {
        rho_protocol::ToolContent::Text { text } => Some(ToolContent::Text(text)),
        rho_protocol::ToolContent::Image { .. } => Some(ToolContent::Text("[image]".to_owned())),
        rho_protocol::ToolContent::Diff {
            path,
            old_text,
            new_text,
            deleted,
        } => {
            let new_text = (!deleted).then_some(new_text.as_str());
            let patch = git_patch(&path, old_text.as_deref(), new_text);
            Some(ToolContent::Diff { path, patch })
        }
        _ => None,
    }
}

/// The change from `old` to `new` as a one-file patch in git format, which
/// is what diff viewers read. No `old` is a new file; no `new`, a deleted
/// one.
fn git_patch(path: &std::path::Path, old: Option<&str>, new: Option<&str>) -> String {
    let name = path.to_string_lossy();
    let name = name.trim_start_matches('/');
    let (before, after) = (format!("a/{name}"), format!("b/{name}"));
    let from = old.map_or("/dev/null", |_| before.as_str());
    let to = new.map_or("/dev/null", |_| after.as_str());
    let diff = similar::TextDiff::from_lines(old.unwrap_or_default(), new.unwrap_or_default());
    let hunks = diff.unified_diff().header(from, to).to_string();
    format!("diff --git {before} {after}\n{hunks}")
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
