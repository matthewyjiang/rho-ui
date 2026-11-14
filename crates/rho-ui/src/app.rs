//! The root view: owns a connection to each machine's daemon, their event
//! pumps, and the open sessions. Everything a daemon pushes enters here and
//! is routed to the [`SessionView`] of its session. The open sessions
//! include subagents' sessions opened from their parent, which the sidebar
//! does not list: they show nested under the parent, and go when it goes.
//!
//! The sidebar and the session view are drawn as cached views: each redraws
//! only when it changes, so a hover in one never rebuilds the other.

use std::{path::PathBuf, rc::Rc, time::Duration};

use gpui_kit::component::{
    button::Button,
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader},
    h_flex,
    notification::Notification,
    ActiveTheme as _, IconName, WindowExt as _,
};
use gpui_kit::{
    div, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StyleRefinement, Styled as _, Subscription, Task,
    Window,
};
use rho_ui_core::{
    Attached, Checkout, ConfigOption, Daemon, DaemonError, DaemonEvent, DaemonTarget, Detached,
    Job, Prompt, Session, SessionId,
};

use crate::{
    background_log::BackgroundLog,
    draft::{DraftEvent, DraftView},
    kinetic::Kinetic,
    machines::{Machine, MachineId, MachineStatus, Machines, Place, RemoteMachine, SessionKey},
    projects::Projects,
    rho_settings::RhoSettings,
    session_view::{Loading, SessionEvent, SessionView},
    settings::{Appearance, Settings},
    settings_view,
    sidebar::{SidebarEvent, SidebarState, SidebarView},
    workspaces::{WorkspaceRef, Workspaces},
    NewSession, NewSessionIn, OpenSettings, ToggleSidebar,
};

/// How often the sidebar asks each daemon which sessions have work under
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
    /// Where new sessions start on this machine. Resumed sessions keep
    /// their own directory.
    pub(crate) workspace: PathBuf,
    /// How to reach this machine's daemon.
    pub(crate) daemon: DaemonTarget,
    /// How to reach a remote machine's: [`RemoteMachine::ssh_target`],
    /// except in tests, which reach fake daemons.
    pub(crate) remote: Rc<dyn Fn(&RemoteMachine) -> DaemonTarget>,
}

impl LaunchConfig {
    /// Every machine `settings` asks for, this one first.
    fn machines(&self, settings: &Settings) -> Vec<Machine> {
        let local = Machine {
            id: MachineId::local(),
            label: settings.local_label.clone(),
            target: self.daemon.clone(),
            clone_folder: settings.local_clone_folder.clone().into(),
        };
        let remote = settings.machines.iter().map(|remote| Machine {
            id: remote.id(),
            label: remote.shown_label().to_owned().into(),
            target: (self.remote)(remote),
            clone_folder: remote.clone_folder.clone().into(),
        });
        std::iter::once(local).chain(remote).collect()
    }
}

/// The connection to one machine's daemon. How it is doing, as the views
/// show it, is in [`Machines`].
struct Connection {
    machine: MachineId,
    /// While connected.
    daemon: Option<Daemon>,
    /// Which connection this is, out of every one this run made. Replies
    /// to requests made on an earlier one belong to sessions that are no
    /// longer open, and are dropped.
    epoch: u64,
    /// The daemon said it is restarting in place, so the connection's end
    /// is expected, and answered by connecting again.
    restarting: bool,
    /// Connects to the daemon, then pumps its events. Dropping or replacing
    /// it stops both.
    task: Task<()>,
    /// Polls the daemon's jobs while connected; see [`JOB_POLL`].
    jobs_task: Task<()>,
    /// The jobs as last polled, to tell when they change.
    jobs: Vec<Job>,
    /// The latest `list_sessions`; a newer refresh replaces it.
    sessions_task: Task<()>,
}

impl Connection {
    fn new(machine: MachineId) -> Self {
        Self {
            machine,
            daemon: None,
            epoch: 0,
            restarting: false,
            task: Task::ready(()),
            jobs_task: Task::ready(()),
            jobs: Vec::new(),
            sessions_task: Task::ready(()),
        }
    }
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
    id: SessionKey,
    view: Entity<SessionView>,
    _subscription: Subscription,
}

pub(crate) struct RhoApp {
    launch: LaunchConfig,
    /// The machines the settings ask for, shared with the views that label
    /// things with them.
    machines: Entity<Machines>,
    /// One per machine, in the machines' order.
    connections: Vec<Connection>,
    /// rho's own settings on each connected machine, for Settings.
    rho_settings: Entity<RhoSettings>,
    /// The epoch the next connection gets; see [`Connection::epoch`].
    next_epoch: u64,
    workspaces: Entity<Workspaces>,
    /// The projects a new session can start in.
    projects: Entity<Projects>,
    /// Where the last new session started, for the next one.
    last_project: Option<Place>,
    sidebar: Entity<SidebarView>,
    /// Sessions opened in this run. A handful at most, so lookups by ID scan.
    sessions: Vec<OpenSession>,
    pane: Pane,
    /// Smooth wheel scrolling and touchpad flings for the whole window.
    kinetic: Rc<Kinetic>,
    focus_handle: FocusHandle,
    _subscriptions: [Subscription; 3],
}

impl RhoApp {
    /// `sidebar` holds what the sidebar remembers, and `projects` the
    /// user's projects; see [`SidebarState::load`] and [`Projects::load`].
    /// The machines come from the [`Settings`], and follow them as they
    /// change.
    pub(crate) fn new(
        launch: LaunchConfig,
        sidebar: SidebarState,
        projects: Projects,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let workspaces = cx.new(Workspaces::new);
        let projects = cx.new(|_| projects);
        // Empty until `sync_machines` connects to each.
        let machines = cx.new(|_| Machines::default());
        let sidebar = cx.new(|cx| {
            SidebarView::new(
                workspaces.clone(),
                machines.clone(),
                projects.clone(),
                sidebar,
                window,
                cx,
            )
        });
        let sidebar_subscription = cx.subscribe_in(
            &sidebar,
            window,
            |this, _, event: &SidebarEvent, window, cx| match event {
                SidebarEvent::Open { id, cwd, title } => {
                    this.open_session(id.clone(), cwd.clone(), title.clone(), window, cx);
                }
                SidebarEvent::NewSession => this.new_session(window, cx),
                SidebarEvent::Delete { id } => this.delete_session(id.clone(), window, cx),
                SidebarEvent::Retry(machine) => this.connect(machine, window, cx),
                SidebarEvent::OpenSettings => this.open_settings(window, cx),
                SidebarEvent::ToggleCollapsed => this.toggle_sidebar(window, cx),
            },
        );
        let mut app = Self {
            launch,
            machines,
            connections: Vec::new(),
            rho_settings: cx.new(|_| RhoSettings::default()),
            next_epoch: 0,
            workspaces,
            projects,
            last_project: None,
            sidebar,
            sessions: Vec::new(),
            pane: Pane::Empty,
            kinetic: Rc::default(),
            focus_handle,
            _subscriptions: [
                sidebar_subscription,
                // With the system appearance, follow the desktop from light
                // to dark and back.
                cx.observe_window_appearance(window, |_, _, cx| {
                    let settings = cx.global::<Settings>().clone();
                    if settings.appearance == Appearance::System {
                        settings.apply(cx);
                    }
                }),
                // Machines added, removed or changed in the settings.
                cx.observe_global_in::<Settings>(window, |this, window, cx| {
                    this.sync_machines(window, cx);
                }),
            ],
        };
        app.sync_machines(window, cx);
        app
    }

    /// Follows the machines the settings ask for: connects to new ones and
    /// to ones whose address changed, and drops the sessions of ones that
    /// are gone.
    fn sync_machines(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let wanted = self.launch.machines(cx.global::<Settings>());
        let known = self.machines.read(cx);
        if known.iter().eq(wanted.iter()) {
            return;
        }
        let stale: Vec<MachineId> = wanted
            .iter()
            .filter(|machine| {
                known
                    .get(&machine.id)
                    .is_none_or(|known| known.target != machine.target)
            })
            .map(|machine| machine.id.clone())
            .collect();
        // In the settings' order; what is left over is gone.
        let mut old = std::mem::take(&mut self.connections);
        for machine in &wanted {
            let kept = old
                .iter()
                .position(|connection| connection.machine == machine.id)
                .map(|ix| old.remove(ix));
            self.connections
                .push(kept.unwrap_or_else(|| Connection::new(machine.id.clone())));
        }
        for gone in old {
            self.rho_settings
                .update(cx, |settings, cx| settings.forget(&gone.machine, cx));
            self.forget_sessions(&gone.machine, window, cx);
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.remove_machine(&gone.machine, cx));
        }
        self.machines.update(cx, |machines, cx| {
            machines.set(wanted);
            cx.notify();
        });
        for machine in stale {
            self.connect(&machine, window, cx);
        }
        cx.notify();
    }

    fn connection(&self, machine: &MachineId) -> Option<&Connection> {
        self.connections
            .iter()
            .find(|connection| connection.machine == *machine)
    }

    fn connection_mut(&mut self, machine: &MachineId) -> Option<&mut Connection> {
        self.connections
            .iter_mut()
            .find(|connection| connection.machine == *machine)
    }

    /// Whether `epoch` is still `machine`'s connection.
    fn is_current(&self, machine: &MachineId, epoch: u64) -> bool {
        self.connection(machine)
            .is_some_and(|connection| connection.epoch == epoch)
    }

    /// The daemon of `machine`, while connected.
    fn daemon(&self, machine: &MachineId) -> Option<&Daemon> {
        self.connection(machine)
            .and_then(|connection| connection.daemon.as_ref())
    }

    /// The daemon of `machine` and its connection's epoch, while connected.
    fn ready(&self, machine: &MachineId) -> Option<(Daemon, u64)> {
        let connection = self.connection(machine)?;
        Some((connection.daemon.clone()?, connection.epoch))
    }

    /// Records how `machine`'s connection is doing, for the views.
    fn set_status(&self, machine: &MachineId, status: MachineStatus, cx: &mut App) {
        self.machines.update(cx, |machines, cx| {
            if machines.set_status(machine, status) {
                cx.notify();
            }
        });
    }

    /// Connects to `machine`'s daemon, starting it if none runs. Views of
    /// its sessions followed on an earlier connection are dropped; the
    /// sessions themselves, and their turns, stay in the daemon.
    fn connect(&mut self, machine: &MachineId, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.forget_sessions(machine, window, cx);
        self.rho_settings
            .update(cx, |settings, cx| settings.forget(machine, cx));
        self.next_epoch += 1;
        let epoch = self.next_epoch;
        let Some(target) = self
            .machines
            .read(cx)
            .get(machine)
            .map(|known| known.target.clone())
        else {
            return;
        };
        let Some(connection) = self.connection_mut(machine) else {
            return;
        };
        connection.epoch = epoch;
        connection.restarting = false;
        connection.jobs_task = Task::ready(());
        connection.daemon = None;
        let id = machine.clone();
        connection.task = cx.spawn_in(window, async move |this, cx| {
            let mut events = match Daemon::connect(target).await {
                Ok((daemon, events)) => {
                    let stored = this.update_in(cx, |this, window, cx| {
                        this.connected(&id, daemon, window, cx);
                    });
                    if stored.is_err() {
                        return;
                    }
                    events
                }
                Err(error) => {
                    let _ = this.update(cx, |this, cx| this.failed(&id, &error, cx));
                    return;
                }
            };
            // The machine's one event pump: every push from its daemon is
            // routed from here.
            while let Some(event) = events.recv().await {
                let routed = this.update_in(cx, |this, window, cx| {
                    this.route_event(&id, event, window, cx);
                });
                if routed.is_err() {
                    return;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.disconnected(&id, epoch, window, cx);
            });
        });
        self.set_status(machine, MachineStatus::Connecting, cx);
        cx.notify();
    }

    fn connected(
        &mut self,
        machine: &MachineId,
        daemon: Daemon,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let info = daemon.info();
        if let Some(notice) = info.notice() {
            let title = match self.machines.read(cx).tag(machine) {
                Some(label) => format!("rho on {label} is another build"),
                None => "rho daemon is another build".to_owned(),
            };
            window.push_notification(Notification::warning(notice.to_owned()).title(title), cx);
        }
        if !machine.is_local() {
            self.workspaces.update(cx, |workspaces, cx| {
                workspaces.connected(machine, &daemon, cx);
            });
        }
        let status = MachineStatus::Ready {
            version: format!("rho {}", info.version()).into(),
        };
        self.set_status(machine, status, cx);
        self.rho_settings.update(cx, |settings, cx| {
            settings.connected(machine, daemon.clone(), cx);
        });
        let jobs_task = cx.spawn({
            let (daemon, machine) = (daemon.clone(), machine.clone());
            async move |this, cx| loop {
                match daemon.active_jobs().await {
                    Ok(jobs) => {
                        let set = this.update(cx, |this, cx| this.set_jobs(&machine, &jobs, cx));
                        if set.is_err() {
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
        if let Some(connection) = self.connection_mut(machine) {
            connection.jobs_task = jobs_task;
            connection.daemon = Some(daemon);
        }
        self.refresh_sessions(machine, cx);
        // A draft opened while this machine connected waits for it.
        if let Pane::Draft { view, .. } = &self.pane {
            let draft = view.read(cx);
            if draft.cwd().machine == *machine && draft.awaits(draft.request().1) {
                let view = view.clone();
                self.open_draft_session(&view, window, cx);
            }
        }
        cx.notify();
    }

    /// `machine`'s daemon couldn't be reached, because of `error`.
    fn failed(&mut self, machine: &MachineId, error: &DaemonError, cx: &mut Context<'_, Self>) {
        if let Some(connection) = self.connection_mut(machine) {
            connection.daemon = None;
        }
        self.rho_settings
            .update(cx, |settings, cx| settings.forget(machine, cx));
        self.workspaces
            .update(cx, |workspaces, _| workspaces.disconnected(machine));
        self.set_status(machine, MachineStatus::Failed(error.to_string().into()), cx);
        cx.notify();
    }

    /// Shows which sessions have work under way in `machine`'s daemon. A
    /// change is also when the saved list goes stale: a session another
    /// client started appears, or one's turn ended.
    fn set_jobs(&mut self, machine: &MachineId, jobs: &[Job], cx: &mut Context<'_, Self>) {
        let Some(connection) = self.connection_mut(machine) else {
            return;
        };
        let same = connection.jobs.len() == jobs.len()
            && connection
                .jobs
                .iter()
                .zip(jobs)
                .all(|(was, now)| was.is_same_work(now));
        if same {
            return;
        }
        jobs.clone_into(&mut connection.jobs);
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_jobs(machine, jobs, cx));
        self.refresh_sessions(machine, cx);
    }

    /// Drops the views of `machine`'s open sessions, and a draft there, as
    /// the connection that followed them is gone.
    fn forget_sessions(
        &mut self,
        machine: &MachineId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let shown = match &self.pane {
            Pane::Session(view) => view.read(cx).key().machine == *machine,
            Pane::Draft { view, .. } => view.read(cx).cwd().machine == *machine,
            Pane::Empty => false,
        };
        if shown {
            self.show(Pane::Empty, window, cx);
        }
        self.sessions.retain(|open| open.id.machine != *machine);
        if let Some(connection) = self.connection_mut(machine) {
            connection.jobs.clear();
        }
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.clear_live(machine, cx));
    }

    /// The session `id`, if it was opened in this run.
    fn opened(&self, id: &SessionKey) -> Option<&OpenSession> {
        self.sessions.iter().find(|open| open.id == *id)
    }

    fn route_event(
        &mut self,
        machine: &MachineId,
        event: DaemonEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let key = |session: SessionId| SessionKey::new(machine.clone(), session);
        match event {
            DaemonEvent::Update { session, update } => {
                let session = key(session);
                if let Some(open) = self.opened(&session) {
                    open.view
                        .update(cx, |view, cx| view.apply(update, window, cx));
                } else if let Some(draft) = self.draft_with(&session, cx) {
                    draft.update(cx, |draft, cx| draft.apply(update, window, cx));
                }
            }
            DaemonEvent::Detached { session, reason } => match reason {
                Detached::Behind => self.reopen_session(&key(session), window, cx),
                Detached::Deleted => self.session_deleted(&key(session), window, cx),
                Detached::Restarting => {
                    if let Some(connection) = self.connection_mut(machine) {
                        connection.restarting = true;
                    }
                }
                Detached::Unloaded => {
                    if let Some(open) = self.opened(&key(session)) {
                        open.view.update(cx, |view, _| view.unload());
                    }
                }
            },
            DaemonEvent::AgentSettingsChanged(changed) => {
                self.rho_settings
                    .update(cx, |settings, cx| settings.changed(machine, changed, cx));
            }
        }
    }

    /// Opens Settings over the window, with rho's own settings on each
    /// machine.
    fn open_settings(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        settings_view::open(&self.machines, &self.rho_settings, window, cx);
    }

    /// `machine`'s connection `epoch` ended. After the daemon said it was
    /// restarting, that is expected, and answered by connecting again: the
    /// daemon restarts in place on the same socket, and the handshake waits
    /// until it answers.
    fn disconnected(
        &mut self,
        machine: &MachineId,
        epoch: u64,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.is_current(machine, epoch) {
            return;
        }
        let restarting = self
            .connection(machine)
            .is_some_and(|connection| connection.restarting);
        if restarting {
            self.connect(machine, window, cx);
        } else {
            self.forget_sessions(machine, window, cx);
            self.failed(machine, &DaemonError::Closed, cx);
        }
    }

    /// Reads session `id` afresh after the daemon stopped sending its
    /// events: a new view replaces the one that fell behind, and its
    /// subagents' sessions go with it.
    fn reopen_session(&mut self, id: &SessionKey, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(view) = self.opened(id).map(|open| open.view.clone()) else {
            return;
        };
        let shown = matches!(&self.pane, Pane::Session(shown)
            if *shown == view || shown.read(cx).descends_from(&view, cx));
        let (live, parent) = {
            let view = view.read(cx);
            (view.live(), view.parent())
        };
        self.forget_session(id, window, cx);
        match parent {
            Some(parent) if shown => {
                let title = live.title.unwrap_or_default();
                self.open_subagent(&parent, live.id.id, title, window, cx);
            }
            // Opened again from its parent when wanted.
            Some(_) => {}
            None if shown => self.open_session(live.id, live.cwd, live.title, window, cx),
            // Its row follows the daemon's jobs again until it is reopened.
            None => self
                .sidebar
                .update(cx, |sidebar, cx| sidebar.close_live(id, cx)),
        }
    }

    /// Drops a session that is gone from its daemon, with its record of
    /// background work.
    fn session_deleted(
        &mut self,
        id: &SessionKey,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.forget_session(id, window, cx);
        cx.global::<BackgroundLog>().forget(id);
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.remove(id, cx));
        self.refresh_sessions(&id.machine, cx);
    }

    /// Reloads `machine`'s saved-session list in the background.
    fn refresh_sessions(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let Some(daemon) = self.daemon(machine).cloned() else {
            return;
        };
        let id = machine.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = daemon.list_sessions().await;
            let _ = this.update(cx, |this, cx| {
                let error = match result {
                    Ok(sessions) => {
                        this.sidebar
                            .update(cx, |sidebar, cx| sidebar.set_saved(&id, sessions, cx));
                        None
                    }
                    Err(error) => Some(error.to_string().into()),
                };
                this.machines.update(cx, |machines, cx| {
                    if machines.set_error(&id, error) {
                        cx.notify();
                    }
                });
            });
        });
        if let Some(connection) = self.connection_mut(machine) {
            connection.sessions_task = task;
        }
    }

    /// Where a new session starts: in the project of the session on screen,
    /// else where the last new session started, else the launch directory.
    fn new_session_cwd(&self, cx: &App) -> Place {
        match &self.pane {
            Pane::Session(view) => {
                let live = view.read(cx).live();
                Place::new(live.id.machine, live.cwd)
            }
            // Not on a machine since taken off the settings.
            Pane::Empty | Pane::Draft { .. } => self
                .last_project
                .clone()
                .filter(|project| self.machines.read(cx).get(&project.machine).is_some())
                .unwrap_or_else(|| Place::new(MachineId::local(), self.launch.workspace.clone())),
        }
    }

    /// Shows a draft for a new session and opens the session right away; see
    /// [`DraftView`].
    pub(crate) fn new_session(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let cwd = self.new_session_cwd(cx);
        // An open draft is reused, so its text and settings survive.
        let pane = std::mem::replace(&mut self.pane, Pane::Empty);
        let pane = match pane {
            Pane::Draft { .. } => pane,
            Pane::Empty | Pane::Session(_) => {
                let (workspaces, projects, machines) =
                    (&self.workspaces, &self.projects, &self.machines);
                let view =
                    cx.new(|cx| DraftView::new(cwd, workspaces, projects, machines, window, cx));
                let subscription = cx.subscribe_in(
                    &view,
                    window,
                    |this, draft, event: &DraftEvent, window, cx| match event {
                        DraftEvent::Start { session, prompt } => {
                            let cwd = draft.read(cx).cwd().clone();
                            this.start_session(
                                draft,
                                (**session).clone(),
                                &cwd,
                                prompt.clone(),
                                window,
                                cx,
                            );
                        }
                        DraftEvent::StartInWorktree { config, prompt } => {
                            this.start_in_worktree(
                                draft,
                                config.clone(),
                                prompt.clone(),
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

    /// Collapses the sidebar to its rail, or opens it again. The keyboard
    /// stays where it is, unless it was in the sidebar's search, which
    /// collapsing hides: then it goes to the main pane.
    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let searching = self
            .sidebar
            .read(cx)
            .search_focus_handle(cx)
            .is_focused(window);
        let collapsed = self.sidebar.update(cx, SidebarView::toggle_collapsed);
        if collapsed && searching {
            self.focus_pane(window, cx);
        }
        // The sidebar's width is laid out here.
        cx.notify();
    }

    /// Shows a new session's draft with its project list open.
    fn new_session_in(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.new_session(window, cx);
        if let Pane::Draft { view, .. } = &self.pane {
            view.update(cx, |draft, cx| draft.choose_project(window, cx));
        }
    }

    /// The shown draft, if its session is `id`.
    fn draft_with(&self, id: &SessionKey, cx: &App) -> Option<Entity<DraftView>> {
        match &self.pane {
            Pane::Draft { view, .. } if view.read(cx).session_key().as_ref() == Some(id) => {
                Some(view.clone())
            }
            _ => None,
        }
    }

    /// Opens the session for `draft` and follows it. The draft takes the
    /// session before the attach is sent, so the events that follow the
    /// attach always have somewhere to go. While its machine connects, the
    /// draft waits, and [`Self::connected`] opens it.
    fn open_draft_session(
        &mut self,
        draft: &Entity<DraftView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let (cwd, request) = draft.read(cx).request();
        let machine = cwd.machine.clone();
        let Some((daemon, epoch)) = self.ready(&machine) else {
            let machines = self.machines.read(cx);
            if machines.status(&machine) == Some(&MachineStatus::Connecting) {
                return;
            }
            let label = machines.label(&machine);
            draft.update(cx, |draft, cx| {
                draft.fail(format!("Not connected to {label}"), cx);
            });
            return;
        };
        let draft = draft.downgrade();
        cx.spawn_in(window, async move |this, cx| {
            let setup = daemon.new_session(&cwd.path, Checkout::Local).await;
            let opened = this.update_in(cx, |this, window, cx| {
                // A draft dropped meanwhile, or moved to another project,
                // leaves the session unfollowed, and the daemon discards one
                // that saved nothing.
                let draft = draft.upgrade().filter(|draft| {
                    this.is_current(&machine, epoch) && draft.read(cx).awaits(request)
                })?;
                match setup {
                    Ok(setup) => {
                        let session = Session::from_setup(setup);
                        let id = SessionKey::new(machine.clone(), session.id().clone());
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
            let attached = daemon.attach(&id.id).await;
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
        id: &SessionKey,
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
            let (daemon, id) = (daemon.clone(), id.id.clone());
            cx.spawn(async move |_, _| {
                let _ = daemon.detach(&id).await;
            })
            .detach();
        }
    }

    /// Starts `draft`'s first message, `prompt`, in a session in a new
    /// worktree of the draft's directory: opens it, which adds the
    /// worktree, gives it `config`, the draft session's settings, follows
    /// it, then shows it and sends `prompt`. Until then the draft stays, with
    /// its own session, so a failure leaves it as it was, and only a
    /// session that starts makes a worktree.
    fn start_in_worktree(
        &mut self,
        draft: &Entity<DraftView>,
        config: Vec<ConfigOption>,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let cwd = draft.read(cx).cwd().clone();
        let machine = cwd.machine.clone();
        let Some((daemon, epoch)) = self.ready(&machine) else {
            return;
        };
        let draft = draft.downgrade();
        cx.spawn_in(window, async move |this, cx| {
            let opened = async {
                let setup = daemon.new_session(&cwd.path, Checkout::NewWorktree).await?;
                let mut session = Session::from_setup(setup);
                // Nothing follows the session yet, so its events about these
                // changes go nowhere; the answers carry the settings.
                let options = daemon
                    .match_config(session.id(), &config, session.config().to_vec())
                    .await;
                let attached = match options {
                    Ok(options) => {
                        session.set_config(options);
                        daemon.attach(session.id()).await
                    }
                    Err(error) => Err(error),
                };
                Ok::<_, DaemonError>((session, attached))
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let draft = draft
                    .upgrade()
                    .filter(|draft| this.is_current(&machine, epoch) && draft.read(cx).launching());
                match (opened, draft) {
                    (Ok((mut session, Ok(attached))), Some(draft)) => {
                        let cwd = Place::new(machine.clone(), attached.workspace());
                        session.attached(attached);
                        this.start_session(&draft, session, &cwd, prompt, window, cx);
                    }
                    (Ok((session, attached)), draft) => {
                        // Not wanted, or not set up: unfollowed and unsaved,
                        // the daemon discards it, and its clean worktree.
                        let id = session.id().clone();
                        let daemon = daemon.clone();
                        cx.spawn(async move |_, _| {
                            let _ = daemon.detach(&id).await;
                        })
                        .detach();
                        if let (Err(error), Some(draft)) = (attached, draft) {
                            draft
                                .update(cx, |draft, cx| draft.launch_failed(error.to_string(), cx));
                        }
                    }
                    (Err(error), Some(draft)) => {
                        draft.update(cx, |draft, cx| draft.launch_failed(error.to_string(), cx));
                    }
                    (Err(_), None) => {}
                }
            });
        })
        .detach();
    }

    /// Shows `draft`'s session, which works in `cwd`, in its own view and
    /// sends `prompt` as its first.
    fn start_session(
        &mut self,
        draft: &Entity<DraftView>,
        session: Session,
        cwd: &Place,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(daemon) = self.daemon(&cwd.machine).cloned() else {
            return;
        };
        // The project, not a worktree the daemon added in it.
        self.last_project = Some(draft.read(cx).cwd().clone());
        let view = self.open_new_session(daemon, cwd, session, prompt, window, cx);
        self.set_pane(Pane::Session(view), cx);
        self.focus_pane(window, cx);
    }

    /// Deletes a saved session in its daemon, then drops it here.
    fn delete_session(&mut self, id: SessionKey, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(daemon) = self.daemon(&id.machine).cloned() else {
            return;
        };
        // The daemon refuses to delete a session a client follows, so that
        // one open in another client survives. This client lets go first.
        let open = self.opened(&id).is_some();
        cx.spawn_in(window, async move |this, cx| {
            if open {
                let _ = daemon.detach(&id.id).await;
            }
            let result = daemon.delete_session(&id.id).await;
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

    /// Drops the open session `id`, if any, with the subagents' sessions
    /// opened under it, and empties the pane if it showed one of them.
    fn forget_session(&mut self, id: &SessionKey, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(closed) = self.opened(id).map(|open| open.view.clone()) else {
            return;
        };
        self.sessions
            .retain(|open| open.view != closed && !open.view.read(cx).descends_from(&closed, cx));
        let gone = match &self.pane {
            Pane::Session(view) => !self.sessions.iter().any(|open| open.view == *view),
            Pane::Empty | Pane::Draft { .. } => false,
        };
        if gone {
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
        // A subagent's session marks the row of the session it is nested in.
        let active = match &pane {
            Pane::Session(view) => Some(view.read(cx).root_id(cx)),
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
            Pane::Session(view) => view.read(cx).focus_target(cx),
            Pane::Empty => self.focus_handle.clone(),
        };
        window.focus(&handle, cx);
    }

    /// Opens a view for a session just made and sends `prompt` as its
    /// first.
    fn open_new_session(
        &mut self,
        daemon: Daemon,
        cwd: &Place,
        session: Session,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<SessionView> {
        let id = SessionKey::new(cwd.machine.clone(), session.id().clone());
        // Made here, so rho-ui's own from the start, even if the prompt fails.
        self.sidebar.update(cx, |sidebar, cx| sidebar.own(&id, cx));
        let workspace = WorkspaceRef::new(&self.workspaces, cwd, cx);
        let remote = self.machines.read(cx).remote_label(&cwd.machine);
        let view = cx.new(|cx| {
            SessionView::new(
                daemon,
                session,
                workspace,
                remote,
                Loading::Ready,
                window,
                cx,
            )
        });
        self.insert_session(id, view.clone(), window, cx);
        view.update(cx, |view, cx| view.submit(prompt, window, cx));
        view
    }

    /// Opens a saved session, or switches to it if it is already open.
    /// `title` is the listed title, shown until the daemon answers.
    pub(crate) fn open_session(
        &mut self,
        id: SessionKey,
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
        let Some(daemon) = self.daemon(&id.machine).cloned() else {
            return;
        };
        // The session must exist before the attach: the conversation
        // replays as events that may arrive before the attach's answer.
        let mut session = Session::new(id.id.clone());
        session.set_title(title.map(String::from));
        let cwd = Place::new(id.machine.clone(), cwd);
        let remote = self.machines.read(cx).remote_label(&cwd.machine);
        let workspace = WorkspaceRef::new(&self.workspaces, &cwd, cx);
        let view = cx.new(|cx| {
            SessionView::new(
                daemon.clone(),
                session,
                workspace,
                remote,
                Loading::Attaching,
                window,
                cx,
            )
        });
        self.insert_session(id.clone(), view.clone(), window, cx);
        self.show(Pane::Session(view), window, cx);

        cx.spawn_in(window, async move |this, cx| {
            let attached = daemon.attach(&id.id).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.attached(&daemon, &id, attached, window, cx);
            });
        })
        .detach();
    }

    /// Opens the session of a subagent that `parent` started, on its
    /// machine, nested under it, or switches to it if it is open. `title` is the subagent's task,
    /// shown until the daemon answers. Only the subagent writes to its
    /// session, so the view observes it rather than attaching.
    fn open_subagent(
        &mut self,
        parent: &Entity<SessionView>,
        id: SessionId,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let id = SessionKey::new(parent.read(cx).key().machine, id);
        match self.opened(&id).map(|open| open.view.clone()) {
            // One that couldn't be observed, or whose events stopped as its
            // run ended, is read afresh.
            Some(view) if view.read(cx).failed_to_attach() || view.read(cx).is_unloaded() => {
                self.forget_session(&id, window, cx);
            }
            Some(view) => {
                self.show(Pane::Session(view), window, cx);
                return;
            }
            None => {}
        }
        let Some(daemon) = self.daemon(&id.machine).cloned() else {
            return;
        };
        // As for an attach, the session must exist before the observe.
        let mut session = Session::new(id.id.clone());
        session.set_title(Some(title.into()));
        let view = cx.new(|cx| SessionView::subagent(daemon.clone(), session, parent, window, cx));
        self.insert_session(id.clone(), view.clone(), window, cx);
        self.show(Pane::Session(view), window, cx);

        cx.spawn_in(window, async move |this, cx| {
            let observed = daemon.observe(&id.id).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.attached(&daemon, &id, observed, window, cx);
            });
        })
        .detach();
    }

    /// Adds `view` to the open sessions. The sidebar lists it, unless it is
    /// a subagent's.
    fn insert_session(
        &mut self,
        id: SessionKey,
        view: Entity<SessionView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &SessionEvent, window, cx| match event {
                SessionEvent::LiveChanged => {
                    if !view.read(cx).is_subagent() {
                        let live = view.read(cx).live();
                        this.sidebar
                            .update(cx, |sidebar, cx| sidebar.set_live(live, cx));
                    }
                }
                SessionEvent::Prompted => {
                    let id = view.read(cx).key();
                    this.sidebar.update(cx, |sidebar, cx| sidebar.own(&id, cx));
                }
                SessionEvent::Saved => {
                    let machine = view.read(cx).key().machine;
                    this.refresh_sessions(&machine, cx);
                }
                SessionEvent::OpenSubagent { session, title } => {
                    this.open_subagent(view, session.clone(), title.clone(), window, cx);
                }
                SessionEvent::Show(target) => {
                    let open = target
                        .upgrade()
                        .filter(|target| this.sessions.iter().any(|open| open.view == *target));
                    if let Some(target) = open {
                        this.show(Pane::Session(target), window, cx);
                    }
                }
            },
        );
        if !view.read(cx).is_subagent() {
            let live = view.read(cx).live();
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.set_live(live, cx));
        }
        self.sessions.push(OpenSession {
            id,
            view,
            _subscription: subscription,
        });
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
    /// The user's projects, for UI tests.
    pub(crate) fn projects<'a>(&self, cx: &'a App) -> &'a [Place] {
        self.projects.read(cx).roots()
    }
    /// `machine`'s home directory, as its daemon said, for UI tests.
    pub(crate) fn workspaces_home(&self, machine: &MachineId, cx: &App) -> Option<PathBuf> {
        self.workspaces.read(cx).home(machine)
    }
    /// The sidebar's search, for UI tests.
    pub(crate) fn sidebar_search(&self, cx: &App) -> FocusHandle {
        self.sidebar.read(cx).search_focus_handle(cx)
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
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.open_settings(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                this.toggle_sidebar(window, cx);
            }))
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // First, so it sees scroll events before any scroll area.
            .child(Kinetic::hook(&self.kinetic));

        let pane_style = || StyleRefinement::default().flex_1().min_w_0().h_full();
        let sidebar_width = self.sidebar.read(cx).width();
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
                            .w(sidebar_width)
                            .h_full()
                            .flex_shrink_0(),
                    ),
                )
                .child(main),
        )
        .into_any_element()
    }
}
