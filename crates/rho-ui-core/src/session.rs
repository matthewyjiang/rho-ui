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

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use rho_protocol::{
    AnswerStop, CardTone, JobState, Resolution, SessionAttachResponse, SessionEvent,
    SessionOptions, ToolCallUpdate, TurnOutcome,
};

use crate::{
    background::{BackgroundKind, BackgroundStatus, BackgroundWork},
    daemon::SessionId,
    input::{InputId, InputRequest},
    options::{commands, config_options, CommandInfo, ConfigOption},
    prompt::{Attachment, Prompt},
    tool_card::ToolCard,
};

/// An opaque event for one session. Only [`Session::apply`] reads it.
#[derive(Debug)]
pub struct SessionUpdate(pub(crate) SessionEvent);

/// What `rho/session/new` returned. Hand it to [`Session::from_setup`].
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSetup {
    pub(crate) id: SessionId,
    pub(crate) options: SessionOptions,
    pub(crate) worktree: Option<Worktree>,
}

impl SessionSetup {
    /// The session this setup belongs to.
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// The worktree the daemon added for the session, for one opened in
    /// [`Checkout::NewWorktree`].
    pub fn worktree(&self) -> Option<&Worktree> {
        self.worktree.as_ref()
    }
}

/// Where a new session works.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Checkout {
    /// In its workspace, as it is.
    #[default]
    Local,
    /// In a git worktree the daemon adds for it, on a new branch from the
    /// repository's `HEAD`, so sessions in one repository never write the
    /// same files. The daemon removes the worktree once it is clean and
    /// merged, and keeps the branch.
    NewWorktree,
}

/// A git worktree the daemon added for a session to work in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub(crate) branch: String,
    pub(crate) path: PathBuf,
}

impl Worktree {
    /// The branch checked out in it, new when the session opened.
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// Its root directory, under the rho home.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// What `rho/session/attach` returned. Hand it to [`Session::attached`].
#[derive(Debug)]
pub struct Attached(pub(crate) Box<SessionAttachResponse>);

impl Attached {
    /// The directory the session works in: in its worktree, for one that
    /// has one.
    pub fn workspace(&self) -> &Path {
        &self.0.workspace
    }

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
    card: Option<ToolCard>,
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

    /// Output and file changes, in order. The output is what the model
    /// read; [`Self::card`] says what to show a person instead.
    pub fn content(&self) -> &[ToolContent] {
        &self.content
    }

    /// How the call looks, once the daemon sent its card. A call cut off
    /// before it finished keeps its last one.
    pub fn card(&self) -> Option<&ToolCard> {
        self.card.as_ref()
    }

    /// The call in one line: its card's header, or else its title.
    pub fn label(&self) -> &str {
        self.card
            .as_ref()
            .map(ToolCard::header)
            .filter(|header| !header.is_empty())
            .unwrap_or(&self.title)
    }
}

/// What the user sent: Markdown text and what was attached to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserMessage {
    id: String,
    prompt: Prompt,
}

impl UserMessage {
    /// The daemon's ID for the message.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Its text, which may be empty when it is only attachments.
    pub fn text(&self) -> &str {
        self.prompt.text()
    }

    /// Its images and files, in order.
    pub fn attachments(&self) -> &[Attachment] {
        self.prompt.attachments()
    }
}

/// Text from the agent, as Markdown.
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
    /// The notice a card shows, titled `title`. Its detail is the first of
    /// `bodies` that isn't empty: the card's Markdown body, then its plain
    /// one.
    fn card(title: String, tone: CardTone, bodies: [String; 2]) -> Self {
        Self {
            level: match tone {
                CardTone::Warning => NoticeLevel::Warning,
                CardTone::Error => NoticeLevel::Error,
                _ => NoticeLevel::Info,
            },
            title,
            description: bodies.into_iter().find(|body| !body.is_empty()),
        }
    }

    /// How loud it is.
    pub fn level(&self) -> NoticeLevel {
        self.level
    }

    /// One line.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// More detail, if any, in Markdown: a card's body, such as the whole
    /// result a subagent returned, which can run long.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

/// One row of the transcript.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// What the user sent.
    User(UserMessage),
    /// The agent's reply.
    Agent(Message),
    /// The agent's visible reasoning.
    Thought(Message),
    /// A tool call.
    Tool(ToolCall),
    /// A notice from the agent.
    Notice(Notice),
    /// The daemon began a turn of its own, as it does when the session's
    /// background work finishes or a subagent asks its parent to act, to
    /// give the model the result. It opens that turn, as a prompt opens one. Only a turn followed live has
    /// one: the daemon's replay of a conversation does not mark them.
    Wake,
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

/// A message held until the turn that could not take it ends.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    prompt: Prompt,
    /// The turn that had ended last when it was sent. Once a later one
    /// ended and none runs, it is due.
    after: Option<TurnId>,
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
    /// Background work, as the daemon last listed it.
    background: Vec<BackgroundWork>,
    /// Background work seen to end, in the order it ended; only ever
    /// appended to. See [`Self::ended_background`].
    ended_background: Vec<BackgroundWork>,
    /// Background work an earlier run of the client saw running, until the
    /// attachment catches up; see [`Self::remember_background`].
    remembered_running: Vec<BackgroundWork>,
    /// A message the turn could not take; see [`Self::hold`].
    held: Option<Held>,
    /// Steers a turn dropped when it ended; see [`Self::take_dropped_steers`].
    dropped_steers: Vec<String>,
    /// IDs of steers that reached the conversation and that
    /// [`Self::steer_sent`] has not recorded. One sent from here lands in
    /// it when its event beats the daemon's answer to the steer; another
    /// client's steers land in it too, and stay. Emptied when the next
    /// turn starts, by when every answer to this turn's steers is in.
    early_steers: Vec<SteerId>,
    /// The turn that last ended, so [`Self::turn_began`] can tell an answer
    /// that arrives after its whole turn.
    ended_turn: Option<TurnId>,
    /// A turn began live and nothing of it that [`opening`] reads has
    /// arrived yet: its prompt opens it, or else an [`Entry::Wake`] does.
    unopened_turn: bool,
    /// The directory the session works in, once attached. Diffs name their
    /// files relative to it.
    workspace: Option<PathBuf>,
    sync: Sync,
}

/// How far the session's attachment has come.
#[derive(Clone, Debug)]
enum Sync {
    /// The attach has not answered. The events that arrive are kept until
    /// it does.
    Attaching(Vec<SessionEvent>),
    /// The attach answered, and this many events of its replay are still
    /// to come.
    Replaying(u64),
    /// Every event is something happening now.
    Live,
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
            background: Vec::new(),
            ended_background: Vec::new(),
            remembered_running: Vec::new(),
            held: None,
            dropped_steers: Vec::new(),
            early_steers: Vec::new(),
            ended_turn: None,
            unopened_turn: false,
            workspace: None,
            sync: Sync::Attaching(Vec::new()),
        }
    }

    /// The session `rho/session/new` opened, with the settings it answered
    /// with, so they show before the session's attachment answers. Like
    /// [`Self::new`], it holds its events until [`Self::attached`].
    pub fn from_setup(setup: SessionSetup) -> Self {
        let mut session = Self::new(setup.id);
        session.set_options(setup.options);
        session
    }

    /// Records the attach response, then applies the events held for it.
    /// The transcript may have grown; nothing in it changed before this.
    pub fn attached(&mut self, attached: Attached) {
        let response = *attached.0;
        self.workspace = Some(response.workspace);
        if response.title.is_some() {
            self.title = response.title;
        }
        self.set_options(response.options);
        self.settle(response.state);
        for request in response.inputs {
            self.open_input(InputRequest::from_wire(request));
        }
        self.set_background(response.background);
        let replay = match response.replay_events {
            0 => Sync::Live,
            events => Sync::Replaying(events),
        };
        let caught_up = matches!(replay, Sync::Live);
        let held = std::mem::replace(&mut self.sync, replay);
        // The events held so far all came after the response.
        if caught_up {
            self.settle_remembered();
        }
        if let Sync::Attaching(held) = held {
            for event in held {
                self.apply_event(event);
            }
        }
    }

    /// Background work an earlier run of the client saw, given before the
    /// attach: what had ended, in the order it ended, as
    /// [`Self::ended_background`] listed it, and what still ran. What ended
    /// starts [`Self::ended_background`]. What still ran and the daemon no
    /// longer lists once the attachment caught up ended while the client was
    /// away, and joins it as [`BackgroundStatus::Ended`].
    pub fn remember_background(&mut self, seen: impl IntoIterator<Item = BackgroundWork>) {
        debug_assert!(
            matches!(self.sync, Sync::Attaching(_)),
            "background work is remembered before the attach"
        );
        for work in seen {
            if work.status() == BackgroundStatus::Running {
                self.remembered_running.push(work);
            } else {
                self.ended_background.push(work);
            }
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

    /// Marks a prompt that the daemon refused before its turn began. Only
    /// for a session at rest: a refusal says nothing about a turn another
    /// client has under way.
    pub fn fail(&mut self, error: String) {
        self.state = RunState::Failed(error);
    }

    /// Records that the daemon took a prompt and began `turn`. The turn's
    /// own events arrive on another path, before or after this: if none
    /// has yet, the session is running from now, and if the turn already
    /// ended, nothing changes.
    pub fn turn_began(&mut self, turn: &TurnId) {
        if self.ended_turn.as_ref() != Some(turn) && !self.state.is_busy() {
            self.state = RunState::Running;
        }
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
        !matches!(self.sync, Sync::Live)
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

    /// The session's background work: what still runs, and what ended
    /// whose result the model has yet to be given, with how it ended.
    pub fn background(&self) -> &[BackgroundWork] {
        &self.background
    }

    /// Background work this session has seen end, with how it ended, in
    /// the order it ended, after what [`Self::remember_background`] gave.
    /// Unlike [`Self::background`], it keeps work the daemon stopped
    /// listing once the model had its result, so how each piece ended can
    /// still be looked up. Work seen running that the daemon stopped
    /// listing without saying how it ended is here as
    /// [`BackgroundStatus::Ended`]; work nobody here saw run, as work
    /// started and ended between two attaches, is not. Only ever appended
    /// to.
    pub fn ended_background(&self) -> &[BackgroundWork] {
        &self.ended_background
    }

    /// The turn that ended last, as far as this session has heard. Take it
    /// when sending a message, for [`Self::hold`].
    pub fn ended_turn(&self) -> Option<&TurnId> {
        self.ended_turn.as_ref()
    }

    /// Whether a turn runs now, or one ended after `ended`, an earlier
    /// [`Self::ended_turn`].
    pub fn has_turn_since(&self, ended: Option<&TurnId>) -> bool {
        self.state.is_busy() || self.ended_turn.as_ref() != ended
    }

    /// Holds `prompt`, a message sent while a turn was under way that the
    /// turn refused, or that a steer can't carry (attachments), or that
    /// would overtake one already held. It goes as a prompt once that turn ends;
    /// see [`Self::take_due`].
    /// `sent_after` is [`Self::ended_turn`] as it was when `prompt` was
    /// sent: the refusing turn is a later one, which may have ended already,
    /// or whose events may not have arrived yet. A second message joins the
    /// first.
    pub fn hold(&mut self, prompt: Prompt, sent_after: Option<TurnId>) {
        self.held = Some(match self.held.take() {
            Some(mut held) => {
                held.prompt.append(prompt);
                held
            }
            None => Held {
                prompt,
                after: sent_after,
            },
        });
    }

    /// The held message, if any.
    pub fn held(&self) -> Option<&Prompt> {
        self.held.as_ref().map(|held| &held.prompt)
    }

    /// Takes the held message back, as when the user withdraws it.
    pub fn take_held(&mut self) -> Option<Prompt> {
        self.held.take().map(|held| held.prompt)
    }

    /// Takes the held message once it is due: a turn ended after it was
    /// sent, and none runs.
    pub fn take_due(&mut self) -> Option<Prompt> {
        let due = self
            .held
            .as_ref()
            .is_some_and(|held| !self.state.is_busy() && self.ended_turn != held.after);
        if due {
            self.take_held()
        } else {
            None
        }
    }

    /// What the session waits on a person for, oldest first.
    pub fn inputs(&self) -> &[InputRequest] {
        &self.inputs
    }

    /// Takes request `id` out, as the UI does once the daemon took its
    /// answer. The daemon's own word that it closed is then a no-op.
    pub fn close_input(&mut self, id: InputId) -> Option<InputRequest> {
        let at = self.inputs.iter().position(|request| request.id() == id)?;
        Some(self.inputs.remove(at))
    }

    /// Adds an open request, replacing one of the same ID: an attach
    /// response and its replay may both carry it.
    fn open_input(&mut self, request: InputRequest) {
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
        if let Some(at) = self.early_steers.iter().position(|early| *early == id) {
            self.early_steers.swap_remove(at);
        } else if self.state.is_busy() {
            self.steers.push(Steer { id, text });
        } else {
            // The turn ended before this answer arrived, and with it the
            // steer the model never read.
            self.dropped_steers.push(text);
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
        match &mut self.sync {
            Sync::Attaching(held) => {
                held.push(update.0);
                None
            }
            Sync::Replaying(_) | Sync::Live => self.apply_event(update.0),
        }
    }

    fn apply_event(&mut self, event: SessionEvent) -> Option<usize> {
        let mut caught_up = false;
        if let Sync::Replaying(left) = &mut self.sync {
            *left -= 1;
            if *left == 0 {
                self.sync = Sync::Live;
                caught_up = true;
            }
        }
        let changed = self.apply_change(event);
        if caught_up {
            self.settle_remembered();
        }
        changed
    }

    /// Settles the background work an earlier run saw running, once the
    /// attachment caught up: what the daemon no longer lists, and the
    /// replay did not show end, ended unseen.
    fn settle_remembered(&mut self) {
        for work in std::mem::take(&mut self.remembered_running) {
            self.lose(&work);
        }
    }

    /// Adds `work`, seen running and no longer listed, to the ended work as
    /// [`BackgroundStatus::Ended`], unless how it ended is known.
    fn lose(&mut self, work: &BackgroundWork) {
        let known = self
            .background
            .iter()
            .chain(&self.ended_background)
            .any(|seen| seen.id() == work.id());
        if !known {
            self.ended_background.push(work.ended_unseen());
        }
    }

    /// [`Self::apply_event`] once the attachment's place in the replay is
    /// counted.
    fn apply_change(&mut self, event: SessionEvent) -> Option<usize> {
        self.open_turn(&event);
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
                Some(self.put_user(UserMessage {
                    id: message.as_str().to_owned(),
                    prompt: Prompt::from_wire(&content),
                }))
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
                title,
                markdown,
                plain,
                tone,
                ..
            } => Some(self.push_notice(Notice::card(title, tone, [markdown, plain]))),
            SessionEvent::State { state, turn } => {
                if !state.in_progress() {
                    // A turn the daemon's restart cut short ends here,
                    // with no `turn_ended` before it.
                    if let Some(turn) = &turn {
                        self.ended_turn = Some(TurnId::from_wire(turn));
                    }
                }
                self.settle(state);
                None
            }
            SessionEvent::TurnEnded { turn, outcome } => {
                self.ended_turn = Some(TurnId::from_wire(&turn));
                self.end_turn(outcome);
                None
            }
            SessionEvent::Plan { entries } => {
                self.plan = entries.into_iter().map(plan_item).collect();
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
            event @ (SessionEvent::InputRequested { .. }
            | SessionEvent::InputResolved { .. }
            | SessionEvent::InputHeld { .. }) => self.track_input(event),
            SessionEvent::Background { work } => {
                self.set_background(work);
                None
            }
            // Compaction and provider progress, and cache warming, are not
            // shown yet.
            _ => None,
        }
    }

    /// Follows the session's open requests: one opened, closed, or held,
    /// which drops its timeout. A request nobody answered by its deadline
    /// leaves a notice of the answer rho gave, the entry this returns: the
    /// user may have been about to answer it.
    fn track_input(&mut self, event: SessionEvent) -> Option<usize> {
        match event {
            SessionEvent::InputRequested { request } => {
                self.open_input(InputRequest::from_wire(*request));
            }
            SessionEvent::InputResolved { id, resolution } => {
                let closed = self.close_input(InputId(id.0))?;
                let timeout = closed
                    .timeout()
                    .filter(|_| resolution == Resolution::TimedOut)?;
                return Some(self.push_notice(Notice {
                    level: NoticeLevel::Warning,
                    title: format!(
                        "No answer came in time, so rho answered “{}”",
                        timeout.fallback()
                    ),
                    description: Some(closed.title().to_owned()),
                }));
            }
            SessionEvent::InputHeld { id } => {
                let held = self
                    .inputs
                    .iter_mut()
                    .find(|open| open.id() == InputId(id.0));
                if let Some(request) = held {
                    request.hold();
                }
            }
            _ => {}
        }
        None
    }

    /// Opens the turn that began live, if `event` is the first of it that
    /// [`opening`] reads: with an [`Entry::Wake`], unless it is the prompt.
    fn open_turn(&mut self, event: &SessionEvent) {
        if let Some(opening) = opening(event) {
            if std::mem::take(&mut self.unopened_turn) && opening == Opening::Wake {
                self.entries.push(Entry::Wake);
            }
        }
    }

    fn set_background(&mut self, work: Vec<rho_protocol::BackgroundWork>) {
        let listed = work.into_iter().map(BackgroundWork::from_wire).collect();
        let before = std::mem::replace(&mut self.background, listed);
        // A kind this client doesn't know has no ID to tell it apart from
        // the same work in the next list, so it is left out.
        let ended = self.background.iter().filter(|work| {
            work.status() != BackgroundStatus::Running && work.kind() != BackgroundKind::Other
        });
        for work in ended {
            if !self
                .ended_background
                .iter()
                .any(|seen| seen.id() == work.id())
            {
                self.ended_background.push(work.clone());
            }
        }
        // Work a list drops while it ran ended, and the model had its
        // result, between two lists. A replay's lists are of the past, and
        // replace the attach's newer one, so only live lists say so.
        if matches!(self.sync, Sync::Live) {
            let dropped = before.iter().filter(|work| {
                work.status() == BackgroundStatus::Running && work.kind() != BackgroundKind::Other
            });
            for work in dropped {
                self.lose(work);
            }
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
            JobState::Running | JobState::Queued => {
                if !self.state.is_busy() {
                    self.early_steers.clear();
                    // A turn of the past, as an attach catches up, opens
                    // nothing here.
                    self.unopened_turn = matches!(self.sync, Sync::Live);
                }
                self.state = RunState::Running;
            }
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
                self.unopened_turn = false;
            }
            // Already settled, or a state a newer daemon added.
            _ => {}
        }
    }

    fn end_turn(&mut self, outcome: TurnOutcome) {
        self.unopened_turn = false;
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
    }

    fn push_notice(&mut self, notice: Notice) -> usize {
        self.entries.push(Entry::Notice(notice));
        self.entries.len() - 1
    }

    /// Puts `message` in place of the user message with its ID, or after
    /// the rest if it is new, and returns its index: a user message always
    /// comes whole.
    fn put_user(&mut self, message: UserMessage) -> usize {
        let found = self
            .entries
            .iter()
            .rposition(|entry| matches!(entry, Entry::User(user) if user.id == message.id));
        if let Some(at) = found {
            self.entries[at] = Entry::User(message);
            at
        } else {
            self.entries.push(Entry::User(message));
            self.entries.len() - 1
        }
    }

    /// The agent message `id` and its index, appending `make(empty)` if it
    /// is new. Searches from the end: updates almost always touch the
    /// newest rows.
    fn message_mut(&mut self, id: &str, make: fn(Message) -> Entry) -> (usize, &mut Message) {
        let found = self.entries.iter().rposition(|entry| {
            matches!(entry, Entry::Agent(message) | Entry::Thought(message) if message.id == id)
        });
        let at = found.unwrap_or_else(|| {
            self.entries.push(make(Message {
                id: id.to_owned(),
                text: String::new(),
            }));
            self.entries.len() - 1
        });
        match &mut self.entries[at] {
            Entry::Agent(message) | Entry::Thought(message) => (at, message),
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
                card: None,
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

        let workspace = self.workspace.clone();
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
            tool.content = content
                .into_iter()
                .filter_map(|content| tool_content(content, workspace.as_deref()))
                .collect();
        }
        if let Some(display) = update.display {
            tool.card = Some(ToolCard::from_wire(display));
        }
        at
    }
}

#[cfg(test)]
impl Session {
    /// A session past its attach response, for tests that only feed events.
    pub(crate) fn following(id: SessionId) -> Self {
        let mut session = Self::new(id);
        session.sync = Sync::Live;
        session
    }
}

/// What opens a turn that began live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Opening {
    /// Its prompt, which the daemon echoes as the turn's user message.
    Prompt,
    /// Model output or a steer before any prompt: the daemon began the turn
    /// itself.
    Wake,
}

/// Whether `event` opens the turn under way, and how. A notice or card does
/// not: the daemon may send one before a prompt's echo, as a settings
/// change does.
fn opening(event: &SessionEvent) -> Option<Opening> {
    match event {
        SessionEvent::UserMessage { steer: None, .. } => Some(Opening::Prompt),
        SessionEvent::UserMessage { steer: Some(_), .. }
        | SessionEvent::AgentMessageChunk { .. }
        | SessionEvent::ThoughtChunk { .. }
        | SessionEvent::ToolCall(_) => Some(Opening::Wake),
        _ => None,
    }
}

/// A turn's ID, from [`crate::Daemon::prompt`]; pass it to
/// [`Session::turn_began`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnId(String);

impl TurnId {
    pub(crate) fn from_wire(id: &rho_protocol::TurnId) -> Self {
        Self(id.as_str().to_owned())
    }
}

/// A steer's ID, from [`crate::Daemon::steer`]; pass it to
/// [`Session::steer_sent`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SteerId(pub(crate) String);

fn plan_item(entry: rho_protocol::PlanEntry) -> PlanItem {
    PlanItem {
        text: entry.content,
        status: match entry.status {
            rho_protocol::PlanStatus::InProgress => PlanStatus::InProgress,
            rho_protocol::PlanStatus::Completed => PlanStatus::Done,
            _ => PlanStatus::Pending,
        },
    }
}

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

fn tool_content(
    content: rho_protocol::ToolContent,
    workspace: Option<&Path>,
) -> Option<ToolContent> {
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
            let name = workspace
                .and_then(|workspace| path.strip_prefix(workspace).ok())
                .unwrap_or(&path);
            let patch = git_patch(name, old_text.as_deref(), new_text);
            Some(ToolContent::Diff { path, patch })
        }
        _ => None,
    }
}

/// The change from `old` to `new` as a one-file patch in git format, which
/// is what diff viewers read, naming the file `path`. No `old` is a new
/// file; no `new`, a deleted one.
fn git_patch(path: &Path, old: Option<&str>, new: Option<&str>) -> String {
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
