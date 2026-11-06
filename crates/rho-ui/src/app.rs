//! The root view: owns the agent process, the event pump, and the open
//! sessions. Everything the agent pushes enters here and is routed to the
//! [`SessionView`] of its session.
//!
//! The sidebar and the session view are drawn as cached views: each redraws
//! only when it changes, so a hover in one never rebuilds the other.

use std::{path::PathBuf, rc::Rc};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle},
    h_flex,
    notification::Notification,
    spinner::Spinner,
    ActiveTheme as _, IconName, WindowExt as _,
};
use gpui_kit::{
    div, rems, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled as _, Subscription, Task, Window,
};
use rho_ui_core::{
    Agent, AgentCommand, AgentError, AgentEvent, Session, SessionId, SessionSetup, SessionUpdate,
};

use crate::{
    draft::{DraftEvent, DraftView},
    kinetic::Kinetic,
    session_view::{Loading, SessionEvent, SessionView},
    settings_view::{Closed, SettingsView},
    shelf::Shelves,
    sidebar::{SidebarEvent, SidebarView},
    workspaces::{WorkspaceRef, Workspaces},
    NewSession, OpenSettings,
};

/// What the command line asked for.
pub(crate) struct LaunchConfig {
    /// Where new sessions start. Resumed sessions keep their own directory.
    pub(crate) workspace: PathBuf,
    pub(crate) command: AgentCommand,
    /// The command as typed, for the failure screen.
    pub(crate) command_line: String,
}

/// The agent process lifecycle.
enum AgentStatus {
    Starting,
    Ready(Agent),
    /// Startup failed or the process exited; the message is shown full-window.
    Failed(String),
}

/// What the main pane shows.
enum Pane {
    Empty,
    /// A new session before its first message. Switching away drops it.
    Draft {
        view: Entity<DraftView>,
        _subscription: Subscription,
    },
    /// One of [`RhoApp::sessions`].
    Session(Entity<SessionView>),
}

/// The settings screen, while open.
struct SettingsScreen {
    view: Entity<SettingsView>,
    _subscription: Subscription,
}

/// A session the user opened in this run, with the subscription that keeps
/// the sidebar in step with it.
struct OpenSession {
    id: SessionId,
    view: Entity<SessionView>,
    _subscription: Subscription,
}

pub(crate) struct RhoApp {
    launch: LaunchConfig,
    status: AgentStatus,
    /// Counts agent starts. Replies to requests made before a restart
    /// belong to the old process and are dropped.
    epoch: u64,
    workspaces: Entity<Workspaces>,
    sidebar: Entity<SidebarView>,
    /// Sessions opened in this run. A handful at most, so lookups by ID scan.
    sessions: Vec<OpenSession>,
    pane: Pane,
    settings: Option<SettingsScreen>,
    /// Smooth wheel scrolling and touchpad flings for the whole window.
    kinetic: Rc<Kinetic>,
    /// `new_session` calls in flight. While any are, updates for unknown
    /// sessions are kept, since they may belong to the session being made.
    creating: usize,
    early_updates: Vec<(SessionId, SessionUpdate)>,
    focus_handle: FocusHandle,
    /// Starts the agent, then pumps its events. Dropping or replacing it
    /// stops both.
    agent_task: Task<()>,
    /// The latest `list_sessions`; a newer refresh replaces it.
    sessions_task: Task<()>,
    _sidebar_subscription: Subscription,
}

impl RhoApp {
    /// `shelves` holds the settled and snoozed marks; see [`Shelves::load`].
    pub(crate) fn new(
        launch: LaunchConfig,
        shelves: Shelves,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let workspaces = cx.new(Workspaces::new);
        let sidebar = cx.new(|cx| SidebarView::new(workspaces.clone(), shelves, window, cx));
        let sidebar_subscription = cx.subscribe_in(
            &sidebar,
            window,
            |this, _, event: &SidebarEvent, window, cx| match event {
                SidebarEvent::Open { id, cwd, title } => {
                    this.open_session(id.clone(), cwd.clone(), title.clone(), window, cx);
                }
                SidebarEvent::NewSession => this.new_session(window, cx),
                SidebarEvent::Delete { id } => this.delete_session(id.clone(), window, cx),
                SidebarEvent::OpenSettings => this.open_settings(window, cx),
            },
        );
        let mut app = Self {
            launch,
            status: AgentStatus::Starting,
            epoch: 0,
            workspaces,
            sidebar,
            sessions: Vec::new(),
            pane: Pane::Empty,
            settings: None,
            kinetic: Rc::default(),
            creating: 0,
            early_updates: Vec::new(),
            focus_handle,
            agent_task: Task::ready(()),
            sessions_task: Task::ready(()),
            _sidebar_subscription: sidebar_subscription,
        };
        app.start_agent(window, cx);
        app
    }

    /// (Re)starts the agent. Sessions from a previous process are dropped:
    /// their sessions are not loaded in the new one.
    fn start_agent(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.forget_sessions(cx);
        self.epoch += 1;
        self.creating = 0;
        self.early_updates.clear();
        self.status = AgentStatus::Starting;
        cx.notify();

        let command = self.launch.command.clone();
        self.agent_task = cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let started = Agent::start(command, |driver| executor.spawn(driver).detach()).await;
            let events = match started {
                Ok((agent, events)) => {
                    let stored = this.update(cx, |this, cx| {
                        let info = agent.info();
                        let label = format!("{} {}", info.name(), info.version());
                        this.sidebar
                            .update(cx, |sidebar, cx| sidebar.set_agent(label, cx));
                        this.status = AgentStatus::Ready(agent);
                        this.refresh_sessions(cx);
                        cx.notify();
                    });
                    if stored.is_err() {
                        return;
                    }
                    events
                }
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.status = AgentStatus::Failed(error.to_string());
                        cx.notify();
                    });
                    return;
                }
            };
            // The one event pump: every agent push is routed from here.
            while let Some(event) = events.recv().await {
                let routed =
                    this.update_in(cx, |this, window, cx| this.route_event(event, window, cx));
                if routed.is_err() {
                    break;
                }
            }
        });
    }

    /// Drops every open session: they are gone with the agent.
    fn forget_sessions(&mut self, cx: &mut Context<'_, Self>) {
        self.sessions.clear();
        self.set_pane(Pane::Empty, cx);
        self.sidebar.update(cx, SidebarView::clear_live);
    }

    /// The session `id`, if it was opened in this run.
    fn opened(&self, id: &SessionId) -> Option<&OpenSession> {
        self.sessions.iter().find(|open| open.id == *id)
    }

    fn agent(&self) -> Option<&Agent> {
        match &self.status {
            AgentStatus::Ready(agent) => Some(agent),
            _ => None,
        }
    }

    fn route_event(&mut self, event: AgentEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        match event {
            AgentEvent::Update { session, update } => {
                if let Some(open) = self.opened(&session) {
                    open.view
                        .update(cx, |view, cx| view.apply(update, window, cx));
                } else if let Some(draft) = self.draft_with(&session, cx) {
                    draft.update(cx, |draft, cx| draft.apply(update, window, cx));
                } else if self.creating > 0 {
                    self.early_updates.push((session, update));
                }
            }
            AgentEvent::Permission(request) => {
                // Unknown sessions drop the request, which answers "cancelled".
                if let Some(open) = self.opened(request.session()) {
                    open.view
                        .update(cx, |view, cx| view.push_approval(request, window, cx));
                }
            }
            AgentEvent::Exited { error } => {
                self.forget_sessions(cx);
                self.status = AgentStatus::Failed(
                    error.unwrap_or_else(|| "The agent closed the connection.".to_owned()),
                );
                cx.notify();
            }
        }
    }

    /// Reloads the saved-session list in the background.
    fn refresh_sessions(&mut self, cx: &mut Context<'_, Self>) {
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        self.sessions_task = cx.spawn(async move |this, cx| {
            let result = agent.list_sessions().await;
            let _ = this.update(cx, |this, cx| {
                this.sidebar.update(cx, |sidebar, cx| match result {
                    Ok(sessions) => sidebar.set_saved(sessions, cx),
                    Err(error) => sidebar.set_error(error.to_string(), cx),
                });
            });
        });
    }

    /// Shows a draft for a new session and opens the session right away; see
    /// [`DraftView`].
    pub(crate) fn new_session(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.agent().is_none() {
            return;
        }
        // An open draft is reused, so its text and settings survive.
        let pane = std::mem::replace(&mut self.pane, Pane::Empty);
        let pane = match pane {
            Pane::Draft { .. } => pane,
            Pane::Empty | Pane::Session(_) => {
                let cwd = self.launch.workspace.clone();
                let workspace = WorkspaceRef::new(&self.workspaces, &cwd, cx);
                let view = cx.new(|cx| DraftView::new(cwd, workspace, window, cx));
                let subscription = cx.subscribe_in(
                    &view,
                    window,
                    |this, draft, event: &DraftEvent, window, cx| match event {
                        DraftEvent::Start { session, text } => {
                            this.start_session(
                                draft,
                                (**session).clone(),
                                text.clone(),
                                window,
                                cx,
                            );
                        }
                        DraftEvent::Retry => this.open_draft_session(draft, window, cx),
                    },
                );
                self.open_draft_session(&view, window, cx);
                Pane::Draft {
                    view,
                    _subscription: subscription,
                }
            }
        };
        self.show(pane, window, cx);
    }

    /// The shown draft, if its session is `id`.
    fn draft_with(&self, id: &SessionId, cx: &App) -> Option<Entity<DraftView>> {
        match &self.pane {
            Pane::Draft { view, .. } if view.read(cx).session_id() == Some(id) => {
                Some(view.clone())
            }
            _ => None,
        }
    }

    /// Makes the session for `draft`. A draft dropped meanwhile gets its
    /// session closed.
    fn open_draft_session(
        &mut self,
        draft: &Entity<DraftView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        let cwd = draft.read(cx).cwd().to_path_buf();
        let draft = draft.downgrade();
        let epoch = self.epoch;
        self.creating += 1;
        cx.spawn_in(window, async move |this, cx| {
            let result = agent.new_session(&cwd).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.epoch != epoch {
                    return;
                }
                this.creating -= 1;
                match (result, draft.upgrade()) {
                    (Ok(setup), Some(draft)) => {
                        let id = setup.id().clone();
                        let mut session = Session::new(id.clone());
                        session.set_setup(setup);
                        for update in this.take_early_updates(&id) {
                            session.apply(update);
                        }
                        draft.update(cx, |draft, cx| draft.open(agent, session, window, cx));
                    }
                    (Ok(setup), None) => {
                        let id = setup.id().clone();
                        cx.spawn(async move |_, _| {
                            let _ = agent.close_session(&id).await;
                        })
                        .detach();
                    }
                    (Err(error), Some(draft)) => {
                        draft.update(cx, |draft, cx| draft.fail(error.to_string(), cx));
                    }
                    (Err(_), None) => {}
                }
                if this.creating == 0 {
                    this.early_updates.clear();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Updates that arrived for session `id` before it was known here.
    fn take_early_updates(&mut self, id: &SessionId) -> Vec<SessionUpdate> {
        let early = std::mem::take(&mut self.early_updates);
        let (mine, others): (Vec<_>, Vec<_>) =
            early.into_iter().partition(|(session, _)| session == id);
        self.early_updates = others;
        mine.into_iter().map(|(_, update)| update).collect()
    }

    /// Shows `draft`'s session in its own view and sends `text` as its first
    /// prompt.
    fn start_session(
        &mut self,
        draft: &Entity<DraftView>,
        session: Session,
        text: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        let cwd = draft.read(cx).cwd().to_path_buf();
        let view = self.open_new_session(agent, cwd, session, text, window, cx);
        self.set_pane(Pane::Session(view), cx);
        // Behind the settings screen, the keyboard stays there.
        if self.settings.is_none() {
            self.focus_pane(window, cx);
        }
    }

    /// Deletes a saved session in the agent, then drops it here.
    fn delete_session(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = agent.delete_session(&id).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.forget_session(&id, window, cx);
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.remove(&id, cx));
                    this.refresh_sessions(cx);
                }
                Err(error) => window.push_notification(
                    Notification::error(error.to_string()).title("Couldn't delete the session"),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Drops the open session `id`, if any, and empties the pane if it
    /// showed it.
    fn forget_session(&mut self, id: &SessionId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(ix) = self.sessions.iter().position(|open| open.id == *id) else {
            return;
        };
        let closed = self.sessions.remove(ix);
        if matches!(&self.pane, Pane::Session(view) if *view == closed.view) {
            self.show(Pane::Empty, window, cx);
        }
        cx.notify();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.settings.is_some() {
            return;
        }
        let view = cx.new(|cx| SettingsView::new(window, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, _: &Closed, window, cx| {
            this.settings = None;
            this.focus_pane(window, cx);
            cx.notify();
        });
        self.settings = Some(SettingsScreen {
            view,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Shows `pane` in the main area, closing settings, and puts the
    /// keyboard in it.
    fn show(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.settings = None;
        self.set_pane(pane, cx);
        self.focus_pane(window, cx);
    }

    /// Puts `pane` in the main area and marks its row in the sidebar,
    /// leaving the keyboard where it is.
    fn set_pane(&mut self, pane: Pane, cx: &mut Context<'_, Self>) {
        let active = match &pane {
            Pane::Session(view) => Some(view.read(cx).id().clone()),
            Pane::Empty | Pane::Draft { .. } => None,
        };
        self.pane = pane;
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_active(active, cx));
        cx.notify();
    }

    /// Puts the keyboard in the main pane's composer.
    fn focus_pane(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let handle = match &self.pane {
            Pane::Draft { view, .. } => view.read(cx).composer_focus_handle(cx),
            Pane::Session(view) => view.read(cx).composer_focus_handle(cx),
            Pane::Empty => self.focus_handle.clone(),
        };
        window.focus(&handle, cx);
    }

    /// Opens a view for a session just made and sends `text` as its
    /// first prompt.
    fn open_new_session(
        &mut self,
        agent: Agent,
        cwd: PathBuf,
        session: Session,
        text: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<SessionView> {
        let id = session.id().clone();
        // Made here, so rho-ui's own from the start, even if the prompt fails.
        self.sidebar.update(cx, |sidebar, cx| sidebar.own(&id, cx));
        let workspace = WorkspaceRef::new(&self.workspaces, &cwd, cx);
        let view = cx
            .new(|cx| SessionView::new(agent, session, cwd, workspace, Loading::Ready, window, cx));
        self.insert_session(id, view.clone(), window, cx);
        view.update(cx, |view, cx| view.submit(text, window, cx));
        view
    }

    /// Opens a saved session, or switches to it if it is already open.
    /// `title` is the saved title: resuming does not replay it.
    pub(crate) fn open_session(
        &mut self,
        id: SessionId,
        cwd: PathBuf,
        title: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(open) = self.opened(&id) {
            self.show(Pane::Session(open.view.clone()), window, cx);
            return;
        }
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        // The session must exist before resume: history replays as events
        // before `resume_session` returns.
        let mut session = Session::new(id.clone());
        session.set_title(title.map(String::from));
        let workspace = WorkspaceRef::new(&self.workspaces, &cwd, cx);
        let view = cx.new(|cx| {
            SessionView::new(
                agent.clone(),
                session,
                cwd.clone(),
                workspace,
                Loading::Resuming,
                window,
                cx,
            )
        });
        self.insert_session(id.clone(), view.clone(), window, cx);
        self.show(Pane::Session(view.clone()), window, cx);

        let view = view.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let result: Result<SessionSetup, AgentError> = agent.resume_session(&id, &cwd).await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.finish_loading(result, window, cx);
            });
        })
        .detach();
    }

    /// Adds `view` to the open sessions.
    fn insert_session(
        &mut self,
        id: SessionId,
        view: Entity<SessionView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &SessionEvent, _, cx| match event {
                SessionEvent::LiveChanged => {
                    let live = view.read(cx).live();
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.set_live(live, cx));
                }
                SessionEvent::Prompted => {
                    let id = view.read(cx).id().clone();
                    this.sidebar.update(cx, |sidebar, cx| sidebar.own(&id, cx));
                }
                SessionEvent::Saved => this.refresh_sessions(cx),
            },
        );
        let live = view.read(cx).live();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_live(live, cx));
        self.sessions.push(OpenSession {
            id,
            view,
            _subscription: subscription,
        });
    }

    fn render_starting() -> impl IntoElement {
        Empty::new().size_full().header(
            EmptyHeader::new()
                .media(EmptyMedia::new().child(Spinner::new()))
                .description(EmptyDescription::new().child("Starting the agent…")),
        )
    }

    fn render_failed(&self, message: &str, cx: &mut Context<'_, Self>) -> impl IntoElement {
        Empty::new()
            .size_full()
            .header(
                EmptyHeader::new()
                    .title(EmptyTitle::new().text_lg().child("The agent isn't running"))
                    .description(
                        EmptyDescription::new()
                            .child(format!("Command: {}", self.launch.command_line)),
                    ),
            )
            .content(
                EmptyContent::new()
                    .max_w(rems(36.))
                    .child(
                        // The agent's own words: often a stack trace, so
                        // monospace and scrollable.
                        div()
                            .id("agent-error")
                            .w_full()
                            .max_h(rems(16.))
                            .overflow_y_scroll()
                            .p_3()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary)
                            .text_left()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(message.to_owned()),
                    )
                    .child(
                        Button::new("retry-agent")
                            .primary()
                            .icon(IconName::RotateCw)
                            .label("Retry")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.start_agent(window, cx)),
                            ),
                    ),
            )
    }

    /// The main pane when no session is open.
    fn render_no_session(cx: &mut Context<'_, Self>) -> impl IntoElement {
        Empty::new()
            .h_full()
            .header(
                EmptyHeader::new().description(
                    EmptyDescription::new()
                        .child("Open a session from the sidebar or start a new one."),
                ),
            )
            .content(
                EmptyContent::new().child(
                    Button::new("empty-new-session")
                        .outline()
                        .icon(IconName::Plus)
                        .label("New session")
                        .on_click(cx.listener(|this, _, window, cx| this.new_session(window, cx))),
                ),
            )
    }
}

#[cfg(test)]
impl RhoApp {
    /// The session the main pane shows, for UI tests.
    pub(crate) fn active_session(&self) -> Option<Entity<SessionView>> {
        match &self.pane {
            Pane::Session(view) => Some(view.clone()),
            Pane::Empty | Pane::Draft { .. } => None,
        }
    }
}

impl Render for RhoApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let _ = window;
        let root = div()
            .id("rho-app")
            .key_context("RhoApp")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &NewSession, window, cx| this.new_session(window, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings(window, cx);
            }))
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // First, so it sees scroll events before any scroll area.
            .child(Kinetic::hook(&self.kinetic));

        match &self.status {
            AgentStatus::Starting => return root.child(Self::render_starting()).into_any_element(),
            AgentStatus::Failed(message) => {
                let message = message.clone();
                return root
                    .child(self.render_failed(&message, cx))
                    .into_any_element();
            }
            AgentStatus::Ready(_) => {}
        }

        if let Some(settings) = &self.settings {
            return root.child(settings.view.clone()).into_any_element();
        }
        let pane_style = || StyleRefinement::default().flex_1().min_w_0().h_full();
        let main = match &self.pane {
            Pane::Session(view) => view.clone().cached(pane_style()).into_any_element(),
            Pane::Draft { view, .. } => view.clone().cached(pane_style()).into_any_element(),
            Pane::Empty => Self::render_no_session(cx).into_any_element(),
        };
        root.child(
            h_flex()
                .size_full()
                .items_stretch()
                .child(
                    self.sidebar.clone().cached(
                        StyleRefinement::default()
                            .w(rems(18.))
                            .h_full()
                            .flex_shrink_0(),
                    ),
                )
                .child(main),
        )
        .into_any_element()
    }
}
