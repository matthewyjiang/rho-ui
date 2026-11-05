//! The root view: owns the agent process, the event pump, and the open
//! threads. Everything the agent pushes enters here and is routed to the
//! [`ThreadView`] of its session.
//!
//! The sidebar and the thread view are drawn as cached views: each redraws
//! only when it changes, so a hover in one never rebuilds the other.

use std::path::PathBuf;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    notification::Notification,
    spinner::Spinner,
    v_flex, ActiveTheme as _, IconName, StyledExt as _, WindowExt as _,
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
    projects::{ProjectRef, Projects},
    shelf::Shelves,
    sidebar::{SidebarEvent, SidebarView},
    thread_view::{Loading, ThreadEvent, ThreadView},
    NewThread,
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
    /// Threads opened in this run. Only appended to, and cleared when the
    /// agent restarts, so `active` can be an index. A handful at most, so
    /// lookups by ID scan.
    threads: Vec<OpenThread>,
    active: Option<usize>,
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
            },
        );
        let mut app = Self {
            launch,
            status: AgentStatus::Starting,
            epoch: 0,
            projects,
            sidebar,
            threads: Vec::new(),
            active: None,
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
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_creating(0, cx));
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
        self.active = None;
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

    pub(crate) fn new_thread(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        let cwd = self.launch.workspace.clone();
        let epoch = self.epoch;
        self.creating += 1;
        let starting = self.creating;
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_creating(starting, cx));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = agent.new_session(&cwd).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.epoch != epoch {
                    return;
                }
                this.creating -= 1;
                let starting = this.creating;
                this.sidebar
                    .update(cx, |sidebar, cx| sidebar.set_creating(starting, cx));
                match result {
                    Ok(setup) => this.open_new_session(agent, cwd, setup, window, cx),
                    Err(error) => window.push_notification(
                        Notification::error(error.to_string()).title("Couldn't start a thread"),
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

    fn open_new_session(
        &mut self,
        agent: Agent,
        cwd: PathBuf,
        setup: SessionSetup,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let id = setup.id().clone();
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
        let ix = self.insert_thread(id, view, window, cx);
        self.activate(ix, window, cx);
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
        if let Some(ix) = self.threads.iter().position(|open| open.id == id) {
            self.activate(ix, window, cx);
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
        let ix = self.insert_thread(id.clone(), view.clone(), window, cx);
        self.activate(ix, window, cx);

        let view = view.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let result: Result<SessionSetup, AgentError> = agent.resume_session(&id, &cwd).await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.finish_loading(result, window, cx);
            });
        })
        .detach();
    }

    /// Adds `view` to the open threads and returns its index.
    fn insert_thread(
        &mut self,
        id: SessionId,
        view: Entity<ThreadView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> usize {
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &ThreadEvent, _, cx| match event {
                ThreadEvent::LiveChanged => {
                    let live = view.read(cx).live();
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.set_live(live, cx));
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
        self.threads.len() - 1
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        let open = &self.threads[ix];
        let handle = open.view.read(cx).composer_focus_handle(cx);
        window.focus(&handle, cx);
        let id = open.id.clone();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_active(Some(id), cx));
        self.active = Some(ix);
        cx.notify();
    }

    fn render_starting(cx: &mut Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .text_color(cx.theme().muted_foreground)
            .child(Spinner::new())
            .child(div().text_sm().child("Starting the agent…"))
    }

    fn render_failed(&self, message: &str, cx: &mut Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_8()
            .child(
                v_flex()
                    .max_w(rems(36.))
                    .gap_3()
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .child("The agent isn't running"),
                    )
                    .child(
                        div()
                            .id("agent-error")
                            .max_h(rems(16.))
                            .overflow_y_scroll()
                            .p_3()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary)
                            .text_sm()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(message.to_owned()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Command: {}", self.launch.command_line)),
                    )
                    .child(
                        h_flex().child(
                            Button::new("retry-agent")
                                .primary()
                                .icon(IconName::RotateCw)
                                .label("Retry")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.start_agent(window, cx)),
                                ),
                        ),
                    ),
            )
    }

    /// The main pane when no thread is open.
    fn render_no_thread(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .flex_1()
            .h_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Open a thread from the sidebar or start a new one."),
            )
            .child(
                Button::new("empty-new-thread")
                    .outline()
                    .icon(IconName::Plus)
                    .label("New thread")
                    .loading(self.creating > 0)
                    .on_click(cx.listener(|this, _, window, cx| this.new_thread(window, cx))),
            )
    }
}

#[cfg(test)]
impl RhoApp {
    /// The thread the main pane shows, for UI tests.
    pub(crate) fn active_thread(&self) -> Option<Entity<ThreadView>> {
        self.active.map(|ix| self.threads[ix].view.clone())
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
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground);

        match &self.status {
            AgentStatus::Starting => {
                return root.child(Self::render_starting(cx)).into_any_element()
            }
            AgentStatus::Failed(message) => {
                let message = message.clone();
                return root
                    .child(self.render_failed(&message, cx))
                    .into_any_element();
            }
            AgentStatus::Ready(_) => {}
        }

        let main = match self.active {
            Some(ix) => self.threads[ix]
                .view
                .clone()
                .cached(StyleRefinement::default().flex_1().min_w_0().h_full())
                .into_any_element(),
            None => self.render_no_thread(cx).into_any_element(),
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
