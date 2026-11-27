//! One session: a title bar naming its project and the sessions above it,
//! the transcript, what a turn asks (approvals and questions), and the
//! composer with the session settings.
//! Owns the session's [`Session`] state and every request the user makes in
//! it.
//!
//! A subagent's session opens from its row in the background work, nested
//! under the session that started it. Only the subagent writes to it, so
//! its view only watches: no composer, no approvals (the subagent's
//! questions go to its parent), and no stopping its work. Where the
//! composer would be, it names the agent and model the subagent runs as.
//!
//! While a turn runs, whoever started it, the composer steers it: Enter adds
//! the text to the turn's next model request, and it waits above the box
//! until the model reads it. A message the turn can't take, as one sent
//! just as the daemon began a turn of its own to deliver background work,
//! waits above the box too, and goes as a prompt once that turn ends. So
//! does a message with attachments, as a steer carries only text, and any
//! message after one waiting, which would otherwise overtake it.
//!
//! A side chat is a fork of the session (`rho/session/fork`), started with
//! `/side` or `/btw` in the composer, or from the right panel. It shows in
//! a tab of the panel as a view of its own, with no title bar: the fork's
//! conversation, and a composer that writes to the fork alone. Closing its
//! tab winds it down, then deletes it ([`SessionView::discard`]).
//!
//! Text selected in the transcript gets an "Add to prompt" button where the
//! drag ended, which quotes it above the composer's text box; see
//! [`ComposerState::add_quote`].
//!
//! The view lives as long as the session is open, across the connection
//! that follows it: when the daemon stops sending the session's events, as
//! when the view fell behind, the daemon restarted, or the app let the
//! session go while it was off screen, the app follows it again on the
//! same view ([`SessionView::reattach`]), so the composer, the scroll
//! position and what is unfolded stay. A daemon that replays the whole
//! conversation instead of what came since keeps the rows on screen until
//! the replay is all there, then builds them again from the first entry
//! that changed or holds a `ui` surface, whose panel the replay made again
//! ([`Loading::Replaying`]): only a change above it moves the scroll. A prompt whose try reached a
//! daemon that was stopping, or a connection that ended, goes again under
//! its key once the session is followed again, so it runs once.
//!
//! The session's running background work is listed above the box, with a
//! button to stop each piece; what ended is behind a button in the title
//! bar. The view records the work as it starts and ends, and a session
//! opened again after a restart lists what ended from that record (see
//! [`crate::background_log`]).

mod find_bar;
mod side_chat;

use std::{rc::Rc, time::Duration};

use web_time::Instant;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::base::{Positioner, TextSelection};
use gpui_kit::component::{
    alert::Alert,
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia},
    h_flex,
    input::{InputEvent, Search},
    message_scroller::{MessageScroller, MessageScrollerState},
    spinner::Spinner,
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _, ThemeStyled as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    deferred, div, point, px, rems, Anchor, AnyElement, App, AppContext as _, Context, Entity,
    EventEmitter, FocusHandle, Focusable as _, InteractiveElement as _, IntoElement, MouseButton,
    MouseUpEvent, ParentElement as _, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription, Task, WeakEntity,
    Window,
};
use rho_ui_core::{
    surface::SurfaceAction, Answer, Attached, Attachment, BackgroundId, BackgroundStatus,
    Continuity, Cursor, Daemon, DaemonError, Entry, InputId, JobActivity, Prompt, PromptKey,
    PullRequestId, Run, RunState, Session, SessionId, SessionUpdate, SteerId, SurfaceActionError,
    TurnId, Unsteered,
};

use self::find_bar::FindBar;
use self::side_chat::{side_question, Discarding};
use crate::{
    added_dirs::{AddedDirs, DirsAccess},
    auths::Auths,
    background::{
        processes::Processes, BackgroundHistory, BackgroundList, BackgroundSurface, OpenHandler,
        Opens, OpensOutput, Sources, StopHandler,
    },
    background_log::{BackgroundLog, Recorded},
    composer::{Composer, ComposerMode, ComposerState, McpLink},
    input_card::{InputCard, InputCardEvent, Queue},
    leads::Kind,
    machines::{Place, SessionKey},
    mcp_status::McpStatus,
    pictures::{PictureRead, Pictures},
    plan::PlanList,
    pr_mark::pr_pill,
    projects::Projects,
    pull_requests::PullRequests,
    rail,
    right_panel::Visibility,
    session_list::{Activity, LiveSession, NEW_SESSION, UNTITLED},
    session_settings::{model_summary, Availability, SessionSettings, SettingsEvent, Turn},
    sign_in::SignInSlot,
    surface_view::{SurfaceEvent, SurfacePanel, Surfaces},
    title_bar::TitleBar,
    transcript::{find::Direction, ActionHandler, Handlers, ItemContent, Transcript},
    usage::UsageRing,
    work::Disclosure,
    workspaces::{Workspace, WorkspaceRef},
    FindInTranscript, StopTurn,
};

/// How often a running turn's header redraws: it counts whole seconds.
const TICK: Duration = Duration::from_secs(1);

/// What the app needs to hear from a session.
pub(crate) enum SessionEvent {
    /// What the sidebar shows of it changed; see [`SessionView::live`].
    LiveChanged,
    /// The saved session changed (a prompt was accepted or a turn ended);
    /// reload the session list.
    Saved,
    /// Open the session of a subagent this one started, nested under it.
    OpenSubagent {
        session: SessionId,
        /// The subagent's task, its title until its session says.
        title: SharedString,
    },
    /// Open another session on this one's machine, such as one this one
    /// started, from a card about it.
    GoToSession(SessionId),
    /// Show a session above this one, from the title bar.
    Show(WeakEntity<SessionView>),
    /// Start a new session in this one's project, from the title bar.
    NewSession,
    /// Show this session's pull request in the right panel, from the
    /// title bar.
    ShowPullRequest(PullRequestId),
    /// Show this session's Background surface in the right panel, which
    /// shows the output of the process a row opened.
    ShowBackground,
    /// Show this session's context in the right panel, from the context
    /// ring in its composer.
    ShowContext,
    /// Start a side chat of this session, sending the prompt first if
    /// there is one; see [`SessionView::start_side_chat`].
    SideChat(Option<Prompt>),
    /// This side chat's tab closed and its turn has ended: delete it.
    Discard,
}

/// Who writes the session, and where its view sits.
enum Role {
    /// A session the user started or opened, in the main pane.
    Main,
    /// A subagent's session, nested under the session that started it.
    /// Only the subagent writes to it, so the view only watches.
    Subagent(WeakEntity<SessionView>),
    /// A side chat: a fork of the session, shown in a tab of its right
    /// panel.
    Side(WeakEntity<SessionView>),
}

/// How the session's events reach the view, as its machine's connection
/// is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    /// A connection follows the session, or is attaching to it.
    Following,
    /// The connection ended as the daemon restarted in place, and the app
    /// connects again by itself.
    Reconnecting,
    /// The connection ended, and rho-ui can't tell a restart from a stop:
    /// the app connects again only when the user asks.
    Lost,
    /// The daemon refused to follow the session again, as when another
    /// client deleted it meanwhile, in these words. What the view showed
    /// stays; the next connection tries again.
    Refused(String),
}

/// How the app follows a session again, from [`SessionView::reattach`].
pub(crate) struct Follow {
    /// Where its events left off.
    pub(crate) cursor: Option<Cursor>,
    /// A subagent's session is observed, not attached to.
    pub(crate) observe: bool,
}

/// A prompt on its way to the daemon.
enum Outgoing {
    /// Sent; the daemon has yet to answer.
    Sent,
    /// Its try reached a daemon that was stopping, or a connection that
    /// ended, so its turn may or may not have begun. It goes again under
    /// the same key once an attach answers, which the daemon then takes
    /// as the same prompt.
    Waiting { prompt: Prompt, key: PromptKey },
}

/// Whether the session is usable yet.
pub(crate) enum Loading {
    /// A new session, or a saved one whose replay has caught up.
    Ready,
    /// The attach to a saved session has not answered yet, or its replay
    /// has not caught up: its rows wait for the whole replay. Only
    /// `after_change` ends it, which the event that catches up and the
    /// attach's answer both reach.
    Attaching,
    /// The daemon replays the whole conversation to a session already
    /// shown, as when it unloaded the session while the app let it go.
    /// Until the replay catches up, the rows stay as they were, drawn from
    /// the entries they were built from, which this holds; then
    /// `after_change` builds them again from the first entry that changed.
    Replaying(Vec<Entry>),
    /// The attach failed.
    Failed(String),
}

impl Loading {
    /// The entries of `session` the rows were built from, once there are
    /// rows: while the daemon replays the whole conversation over what was
    /// shown, those it showed.
    fn drawn<'a>(&'a self, session: &'a Session) -> Option<&'a [Entry]> {
        match self {
            Self::Ready => Some(session.entries()),
            Self::Replaying(shown) => Some(shown),
            Self::Attaching | Self::Failed(_) => None,
        }
    }
}

pub(crate) struct SessionView {
    daemon: Daemon,
    session: Session,
    /// The session's directory, on its machine.
    cwd: Place,
    /// The workspace and branch, for the header.
    workspace: WorkspaceRef,
    role: Role,
    /// How far a side chat whose tab closed is from being deleted.
    discarding: Discarding,
    /// The daemon stopped sending an observed session's events, as when a
    /// subagent's run ends: what it shows is all there is until it is
    /// observed again.
    unloaded: bool,
    loading: Loading,
    transcript: Transcript,
    composer: Entity<ComposerState>,
    settings: Entity<SessionSettings>,
    /// The sign-in its model's auth profile needs, while it is signed out;
    /// see [`Self::with_sign_in`].
    sign_in: Option<SignInSlot>,
    /// The directories the session reaches besides its workspace, in the
    /// title bar.
    added_dirs: Entity<AddedDirs>,
    /// Where the title bar reads the session's pull request, redrawn as
    /// it changes; see [`Self::with_pull_requests`].
    prs: Option<(Entity<PullRequests>, Subscription)>,
    /// The oldest open request, while there is one, and its events.
    card: Option<(Entity<InputCard>, Subscription)>,
    /// A prompt on its way to the daemon.
    outgoing: Option<Outgoing>,
    /// A side chat's first prompt, sent once the replay of the conversation
    /// it forked has caught up, so its turn is the newest.
    opening: Option<Prompt>,
    /// How the session's events reach the view.
    link: Link,
    /// How many attaches have answered: a prompt's try that failed as the
    /// connection ended goes again at once if one answered since it went.
    attachments: u64,
    /// Background work the user asked to stop that still runs.
    stopping: Vec<BackgroundId>,
    /// The subagent and workflow runs the session started, as the daemon's
    /// job list last said; see [`Self::set_runs`].
    runs: Vec<Run>,
    /// The session's background processes as the daemon reads them, and
    /// the output the user follows.
    processes: Entity<Processes>,
    /// What the background log holds of the session's work.
    recorded: Recorded,
    /// The first prompt was accepted, so the session is saved.
    prompted: bool,
    /// When the running turn started.
    busy_since: Option<Instant>,
    /// When the turn under way started, for a session attached mid-turn:
    /// the daemon says how long it had run.
    turn_began: Option<Instant>,
    /// When the oldest open request arrived.
    waiting_since: Option<Instant>,
    /// What the sidebar was last told in [`SessionEvent::LiveChanged`];
    /// `None` once the view follows the session again, so the app hears of
    /// it when it has caught up, whatever changed.
    reported: Option<LiveSession>,
    /// The kind of session the composer's placeholder and the added
    /// directories are worded for.
    kind: Kind,
    /// A request failed outside a turn, such as a settings change.
    error: Option<String>,
    /// The surfaces the session's `ui` calls showed.
    surfaces: Surfaces,
    /// Whether the agent's todo list above the composer is open.
    plan: Disclosure,
    /// Text selected in the transcript, offered to the composer as a quote.
    selected: Option<Selected>,
    /// The find bar over the transcript, while open.
    find: Option<FindBar>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
    /// What the title bar reads besides the session.
    title_bar: TitleBarSources,
    /// The session's MCP servers, once it is loaded, for the mark in the
    /// composer's footer; a subagent's session, with no composer, has none.
    mcp: Option<Entity<McpStatus>>,
    /// Redraws "Working for" while a turn runs.
    _tick: Task<()>,
}

/// What a session's title bar reads besides the session.
#[derive(Default)]
struct TitleBarSources {
    /// The user's projects, which say which of a plain session's added
    /// directories it names; see [`SessionView::with_projects`]. `None`
    /// for a subagent's view, which names its root session's.
    projects: Option<Entity<Projects>>,
    /// What redraws it: for a subagent's view, the sessions above it, as
    /// they are renamed, and its root's projects; else the projects.
    _redraws: Vec<Subscription>,
}

impl EventEmitter<SessionEvent> for SessionView {}

/// Text selected in the transcript, and where its drag ended.
struct Selected {
    text: String,
    /// Where the pointer let go, in window coordinates: the button floats
    /// above it.
    at: Point<Pixels>,
}

impl SessionView {
    /// A saved session starts as [`Loading::Attaching`] until its replay
    /// has caught up.
    pub(crate) fn new(
        daemon: Daemon,
        mut session: Session,
        workspace: WorkspaceRef,
        remote: Option<SharedString>,
        loading: Loading,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let cwd = workspace.read(cx).place().clone();
        let pictures = cx.new(|_| Pictures::files(cwd.clone(), workspace.table().clone()));
        let transcript = Transcript::new(pictures, cx);
        // A session about to attach may have run background work in an
        // earlier run of the app, which the daemon may no longer list.
        let recorded = if matches!(loading, Loading::Attaching) {
            let key = SessionKey::new(cwd.machine.clone(), session.id().clone());
            cx.global::<BackgroundLog>().remember(&key, &mut session)
        } else {
            Recorded::default()
        };
        let composer = cx.new(|cx| ComposerState::new(remote, window, cx));
        let input = composer.read(cx).input().clone();
        let settings = cx.new(|cx| {
            SessionSettings::new(
                daemon.clone(),
                session.id().clone(),
                session.options(),
                window,
                cx,
            )
        });
        // Listed only, until [`Self::with_projects`] lets the user add.
        let added_dirs = AddedDirs::of_session(
            &daemon,
            &session,
            &workspace,
            DirsAccess::ListOnly,
            Kind::Session,
            window,
            cx,
        );
        let processes = cx.new(|_| Processes::new(daemon.clone(), session.id().clone()));
        let subscriptions =
            vec![
                cx.subscribe_in(&settings, window, Self::on_settings_event),
                // The rows say what the daemon read of each process.
                cx.observe(&processes, |_, _, cx| cx.notify()),
                // The jump-to-latest button follows the scroll position.
                cx.observe(transcript.scroller(), Self::scroller_changed),
                // A picture a message shows was read.
                cx.subscribe(transcript.pictures(), |this, _, read: &PictureRead, cx| {
                    this.transcript.picture_read(read, cx);
                    cx.notify();
                }),
                // The title bar shows the project, which can change.
                cx.observe(workspace.table(), |_, _, cx| cx.notify()),
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
                        InputEvent::Change => cx.notify(),
                        _ => {}
                    }
                }),
                // Attachments added or removed.
                cx.observe(&composer, |_, _, cx| cx.notify()),
            ];
        let mut view = Self {
            daemon,
            session,
            cwd,
            workspace,
            role: Role::Main,
            discarding: Discarding::No,
            unloaded: false,
            // A resumed session is already saved.
            prompted: matches!(loading, Loading::Attaching),
            loading,
            transcript,
            composer,
            settings,
            sign_in: None,
            added_dirs,
            prs: None,
            card: None,
            outgoing: None,
            opening: None,
            link: Link::Following,
            attachments: 0,
            stopping: Vec::new(),
            runs: Vec::new(),
            processes,
            recorded,
            busy_since: None,
            turn_began: None,
            waiting_since: None,
            reported: None,
            kind: Kind::Session,
            error: None,
            surfaces: Surfaces::default(),
            plan: Disclosure::default(),
            selected: None,
            find: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
            title_bar: TitleBarSources::default(),
            mcp: None,
            _tick: Self::tick(cx),
        };
        view.transcript.sync(view.session.entries(), cx);
        view.sync_settings(window, cx);
        view.sync_kind(window, cx);
        let link = view.mcp_link();
        view.composer
            .update(cx, |composer, _| composer.set_mcp(link));
        view.reported = Some(view.live());
        view
    }

    /// Redraws "Working for" every [`TICK`] while a turn runs.
    fn tick(cx: &Context<'_, Self>) -> Task<()> {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            let tick = this.update(cx, |this, cx| {
                if this.busy_since.is_some() {
                    cx.notify();
                }
            });
            if tick.is_err() {
                break;
            }
        })
    }

    /// The session of a subagent that `parent` started, whose observe has
    /// not answered yet. It works where its parent does, and only lists
    /// its added directories: only its run changes them.
    pub(crate) fn subagent(
        daemon: Daemon,
        session: Session,
        parent: &Entity<SessionView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let (workspace, remote) = {
            let parent = parent.read(cx);
            (parent.workspace.clone(), parent.composer.read(cx).remote())
        };
        // The title bar names every session above this one, and the
        // projects of the one at the top.
        let mut renamed = Vec::new();
        let mut above = Some(parent.clone());
        while let Some(session) = above {
            renamed.push(cx.subscribe(&session, |_, _, event: &SessionEvent, cx| {
                if matches!(event, SessionEvent::LiveChanged) {
                    cx.notify();
                }
            }));
            let (projects, parent) = {
                let session = session.read(cx);
                (session.title_bar.projects.clone(), session.parent())
            };
            if let Some(projects) = projects {
                renamed.push(cx.observe(&projects, |_, _, cx| cx.notify()));
            }
            above = parent;
        }
        let view = Self::new(
            daemon,
            session,
            workspace,
            remote,
            Loading::Attaching,
            window,
            cx,
        );
        Self {
            role: Role::Subagent(parent.downgrade()),
            title_bar: TitleBarSources {
                projects: None,
                _redraws: renamed,
            },
            ..view
        }
    }

    /// A side chat of `parent`: its fork, whose attach has not answered
    /// yet. It works where its parent does, and sends `prompt` once the
    /// attach answers.
    pub(crate) fn side(
        daemon: Daemon,
        session: Session,
        parent: &Entity<SessionView>,
        prompt: Option<Prompt>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let (workspace, remote) = {
            let parent = parent.read(cx);
            (parent.workspace.clone(), parent.composer.read(cx).remote())
        };
        let view = Self::new(
            daemon,
            session,
            workspace,
            remote,
            Loading::Attaching,
            window,
            cx,
        );
        Self {
            role: Role::Side(parent.downgrade()),
            opening: prompt,
            ..view
        }
    }

    /// Lets the user add directories to the session from its title bar,
    /// and offers `projects`, the user's, to add. A subagent's session
    /// never gets this, and a side chat has no title bar.
    pub(crate) fn with_projects(
        self,
        projects: &Entity<Projects>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let added_dirs = AddedDirs::of_session(
            &self.daemon,
            &self.session,
            &self.workspace,
            DirsAccess::Add(projects.clone()),
            self.kind(cx),
            window,
            cx,
        );
        Self {
            added_dirs,
            title_bar: TitleBarSources {
                projects: Some(projects.clone()),
                _redraws: vec![cx.observe(projects, |_, _, cx| cx.notify())],
            },
            ..self
        }
    }

    /// Offers the sign-in the session's model needs, above the composer,
    /// while its auth profile is signed out on its machine, as `auths`
    /// says. A subagent's session has no composer, so it gets none.
    pub(crate) fn with_sign_in(
        mut self,
        auths: Entity<Auths>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let machine = self.cwd.machine.clone();
        self.sign_in = Some(SignInSlot::new(
            auths,
            machine,
            Self::model_auth,
            window,
            cx,
        ));
        self
    }

    /// Marks the session's pull request in its title bar, as `prs` has it.
    /// A subagent's session gets none: the sidebar, which has the PRs of
    /// its sessions listed, doesn't list it.
    pub(crate) fn with_pull_requests(
        mut self,
        prs: Entity<PullRequests>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let redraw = cx.observe(&prs, |_, _, cx| cx.notify());
        self.prs = Some((prs, redraw));
        self
    }

    /// The auth profile the session's model runs on, once known.
    fn model_auth(&self) -> Option<&str> {
        self.session.auth()
    }

    /// The daemon of the session's machine, which changes as the session
    /// is followed again on another connection.
    pub(crate) fn daemon(&self) -> &Daemon {
        &self.daemon
    }

    /// The session, and the machine it is on.
    pub(crate) fn key(&self) -> SessionKey {
        SessionKey::new(self.cwd.machine.clone(), self.session.id().clone())
    }

    /// The session that started this one, or that a side chat forks, while
    /// it is open.
    pub(crate) fn parent(&self) -> Option<Entity<SessionView>> {
        match &self.role {
            Role::Main => None,
            Role::Subagent(parent) | Role::Side(parent) => parent.upgrade(),
        }
    }

    /// Whether this is a subagent's session, which the view only watches.
    pub(crate) fn is_subagent(&self) -> bool {
        matches!(self.role, Role::Subagent(_))
    }

    /// Whether this is a side chat, in the right panel.
    pub(crate) fn is_side(&self) -> bool {
        matches!(self.role, Role::Side(_))
    }

    /// Whether the sidebar lists the session: not a subagent's, nor a side
    /// chat, which show under the session they belong to.
    pub(crate) fn is_listed(&self) -> bool {
        matches!(self.role, Role::Main)
    }

    /// Whether something in the view has the keyboard.
    pub(crate) fn has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
    }

    /// The session at the top of this one's nesting: itself, unless it is
    /// a subagent's or a side chat.
    pub(crate) fn root_id(&self, cx: &App) -> SessionKey {
        match self.parent() {
            Some(parent) => parent.read(cx).root_id(cx),
            None => self.key(),
        }
    }

    /// Whether `ancestor` started this session, or one above it.
    pub(crate) fn descends_from(&self, ancestor: &Entity<SessionView>, cx: &App) -> bool {
        self.parent().is_some_and(|parent| {
            parent == *ancestor || parent.read(cx).descends_from(ancestor, cx)
        })
    }

    /// The daemon stopped sending this observed session's events, so what
    /// it shows may be out of date: observe it again to read it afresh.
    pub(crate) fn unload(&mut self) {
        self.unloaded = true;
    }

    /// Whether [`Self::unload`] was called, for UI tests.
    #[cfg(test)]
    pub(crate) fn is_unloaded(&self) -> bool {
        self.unloaded
    }

    /// Whether the attach failed, so the view shows only why.
    pub(crate) fn failed_to_attach(&self) -> bool {
        matches!(self.loading, Loading::Failed(_))
    }

    /// What the sidebar shows for this session.
    pub(crate) fn live(&self) -> LiveSession {
        LiveSession {
            id: self.key(),
            cwd: self.cwd.path.clone(),
            title: self.name().map(SharedString::from),
            placeholder: self.placeholder(),
            activity: self.activity(),
            kind: self.agent_kind(),
            // Empty until the session's options arrive, which isn't none.
            added_dirs: self
                .session
                .options()
                .map(|_| self.session.added_dirs().to_vec()),
        }
    }

    /// The projects the session works in, as its title bar names them: a
    /// subagent's are those of the session it runs under, whose work it
    /// does.
    pub(crate) fn projects<'a>(&self, cx: &'a App) -> Vec<&'a Workspace> {
        if let Some(parent) = self.parent() {
            return parent.read(cx).projects(cx);
        }
        let projects = self.title_bar.projects.as_ref();
        let projects = projects.map(|projects| projects.read(cx));
        let dirs = self.added_dirs.read(cx);
        dirs.projects(Some(self.workspace.index()), projects, cx)
    }

    /// By the agent it runs as, once the session says which: `None` until
    /// then, which [`Self::kind`] takes for a plain session.
    pub(crate) fn agent_kind(&self) -> Option<Kind> {
        self.session.agent().map(Kind::of)
    }

    /// A lead's, if it is a lead or a view above it is: a lead's subagent
    /// or side chat works in the lead's folder, which is in no project. A
    /// session whose agent isn't known yet counts as a plain one.
    pub(crate) fn kind(&self, cx: &App) -> Kind {
        let own = self.agent_kind().unwrap_or(Kind::Session);
        match self.parent() {
            Some(parent) if own == Kind::Session => parent.read(cx).kind(cx),
            _ => own,
        }
    }

    /// rho's title, or until it sends one (rho names a session after its
    /// first turn), the first prompt, as rho lists an untitled session.
    fn name(&self) -> Option<String> {
        self.session
            .title()
            .map(str::to_owned)
            .or_else(|| first_prompt(&self.session))
    }

    fn title(&self) -> String {
        self.name().unwrap_or_else(|| self.placeholder().to_owned())
    }

    /// The title until rho gives one: a saved session is untitled.
    fn placeholder(&self) -> &'static str {
        if self.prompted {
            UNTITLED
        } else {
            NEW_SESSION
        }
    }

    /// Where the keyboard goes when the session is shown: its composer, or
    /// for a subagent's, which has none, the view.
    pub(crate) fn focus_target(&self, cx: &App) -> FocusHandle {
        if self.is_subagent() {
            self.focus_handle.clone()
        } else {
            self.composer.focus_handle(cx)
        }
    }

    fn is_busy(&self) -> bool {
        self.outgoing.is_some() || self.session.state().is_busy()
    }

    fn activity(&self) -> Activity {
        // What the view knows of the session is stale until a connection
        // follows it again.
        if matches!(self.link, Link::Lost | Link::Refused(_)) {
            return Activity::Idle;
        }
        let running = || {
            self.session
                .background()
                .iter()
                .any(|work| work.status() == BackgroundStatus::Running)
        };
        match (self.waiting_since, self.busy_since) {
            (Some(since), _) => Activity::Waiting(since),
            (None, Some(since)) => Activity::Working(since),
            (None, None) if running() => Activity::Background,
            (None, None) => Activity::Idle,
        }
    }

    pub(crate) fn apply(
        &mut self,
        update: SessionUpdate,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let asked = !self.session.inputs().is_empty();
        let changed = self.session.apply(update);
        let building = matches!(self.loading, Loading::Ready);
        // A saved session's history goes into the session alone, with no
        // redraw per event; the event that catches up builds the
        // transcript once, in `after_change`.
        if self.session.is_replaying() && !building {
            return;
        }
        // The session names the entry it changed; only its row is remeasured.
        // A new entry gets its row in `after_change`.
        if let Some(ix) = changed.filter(|_| building) {
            self.transcript.changed(self.session.entries(), ix, cx);
        }
        // A turn that ended before reading a steer dropped it.
        let dropped = self.session.take_dropped_steers();
        if !dropped.is_empty() {
            self.give_back(dropped.join("\n").into(), window, cx);
        }
        self.after_change(window, cx);
        if !asked {
            self.focus_new_card(window, cx);
        }
    }

    /// Records the answer to the attach.
    pub(crate) fn attached(
        &mut self,
        result: Result<Attached, DaemonError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let asked = !self.session.inputs().is_empty();
        let answered = result.is_ok();
        match result {
            Ok(attached) => {
                // A turn this client began meanwhile is already timed.
                self.turn_began = attached
                    .turn_elapsed()
                    .filter(|_| self.busy_since.is_none())
                    .and_then(|elapsed| Instant::now().checked_sub(elapsed));
                // Ready once the replay has caught up; see `after_change`.
                if let Continuity::Replaced(shown) = self.session.attached(attached) {
                    self.replaced(shown);
                }
                self.attachments += 1;
            }
            // The connection ends next; the next one follows the session
            // again.
            Err(DaemonError::Closed | DaemonError::Stopping(_)) => {}
            // Shown before: what it showed, and what was typed, stay.
            Err(error) if self.loading.drawn(&self.session).is_some() => {
                self.link = Link::Refused(error.to_string());
                self.give_back_waiting(window, cx);
                self.report_live(cx);
            }
            Err(error) => {
                self.loading = Loading::Failed(error.to_string());
                self.give_back_waiting(window, cx);
                self.give_back_opening(window, cx);
            }
        }
        self.after_change(window, cx);
        if answered {
            self.send_waiting(window, cx);
        }
        // A question asked before the attach answered arrives with it.
        if !asked {
            self.focus_new_card(window, cx);
        }
    }

    /// Follows the session again on `daemon`, the daemon of its machine
    /// now, as once the machine connected again or the daemon stopped
    /// sending the session's events. Returns how, for the attach or
    /// observe whose answer goes to [`Self::attached`]; `None` for a view
    /// with nothing to follow (see [`Self::follows_again`]).
    pub(crate) fn reattach(
        &mut self,
        daemon: Daemon,
        cx: &mut Context<'_, Self>,
    ) -> Option<Follow> {
        self.link = Link::Following;
        cx.notify();
        if !self.follows_again() {
            return None;
        }
        self.reported = None;
        // The turn under way, if any, is timed again from what the attach
        // says: it may have ended meanwhile, or another begun.
        self.busy_since = None;
        self.turn_began = None;
        self.transcript.forget_turn(cx);
        self.settings
            .update(cx, |settings, _| settings.set_daemon(daemon.clone()));
        self.added_dirs
            .update(cx, |dirs, _| dirs.set_daemon(daemon.clone()));
        self.processes
            .update(cx, |processes, _| processes.set_daemon(daemon.clone()));
        // The daemon loads the session again, starting its servers anew:
        // they are read once it is caught up.
        self.mcp = None;
        self.daemon = daemon;
        let link = self.mcp_link();
        self.composer
            .update(cx, |composer, _| composer.set_mcp(link));
        Some(Follow {
            cursor: self.session.reattach(),
            observe: self.is_subagent(),
        })
    }

    /// Records how the session's connection is doing, from the app. A
    /// prompt waiting for a connection goes back in the composer once none
    /// will come by itself.
    pub(crate) fn set_link(&mut self, link: Link, window: &mut Window, cx: &mut Context<'_, Self>) {
        let lost = link == Link::Lost;
        self.link = link;
        if lost {
            self.give_back_waiting(window, cx);
        }
        self.report_live(cx);
        cx.notify();
    }

    /// Puts the prompt waiting for a connection back in the composer, as
    /// none will send it: the connection is lost, the daemon refused to
    /// follow the session again, or the user stopped it.
    fn give_back_waiting(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        match self.outgoing.take() {
            Some(Outgoing::Waiting { prompt, .. }) => {
                self.give_back(prompt, window, cx);
                self.after_change(window, cx);
            }
            other => self.outgoing = other,
        }
    }

    /// Whether the app should follow the session again once its machine
    /// connects: unless its first attach failed, or it is an observed
    /// session whose events ended for good.
    pub(crate) fn follows_again(&self) -> bool {
        !self.unloaded && !self.failed_to_attach()
    }

    /// Whether the app may stop following the session while it is off
    /// screen, so the daemon can unload it: it is saved, as the daemon
    /// discards an unsaved one nothing follows, its attach has answered and
    /// caught up, and nothing under way needs the daemon's events. No turn
    /// runs or waits on a person, no background work runs or waits for the
    /// daemon's own turn to give the model its result, and no message is
    /// held or on its way. The view stays, with what was typed in it, and
    /// showing it follows the session again. One with nothing to follow
    /// (see [`Self::follows_again`]) has nothing to let go, and a side chat
    /// is followed while its tab is open.
    pub(crate) fn can_let_go(&self) -> bool {
        let session = &self.session;
        !self.is_side()
            && self.prompted
            && self.follows_again()
            && matches!(self.loading, Loading::Ready)
            && !session.is_replaying()
            && !session.state().is_busy()
            && session.inputs().is_empty()
            && session.background().is_empty()
            && session.held().is_none()
            && self.outgoing.is_none()
    }

    /// The replay caught up: a saved session's rows can be built. Rows kept
    /// through a replay of the whole conversation are built again from the
    /// first entry that changed, so where the transcript was scrolled to,
    /// and what was unfolded, stay above it.
    fn caught_up(&mut self, cx: &mut Context<'_, Self>) {
        // The daemon has the session loaded now, so it can say how its MCP
        // servers are doing.
        if self.mcp.is_none() && !self.is_subagent() {
            let (daemon, id) = (self.daemon.clone(), self.session.id().clone());
            let mcp = cx.new(|cx| {
                let mut mcp = McpStatus::new(daemon, id);
                mcp.reload(cx);
                mcp
            });
            self.mcp = Some(mcp);
        }
        match std::mem::replace(&mut self.loading, Loading::Ready) {
            Loading::Ready | Loading::Attaching => {}
            Loading::Replaying(shown) => {
                let from = first_change(&shown, self.session.keep_arrivals(&shown));
                // `from`, not the start that the replay itself reports.
                self.session.take_removed();
                self.transcript.truncate(from, self.session.entries(), cx);
                // The replay placed its surfaces again, for a store of
                // their own.
                self.surfaces = Surfaces::default();
            }
            failed @ Loading::Failed(_) => self.loading = failed,
        }
    }

    /// The daemon replays the whole conversation over what the view
    /// showed, `shown`: its rows stay until it is all there (see
    /// [`Loading::Replaying`]). The composer, and what only this client
    /// knows, stay.
    fn replaced(&mut self, shown: Vec<Entry>) {
        self.loading = match std::mem::replace(&mut self.loading, Loading::Attaching) {
            Loading::Ready => Loading::Replaying(shown),
            // Its rows still draw the entries they were built from, not the
            // part of a replay `shown` holds.
            replaying @ Loading::Replaying(_) => replaying,
            // Nothing was drawn.
            Loading::Attaching | Loading::Failed(_) => {
                self.surfaces = Surfaces::default();
                Loading::Attaching
            }
        };
    }

    /// Moves the keyboard into a card that just opened if the user is in
    /// this session; never steals focus from another one, nor from a
    /// message being typed, whose Enter would then answer instead of send.
    fn focus_new_card(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let typing = !self.composer.read(cx).is_empty(cx);
        // `contains_focused` reads the last painted frame, which a session
        // opened this frame isn't in yet, so its focused composer counts too.
        let here = self.focus_handle.contains_focused(window, cx)
            || self.focus_target(cx).is_focused(window);
        if !typing && here {
            self.focus_card(window, cx);
        }
    }

    /// Puts `prompt` back in the composer, before anything typed or pasted
    /// since.
    fn give_back(&self, prompt: Prompt, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.composer
            .update(cx, |composer, cx| composer.give_back(prompt, window, cx));
    }

    /// Brings the scroller, settings, timers and sidebar up to date with
    /// `session`.
    fn after_change(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let replaying = self.session.is_replaying();
        if !replaying {
            self.caught_up(cx);
        }
        // A saved session's rows are built only once its replay is all
        // there, so a long session opens at its end rather than drawing as
        // it replays. A new one's replay is short, and builds as it comes.
        if matches!(self.loading, Loading::Ready) {
            if let Some(from) = self.session.take_removed() {
                self.transcript.truncate(from, self.session.entries(), cx);
            }
            self.transcript.sync(self.session.entries(), cx);
            self.surfaces.sync(&mut self.session, window, cx);
            self.transcript.set_delay(self.session.delay());
        }
        self.sync_settings(window, cx);
        self.sync_kind(window, cx);
        self.sync_card(window, cx);
        // Until the attachment has caught up, the session's state is the
        // conversation's past: no turn starts or ends in it now.
        if replaying {
            cx.notify();
            return;
        }
        let work = self.session.background();
        self.processes
            .update(cx, |processes, cx| processes.sync(work, cx));
        let now = Instant::now();
        let busy = self.is_busy();
        match (busy, self.busy_since, self.turn_began) {
            // Attached with a turn under way: it is the newest one.
            (true, None, Some(since)) => {
                self.turn_began = None;
                self.busy_since = Some(since);
                self.transcript.resume_turn(since, cx);
            }
            // A turn began, from here or from another client.
            (true, None, None) => {
                self.busy_since = Some(now);
                self.transcript.expect_turn(now);
            }
            (false, Some(_), _) => {
                self.busy_since = None;
                self.transcript.end_turn(self.session.entries(), cx);
                // A turn can start a server, or see one fail.
                if let Some(mcp) = &self.mcp {
                    mcp.update(cx, McpStatus::reload);
                }
                // A sign-out elsewhere fails the next turn: read whether
                // the model's profile is still signed in.
                if matches!(self.session.state(), RunState::Failed(_)) {
                    if let Some(slot) = &self.sign_in {
                        slot.refresh(cx);
                    }
                }
                cx.emit(SessionEvent::Saved);
            }
            _ => {}
        }
        self.waiting_since = match (self.session.inputs().is_empty(), self.waiting_since) {
            (true, _) => None,
            (false, since) => Some(since.unwrap_or(now)),
        };
        // A stop that took leaves the work no longer running.
        let work = self.session.background();
        self.stopping
            .retain(|id| work.iter().any(|work| work.id() == id && work.can_stop()));
        let key = self.key();
        cx.global::<BackgroundLog>()
            .record(&key, &self.session, &mut self.recorded);
        self.report_live(cx);
        cx.notify();
        if matches!(self.loading, Loading::Ready) {
            if let Some(prompt) = self.opening.take() {
                self.submit(prompt, window, cx);
            }
        }
        self.send_held(window, cx);
        self.discard_when_idle(cx);
    }

    /// Tells the app when what the sidebar shows of it changed, and once a
    /// view that follows the session again has caught up.
    fn report_live(&mut self, cx: &mut Context<'_, Self>) {
        let live = Some(self.live());
        if self.reported != live {
            self.reported = live;
            cx.emit(SessionEvent::LiveChanged);
        }
    }

    /// Sends the composer's message: as a prompt, or while a turn runs, as
    /// a steer for it. A message with attachments can't steer, so it waits
    /// for the turn to end, and so does any message after one waiting.
    fn send(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        // Until a connection follows the session, the message stays in the
        // composer.
        if self.outgoing.is_some()
            || !matches!(self.loading, Loading::Ready)
            || self.link != Link::Following
        {
            return;
        }
        if !self.composer.read(cx).can_send(cx) {
            return;
        }
        let side = side_question(self.composer.read(cx).prompt(cx).text()).map(str::to_owned);
        if let Some(question) = side {
            self.send_side_question(question, window, cx);
            return;
        }
        let prompt = self
            .composer
            .update(cx, |composer, cx| composer.take(window, cx));
        if !self.session.state().is_busy() {
            self.submit(prompt, window, cx);
        } else if prompt.attachments().is_empty() && self.session.held().is_none() {
            self.steer(prompt.into_parts().0, window, cx);
        } else {
            // After a held message, a steer would reach the model first.
            let ended = self.session.ended_turn().cloned();
            self.hold(prompt, ended, window, cx);
        }
    }

    /// Sends `prompt` as the next prompt. A failed send puts it back in the
    /// composer.
    pub(crate) fn submit(
        &mut self,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.send_prompt(prompt, PromptKey::generate(), window, cx);
    }

    /// Sends the prompt that waits for an attach to answer, if one does.
    fn send_waiting(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        match self.outgoing.take() {
            Some(Outgoing::Waiting { prompt, key }) => self.send_prompt(prompt, key, window, cx),
            other => self.outgoing = other,
        }
    }

    /// Tries `prompt`, named `key` for every try of it. A try that reached
    /// a daemon that was stopping, or a connection that ended, waits to go
    /// again (see [`Outgoing::Waiting`]); one the daemon refused goes back
    /// in the composer.
    fn send_prompt(
        &mut self,
        prompt: Prompt,
        key: PromptKey,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.outgoing = Some(Outgoing::Sent);
        self.error = None;
        // A new session's attach may still be on its way, as when the first
        // message goes at once; `after_change` then takes the state for
        // history. The turn this prompt starts is the next one all the
        // same: its echo can't come before it is sent.
        if self.session.is_replaying() && self.busy_since.is_none() {
            let now = Instant::now();
            self.busy_since = Some(now);
            self.transcript.expect_turn(now);
        }
        self.after_change(window, cx);

        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        let ended = self.session.ended_turn().cloned();
        let attachments = self.attachments;
        cx.spawn_in(window, async move |this, cx| {
            let result = daemon.prompt(&id, &prompt, &key).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.outgoing = None;
                match result {
                    Ok(turn) => {
                        this.session.turn_began(&turn);
                        if !this.prompted {
                            this.prompted = true;
                            cx.emit(SessionEvent::Saved);
                        }
                    }
                    Err(DaemonError::Busy(error)) => {
                        this.refused_busy(prompt, ended, error, window, cx);
                    }
                    // No daemon that could run it had it, or one did and
                    // the answer was lost: the key makes another try safe.
                    Err(DaemonError::Stopping(_) | DaemonError::Closed) => {
                        // Unless nothing will follow the session by itself.
                        if matches!(this.link, Link::Lost | Link::Refused(_)) {
                            this.give_back(prompt, window, cx);
                        } else {
                            this.outgoing = Some(Outgoing::Waiting { prompt, key });
                            // An attach answered since it went.
                            if this.attachments != attachments {
                                this.send_waiting(window, cx);
                            }
                        }
                    }
                    Err(error) => {
                        // A turn another client began meanwhile is not
                        // what failed.
                        if this.session.state().is_busy() {
                            this.error = Some(format!("Couldn't send the message: {error}"));
                        } else {
                            this.session.fail(error.to_string());
                        }
                        // Give the prompt back so it isn't lost.
                        this.give_back(prompt, window, cx);
                    }
                }
                this.after_change(window, cx);
            });
        })
        .detach();
    }

    /// Adds `text` to the running turn. It shows above the composer until
    /// the model reads it; a refused steer goes back in the composer.
    fn steer(&mut self, text: String, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.error = None;
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        let ended = self.session.ended_turn().cloned();
        cx.spawn_in(window, async move |this, cx| {
            let result = daemon.steer(&id, &text).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(steer) => {
                        this.session.steer_sent(steer, text);
                        // The turn may have ended before this answer came.
                        let dropped = this.session.take_dropped_steers();
                        if !dropped.is_empty() {
                            this.give_back(dropped.join("\n").into(), window, cx);
                        }
                    }
                    // The turn is past its last model request, or ended.
                    Err(DaemonError::Busy(error)) => {
                        this.refused_busy(text.into(), ended, error, window, cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("Couldn't steer the turn: {error}"));
                        this.give_back(text.into(), window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `prompt` was refused as busy, the daemon saying `why`. A turn this
    /// view knows of, under way or ended since `prompt` was sent, explains
    /// it: `prompt` waits for that turn. Otherwise either the turn's events
    /// have yet to arrive, or the session is busy with no turn at all, as
    /// while it is deleted; the daemon's job list tells which.
    fn refused_busy(
        &mut self,
        prompt: Prompt,
        sent_after: Option<TurnId>,
        why: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.session.has_turn_since(sent_after.as_ref()) {
            self.hold(prompt, sent_after, window, cx);
            return;
        }
        let daemon = self.daemon.clone();
        cx.spawn_in(window, async move |this, cx| {
            let jobs = daemon.jobs().await;
            let _ = this.update_in(cx, |this, window, cx| {
                let id = this.session.id();
                // Only a turn under way or waiting for a person refuses one.
                let listed = jobs.is_ok_and(|jobs| {
                    jobs.active
                        .iter()
                        .any(|job| job.session() == id && job.activity() != JobActivity::Background)
                });
                // The turn's events may have come in meanwhile.
                if listed || this.session.has_turn_since(sent_after.as_ref()) {
                    this.hold(prompt, sent_after, window, cx);
                } else {
                    this.error = Some(format!("Couldn't send the message: {why}"));
                    this.give_back(prompt, window, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Holds `prompt`, which the session's turn could not take, to send as
    /// a prompt once that turn ends; see [`Session::hold`].
    fn hold(
        &mut self,
        prompt: Prompt,
        sent_after: Option<TurnId>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.session.hold(prompt, sent_after);
        cx.notify();
        self.send_held(window, cx);
    }

    /// Sends the held message as a prompt once it is due.
    fn send_held(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.outgoing.is_some() {
            return;
        }
        if let Some(prompt) = self.session.take_due() {
            self.submit(prompt, window, cx);
        }
    }

    /// Takes back the held message, into the composer.
    fn take_back_held(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(prompt) = self.session.take_held() {
            self.give_back(prompt, window, cx);
        }
        cx.notify();
    }

    /// Asks the daemon to stop background work `work`. Its row says so
    /// until the work ends.
    fn stop_background(
        &mut self,
        work: BackgroundId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.stopping.contains(&work) {
            return;
        }
        self.stopping.push(work.clone());
        cx.notify();
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = daemon.stop_background(&id, &work).await {
                let _ = this.update(cx, |this, cx| {
                    this.stopping.retain(|asked| *asked != work);
                    // Work that ended meanwhile needs no stop; the list
                    // already says how it ended.
                    let work_list = this.session.background();
                    if work_list
                        .iter()
                        .any(|listed| *listed.id() == work && listed.can_stop())
                    {
                        this.error = Some(format!("Couldn't stop it: {error}"));
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Takes the session's runs from a listing of the daemon's jobs, which
    /// says what its background list lacks: where a queued run waits, why
    /// a failed one failed, and a workflow's agent nodes.
    pub(crate) fn set_runs(&mut self, runs: Vec<Run>, cx: &mut Context<'_, Self>) {
        if self.runs != runs {
            self.runs = runs;
            cx.notify();
        }
    }

    /// Takes back a steer the model has not read, into the composer.
    fn unsteer(&mut self, steer: SteerId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = daemon.unsteer(&id, &steer).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Unsteered::Withdrawn) => {
                        if let Some(text) = this.session.withdraw_steer(&steer) {
                            this.give_back(text.into(), window, cx);
                        }
                    }
                    // The model has it; it joins the conversation shortly.
                    Ok(Unsteered::Kept) => {}
                    Err(error) => {
                        this.error = Some(format!("Couldn't take the message back: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn stop(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        // Only its parent stops a subagent's run.
        if !self.is_busy() || self.is_subagent() {
            return;
        }
        // A prompt waiting for the daemon to come back hasn't run here: it
        // goes back in the composer.
        if matches!(self.outgoing, Some(Outgoing::Waiting { .. })) {
            self.give_back_waiting(window, cx);
            return;
        }
        // The turn withdraws its open requests as it ends.
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = daemon.cancel(&id).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("Couldn't stop the turn: {error}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Shows the oldest open request in the card, keeping what the user
    /// filled in while it is the same request. A subagent's session shows
    /// none: its questions go to its parent.
    fn sync_card(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let inputs = if self.is_subagent() {
            &[]
        } else {
            self.session.inputs()
        };
        let queue = Queue::of(inputs);
        let shown = self.card.as_ref().map(|(card, _)| card.clone());
        match (inputs.first(), &shown) {
            (Some(request), Some(card)) if card.read(cx).id() == request.id() => {
                card.update(cx, |card, cx| card.set_request(request, queue, cx));
                return;
            }
            (None, None) => return,
            _ => {}
        }
        // Another request takes the card: the keyboard follows it, if it
        // was in the card.
        let focused = shown.is_some_and(|card| card.read(cx).contains_focus(window, cx));
        self.card = inputs.first().cloned().map(|request| {
            let card = cx.new(|cx| InputCard::new(request, queue, window, cx));
            let subscription = cx.subscribe_in(&card, window, Self::on_card_event);
            (card, subscription)
        });
        if focused && self.card.is_some() {
            self.focus_card(window, cx);
        } else if focused {
            window.focus(&self.composer.focus_handle(cx), cx);
        }
    }

    /// Moves the keyboard into the card, if one shows.
    fn focus_card(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some((card, _)) = &self.card {
            card.update(cx, |card, cx| card.focus(window, cx));
        }
    }

    fn on_card_event(
        &mut self,
        card: &Entity<InputCard>,
        event: &InputCardEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let card = card.downgrade();
        match event {
            InputCardEvent::Answer(request, answer) => {
                // The keyboard goes back to the composer now, not when the
                // daemon answers, which may be after the user moved on.
                // With more requests waiting, it stays to follow the next.
                if self.session.inputs().len() <= 1 {
                    window.focus(&self.composer.focus_handle(cx), cx);
                }
                self.answer(*request, answer.clone(), card, window, cx);
            }
            InputCardEvent::Hold(request) => self.hold_input(*request, card, window, cx),
        }
    }

    /// Sends `answer` to open request `request`, whose card takes no other
    /// answer meanwhile. Once the daemon takes it, the request closes; if
    /// not, the card takes an answer again, filled in as it was.
    fn answer(
        &mut self,
        request: InputId,
        answer: Answer,
        card: WeakEntity<InputCard>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            let sent = daemon.answer(&id, request, &answer).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = sent {
                    let _ = card.update(cx, InputCard::answer_failed);
                    this.error = Some(format!("Couldn't send the answer: {error}"));
                    cx.notify();
                    return;
                }
                this.session.close_input(request);
                this.after_change(window, cx);
            });
        })
        .detach();
    }

    /// Stops request `request`'s timeout, as the user started on it. The
    /// daemon's word that it is held drops the countdown.
    fn hold_input(
        &mut self,
        request: InputId,
        card: WeakEntity<InputCard>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = daemon.hold(&id, request).await {
                let _ = card.update(cx, |card, _| card.hold_failed());
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("Couldn't stop the question's timeout: {error}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// What the user did on a surface: the store takes it, and every
    /// surface it changed draws again.
    pub(crate) fn on_surface_event(
        &mut self,
        panel: &Entity<SurfacePanel>,
        event: &SurfaceEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let surface = panel.read(cx).surface().to_owned();
        let store = self.surfaces.store_mut();
        match event {
            SurfaceEvent::Write { binding, value } => {
                store.write(&surface, binding, value.clone());
            }
            SurfaceEvent::Act { target, action } => {
                let acted = store.act(&surface, target, action);
                for url in &acted.open {
                    cx.open_url(url);
                }
                if let Some(error) = acted.error {
                    self.error = Some(format!("Couldn't run the form's action: {error}"));
                }
                if let Some(action) = acted.send {
                    // The keyboard goes back to the composer, where the
                    // reply is followed, as after an answered question.
                    window.focus(&self.composer.focus_handle(cx), cx);
                    self.send_surface_action(action, panel.downgrade(), window, cx);
                }
            }
        }
        self.surfaces.redraw(window, cx);
        cx.notify();
    }

    /// The panel of `surface` changed, and may have changed height.
    pub(crate) fn surface_resized(&self, surface: &str, cx: &mut Context<'_, Self>) {
        if let Some(drawn) = self.loading.drawn(&self.session) {
            self.transcript.surface_changed(surface, drawn, cx);
        }
    }

    /// Sends `action` to the session. A session that no longer has the
    /// surface refuses it: unless a later call showed the surface since,
    /// it then says it is out of date, with what the daemon said, and
    /// takes no more.
    fn send_surface_action(
        &mut self,
        action: SurfaceAction,
        panel: WeakEntity<SurfacePanel>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        let _ = panel.update(cx, SurfacePanel::sending);
        cx.spawn_in(window, async move |this, cx| {
            let sent = daemon.surface_action(&id, &action).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let _ = panel.update(cx, SurfacePanel::sent);
                match sent {
                    Ok(()) => {}
                    Err(SurfaceActionError::Stale(said)) => {
                        this.surfaces.store_mut().mark_stale(&action, said);
                        this.surfaces.redraw(window, cx);
                    }
                    Err(SurfaceActionError::Failed(error)) => {
                        this.error = Some(format!("Couldn't send the form's action: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Where the composer's MCP requests go: the session, through the
    /// daemon the view has now.
    fn mcp_link(&self) -> McpLink {
        McpLink {
            daemon: self.daemon.clone(),
            session: self.session.id().clone(),
        }
    }

    /// Shows the session's settings, changeable once the session is open,
    /// and offers its commands and skills in the composer.
    fn sync_settings(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let allowed = if matches!(self.loading, Loading::Ready) {
            Availability::Enabled
        } else {
            Availability::Disabled
        };
        let turn = if self.session.state().is_busy() {
            Turn::UnderWay
        } else {
            Turn::Idle
        };
        self.settings.update(cx, |settings, cx| {
            settings.sync(self.session.options(), window, cx);
            settings.sync_started(self.session.started_session_defaults(), cx);
            settings.set_allowed(allowed, cx);
            settings.set_turn(turn, cx);
        });
        self.composer.update(cx, |composer, cx| {
            composer.set_commands(self.session.commands(), cx);
        });
        self.added_dirs.update(cx, |dirs, cx| {
            dirs.sync(self.session.added_dirs(), cx);
        });
    }

    /// Words the empty composer, and the added directories, for the kind
    /// of session this is, which a session opened from the list may only
    /// learn as its replay says its agent.
    fn sync_kind(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let kind = self.kind(cx);
        if kind == self.kind {
            return;
        }
        self.kind = kind;
        self.added_dirs
            .update(cx, |dirs, cx| dirs.set_kind(kind, cx));
        let input = self.composer.read(cx).input().clone();
        input.update(cx, |input, cx| {
            input.set_placeholder(kind.placeholder(), window, cx);
        });
    }

    fn on_settings_event(
        &mut self,
        _: &Entity<SessionSettings>,
        event: &SettingsEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            SettingsEvent::Changed(options) => {
                self.error = None;
                self.session.set_options(options.clone());
            }
            SettingsEvent::Failed(message) => self.error = Some(message.clone()),
        }
        self.after_change(window, cx);
    }

    /// Offers what is selected in the window as a quote, once a press in the
    /// transcript lets go: a click that selects nothing takes the offer back.
    fn offer_quote(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let text = TextSelection::selected_text(window, cx);
        self.selected = (!text.trim().is_empty()).then_some(Selected {
            text,
            at: event.position,
        });
        cx.notify();
    }

    /// Takes back the offer of a quote, as when the transcript scrolls away
    /// from the button.
    fn dismiss_quote(&mut self, cx: &mut Context<'_, Self>) {
        if self.selected.take().is_some() {
            cx.notify();
        }
    }

    /// The "Add to prompt" button floating over the transcript, while text
    /// selected there is offered.
    fn render_add_to_prompt(&self, cx: &mut Context<'_, Self>) -> Option<impl IntoElement> {
        let selected = self.selected.as_ref()?;
        let text = selected.text.clone();
        let button = Button::new("add-to-prompt")
            .small()
            .icon(Lucide::TextQuote)
            .label("Add to prompt")
            .on_click(cx.listener(move |this, _, window, cx| {
                this.selected = None;
                TextSelection::clear(window, cx);
                this.composer.update(cx, |composer, cx| {
                    composer.add_quote(&text, cx);
                    window.focus(&composer.focus_handle(cx), cx);
                });
                cx.notify();
            }));
        // Its bottom left corner 8px above the pointer, kept 8px inside the
        // window.
        let corner = selected.at - point(px(0.), px(8.));
        Some(deferred(
            Positioner::corner(Anchor::BottomLeft, corner)
                .margin(px(8.))
                .occlude()
                .child(
                    div()
                        .id("add-to-prompt-popover")
                        .popover_style(cx)
                        .p_0p5()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_quote(cx)))
                        .child(button),
                ),
        ))
    }

    fn render_transcript(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        // A replaying session may have entries already, but no rows yet.
        if self
            .loading
            .drawn(&self.session)
            .is_none_or(<[Entry]>::is_empty)
        {
            let placeholder = Empty::new();
            return match &self.loading {
                Loading::Attaching | Loading::Replaying(_) => placeholder.header(
                    EmptyHeader::new()
                        .media(EmptyMedia::new().child(Spinner::new()))
                        .description(EmptyDescription::new().child("Loading session…")),
                ),
                Loading::Failed(message) => placeholder.content(
                    EmptyContent::new().max_w(rems(36.)).child(
                        Alert::error("attach-error", SharedString::from(message.clone()))
                            .title("Couldn't open this session"),
                    ),
                ),
                Loading::Ready => placeholder.header(
                    EmptyHeader::new()
                        .description(EmptyDescription::new().child("Send a message to start.")),
                ),
            }
            .into_any_element();
        }

        let view = cx.entity().downgrade();
        let handlers = Rc::new(Self::transcript_handlers(&view));
        let content: ItemContent = {
            let view = view.clone();
            let handlers = Rc::clone(&handlers);
            Rc::new(move |place, _, cx| match view.upgrade() {
                Some(view) => {
                    let this = view.read(cx);
                    this.transcript.render_content(
                        place,
                        this.loading.drawn(&this.session).unwrap_or_default(),
                        &handlers,
                        cx,
                    )
                }
                None => div().into_any_element(),
            })
        };
        div()
            .flex_1()
            .min_h_0()
            // A subagent's session has no composer to quote into.
            .when(!self.is_subagent(), |this| {
                this.capture_any_mouse_up(cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    if event.button == MouseButton::Left {
                        this.offer_quote(event, window, cx);
                    }
                }))
                // The list doesn't stop a scroll's propagation, so this
                // sees each the user makes; following a streaming reply
                // makes no scroll event, and leaves the button.
                .on_scroll(cx.listener(|this, _, _, cx| this.dismiss_quote(cx)))
            })
            .children(self.render_add_to_prompt(cx))
            .child(
                MessageScroller::new(
                    SharedString::from(format!("transcript-{}", self.session.id())),
                    self.transcript.scroller().clone(),
                    move |ix, _, cx| match view.upgrade() {
                        Some(view) => {
                            let this = view.read(cx);
                            this.transcript.render_item(
                                ix,
                                this.loading.drawn(&this.session).unwrap_or_default(),
                                &this.surfaces,
                                &handlers,
                                &content,
                                cx,
                            )
                        }
                        None => div().into_any_element(),
                    },
                )
                .with_list_style(StyleRefinement::default().px_4())
                // Kit pads every row but the last by 32px. The transcript
                // spaces its own rows, so a folded row can take no space.
                .with_row_style(StyleRefinement::default().pb_0())
                .with_bottom_fade(cx.theme().background)
                .size_full(),
            )
            .into_any_element()
    }

    /// Says so while no connection follows the session: what it shows
    /// stays, and comes up to date once one does.
    fn render_link(&self) -> Option<impl IntoElement> {
        let alert = match &self.link {
            Link::Following => return None,
            Link::Reconnecting => Alert::info(
                "link-alert",
                "This session carries on once the Rho daemon is back.",
            )
            .title("Reconnecting"),
            Link::Lost => Alert::warning(
                "link-alert",
                "Retry, or Start, from the sidebar to follow this session again.",
            )
            .title("Lost the connection to the Rho daemon"),
            Link::Refused(why) => Alert::error("link-alert", SharedString::from(why.clone()))
                .title("Couldn't follow this session again"),
        };
        Some(div().id("link-notice").test_support().child(alert.small()))
    }

    /// Errors, approvals and the composer, under the transcript. A
    /// subagent's session shows only how its run failed and its own
    /// running work.
    fn render_footer(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let failure = match self.session.state() {
            RunState::Failed(message) => Some(message.clone()),
            _ => None,
        };
        let footer = v_flex()
            .flex_shrink_0()
            .w_full()
            .max_w(rems(48.))
            .mx_auto()
            .gap_2()
            .px_4()
            .pb_3()
            // Each alert is wrapped so UI tests can find it: Kit's Alert
            // does not register its ID with them.
            .children(self.render_link())
            .when_some(failure, |this, message| {
                this.child(
                    div().id("turn-error").test_support().child(
                        Alert::error("turn-error-alert", SharedString::from(message))
                            .title("The turn failed")
                            .small(),
                    ),
                )
            });
        if self.is_subagent() {
            return footer
                .children(self.render_background(cx))
                .child(self.render_subagent_status(cx))
                .into_any_element();
        }
        let busy = self.is_busy();
        let send_view = cx.entity().downgrade();
        let stop_view = send_view.clone();
        footer
            .when_some(self.error.clone(), |this, message| {
                this.child(
                    div().id("session-error").test_support().child(
                        Alert::warning("session-error-alert", SharedString::from(message))
                            .small()
                            .on_close(cx.listener(|this, _, _, cx| {
                                this.error = None;
                                cx.notify();
                            })),
                    ),
                )
            })
            .children(
                self.sign_in
                    .as_ref()
                    .and_then(|slot| slot.card(self.session.auth(), cx)),
            )
            .when_some(self.card.as_ref(), |this, (card, _)| {
                this.child(card.clone())
            })
            .children(self.render_background(cx))
            .children(PlanList::new(self.session.plan(), self.plan, {
                let view = cx.entity().downgrade();
                move |_, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.plan.toggle();
                        cx.notify();
                    });
                }
            }))
            .children(self.render_steers(cx))
            .children(self.render_held(cx))
            .child(
                Composer::new(
                    &self.composer,
                    if busy {
                        ComposerMode::Stop
                    } else {
                        ComposerMode::Send
                    },
                    move |window, cx| {
                        let _ = send_view.update(cx, |this, cx| this.send(window, cx));
                    },
                    move |window, cx| {
                        let _ = stop_view.update(cx, |this, cx| this.stop(window, cx));
                    },
                )
                .leading(SessionSettings::render_mode(&self.settings, cx))
                .trailing(SessionSettings::render_panel_button(&self.settings, cx))
                .status(self.mcp.clone())
                .usage(self.usage_ring(cx)),
            )
            .into_any_element()
    }

    /// Where a subagent's session would have its composer: the agent and
    /// model it runs as and how full its context is. Read-only, as only the
    /// subagent writes.
    fn render_subagent_status(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let label = runs_as(
            self.session.agent(),
            self.session.options().and_then(model_summary),
        );
        h_flex()
            .id("subagent-status")
            .test_support()
            .gap_2()
            .px_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(Lucide::Bot).xsmall())
            .child(
                div()
                    .id("subagent-runs-as")
                    .test_support()
                    .aria_label(label.clone())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(label),
            )
            .children(self.usage_ring(cx))
    }

    /// How full the context is, for the composer's footer, which opens the
    /// session's context in the right panel. A side chat's opens nothing:
    /// the panel it sits in shows the session it forks.
    fn usage_ring(&self, cx: &Context<'_, Self>) -> Option<UsageRing> {
        let ring = UsageRing::new(self.session.usage()?.clone());
        if self.is_side() {
            return Some(ring);
        }
        let view = cx.entity().downgrade();
        Some(ring.on_click(move |_, cx| {
            let _ = view.update(cx, |_, cx| cx.emit(SessionEvent::ShowContext));
        }))
    }

    /// The steers the turn has taken that the model has not read yet,
    /// each with a button to take it back.
    fn render_steers(&self, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let steers = self.session.pending_steers().enumerate();
        steers
            .map(|(ix, (steer, text))| {
                let steer = steer.clone();
                h_flex()
                    .id(SharedString::from(format!("steer-{ix}")))
                    .test_support()
                    .aria_label(format!("Queued: {text}"))
                    .gap_2()
                    .px_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(Lucide::CornerDownRight).xsmall())
                    .child(div().flex_1().min_w_0().truncate().child(text.to_owned()))
                    .child(
                        Button::new(SharedString::from(format!("unsteer-{ix}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Take it back")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.unsteer(steer.clone(), window, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The running background work; a subagent's session can't stop its
    /// own.
    fn render_background(&self, cx: &mut Context<'_, Self>) -> Option<BackgroundList> {
        let list = BackgroundList::new(
            self.session.background(),
            &self.sources(cx),
            Self::open_handler(cx.entity().downgrade()),
        )?;
        if self.is_subagent() {
            return Some(list);
        }
        Some(list.stoppable(&self.stopping, Self::stop_handler(cx.entity().downgrade())))
    }

    /// What the rows of the session's background work read besides the
    /// work: a side chat's rows open no output, as its panel shows its
    /// session's.
    fn sources<'a>(&'a self, cx: &'a App) -> Sources<'a> {
        Sources {
            runs: &self.runs,
            processes: self.processes.read(cx),
            output: if self.is_side() {
                OpensOutput::No
            } else {
                OpensOutput::Yes
            },
        }
    }

    /// The session's background work, running and ended, for the right
    /// panel's Background surface, which may show a process's output in
    /// its place. A subagent's session can't stop its own.
    pub(crate) fn background_surface(view: &Entity<Self>, cx: &App) -> BackgroundSurface {
        let this = view.read(cx);
        let back = view.downgrade();
        let surface = BackgroundSurface::new(
            this.session.background(),
            this.session.ended_background(),
            &this.sources(cx),
            &this.processes,
            Self::open_handler(view.downgrade()),
            Rc::new(move |_, cx| {
                let _ = back.update(cx, Self::unfollow_output);
            }),
        );
        if this.is_subagent() {
            return surface;
        }
        surface.stoppable(&this.stopping, Self::stop_handler(view.downgrade()))
    }

    /// Stops a piece of the background work, from its row.
    fn stop_handler(view: WeakEntity<Self>) -> StopHandler {
        Rc::new(move |work, window, cx| {
            let _ = view.update(cx, |this, cx| {
                this.stop_background(work.clone(), window, cx);
            });
        })
    }

    /// What a click in the transcript of `view` calls.
    fn transcript_handlers(view: &WeakEntity<Self>) -> Handlers {
        let act: ActionHandler = {
            let view = view.clone();
            Rc::new(move |action, _, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.transcript.act(
                        action,
                        this.loading.drawn(&this.session).unwrap_or_default(),
                        cx,
                    );
                    cx.notify();
                });
            })
        };
        let view = view.clone();
        let open: rail::OpenHandler = Rc::new(move |link, _, cx| {
            let _ = view.update(cx, |this, cx| this.open_link(link.clone(), cx));
        });
        Handlers { act, open }
    }

    /// Opens what a card on the rail is about: a session, or a pull request
    /// in the right panel while the session links it, else in the browser,
    /// since the panel shows only the session's.
    fn open_link(&self, link: rail::Link, cx: &mut Context<'_, Self>) {
        let event = match link {
            rail::Link::PullRequest { id, url } => {
                let key = self.key();
                let set = self.prs.as_ref().and_then(|(prs, _)| prs.read(cx).of(&key));
                let linked = set.is_some_and(|set| {
                    set.link(&id).is_some()
                        || set.branch_pull_request().is_some_and(|pr| pr.id == id)
                });
                if !linked {
                    cx.open_url(&url);
                    return;
                }
                SessionEvent::ShowPullRequest(id)
            }
            rail::Link::Session(id) => SessionEvent::GoToSession(id),
            rail::Link::Subagent { session, title } => {
                SessionEvent::OpenSubagent { session, title }
            }
        };
        cx.emit(event);
    }

    /// Opens what a row of the background work opens: a subagent's
    /// session, or a process's output, in the right panel's Background
    /// surface.
    fn open_handler(view: WeakEntity<Self>) -> OpenHandler {
        Rc::new(move |opens, _, cx| {
            let _ = view.update(cx, |this, cx| match opens {
                Opens::Session { session, title } => cx.emit(SessionEvent::OpenSubagent {
                    session: session.clone(),
                    title: title.clone(),
                }),
                Opens::Output(id) => {
                    this.processes
                        .update(cx, |processes, cx| processes.follow(id, cx));
                    cx.emit(SessionEvent::ShowBackground);
                }
            });
        })
    }

    /// Notes whether the right panel shows this session's Background
    /// surface: the output it may show is read only then.
    pub(crate) fn set_output_shown(&mut self, shown: Visibility, cx: &mut Context<'_, Self>) {
        self.processes
            .update(cx, |processes, cx| processes.set_shown(shown, cx));
    }

    /// Stops following process output: the Background surface shows its
    /// lists again.
    pub(crate) fn unfollow_output(&mut self, cx: &mut Context<'_, Self>) {
        self.processes.update(cx, Processes::unfollow);
    }

    /// The session's pull request, marked as its sidebar row marks it. A
    /// click shows it in the right panel.
    fn render_pr_mark(&self, cx: &Context<'_, Self>) -> Option<Button> {
        let (prs, _) = self.prs.as_ref()?;
        let mark = prs.read(cx).mark(&self.key())?;
        let id = mark.pr.id.clone();
        Some(
            pr_pill("title-pr", &mark, cx.theme()).on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(SessionEvent::ShowPullRequest(id.clone()));
            })),
        )
    }

    /// The project, which starts a new session in it, the sessions above
    /// this one, each a link to it, and this one's title, with its pull
    /// request, added directories and ended background work at the right.
    fn render_title_bar(&self, cx: &mut Context<'_, Self>) -> TitleBar {
        let mut above = Vec::new();
        let mut next = self.parent();
        while let Some(view) = next {
            next = view.read(cx).parent();
            above.push(view);
        }
        let mut bar = TitleBar::new(&self.projects(cx), self.title());
        let this = cx.entity().downgrade();
        // A lead's folder is no project to start a session in.
        if self.kind(cx) == Kind::Session {
            bar = bar.on_project({
                let this = this.clone();
                move |_, cx| {
                    let _ = this.update(cx, |_, cx| cx.emit(SessionEvent::NewSession));
                }
            });
        }
        for view in above.into_iter().rev() {
            let (this, target) = (this.clone(), view.downgrade());
            bar = bar.parent(view.read(cx).title(), move |_, cx| {
                let _ = this.update(cx, |_, cx| cx.emit(SessionEvent::Show(target.clone())));
            });
        }
        let ended = BackgroundHistory::new(
            self.session.ended_background(),
            &self.sources(cx),
            Self::open_handler(cx.entity().downgrade()),
        );
        bar.trailing(Some(
            h_flex()
                .gap_1()
                .children(self.render_pr_mark(cx))
                .child(self.added_dirs.clone())
                .children(ended),
        ))
    }

    /// The message held for when the turn ends, with a button to take it
    /// back.
    fn render_held(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let text = held_summary(self.session.held()?);
        let row = h_flex()
            .id("held")
            .test_support()
            .aria_label(format!("Sends after this turn: {text}"))
            .gap_2()
            .px_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(Lucide::Clock).xsmall())
            .child(div().flex_1().min_w_0().truncate().child(text))
            .child(div().text_xs().child("After this turn"))
            .child(
                Button::new("take-back-held")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip("Take it back")
                    .on_click(cx.listener(|this, _, window, cx| this.take_back_held(window, cx))),
            );
        Some(row.into_any_element())
    }
}

impl SessionView {
    /// The transcript scrolled, or remeasured an item, as a text view does
    /// when its parse lands: find may have been waiting for that parse.
    fn scroller_changed(&mut self, _: Entity<MessageScrollerState>, cx: &mut Context<'_, Self>) {
        let drawn = self.loading.drawn(&self.session).unwrap_or_default();
        self.transcript.find_reparsed(drawn, cx);
        cx.notify();
    }

    /// Opens the find bar, or puts the keyboard back in it with its text
    /// selected.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let drawn = self.loading.drawn(&self.session).unwrap_or_default();
        self.transcript.prepare_find(drawn);
        let bar = self.find.get_or_insert_with(|| FindBar::new(window, cx));
        bar.focus(window, cx);
        cx.notify();
    }

    /// Finds what the find bar holds now.
    fn find_query(&mut self, cx: &mut Context<'_, Self>) {
        let Some(bar) = &self.find else {
            return;
        };
        let query = bar.query(cx);
        let drawn = self.loading.drawn(&self.session).unwrap_or_default();
        self.transcript.find(&query, drawn, cx);
        cx.notify();
    }

    /// Moves to the next or previous match.
    fn find_step(&mut self, direction: Direction, cx: &mut Context<'_, Self>) {
        let drawn = self.loading.drawn(&self.session).unwrap_or_default();
        self.transcript.find_step(direction, drawn, cx);
        cx.notify();
    }

    /// Closes the find bar and clears its matches; what they opened stays
    /// open.
    fn close_find(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.find.take().is_some() {
            self.transcript.close_find(cx);
            window.focus(&self.focus_target(cx), cx);
            cx.notify();
        }
    }
}

#[cfg(test)]
impl SessionView {
    /// The composer's focus, for UI tests.
    pub(crate) fn composer_focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }

    /// The session state behind this view, for UI tests.
    pub(crate) fn session(&self) -> &Session {
        &self.session
    }

    /// What the composer holds, for UI tests.
    pub(crate) fn composer_text(&self, cx: &App) -> String {
        self.composer.read(cx).input().read(cx).value().to_string()
    }

    /// The kind of session the empty composer is worded for, for UI tests.
    #[cfg(test)]
    pub(crate) fn composer_placeholder(&self) -> Kind {
        self.kind
    }

    /// Whether the composer is asking for the MCP catalog, for UI tests.
    #[cfg(test)]
    pub(crate) fn composer_is_asking_catalog(&self, cx: &App) -> bool {
        self.composer.read(cx).is_asking_catalog()
    }

    /// Whether the composer still reads an MCP resource, for UI tests.
    #[cfg(test)]
    pub(crate) fn composer_is_reading(&self, cx: &App) -> bool {
        self.composer.read(cx).is_reading()
    }

    /// The message the composer holds, attachments and all, for UI tests.
    pub(crate) fn composer_prompt(&self, cx: &App) -> Prompt {
        self.composer.read(cx).prompt(cx)
    }

    /// The transcript, for UI tests.
    pub(crate) fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// How far down the transcript is scrolled: the first of the scroller's
    /// items on screen, and the pixels from its top, for UI tests.
    pub(crate) fn scroll_top(&self, cx: &App) -> (usize, f32) {
        let top = self.transcript.scroller().read(cx).logical_scroll_top();
        (top.item_ix, f32::from(top.offset_in_item))
    }
}

impl Render for SessionView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .id("session-view")
            .key_context("SessionView")
            .track_focus(&self.focus_handle)
            // Escape closes the find bar first, wherever the keyboard is.
            .on_action(cx.listener(|this, _: &StopTurn, window, cx| {
                if this.find.is_some() {
                    this.close_find(window, cx);
                } else {
                    this.stop(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FindInTranscript, window, cx| {
                this.open_find(window, cx);
            }))
            // Kit's text fields, such as the composer, take the same keys
            // for their own search, which they pass up when they have none.
            .on_action(cx.listener(|this, _: &Search, window, cx| this.open_find(window, cx)))
            .size_full()
            // A side chat's tab names it.
            .when(!self.is_side(), |this| {
                this.child(self.render_title_bar(cx))
            })
            .children(
                self.find
                    .as_ref()
                    .map(|bar| bar.render(self.transcript.find_count(), cx)),
            )
            .child(self.render_transcript(cx))
            .child(self.render_footer(cx))
    }
}

/// The first entry whose row must be built again over what was built for
/// `shown`: where the replay that replaced it first `differs`
/// ([`Session::keep_arrivals`]), or the first surface before that, whose
/// panel a new store makes again.
fn first_change(shown: &[Entry], differs: usize) -> usize {
    shown[..differs]
        .iter()
        .position(|entry| matches!(entry, Entry::Surface(_)))
        .unwrap_or(differs)
}

/// A held message on one line: its text, and what is attached to it, such
/// as "look · 1 image, 2 files".
fn held_summary(held: &Prompt) -> String {
    let attachments = held.attachments();
    let images = attachments
        .iter()
        .filter(|attachment| matches!(attachment, Attachment::Image(_)))
        .count();
    let count = |count: usize, one: &str| match count {
        0 => None,
        1 => Some(format!("1 {one}")),
        count => Some(format!("{count} {one}s")),
    };
    let attached = [
        count(images, "image"),
        count(attachments.len() - images, "file"),
    ];
    let attached: Vec<_> = attached.into_iter().flatten().collect();
    match (held.text().is_empty(), attached.is_empty()) {
        (_, true) => held.text().to_owned(),
        (true, false) => attached.join(", "),
        (false, false) => format!("{} · {}", held.text(), attached.join(", ")),
    }
}

/// "explorer · Claude Opus 5.5 · High": the agent a subagent's session runs
/// as, then its [`model_summary`]. The default agent goes unnamed, as every
/// session the user starts runs as it. "Subagent" while the daemon has said
/// neither.
fn runs_as(agent: Option<&str>, model: Option<SharedString>) -> SharedString {
    let agent = agent.filter(|agent| *agent != "default");
    match (agent, model) {
        (Some(agent), Some(model)) => format!("{agent} · {model}").into(),
        (Some(agent), None) => SharedString::from(agent.to_owned()),
        (None, Some(model)) => model,
        (None, None) => "Subagent".into(),
    }
}

/// Longest first-prompt title, in characters: rho's `FALLBACK_TITLE_CHARS`,
/// so the header matches the title rho lists for an untitled session.
const PROMPT_TITLE_CHARS: usize = 80;

/// The first prompt's text on one line, cut to [`PROMPT_TITLE_CHARS`]. It
/// runs on every daemon event, so it reads only as far into the prompt as
/// the title needs: one character more tells whether to cut.
fn first_prompt(session: &Session) -> Option<String> {
    let text = session.entries().iter().find_map(|entry| match entry {
        Entry::User(message) => Some(message.text()),
        Entry::Agent(_)
        | Entry::Thought(_)
        | Entry::Tool(_)
        | Entry::Notice(_)
        | Entry::Compaction(_)
        | Entry::Surface(_)
        | Entry::Wake => None,
    })?;
    let mut line: String = text
        .split_whitespace()
        .enumerate()
        .flat_map(|(n, word)| (n > 0).then_some(' ').into_iter().chain(word.chars()))
        .take(PROMPT_TITLE_CHARS + 1)
        .collect();
    if line.is_empty() {
        return None;
    }
    if line.chars().count() > PROMPT_TITLE_CHARS {
        let end = line
            .char_indices()
            .nth(PROMPT_TITLE_CHARS - 1)
            .map_or(line.len(), |(at, _)| at);
        line.truncate(end);
        line.push('…');
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::runs_as;

    /// The default agent goes unnamed, and whichever of agent and model the
    /// daemon has said yet shows alone.
    #[test]
    fn a_subagent_names_its_agent_unless_it_is_the_default() {
        let model = || Some("Huge · High".into());
        let cases = [
            (Some("explorer"), model(), "explorer · Huge · High"),
            (Some("default"), model(), "Huge · High"),
            (Some("explorer"), None, "explorer"),
            (None, model(), "Huge · High"),
            (Some("default"), None, "Subagent"),
        ];
        for (agent, model, label) in cases {
            assert_eq!(runs_as(agent, model).as_ref(), label, "agent {agent:?}");
        }
    }
}
