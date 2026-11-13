//! The root view: owns the daemon connection, the event pump, and the open
//! sessions. Everything the daemon pushes enters here and is routed to the
//! [`SessionView`] of its session.
//!
//! The sidebar and the session view are drawn as cached views: each redraws
//! only when it changes, so a hover in one never rebuilds the other.

use std::{path::PathBuf, rc::Rc, time::Duration};

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
    Attached, Daemon, DaemonError, DaemonEvent, DaemonTarget, Detached, Job, Session, SessionId,
};

use crate::{
    draft::{DraftEvent, DraftView},
    kinetic::Kinetic,
    session_view::{Loading, SessionEvent, SessionView},
    settings::{Appearance, Settings},
    settings_view,
    shelf::Shelves,
    sidebar::{SidebarEvent, SidebarView},
    workspaces::{WorkspaceRef, Workspaces},
    NewSession, NewSessionIn, OpenSettings,
};

/// How often the sidebar asks the daemon which sessions have work under
/// way. The daemon pushes no such list, so rho's terminal UI polls too, at
/// 500 ms while it shows jobs. Against a daemon built from rho's
/// `daemon-v3` (bf3af5b) holding one job, a `rho/job/list` round trip
/// took 0.18 ms at the median and 0.22 ms at the 99th percentile (n = 200),
/// so once a second is under a thousandth of the connection's time, and a
/// turn started in a terminal shows up here within a second. The list holds
/// every job the daemon has a record of, so the cost grows with a rho
/// home's history: measure again before polling faster.
const JOB_POLL: Duration = Duration::from_secs(1);

/// What the command line asked for.
pub(crate) struct LaunchConfig {
    /// Where new sessions start. Resumed sessions keep their own directory.
    pub(crate) workspace: PathBuf,
    pub(crate) daemon: DaemonTarget,
}

/// The daemon connection's lifecycle.
enum DaemonStatus {
    Connecting,
    Ready(Daemon),
    /// The daemon couldn't be reached, or the connection closed; the
    /// message is shown full-window.
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

/// A session the user opened in this run, with the subscription that keeps
/// the sidebar in step with it.
struct OpenSession {
    id: SessionId,
    view: Entity<SessionView>,
    _subscription: Subscription,
}

pub(crate) struct RhoApp {
    launch: LaunchConfig,
    status: DaemonStatus,
    /// Counts connections. Replies to requests made on an earlier one
    /// belong to sessions that are no longer open, and are dropped.
    epoch: u64,
    /// The daemon said it is restarting in place, so the connection's end
    /// is expected, and answered by connecting again.
    restarting: bool,
    workspaces: Entity<Workspaces>,
    /// Where the last new session started, for the next one.
    last_project: Option<PathBuf>,
    sidebar: Entity<SidebarView>,
    /// Sessions opened in this run. A handful at most, so lookups by ID scan.
    sessions: Vec<OpenSession>,
    pane: Pane,
    /// Smooth wheel scrolling and touchpad flings for the whole window.
    kinetic: Rc<Kinetic>,
    focus_handle: FocusHandle,
    /// Connects to the daemon, then pumps its events. Dropping or replacing
    /// it stops both.
    daemon_task: Task<()>,
    /// Polls the daemon's jobs while connected; see [`JOB_POLL`].
    jobs_task: Task<()>,
    /// The jobs as last polled, to tell when they change.
    jobs: Vec<Job>,
    /// The latest `list_sessions`; a newer refresh replaces it.
    sessions_task: Task<()>,
    _sidebar_subscription: Subscription,
    _appearance_subscription: Subscription,
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
                SidebarEvent::OpenSettings => settings_view::open(window, cx),
            },
        );
        let mut app = Self {
            launch,
            status: DaemonStatus::Connecting,
            epoch: 0,
            restarting: false,
            workspaces,
            last_project: None,
            sidebar,
            sessions: Vec::new(),
            pane: Pane::Empty,
            kinetic: Rc::default(),
            focus_handle,
            daemon_task: Task::ready(()),
            jobs_task: Task::ready(()),
            jobs: Vec::new(),
            sessions_task: Task::ready(()),
            _sidebar_subscription: sidebar_subscription,
            // With the system appearance, follow the desktop from light to
            // dark and back.
            _appearance_subscription: cx.observe_window_appearance(window, |_, _, cx| {
                let settings = cx.global::<Settings>().clone();
                if settings.appearance == Appearance::System {
                    settings.apply(cx);
                }
            }),
        };
        app.connect(window, cx);
        app
    }

    /// Connects to the daemon, starting it if none runs. Views of sessions
    /// followed on an earlier connection are dropped; the sessions
    /// themselves, and their turns, stay in the daemon.
    fn connect(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.forget_sessions(cx);
        self.epoch += 1;
        self.restarting = false;
        self.jobs_task = Task::ready(());
        self.status = DaemonStatus::Connecting;
        cx.notify();

        let target = self.launch.daemon.clone();
        self.daemon_task = cx.spawn_in(window, async move |this, cx| {
            let mut events = match Daemon::connect(target).await {
                Ok((daemon, events)) => {
                    let stored =
                        this.update_in(cx, |this, window, cx| this.connected(daemon, window, cx));
                    if stored.is_err() {
                        return;
                    }
                    events
                }
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.status = DaemonStatus::Failed(error.to_string());
                        cx.notify();
                    });
                    return;
                }
            };
            // The one event pump: every daemon push is routed from here.
            while let Some(event) = events.recv().await {
                let routed =
                    this.update_in(cx, |this, window, cx| this.route_event(event, window, cx));
                if routed.is_err() {
                    return;
                }
            }
            let _ = this.update_in(cx, RhoApp::disconnected);
        });
    }

    fn connected(&mut self, daemon: Daemon, window: &mut Window, cx: &mut Context<'_, Self>) {
        let info = daemon.info();
        let label = format!("rho {}", info.version());
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_version(label, cx));
        if let Some(notice) = info.notice() {
            window.push_notification(
                Notification::warning(notice.to_owned()).title("rho daemon is another build"),
                cx,
            );
        }
        self.jobs_task = cx.spawn({
            let daemon = daemon.clone();
            async move |this, cx| loop {
                match daemon.active_jobs().await {
                    Ok(jobs) => {
                        if this.update(cx, |this, cx| this.set_jobs(jobs, cx)).is_err() {
                            return;
                        }
                    }
                    // The event pump reports the connection's end.
                    Err(DaemonError::Closed) => return,
                    // One refused listing is no reason to stop asking.
                    Err(_) => {}
                }
                cx.background_executor().timer(JOB_POLL).await;
            }
        });
        self.status = DaemonStatus::Ready(daemon);
        self.refresh_sessions(cx);
        cx.notify();
    }

    /// Shows which sessions have work under way in the daemon. A change is
    /// also when the saved list goes stale: a session another client
    /// started appears, or one's turn ended.
    fn set_jobs(&mut self, jobs: Vec<Job>, cx: &mut Context<'_, Self>) {
        let same = self.jobs.len() == jobs.len()
            && self
                .jobs
                .iter()
                .zip(&jobs)
                .all(|(was, now)| was.is_same_work(now));
        if same {
            return;
        }
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_jobs(&jobs, cx));
        self.jobs = jobs;
        self.refresh_sessions(cx);
    }

    /// Drops every open session's view, as the connection that followed
    /// them is gone.
    fn forget_sessions(&mut self, cx: &mut Context<'_, Self>) {
        self.sessions.clear();
        self.jobs.clear();
        self.set_pane(Pane::Empty, cx);
        self.sidebar.update(cx, SidebarView::clear_live);
    }

    /// The session `id`, if it was opened in this run.
    fn opened(&self, id: &SessionId) -> Option<&OpenSession> {
        self.sessions.iter().find(|open| open.id == *id)
    }

    fn daemon(&self) -> Option<&Daemon> {
        match &self.status {
            DaemonStatus::Ready(daemon) => Some(daemon),
            _ => None,
        }
    }

    fn route_event(&mut self, event: DaemonEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        match event {
            DaemonEvent::Update { session, update } => {
                if let Some(open) = self.opened(&session) {
                    open.view
                        .update(cx, |view, cx| view.apply(update, window, cx));
                } else if let Some(draft) = self.draft_with(&session, cx) {
                    draft.update(cx, |draft, cx| draft.apply(update, window, cx));
                }
            }
            DaemonEvent::Detached { session, reason } => match reason {
                Detached::Behind => self.reopen_session(&session, window, cx),
                Detached::Deleted => self.session_deleted(&session, window, cx),
                Detached::Restarting => self.restarting = true,
            },
        }
    }

    /// The connection ended. After the daemon said it was restarting, that
    /// is expected, and answered by connecting again: the daemon restarts in
    /// place on the same socket, and the handshake waits until it answers.
    fn disconnected(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.restarting {
            self.connect(window, cx);
        } else {
            self.forget_sessions(cx);
            self.status = DaemonStatus::Failed(DaemonError::Closed.to_string());
            cx.notify();
        }
    }

    /// Reads session `id` afresh after the daemon stopped sending its
    /// events: a new view replaces the one that fell behind.
    fn reopen_session(&mut self, id: &SessionId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(open) = self.opened(id) else {
            return;
        };
        let live = open.view.read(cx).live();
        let shown = matches!(&self.pane, Pane::Session(view) if *view == open.view);
        self.forget_session(id, window, cx);
        if shown {
            self.open_session(live.id, live.cwd, live.title, window, cx);
        } else {
            // Its row follows the daemon's jobs again until it is reopened.
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.close_live(id, cx));
        }
    }

    /// Drops a session that is gone from the daemon.
    fn session_deleted(&mut self, id: &SessionId, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.forget_session(id, window, cx);
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.remove(id, cx));
        self.refresh_sessions(cx);
    }

    /// Reloads the saved-session list in the background.
    fn refresh_sessions(&mut self, cx: &mut Context<'_, Self>) {
        let Some(daemon) = self.daemon().cloned() else {
            return;
        };
        self.sessions_task = cx.spawn(async move |this, cx| {
            let result = daemon.list_sessions().await;
            let _ = this.update(cx, |this, cx| {
                this.sidebar.update(cx, |sidebar, cx| match result {
                    Ok(sessions) => sidebar.set_saved(sessions, cx),
                    Err(error) => sidebar.set_error(error.to_string(), cx),
                });
            });
        });
    }

    /// Where a new session starts: in the project of the session on screen,
    /// else where the last new session started, else the launch directory.
    fn new_session_cwd(&self, cx: &App) -> PathBuf {
        match &self.pane {
            Pane::Session(view) => view.read(cx).live().cwd,
            Pane::Empty | Pane::Draft { .. } => self
                .last_project
                .clone()
                .unwrap_or_else(|| self.launch.workspace.clone()),
        }
    }

    /// Shows a draft for a new session and opens the session right away; see
    /// [`DraftView`].
    pub(crate) fn new_session(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.daemon().is_none() {
            return;
        }
        let cwd = self.new_session_cwd(cx);
        // An open draft is reused, so its text and settings survive.
        let pane = std::mem::replace(&mut self.pane, Pane::Empty);
        let pane = match pane {
            Pane::Draft { .. } => pane,
            Pane::Empty | Pane::Session(_) => {
                let workspaces = &self.workspaces;
                let view = cx.new(|cx| DraftView::new(cwd, workspaces, window, cx));
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
                        DraftEvent::OpenSession => this.open_draft_session(draft, window, cx),
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

    /// Shows a new session's draft with its project list open.
    fn new_session_in(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.new_session(window, cx);
        if let Pane::Draft { view, .. } = &self.pane {
            view.update(cx, |draft, cx| draft.choose_project(window, cx));
        }
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

    /// Opens the session for `draft` and follows it. The draft takes the
    /// session before the attach is sent, so the events that follow the
    /// attach always have somewhere to go.
    fn open_draft_session(
        &mut self,
        draft: &Entity<DraftView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(daemon) = self.daemon().cloned() else {
            return;
        };
        let (cwd, request) = draft.read(cx).request();
        let draft = draft.downgrade();
        let epoch = self.epoch;
        cx.spawn_in(window, async move |this, cx| {
            let setup = daemon.new_session(&cwd).await;
            let opened = this.update_in(cx, |this, window, cx| {
                // A draft dropped meanwhile, or moved to another project,
                // leaves the session unfollowed, and the daemon discards one
                // that saved nothing.
                let draft = draft
                    .upgrade()
                    .filter(|draft| this.epoch == epoch && draft.read(cx).awaits(request))?;
                match setup {
                    Ok(setup) => {
                        let session = Session::from_setup(setup);
                        let id = session.id().clone();
                        let daemon = daemon.clone();
                        draft.update(cx, |draft, cx| draft.open(daemon, session, window, cx));
                        Some(id)
                    }
                    Err(error) => {
                        draft.update(cx, |draft, cx| draft.fail(error.to_string(), cx));
                        None
                    }
                }
            });
            let Ok(Some(id)) = opened else {
                return;
            };
            let attached = daemon.attach(&id).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.attached(&daemon, &id, attached, window, cx);
            });
        })
        .detach();
    }

    /// Hands the answer to an attach to whoever holds session `id` now: its
    /// draft, or its view once the first message was sent. With neither
    /// left, nothing follows the session, so the daemon is told.
    fn attached(
        &mut self,
        daemon: &Daemon,
        id: &SessionId,
        attached: Result<Attached, DaemonError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(draft) = self.draft_with(id, cx) {
            draft.update(cx, |draft, cx| draft.attached(attached, window, cx));
        } else if let Some(open) = self.opened(id) {
            open.view
                .update(cx, |view, cx| view.attached(attached, window, cx));
        } else if attached.is_ok() {
            let (daemon, id) = (daemon.clone(), id.clone());
            cx.spawn(async move |_, _| {
                let _ = daemon.detach(&id).await;
            })
            .detach();
        }
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
        let Some(daemon) = self.daemon().cloned() else {
            return;
        };
        let cwd = draft.read(cx).cwd().to_path_buf();
        self.last_project = Some(cwd.clone());
        let view = self.open_new_session(daemon, cwd, session, text, window, cx);
        self.set_pane(Pane::Session(view), cx);
        self.focus_pane(window, cx);
    }

    /// Deletes a saved session in the daemon, then drops it here.
    fn delete_session(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(daemon) = self.daemon().cloned() else {
            return;
        };
        // The daemon refuses to delete a session a client follows, so that
        // one open in another client survives. This client lets go first.
        let open = self.opened(&id).is_some();
        cx.spawn_in(window, async move |this, cx| {
            if open {
                let _ = daemon.detach(&id).await;
            }
            let result = daemon.delete_session(&id).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => this.session_deleted(&id, window, cx),
                Err(error) => {
                    window.push_notification(
                        Notification::error(error.to_string()).title("Couldn't delete the session"),
                        cx,
                    );
                    // Its view no longer follows it: read it afresh.
                    this.reopen_session(&id, window, cx);
                }
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

    /// Shows `pane` in the main area and puts the keyboard in it.
    fn show(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<'_, Self>) {
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

    /// Puts the keyboard in the main pane's composer, unless a dialog such
    /// as settings is open: it keeps the keyboard until it closes.
    fn focus_pane(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
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
        daemon: Daemon,
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
        let view = cx.new(|cx| {
            SessionView::new(daemon, session, cwd, workspace, Loading::Ready, window, cx)
        });
        self.insert_session(id, view.clone(), window, cx);
        view.update(cx, |view, cx| view.submit(text, window, cx));
        view
    }

    /// Opens a saved session, or switches to it if it is already open.
    /// `title` is the listed title, shown until the daemon answers.
    pub(crate) fn open_session(
        &mut self,
        id: SessionId,
        cwd: PathBuf,
        title: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match self.opened(&id).map(|open| open.view.clone()) {
            // A click on a session that couldn't be opened tries again.
            Some(view) if view.read(cx).failed_to_attach() => {
                self.forget_session(&id, window, cx);
            }
            Some(view) => {
                self.show(Pane::Session(view), window, cx);
                return;
            }
            None => {}
        }
        let Some(daemon) = self.daemon().cloned() else {
            return;
        };
        // The session must exist before the attach: the conversation
        // replays as events that may arrive before the attach's answer.
        let mut session = Session::new(id.clone());
        session.set_title(title.map(String::from));
        let workspace = WorkspaceRef::new(&self.workspaces, &cwd, cx);
        let view = cx.new(|cx| {
            SessionView::new(
                daemon.clone(),
                session,
                cwd,
                workspace,
                Loading::Attaching,
                window,
                cx,
            )
        });
        self.insert_session(id.clone(), view.clone(), window, cx);
        self.show(Pane::Session(view), window, cx);

        cx.spawn_in(window, async move |this, cx| {
            let attached = daemon.attach(&id).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.attached(&daemon, &id, attached, window, cx);
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

    fn render_connecting() -> impl IntoElement {
        Empty::new().size_full().header(
            EmptyHeader::new()
                .media(EmptyMedia::new().child(Spinner::new()))
                .description(EmptyDescription::new().child("Connecting to the rho daemon…")),
        )
    }

    fn render_failed(&self, message: &str, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let reached = match &self.launch.daemon {
            DaemonTarget::Home { home, rho } => format!(
                "Rho home: {}, started with: {} daemon start",
                home.display(),
                rho.display()
            ),
            DaemonTarget::Socket(socket) => format!("Socket: {}", socket.display()),
        };
        Empty::new()
            .size_full()
            .header(
                EmptyHeader::new()
                    .title(
                        EmptyTitle::new()
                            .text_lg()
                            .child("Can't reach the rho daemon"),
                    )
                    .description(EmptyDescription::new().child(reached)),
            )
            .content(
                EmptyContent::new()
                    .max_w(rems(36.))
                    .child(
                        // rho's own words: sometimes long, so monospace and
                        // scrollable.
                        div()
                            .id("daemon-error")
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
                        Button::new("retry-daemon")
                            .primary()
                            .icon(IconName::RotateCw)
                            .label("Retry")
                            .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
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
    /// The new session's draft the main pane shows, for UI tests.
    pub(crate) fn draft(&self) -> Option<Entity<DraftView>> {
        match &self.pane {
            Pane::Draft { view, .. } => Some(view.clone()),
            Pane::Empty | Pane::Session(_) => None,
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
            .on_action(cx.listener(|this, _: &NewSessionIn, window, cx| {
                this.new_session_in(window, cx);
            }))
            .on_action(|_: &OpenSettings, window, cx| settings_view::open(window, cx))
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // First, so it sees scroll events before any scroll area.
            .child(Kinetic::hook(&self.kinetic));

        match &self.status {
            DaemonStatus::Connecting => {
                return root.child(Self::render_connecting()).into_any_element();
            }
            DaemonStatus::Failed(message) => {
                let message = message.clone();
                return root
                    .child(self.render_failed(&message, cx))
                    .into_any_element();
            }
            DaemonStatus::Ready(_) => {}
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
