//! The root view: owns the agent process, the event pump, and the open
//! threads. Everything the agent pushes enters here and is routed to the
//! [`ThreadView`] of its session.
//!
//! The sidebar and the thread view are drawn as cached views: each redraws
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
    div, rems, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, StyleRefinement,
    Styled as _, Subscription, Task, Window,
};
use rho_ui_core::{
    Agent, AgentCommand, AgentError, AgentEvent, SessionId, SessionSetup, Thread, ThreadUpdate,
};

use crate::{
    draft::{DraftView, Start},
    kinetic::Kinetic,
    projects::{ProjectRef, Projects},
    settings_view::{Closed, SettingsView},
    shelf::Shelves,
    sidebar::{SidebarEvent, SidebarView},
    thread_view::{Loading, ThreadEvent, ThreadView},
    NewThread, OpenSettings,
};

/// What the command line asked for.
pub(crate) struct LaunchConfig {
    /// Where new threads start. Resumed threads keep their own directory.
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
    /// A new thread before its first message. Switching away drops it.
    Draft {
        view: Entity<DraftView>,
        _subscription: Subscription,
    },
    /// One of [`RhoApp::threads`].
    Thread(Entity<ThreadView>),
}

/// The settings screen, while open.
struct SettingsScreen {
    view: Entity<SettingsView>,
    _subscription: Subscription,
}

/// A thread the user opened in this run, with the subscription that keeps
/// the sidebar in step with it.
struct OpenThread {
    id: SessionId,
    view: Entity<ThreadView>,
    _subscription: Subscription,
}

pub(crate) struct RhoApp {
    launch: LaunchConfig,
    status: AgentStatus,
    /// Counts agent starts. Replies to requests made before a restart
    /// belong to the old process and are dropped.
    epoch: u64,
    projects: Entity<Projects>,
    sidebar: Entity<SidebarView>,
    /// Threads opened in this run. A handful at most, so lookups by ID scan.
    threads: Vec<OpenThread>,
    pane: Pane,
    settings: Option<SettingsScreen>,
    /// Smooth wheel scrolling and touchpad flings for the whole window.
    kinetic: Rc<Kinetic>,
    /// `new_session` calls in flight. While any are, updates for unknown
    /// sessions are kept, since they may belong to the session being made.
    creating: usize,
    early_updates: Vec<(SessionId, ThreadUpdate)>,
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
        let projects = cx.new(Projects::new);
        let sidebar = cx.new(|cx| SidebarView::new(projects.clone(), shelves, window, cx));
        let sidebar_subscription = cx.subscribe_in(
            &sidebar,
            window,
            |this, _, event: &SidebarEvent, window, cx| match event {
                SidebarEvent::Open { id, cwd, title } => {
                    this.open_session(id.clone(), cwd.clone(), title.clone(), window, cx);
                }
                SidebarEvent::NewThread => this.new_thread(window, cx),
                SidebarEvent::Delete { id } => this.delete_thread(id.clone(), window, cx),
                SidebarEvent::OpenSettings => this.open_settings(window, cx),
            },
        );
        let mut app = Self {
            launch,
            status: AgentStatus::Starting,
            epoch: 0,
            projects,
            sidebar,
            threads: Vec::new(),
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

    /// (Re)starts the agent. Threads from a previous process are dropped:
    /// their sessions are not loaded in the new one.
    fn start_agent(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.forget_threads(cx);
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

    /// Drops every open thread: their sessions are gone with the agent.
    fn forget_threads(&mut self, cx: &mut Context<'_, Self>) {
        self.threads.clear();
        self.set_pane(Pane::Empty, cx);
        self.sidebar.update(cx, SidebarView::clear_live);
    }

    fn thread(&self, id: &SessionId) -> Option<&OpenThread> {
        self.threads.iter().find(|open| open.id == *id)
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
                if let Some(open) = self.thread(&session) {
                    open.view
                        .update(cx, |view, cx| view.apply(update, window, cx));
                } else if self.creating > 0 {
                    self.early_updates.push((session, update));
                }
            }
            AgentEvent::Permission(request) => {
                // Unknown sessions drop the request, which answers "cancelled".
                if let Some(open) = self.thread(request.session()) {
                    open.view
                        .update(cx, |view, cx| view.push_approval(request, window, cx));
                }
            }
            AgentEvent::Exited { error } => {
                self.forget_threads(cx);
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

    /// Shows a draft for a new thread. The session starts with its first
    /// message; see [`Self::start_thread`].
    pub(crate) fn new_thread(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.agent().is_none() {
            return;
        }
        // An open draft is reused, so its text survives.
        let pane = std::mem::replace(&mut self.pane, Pane::Empty);
        let pane = match pane {
            Pane::Draft { .. } => pane,
            Pane::Empty | Pane::Thread(_) => {
                let cwd = self.launch.workspace.clone();
                let project = ProjectRef::new(&self.projects, &cwd, cx);
                let view = cx.new(|cx| DraftView::new(cwd, project, window, cx));
                let subscription =
                    cx.subscribe_in(&view, window, |this, draft, start: &Start, window, cx| {
                        this.start_thread(draft.clone(), start.text.clone(), window, cx);
                    });
                Pane::Draft {
                    view,
                    _subscription: subscription,
                }
            }
        };
        self.show(pane, window, cx);
    }

    /// Makes the session for `draft` and sends its first message.
    fn start_thread(
        &mut self,
        draft: Entity<DraftView>,
        text: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        let cwd = draft.read(cx).cwd().to_path_buf();
        let epoch = self.epoch;
        self.creating += 1;
        cx.spawn_in(window, async move |this, cx| {
            let result = agent.new_session(&cwd).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.epoch != epoch {
                    return;
                }
                this.creating -= 1;
                // The user may have moved on while it started.
                let shown = matches!(&this.pane, Pane::Draft { view, .. } if *view == draft);
                match result {
                    Ok(setup) => {
                        let view = this.open_new_session(agent, cwd, setup, text, window, cx);
                        if shown {
                            this.set_pane(Pane::Thread(view), cx);
                            // Behind the settings screen, the keyboard stays there.
                            if this.settings.is_none() {
                                this.focus_pane(window, cx);
                            }
                        }
                    }
                    Err(error) if shown => {
                        draft.update(cx, |draft, cx| draft.fail(error.to_string(), cx));
                    }
                    Err(error) => window.push_notification(
                        Notification::error(format!("{error}\n\nYour message: {text}"))
                            .title("Couldn't start the thread"),
                        cx,
                    ),
                }
                if this.creating == 0 {
                    this.early_updates.clear();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Deletes a saved thread in the agent, then drops it here.
    fn delete_thread(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = agent.delete_session(&id).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.close_thread(&id, window, cx);
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.remove(&id, cx));
                    this.refresh_sessions(cx);
                }
                Err(error) => window.push_notification(
                    Notification::error(error.to_string()).title("Couldn't delete the thread"),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Drops the open thread `id`, if any, and empties the pane if it
    /// showed it.
    fn close_thread(&mut self, id: &SessionId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(ix) = self.threads.iter().position(|open| open.id == *id) else {
            return;
        };
        let closed = self.threads.remove(ix);
        if matches!(&self.pane, Pane::Thread(view) if *view == closed.view) {
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
            Pane::Thread(view) => Some(view.read(cx).id().clone()),
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
            Pane::Thread(view) => view.read(cx).composer_focus_handle(cx),
            Pane::Empty => self.focus_handle.clone(),
        };
        window.focus(&handle, cx);
    }

    /// Opens the thread for a session just made and sends `text` as its
    /// first prompt.
    fn open_new_session(
        &mut self,
        agent: Agent,
        cwd: PathBuf,
        setup: SessionSetup,
        text: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<ThreadView> {
        let id = setup.id().clone();
        // Made here, so rho-ui's own from the start, even if the prompt fails.
        self.sidebar.update(cx, |sidebar, cx| sidebar.own(&id, cx));
        let mut thread = Thread::new(id.clone());
        thread.set_setup(setup);
        let project = ProjectRef::new(&self.projects, &cwd, cx);
        let view =
            cx.new(|cx| ThreadView::new(agent, thread, cwd, project, Loading::Ready, window, cx));
        let early = std::mem::take(&mut self.early_updates);
        let (mine, others): (Vec<_>, Vec<_>) =
            early.into_iter().partition(|(session, _)| *session == id);
        self.early_updates = others;
        view.update(cx, |view, cx| {
            for (_, update) in mine {
                view.apply(update, window, cx);
            }
        });
        self.insert_thread(id, view.clone(), window, cx);
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
        if let Some(open) = self.thread(&id) {
            self.show(Pane::Thread(open.view.clone()), window, cx);
            return;
        }
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        // The thread must exist before resume: history replays as events
        // before `resume_session` returns.
        let mut thread = Thread::new(id.clone());
        thread.set_title(title.map(String::from));
        let project = ProjectRef::new(&self.projects, &cwd, cx);
        let view = cx.new(|cx| {
            ThreadView::new(
                agent.clone(),
                thread,
                cwd.clone(),
                project,
                Loading::Resuming,
                window,
                cx,
            )
        });
        self.insert_thread(id.clone(), view.clone(), window, cx);
        self.show(Pane::Thread(view.clone()), window, cx);

        let view = view.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let result: Result<SessionSetup, AgentError> = agent.resume_session(&id, &cwd).await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.finish_loading(result, window, cx);
            });
        })
        .detach();
    }

    /// Adds `view` to the open threads.
    fn insert_thread(
        &mut self,
        id: SessionId,
        view: Entity<ThreadView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &ThreadEvent, _, cx| match event {
                ThreadEvent::LiveChanged => {
                    let live = view.read(cx).live();
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.set_live(live, cx));
                }
                ThreadEvent::Prompted => {
                    let id = view.read(cx).id().clone();
                    this.sidebar.update(cx, |sidebar, cx| sidebar.own(&id, cx));
                }
                ThreadEvent::Saved => this.refresh_sessions(cx),
            },
        );
        let live = view.read(cx).live();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_live(live, cx));
        self.threads.push(OpenThread {
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

    /// The main pane when no thread is open.
    fn render_no_thread(cx: &mut Context<'_, Self>) -> impl IntoElement {
        Empty::new()
            .h_full()
            .header(EmptyHeader::new().description(
                EmptyDescription::new().child("Open a thread from the sidebar or start a new one."),
            ))
            .content(
                EmptyContent::new().child(
                    Button::new("empty-new-thread")
                        .outline()
                        .icon(IconName::Plus)
                        .label("New thread")
                        .on_click(cx.listener(|this, _, window, cx| this.new_thread(window, cx))),
                ),
            )
    }
}

#[cfg(test)]
impl RhoApp {
    /// The thread the main pane shows, for UI tests.
    pub(crate) fn active_thread(&self) -> Option<Entity<ThreadView>> {
        match &self.pane {
            Pane::Thread(view) => Some(view.clone()),
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
            .on_action(cx.listener(|this, _: &NewThread, window, cx| this.new_thread(window, cx)))
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
            Pane::Thread(view) => view.clone().cached(pane_style()).into_any_element(),
            Pane::Draft { view, .. } => view.clone().cached(pane_style()).into_any_element(),
            Pane::Empty => Self::render_no_thread(cx).into_any_element(),
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
