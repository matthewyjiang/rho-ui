//! The root view: owns the agent process, the event pump, and the open
//! threads. Everything the agent pushes enters here and is routed to the
//! [`ThreadView`] of its session.

use std::{collections::HashMap, path::PathBuf};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    notification::Notification,
    spinner::Spinner,
    status_bar::StatusBar,
    v_flex, ActiveTheme as _, IconName, StyledExt as _, WindowExt as _,
};
use gpui_kit::{
    div, rems, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window,
};
use rho_ui_core::{
    Agent, AgentCommand, AgentError, AgentEvent, SessionId, SessionSetup, SessionSummary, Thread,
    ThreadUpdate,
};

use crate::{
    sidebar::{self, SessionRow},
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
    view: Entity<ThreadView>,
    _subscription: Subscription,
}

pub(crate) struct RhoApp {
    launch: LaunchConfig,
    status: AgentStatus,
    /// Saved sessions, newest first, from the last `list_sessions`.
    sessions: Vec<SessionSummary>,
    sessions_error: Option<String>,
    threads: HashMap<SessionId, OpenThread>,
    active: Option<SessionId>,
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
}

impl RhoApp {
    pub(crate) fn new(
        launch: LaunchConfig,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let mut app = Self {
            launch,
            status: AgentStatus::Starting,
            sessions: Vec::new(),
            sessions_error: None,
            threads: HashMap::new(),
            active: None,
            creating: 0,
            early_updates: Vec::new(),
            focus_handle,
            agent_task: Task::ready(()),
            sessions_task: Task::ready(()),
        };
        app.start_agent(window, cx);
        app
    }

    /// (Re)starts the agent. Threads from a previous process are dropped:
    /// their sessions are not loaded in the new one.
    fn start_agent(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.threads.clear();
        self.active = None;
        self.creating = 0;
        self.early_updates.clear();
        self.sessions_error = None;
        self.status = AgentStatus::Starting;
        cx.notify();

        let command = self.launch.command.clone();
        self.agent_task = cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let started = Agent::start(command, |driver| executor.spawn(driver).detach()).await;
            let events = match started {
                Ok((agent, events)) => {
                    let stored = this.update(cx, |this, cx| {
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

    fn agent(&self) -> Option<&Agent> {
        match &self.status {
            AgentStatus::Ready(agent) => Some(agent),
            _ => None,
        }
    }

    fn route_event(&mut self, event: AgentEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        match event {
            AgentEvent::Update { session, update } => {
                if let Some(open) = self.threads.get(&session) {
                    open.view
                        .update(cx, |view, cx| view.apply(update, window, cx));
                } else if self.creating > 0 {
                    self.early_updates.push((session, update));
                }
            }
            AgentEvent::Permission(request) => {
                // Unknown sessions drop the request, which answers "cancelled".
                if let Some(open) = self.threads.get(request.session()) {
                    open.view
                        .update(cx, |view, cx| view.push_approval(request, window, cx));
                }
            }
            AgentEvent::Exited { error } => {
                self.threads.clear();
                self.active = None;
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
                match result {
                    Ok(mut sessions) => {
                        // RFC 3339 stamps from one agent sort as text.
                        sessions.sort_by(|a, b| b.updated_at().cmp(&a.updated_at()));
                        this.sessions = sessions;
                        this.sessions_error = None;
                    }
                    Err(error) => this.sessions_error = Some(error.to_string()),
                }
                cx.notify();
            });
        });
    }

    pub(crate) fn new_thread(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        let cwd = self.launch.workspace.clone();
        self.creating += 1;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = agent.new_session(&cwd).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.creating -= 1;
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
        let view = cx.new(|cx| ThreadView::new(agent, thread, cwd, Loading::Ready, window, cx));
        let early = std::mem::take(&mut self.early_updates);
        let (mine, others): (Vec<_>, Vec<_>) =
            early.into_iter().partition(|(session, _)| *session == id);
        self.early_updates = others;
        view.update(cx, |view, cx| {
            for (_, update) in mine {
                view.apply(update, window, cx);
            }
        });
        self.insert_thread(id.clone(), view, window, cx);
        self.activate(id, window, cx);
    }

    /// Opens a saved session, or switches to it if it is already open.
    pub(crate) fn open_session(
        &mut self,
        id: SessionId,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.threads.contains_key(&id) {
            self.activate(id, window, cx);
            return;
        }
        let Some(agent) = self.agent().cloned() else {
            return;
        };
        // The thread must exist before resume: history replays as events
        // before `resume_session` returns.
        let thread = Thread::new(id.clone());
        let view = cx.new(|cx| {
            ThreadView::new(
                agent.clone(),
                thread,
                cwd.clone(),
                Loading::Resuming,
                window,
                cx,
            )
        });
        self.insert_thread(id.clone(), view.clone(), window, cx);
        self.activate(id.clone(), window, cx);

        let view = view.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let result: Result<SessionSetup, AgentError> = agent.resume_session(&id, &cwd).await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.finish_loading(result, window, cx);
            });
        })
        .detach();
    }

    fn insert_thread(
        &mut self,
        id: SessionId,
        view: Entity<ThreadView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let subscription =
            cx.subscribe_in(
                &view,
                window,
                |this, _, event: &ThreadEvent, _, cx| match event {
                    ThreadEvent::SidebarChanged => cx.notify(),
                    ThreadEvent::Saved => this.refresh_sessions(cx),
                },
            );
        self.threads.insert(
            id,
            OpenThread {
                view,
                _subscription: subscription,
            },
        );
    }

    fn activate(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(open) = self.threads.get(&id) {
            let handle = open.view.read(cx).composer_focus_handle(cx);
            window.focus(&handle, cx);
        }
        self.active = Some(id);
        cx.notify();
    }

    /// Sidebar rows: threads opened in this run that the saved list does not
    /// have yet, then the saved sessions.
    fn session_rows(&self, cx: &App) -> Vec<SessionRow> {
        let now = std::time::SystemTime::now();
        let mut unsaved: Vec<SessionRow> = self
            .threads
            .iter()
            .filter(|(id, _)| !self.sessions.iter().any(|session| session.id() == *id))
            .map(|(id, open)| {
                let view = open.view.read(cx);
                SessionRow {
                    id: id.clone(),
                    cwd: view.cwd().to_path_buf(),
                    title: view.title().into(),
                    detail: sidebar::dir_name(view.cwd()).into(),
                    active: self.active.as_ref() == Some(id),
                    attention: view.attention(),
                }
            })
            .collect();
        unsaved.sort_by(|a, b| a.id.cmp(&b.id));
        unsaved.extend(self.sessions.iter().map(|session| {
            let open = self
                .threads
                .get(session.id())
                .map(|open| open.view.read(cx));
            let title = open
                .and_then(|view| view.live_title())
                .or(session.title())
                .unwrap_or("Untitled");
            SessionRow {
                id: session.id().clone(),
                cwd: session.cwd().to_path_buf(),
                title: title.to_owned().into(),
                detail: sidebar::session_detail(session, now).into(),
                active: self.active.as_ref() == Some(session.id()),
                attention: open
                    .map(super::thread_view::ThreadView::attention)
                    .unwrap_or_default(),
            }
        }));
        unsaved
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
    fn render_no_thread(&self, agent: &Agent, cx: &mut Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .flex_1()
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
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_thread(window, cx)),
                            ),
                    ),
            )
            .child(StatusBar::new().right(format!(
                "{} {}",
                agent.info().name(),
                agent.info().version()
            )))
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

        let agent = match &self.status {
            AgentStatus::Starting => {
                return root.child(Self::render_starting(cx)).into_any_element()
            }
            AgentStatus::Failed(message) => {
                let message = message.clone();
                return root
                    .child(self.render_failed(&message, cx))
                    .into_any_element();
            }
            AgentStatus::Ready(agent) => agent.clone(),
        };

        let rows = self.session_rows(cx);
        let sessions_error = self.sessions_error.clone();
        let main = match self.active.as_ref().and_then(|id| self.threads.get(id)) {
            Some(open) => open.view.clone().into_any_element(),
            None => self.render_no_thread(&agent, cx).into_any_element(),
        };

        root.child(
            h_flex()
                .size_full()
                .items_stretch()
                .child(sidebar::render(rows, self.creating, sessions_error, cx))
                .child(v_flex().flex_1().min_w_0().h_full().child(main)),
        )
        .into_any_element()
    }
}
