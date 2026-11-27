//! The root view: owns a connection to each machine's daemon, their event
//! pumps, and the open sessions. Everything a daemon pushes enters here and
//! is routed to the [`SessionView`] of its session. The open sessions
//! include subagents' sessions opened from their parent, which the sidebar
//! does not list: they show nested under the parent, and go when it goes.
//! It also keeps the drafts of new sessions the user left with a message,
//! which the sidebar lists until they are sent or discarded.
//!
//! An open session keeps its view while its machine's connection comes and
//! goes: when the daemon stops sending its events, or the machine connects
//! again, the app follows it again on the same view, from where its events
//! left off ([`RhoApp::reattach`]). rho-ui connects again by itself only
//! when it expects the daemon to restart: it said so, an upgrade waits, or
//! rho-ui updated rho (see [`RhoApp::expect_restart`]). Otherwise it waits
//! for the user's Retry, as with no session followed it can't tell a
//! restart from a stop.
//!
//! The view stays, too, while the session is off screen with nothing under
//! way and the app stops following it, so the daemon can unload it; showing
//! it follows it again ([`RhoApp::sync_following`]).
//!
//! The sidebar, the session view and the right panel are drawn as cached
//! views: each redraws only when it changes, so a hover in one never
//! rebuilds the others.

use std::{path::PathBuf, rc::Rc, time::Duration};

use web_time::Instant;

use gpui_kit::component::{
    button::Button,
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader},
    h_flex,
    input::Search,
    notification::Notification,
    ActiveTheme as _, IconName, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, App, AppContext as _, Context, DragMoveEvent, Entity, EntityId, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StyleRefinement, Styled as _, Subscription, Task, Window,
};
use rho_ui_core::{
    Attached, Daemon, DaemonError, DaemonEvent, Detached, Job, Jobs, NewSessionMark,
    PendingUpgrade, Prompt, Session, SessionId, VersionFix, VersionMismatch, LEAD_AGENT,
};

mod attention;
mod daemon_control;
mod opening;
mod side_chat;

use attention::Attention;

use crate::machines::RemoteMachine;
use crate::{
    app_update::AppUpdate,
    attention::Notifier,
    auths::Auths,
    background_log::BackgroundLog,
    cloning,
    configured::Configured,
    draft::{DraftEvent, DraftSettings, DraftView},
    forge::Forges,
    kinetic::Kinetic,
    leads::Kind,
    machines::{
        DaemonAction, Listing, Machine, MachineId, MachineStatus, Machines, Place, Reach,
        SessionKey,
    },
    no_project::Root,
    projects::Projects,
    pull_requests::{Focus, PullRequests},
    rho_install::{render_setup, Installs, InstallsEvent, Offer},
    rho_settings::RhoSettings,
    right_panel::{
        PanelLayout, ResizeRightPanel, RightPanel, TerminalShell, TerminalShortcut, MIN_PANE_REMS,
    },
    session_list::DraftRow,
    session_view::{Link, Loading, SessionEvent, SessionView},
    settings::{Appearance, Settings},
    settings_view,
    sidebar::{ResizeSidebar, SidebarEvent, SidebarModels, SidebarState, SidebarView},
    window_chrome,
    workspaces::{self, WorkspaceRef, Workspaces},
    worktrees::Worktrees,
    FindInTranscript, NewSession, NewSessionIn, OpenSettings, ToggleRightPanel, ToggleSidebar,
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

/// How often to ask a daemon for the upgrade it waits to run when its jobs
/// stay the same. `rho/daemon/status` reads the daemon's settings and its
/// credential store each time, a D-Bus call on Linux, so it isn't asked
/// with every job poll. An upgrade appears when rho is replaced, which
/// rho-ui asks after at once when it updates rho itself; one replaced from
/// a terminal shows within this.
const UPGRADE_POLL: Duration = Duration::from_secs(30);

/// What the command line asked for.
pub(crate) struct LaunchConfig {
    /// Where new sessions start on this machine; `None` for its home, as
    /// in a browser, and on the desktop when the app was opened from it
    /// (`default_workspace` in `main.rs`). Resumed sessions keep their own
    /// directory.
    pub(crate) workspace: Option<PathBuf>,
    /// How to reach this machine's daemon and rho.
    pub(crate) local: Reach,
    /// How to reach a remote machine's: [`RemoteMachine::ssh_reach`],
    /// except in tests, which reach fake daemons, and in a browser, which
    /// reaches it through the server ([`RemoteMachine::server_reach`]).
    pub(crate) remote: Rc<dyn Fn(&RemoteMachine) -> Reach>,
    /// What the right panel's terminal runs on this machine.
    pub(crate) shell: TerminalShell,
    /// This machine's rho home, which keeps the folders of its sessions in
    /// no project (see [`crate::no_project`]): `platform::rho_home()`,
    /// except in tests, which keep it in a temporary folder. A browser
    /// reaches even this machine through its link, so it has none.
    pub(crate) rho_home: Option<PathBuf>,
    /// Where notifications go: the desktop's, except in tests.
    pub(crate) notifier: Notifier,
}

impl LaunchConfig {
    /// Every machine `settings` asks for, this one first.
    fn machines(&self, settings: &Settings) -> Vec<Machine> {
        let local = Machine {
            id: MachineId::local(),
            label: settings.local_label.clone(),
            target: self.local.daemon.clone(),
            rho: self.local.rho.clone(),
            clone_folder: settings.local_clone_folder.clone().into(),
        };
        let remote = settings.machines.iter().map(|remote| {
            let Reach { daemon, rho } = (self.remote)(remote);
            Machine {
                id: remote.id(),
                label: remote.shown_label().to_owned().into(),
                target: daemon,
                rho,
                clone_folder: remote.clone_folder.clone().into(),
            }
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
    /// The daemon may restart in place, so the connection's end is
    /// expected, and answered by connecting again: it said it is
    /// restarting, the user asked it to, rho was updated under it, or it
    /// waits to upgrade.
    restarting: bool,
    /// The user stopped the daemon: the connection's end is no failure,
    /// and nothing connects again until the user asks.
    stopped: bool,
    /// rho-ui restarted a daemon that refused it for its older protocol;
    /// a refusal on the connection after says the Rho installed there is
    /// older too. Outlives [`RhoApp::connect`], which that restart calls.
    restarted_older: bool,
    /// Connects to the daemon, then pumps its events. Dropping or replacing
    /// it stops both.
    task: Task<()>,
    /// Polls the daemon's jobs while connected; see [`JOB_POLL`].
    jobs_task: Task<()>,
    /// The jobs as last polled, to tell when they change; `None` until
    /// the connection's first listing, which always reaches the sidebar.
    jobs: Option<Vec<Job>>,
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
            stopped: false,
            restarted_older: false,
            task: Task::ready(()),
            jobs_task: Task::ready(()),
            jobs: None,
            sessions_task: Task::ready(()),
        }
    }
}

/// What the main pane shows.
enum Pane {
    Empty,
    /// One of [`RhoApp::drafts`].
    Draft(Entity<DraftView>),
    /// One of [`RhoApp::sessions`].
    Session(Entity<SessionView>),
}

/// A new session before its first message, with the subscriptions that
/// answer its requests and keep its sidebar row in step.
struct OpenDraft {
    view: Entity<DraftView>,
    /// The user left it with a message, so the sidebar lists it.
    listed: bool,
    _subscriptions: [Subscription; 2],
}

/// A session the user opened in this run, with the subscription that keeps
/// the sidebar in step with it.
struct OpenSession {
    id: SessionKey,
    view: Entity<SessionView>,
    /// The detach of a session the app let go (see
    /// [`RhoApp::sync_following`]). Following it again waits for it, so the
    /// daemon can't take the attach first and then end it.
    let_go: Option<Task<()>>,
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
    /// The auth profiles of each connected machine, for Settings and the
    /// sessions that need a sign-in.
    auths: Entity<Auths>,
    /// The session worktrees of each connected machine, for Settings.
    worktrees: Entity<Worktrees>,
    /// The MCP servers and hooks each connected machine's rho
    /// configures, for Settings.
    configured: Entity<Configured>,
    /// Each machine's rho, apart from its daemon: installing and updating.
    installs: Entity<Installs>,
    /// rho-ui's own updates.
    app_update: Entity<AppUpdate>,
    /// The forge CLIs on each machine, for Settings and pull requests.
    forges: Entity<Forges>,
    /// The pull requests the right panel shows, and the sidebar and the
    /// title bar mark.
    prs: Entity<PullRequests>,
    /// The epoch the next connection gets; see [`Connection::epoch`].
    next_epoch: u64,
    workspaces: Entity<Workspaces>,
    /// The projects a new session can start in.
    projects: Entity<Projects>,
    /// Where the last new session started, for the next one.
    last_project: Option<Place>,
    sidebar: Entity<SidebarView>,
    /// The panel at the right, laid out while [`PanelLayout`].
    panel: Entity<RightPanel>,
    /// Sessions opened in this run, with the subagents' opened under them.
    /// Each keeps its view for the rest of the run, so switching back to
    /// one shows it at once; only its deletion, a reread, or the end of its
    /// connection drops it. Once one is off screen with nothing under way,
    /// the app stops following it in its daemon (see
    /// [`Self::sync_following`]). Lookups by ID scan: the list grows only
    /// as the user opens sessions by hand.
    sessions: Vec<OpenSession>,
    /// The draft on screen, if any, and those left with a message, the
    /// one left last at the end. An empty draft goes once it is left.
    drafts: Vec<OpenDraft>,
    pane: Pane,
    /// A Restart or Stop waits for the daemon's status before it asks;
    /// another click meanwhile is ignored, so it never asks twice.
    asking: bool,
    /// Smooth wheel scrolling and touchpad flings for the whole window.
    kinetic: Rc<Kinetic>,
    /// The count in the title, and the notifications of sessions that
    /// want the user while the window is in the background.
    attention: Attention,
    focus_handle: FocusHandle,
    _subscriptions: [Subscription; 9],
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
        let installs = cx.new(|_| Installs::new(machines.clone()));
        let app_update = cx.new(AppUpdate::new);
        let forges = cx.new(|_| Forges::new(workspaces.clone()));
        let prs = cx.new(|cx| PullRequests::new(forges.clone(), workspaces.clone(), cx));
        let sidebar = cx.new(|cx| {
            let models = SidebarModels {
                workspaces: workspaces.clone(),
                machines: machines.clone(),
                installs: installs.clone(),
                app_update: app_update.clone(),
                projects: projects.clone(),
                prs: prs.clone(),
            };
            SidebarView::new(models, sidebar, window, cx)
        });
        let sidebar_subscription =
            cx.subscribe_in(&sidebar, window, |this, _, event, window, cx| {
                this.sidebar_event(event, window, cx);
            });
        // A session started or stopped wanting the user.
        let attention_subscription = cx.observe_in(&sidebar, window, |this, _, window, cx| {
            this.attention_changed(window, cx);
        });
        let installs_subscription = cx.subscribe_in(
            &installs,
            window,
            |this, _, event: &InstallsEvent, window, cx| match event {
                InstallsEvent::Installed(machine) => this.connect(machine, window, cx),
                InstallsEvent::Updated(machine) => {
                    this.expect_restart(machine);
                    this.ask_upgrade(machine, cx);
                }
            },
        );
        let machines_subscription = cx.subscribe_in(
            &machines,
            window,
            |this, _, action: &DaemonAction, window, cx| this.daemon_action(action, window, cx),
        );
        let shell = launch.shell.clone();
        let notifier = launch.notifier.clone();
        let panel = cx
            .new(|cx| RightPanel::new(workspaces.clone(), shell, prs.clone(), sidebar.clone(), cx));
        let mut app = Self {
            launch,
            machines,
            connections: Vec::new(),
            rho_settings: cx.new(|_| RhoSettings::default()),
            auths: cx.new(|_| Auths::default()),
            worktrees: cx.new(|_| Worktrees::default()),
            configured: cx.new(|_| Configured::default()),
            installs,
            app_update,
            forges,
            prs,
            next_epoch: 0,
            workspaces,
            projects,
            last_project: None,
            sidebar,
            panel,
            sessions: Vec::new(),
            drafts: Vec::new(),
            pane: Pane::Empty,
            asking: false,
            kinetic: Rc::default(),
            attention: Attention::new(notifier),
            focus_handle,
            _subscriptions: [
                sidebar_subscription,
                installs_subscription,
                machines_subscription,
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
                // The UI scale sets the top row's height, which the traffic
                // lights are centered in.
                cx.observe_global_in::<Settings>(window, |_, window, cx| {
                    window_chrome::place_traffic_lights(window, cx);
                }),
                // Pull requests wait while the window is unfocused, and
                // notifications are sent only then.
                cx.observe_window_activation(window, |this, window, cx| {
                    let focus = Focus::of(window);
                    this.prs.update(cx, |prs, cx| prs.set_focus(focus, cx));
                    this.focus_changed(focus, cx);
                }),
                attention_subscription,
                cx.observe_global_in::<PanelLayout>(window, Self::panel_toggled),
            ],
        };
        Self::answer_notifications(window, cx);
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
            self.auths
                .update(cx, |auths, cx| auths.forget(&gone.machine, cx));
            self.worktrees
                .update(cx, |worktrees, cx| worktrees.forget(&gone.machine, cx));
            self.configured
                .update(cx, |configured, cx| configured.forget(&gone.machine, cx));
            self.prs.update(cx, |prs, cx| prs.forget(&gone.machine, cx));
            self.forges
                .update(cx, |forges, cx| forges.forget(&gone.machine, cx));
            self.projects.update(cx, |projects, cx| {
                if projects.set_found(&gone.machine, Vec::new()) {
                    cx.notify();
                }
            });
            self.forget_sessions(&gone.machine, window, cx);
            self.drop_drafts(&gone.machine, window, cx);
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.remove_machine(&gone.machine, cx));
        }
        self.machines.update(cx, |machines, cx| {
            machines.set(wanted);
            cx.notify();
        });
        // A machine at another address has another daemon, which doesn't
        // follow the sessions opened on this one.
        for machine in stale {
            self.forget_sessions(&machine, window, cx);
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

    /// Reads `machine`'s clone folder for the projects in it (see
    /// [`Projects`]). A machine that isn't connected, or a folder that
    /// can't be read, keeps what was last found there, until the next
    /// draft or connection reads it again.
    fn find_projects(&self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let reader = self.workspaces.read(cx).reader(machine);
        let folder = self.machines.read(cx).clone_folder(machine).to_string();
        let executor = cx.background_executor().clone();
        let (projects, machine) = (self.projects.clone(), machine.clone());
        cx.spawn(async move |_, cx| {
            let Ok(found) = cloning::find_projects(reader, folder, executor).await else {
                return;
            };
            projects.update(cx, |projects, cx| {
                if projects.set_found(&machine, found) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Records how `machine`'s connection is doing, for the views.
    fn set_status(&self, machine: &MachineId, status: MachineStatus, cx: &mut App) {
        self.machines.update(cx, |machines, cx| {
            if machines.set_status(machine, status) {
                cx.notify();
            }
        });
    }

    /// Connects to `machine`'s daemon, starting it if none runs. Its open
    /// sessions keep their views, which follow them again once it
    /// connects ([`Self::connected`]).
    fn connect(&mut self, machine: &MachineId, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.set_link(machine, &Link::Reconnecting, window, cx);
        self.rho_settings
            .update(cx, |settings, cx| settings.forget(machine, cx));
        self.auths.update(cx, |auths, cx| auths.forget(machine, cx));
        self.worktrees
            .update(cx, |worktrees, cx| worktrees.forget(machine, cx));
        self.configured
            .update(cx, |configured, cx| configured.forget(machine, cx));
        self.prs.update(cx, |prs, cx| prs.forget(machine, cx));
        self.installs
            .update(cx, |installs, cx| installs.set_upgrade(machine, None, cx));
        self.unprime_attention(machine);
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
        connection.stopped = false;
        connection.jobs_task = Task::ready(());
        connection.jobs = None;
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
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.failed(&id, &error, window, cx);
                    });
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
        self.restart_drafts(machine, window, cx);
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
        // Its CLIs may have changed while it was away; check them afresh
        // when next asked.
        self.forges
            .update(cx, |forges, cx| forges.forget(machine, cx));
        if !workspaces::reads_here(machine) {
            self.workspaces.update(cx, |workspaces, cx| {
                workspaces.connected(machine, &daemon, cx);
            });
        }
        self.find_projects(machine, cx);
        self.find_scratch_root(machine, cx);
        let status = MachineStatus::Ready {
            version: info.version().to_owned().into(),
            pid: info.pid(),
            another_build: info.skew().is_some(),
        };
        self.set_status(machine, status, cx);
        self.rho_settings.update(cx, |settings, cx| {
            settings.connected(machine, daemon.clone(), cx);
        });
        self.auths
            .update(cx, |auths, cx| auths.connected(machine, daemon.clone(), cx));
        self.worktrees.update(cx, |worktrees, cx| {
            worktrees.connected(machine, daemon.clone(), cx);
        });
        self.configured.update(cx, |configured, cx| {
            configured.connected(machine, daemon.clone(), cx);
        });
        self.prs
            .update(cx, |prs, cx| prs.connected(machine, daemon.clone(), cx));
        self.installs.update(cx, |installs, cx| {
            installs.check(machine, Some(info.version()), cx);
        });
        let jobs_task = cx.spawn({
            let (daemon, machine) = (daemon.clone(), machine.clone());
            async move |this, cx| {
                let mut asked_upgrade: Option<Instant> = None;
                loop {
                    let changed = match daemon.jobs().await {
                        Ok(jobs) => {
                            let set = this.update(cx, |this, cx| {
                                let changed = this.set_jobs(&machine, &jobs, cx);
                                this.prime_attention(&machine, cx);
                                changed
                            });
                            match set {
                                Ok(changed) => changed,
                                Err(_) => return,
                            }
                        }
                        // The event pump reports the connection's end.
                        Err(DaemonError::Closed) => return,
                        // One refused listing is no reason to stop asking, but
                        // the sidebar says why its activity is stale.
                        Err(error) => {
                            let shown =
                                this.update(cx, |this, cx| this.jobs_failed(&machine, &error, cx));
                            if shown.is_err() {
                                return;
                            }
                            false
                        }
                    };
                    // What an upgrade waits on is the jobs, so it is asked
                    // after when they change, and now and then besides.
                    // On the executor's clock, which tests move.
                    let now = cx.background_executor().now();
                    let due = asked_upgrade.is_none_or(|at| now - at >= UPGRADE_POLL);
                    if changed || due {
                        asked_upgrade = Some(now);
                        if let Ok(status) = daemon.status().await {
                            let set = this.update(cx, |this, cx| {
                                this.set_upgrade(&machine, status.upgrade, cx);
                            });
                            if set.is_err() {
                                return;
                            }
                        }
                    }
                    cx.background_executor().timer(JOB_POLL).await;
                }
            }
        });
        if let Some(connection) = self.connection_mut(machine) {
            connection.jobs_task = jobs_task;
            connection.daemon = Some(daemon);
            connection.restarted_older = false;
        }
        self.refresh_sessions(machine, cx);
        self.reattach_all(machine, window, cx);
        // Drafts asked for a session while this machine connected.
        for draft in self.drafts_on(machine, cx) {
            let waits = {
                let draft = draft.read(cx);
                draft.awaits(draft.request().1)
            };
            if waits {
                self.open_draft_session(&draft, window, cx);
            }
        }
        cx.notify();
    }

    /// `machine`'s daemon couldn't be reached, because of `error`, or the
    /// user stopped it. Its open sessions say so, and wait for the user's
    /// Retry or Start.
    fn failed(
        &mut self,
        machine: &MachineId,
        error: &DaemonError,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let (mut stopped, mut restarted_older) = (false, false);
        self.unprime_attention(machine);
        if let Some(connection) = self.connection_mut(machine) {
            connection.daemon = None;
            connection.jobs = None;
            stopped = connection.stopped;
            restarted_older = std::mem::take(&mut connection.restarted_older);
        }
        // Nothing says what runs there until it connects again.
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_jobs(machine, &[], cx));
        self.set_link(machine, &Link::Lost, window, cx);
        self.rho_settings
            .update(cx, |settings, cx| settings.forget(machine, cx));
        self.auths.update(cx, |auths, cx| auths.forget(machine, cx));
        self.worktrees
            .update(cx, |worktrees, cx| worktrees.forget(machine, cx));
        self.configured
            .update(cx, |configured, cx| configured.forget(machine, cx));
        self.prs.update(cx, |prs, cx| prs.forget(machine, cx));
        self.workspaces
            .update(cx, |workspaces, _| workspaces.disconnected(machine));
        self.installs.update(cx, |installs, cx| {
            installs.set_upgrade(machine, None, cx);
            // A machine without rho can't connect: say so, and offer it.
            installs.check(machine, None, cx);
        });
        let status = match error {
            _ if stopped => MachineStatus::Stopped,
            DaemonError::Incompatible(VersionMismatch {
                fix: VersionFix::RestartDaemon,
                ..
            }) if restarted_older => MachineStatus::OlderRho,
            DaemonError::Incompatible(VersionMismatch {
                fix: VersionFix::RestartDaemon,
                restart,
                ..
            }) => MachineStatus::OlderDaemon { cost: *restart },
            other => MachineStatus::Failed(other.to_string().into()),
        };
        self.set_status(machine, status, cx);
        self.restart_drafts(machine, window, cx);
        cx.notify();
    }

    /// Shows which sessions have work under way in `machine`'s daemon, and
    /// gives each of its open sessions the runs it started. A change in
    /// the sessions with work is also when the saved list goes stale: a
    /// session another client started appears, or one's turn ended. A
    /// listing also clears why the last one was refused. Returns whether
    /// the sessions with work changed.
    fn set_jobs(&mut self, machine: &MachineId, jobs: &Jobs, cx: &mut Context<'_, Self>) -> bool {
        self.set_listing_error(machine, Listing::Jobs, None, cx);
        for open in self
            .sessions
            .iter()
            .filter(|open| open.id.machine == *machine)
        {
            let runs = jobs.runs.of(&open.id.id);
            open.view.update(cx, |view, cx| view.set_runs(runs, cx));
        }
        // A side chat's job shows in its tab, not as a row of its own.
        let active: Vec<Job> = jobs
            .active
            .iter()
            .filter(|job| {
                !self.sessions.iter().any(|open| {
                    open.id.machine == *machine
                        && open.id.id == *job.session()
                        && open.view.read(cx).is_side()
                })
            })
            .cloned()
            .collect();
        let active = active.as_slice();
        let Some(connection) = self.connection_mut(machine) else {
            return false;
        };
        let same = connection.jobs.as_ref().is_some_and(|jobs| {
            jobs.len() == active.len()
                && jobs
                    .iter()
                    .zip(active)
                    .all(|(was, now)| was.is_same_work(now))
        });
        if same {
            return false;
        }
        connection.jobs = Some(active.to_vec());
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_jobs(machine, active, cx));
        self.refresh_sessions(machine, cx);
        true
    }

    /// Records the upgrade `machine`'s daemon waits to run, from its
    /// status. While one waits, the daemon may restart at any moment.
    fn set_upgrade(
        &mut self,
        machine: &MachineId,
        upgrade: Option<PendingUpgrade>,
        cx: &mut Context<'_, Self>,
    ) {
        if upgrade.is_some() {
            self.expect_restart(machine);
        }
        self.installs.update(cx, |installs, cx| {
            installs.set_upgrade(machine, upgrade, cx);
        });
    }

    /// Asks `machine`'s daemon now for the upgrade it waits to run, as
    /// after rho was updated under it.
    fn ask_upgrade(&self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let Some(daemon) = self.daemon(machine).cloned() else {
            return;
        };
        let machine = machine.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(status) = daemon.status().await {
                let _ = this.update(cx, |this, cx| {
                    this.set_upgrade(&machine, status.upgrade, cx);
                });
            }
        })
        .detach();
    }

    /// `machine`'s daemon refused to list its jobs, because of `error`. The
    /// sidebar says so, as its rows' activity is stale until a listing
    /// succeeds. Repeats of the same refusal change nothing.
    fn jobs_failed(
        &mut self,
        machine: &MachineId,
        error: &DaemonError,
        cx: &mut Context<'_, Self>,
    ) {
        self.set_listing_error(machine, Listing::Jobs, Some(error.to_string().into()), cx);
    }

    /// Records why `machine`'s daemon refused `listing`, or with `None`
    /// that it listed it, for the sidebar.
    fn set_listing_error(
        &self,
        machine: &MachineId,
        listing: Listing,
        error: Option<SharedString>,
        cx: &mut App,
    ) {
        self.machines.update(cx, |machines, cx| {
            if machines.set_error(machine, listing, error) {
                cx.notify();
            }
        });
    }

    /// Drops the views of `machine`'s open sessions, as it is gone from the
    /// settings or reached at another address. Its drafts stay; see
    /// [`Self::restart_drafts`].
    fn forget_sessions(
        &mut self,
        machine: &MachineId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let shown = match &self.pane {
            Pane::Session(view) => view.read(cx).key().machine == *machine,
            Pane::Draft(_) | Pane::Empty => false,
        };
        if shown {
            self.show(Pane::Empty, window, cx);
        }
        self.drop_sessions(|open, _| open.id.machine == *machine, cx);
        if let Some(connection) = self.connection_mut(machine) {
            connection.jobs = None;
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
                // Its pull requests go to their one owner; the update still
                // goes on, so the session's cursor advances.
                if let Some(set) = update.pull_requests() {
                    self.prs
                        .update(cx, |prs, cx| prs.replaced(&session, set.clone(), cx));
                }
                // Its mark changed: the machine's sessions are listed
                // again, in place of a list read before the change, as
                // whoever marked this session may have marked the sessions
                // it started, which nothing here follows.
                if update.mark_changed().is_some() {
                    self.refresh_sessions(&session.machine, cx);
                }
                if let Some(open) = self.opened(&session) {
                    // One let go hears what came since once followed again.
                    if open.let_go.is_none() {
                        open.view
                            .update(cx, |view, cx| view.apply(update, window, cx));
                    }
                } else if let Some(draft) = self.draft_with(&session, cx) {
                    draft.update(cx, |draft, cx| draft.apply(update, window, cx));
                }
            }
            DaemonEvent::Detached { session, reason } => {
                let session = key(session);
                match reason {
                    // Unless the app let it go meanwhile.
                    Detached::Behind
                        if self
                            .opened(&session)
                            .is_some_and(|open| open.let_go.is_none()) =>
                    {
                        self.reattach(&session, window, cx);
                    }
                    Detached::Behind => {}
                    Detached::Deleted => self.session_deleted(&session, window, cx),
                    Detached::Restarting => self.expect_restart(machine),
                    Detached::Unloaded => {
                        if let Some(open) = self.opened(&session) {
                            open.view.update(cx, |view, _| view.unload());
                        }
                    }
                }
            }
            DaemonEvent::AgentSettingsChanged(changed) => {
                self.rho_settings
                    .update(cx, |settings, cx| settings.changed(machine, changed, cx));
            }
            DaemonEvent::LoginPrompt { login, prompt } => {
                self.auths.update(cx, |auths, cx| {
                    auths.login_prompt(machine, login, prompt, cx);
                });
            }
            DaemonEvent::LoginFinished { login, outcome } => {
                self.auths.update(cx, |auths, cx| {
                    auths.login_finished(machine, &login, outcome, cx);
                });
            }
        }
    }

    /// Does what the sidebar asks.
    fn sidebar_event(
        &mut self,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            SidebarEvent::Open {
                id,
                cwd,
                title,
                kind,
            } => {
                self.open_session(id.clone(), cwd.clone(), title.clone(), *kind, window, cx);
            }
            SidebarEvent::NewSession => self.new_session(window, cx),
            SidebarEvent::NewLead => self.new_lead(window, cx),
            SidebarEvent::OpenDraft(id) => self.open_draft(*id, window, cx),
            SidebarEvent::DiscardDraft(id) => self.discard_draft(*id, window, cx),
            SidebarEvent::Delete { id } => self.delete_session(id.clone(), window, cx),
            SidebarEvent::Rename { id, title } => {
                self.rename_session(id.clone(), title.clone(), window, cx);
            }
            SidebarEvent::Mark { marks } => self.mark_sessions(marks.clone(), window, cx),
            SidebarEvent::OpenSettings => self.open_settings(window, cx),
            SidebarEvent::ToggleCollapsed => self.toggle_sidebar(window, cx),
            // The sidebar's width is laid out here.
            SidebarEvent::Resized => cx.notify(),
        }
    }

    /// Opens Settings over the window, with rho's own settings on each
    /// machine.
    fn open_settings(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let app = settings_view::AppState {
            machines: self.machines.clone(),
            rho_settings: self.rho_settings.clone(),
            auths: self.auths.clone(),
            worktrees: self.worktrees.clone(),
            configured: self.configured.clone(),
            project: self.shown_project(cx),
            projects: self.projects.clone(),
            sidebar: self.sidebar.clone(),
            workspaces: self.workspaces.clone(),
            forges: self.forges.clone(),
            installs: self.installs.clone(),
            app_update: self.app_update.clone(),
        };
        settings_view::open(app, window, cx);
    }

    /// The folder of the session or draft on screen, as rho reads its
    /// project's hooks file from it (a worktree's own, not its main
    /// checkout's), which Settings → MCP & hooks shows first.
    fn shown_project(&self, cx: &App) -> Option<Place> {
        match &self.pane {
            Pane::Session(view) => view
                .read(cx)
                .projects(cx)
                .first()
                .map(|project| project.place().clone()),
            Pane::Draft(draft) => draft.read(cx).project(cx),
            Pane::Empty => None,
        }
    }

    /// Notes that `machine`'s daemon may restart in place, so its
    /// connection's end is answered by connecting again; see
    /// [`Self::disconnected`]. The next connection starts without it. A
    /// daemon the user stopped stays stopped.
    fn expect_restart(&mut self, machine: &MachineId) {
        if let Some(connection) = self.connection_mut(machine) {
            if !connection.stopped {
                connection.restarting = true;
            }
        }
    }

    /// `machine`'s connection `epoch` ended. After the daemon said it was
    /// restarting, or while rho was updated under it, that is expected,
    /// and answered by connecting again: the daemon restarts in place on
    /// the same socket, and the handshake waits until it answers. Either
    /// way the open sessions keep their views.
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
            self.failed(machine, &DaemonError::Closed, window, cx);
        }
    }

    /// Tells each of `machine`'s open sessions how its connection is doing.
    fn set_link(&self, machine: &MachineId, link: &Link, window: &mut Window, cx: &mut App) {
        for open in self
            .sessions
            .iter()
            .filter(|open| open.id.machine == *machine)
        {
            open.view
                .update(cx, |view, cx| view.set_link(link.clone(), window, cx));
        }
    }

    /// Follows each of `machine`'s open sessions again, now it connected:
    /// attaching to a session carries on a turn the daemon's restart cut
    /// short, in a rho that resumes them.
    fn reattach_all(
        &mut self,
        machine: &MachineId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // The connection that one let go was followed in is gone, and with
        // it what its detach waited on.
        for open in &mut self.sessions {
            if open.id.machine == *machine && open.let_go.is_some() {
                open.let_go = Some(Task::ready(()));
            }
        }
        let open: Vec<SessionKey> = self
            .sessions
            .iter()
            .filter(|open| open.id.machine == *machine && open.let_go.is_none())
            .map(|open| open.id.clone())
            .collect();
        for id in open {
            self.reattach(&id, window, cx);
        }
        // One let go is followed again if it was shown while the machine
        // was away.
        self.sync_following(window, cx);
    }

    /// Follows open session `id` again on the same view, on its machine's
    /// daemon now, from where its events left off: the daemon stopped
    /// sending them, the machine connected again, or the app let it go
    /// and shows it again. A daemon that still
    /// has the events after that sends only those, so the view keeps its
    /// scroll position and what is unfolded. A subagent's session is
    /// observed again; a view with nothing to follow is left as it is.
    fn reattach(&mut self, id: &SessionKey, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some((daemon, epoch)) = self.ready(&id.machine) else {
            return;
        };
        let Some(open) = self.sessions.iter_mut().find(|open| open.id == *id) else {
            return;
        };
        let detach = open.let_go.take();
        let id = id.clone();
        cx.spawn_in(window, async move |this, cx| {
            // Where its events left off is known once the detach ended
            // their old subscription.
            if let Some(detach) = detach {
                detach.await;
            }
            let follow = this.update(cx, |this, cx| {
                // Left again before this ran, it stays let go.
                let open = this.opened(&id).filter(|open| open.let_go.is_none())?;
                open.view
                    .update(cx, |view, cx| view.reattach(daemon.clone(), cx))
            });
            let Ok(Some(follow)) = follow else {
                return;
            };
            let cursor = follow.cursor.as_ref();
            let attached = if follow.observe {
                daemon.observe(&id.id, cursor).await
            } else {
                daemon.attach(&id.id, cursor).await
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.attached(&daemon, epoch, &id, attached, window, cx);
            });
        })
        .detach();
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
                this.set_listing_error(&id, Listing::Sessions, error, cx);
            });
        });
        if let Some(connection) = self.connection_mut(machine) {
            connection.sessions_task = task;
        }
    }

    /// Where a new session starts: in the project of the session or draft
    /// on screen, else where the last new session started, else the launch
    /// directory, or in a browser this machine's home. A session or draft
    /// in no project, a lead's or a scratch one, counts as none: a new
    /// session never starts in no project.
    fn new_session_cwd(&self, cx: &App) -> Place {
        let on_screen = match &self.pane {
            Pane::Session(view) => {
                let view = view.read(cx);
                (!view.in_no_project(cx)).then(|| {
                    let live = view.live();
                    Place::new(live.id.machine, live.cwd)
                })
            }
            Pane::Draft(view) => {
                let draft = view.read(cx);
                (!draft.in_no_project(cx)).then(|| draft.cwd().clone())
            }
            Pane::Empty => None,
        };
        on_screen.unwrap_or_else(|| {
            self.last_project
                .clone()
                // Not on a machine since taken off the settings.
                .filter(|project| self.machines.read(cx).get(&project.machine).is_some())
                .unwrap_or_else(|| {
                    let local = MachineId::local();
                    let dir = self.launch.workspace.clone().unwrap_or_else(|| {
                        let home = self.workspaces.read(cx).home(&local);
                        home.unwrap_or_else(|| "/".into())
                    });
                    Place::new(local, dir)
                })
        })
    }

    /// Shows a draft for a new session and opens the session right away; see
    /// [`DraftView`].
    pub(crate) fn new_session(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.new_draft(Kind::Session, window, cx);
    }

    /// Shows a draft for a new lead and opens its session right away.
    fn new_lead(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.new_draft(Kind::Lead, window, cx);
    }

    /// Shows a draft of `kind` and opens its session right away: a lead's
    /// on the machine a new session would start on.
    fn new_draft(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<'_, Self>) {
        // A clone or a folder made since the last New session is a project
        // now, for a session to start in or a lead to work in.
        for connection in &self.connections {
            self.find_projects(&connection.machine, cx);
        }
        // An empty draft of the kind on screen is reused, so its settings
        // survive, unless it was moved to no project: a new session starts
        // in a project. One with a message is left for the sidebar, in
        // place of the new one.
        if let Pane::Draft(view) = &self.pane {
            let draft = view.read(cx);
            let scratch = draft.no_project_root(cx) == Some(Root::Scratch);
            if draft.kind() == kind && !draft.has_message(cx) && !scratch {
                let view = view.clone();
                self.show(Pane::Draft(view), window, cx);
                return;
            }
        }
        let cwd = self.new_session_cwd(cx);
        let (workspaces, projects, machines) = (&self.workspaces, &self.projects, &self.machines);
        let auths = self.auths.clone();
        let view = cx.new(|cx| match kind {
            Kind::Session => DraftView::new(cwd, workspaces, projects, machines, auths, window, cx),
            Kind::Lead => DraftView::new_lead(
                cwd.machine,
                workspaces,
                projects,
                machines,
                auths,
                window,
                cx,
            ),
        });
        let subscriptions = [
            cx.subscribe_in(
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
                    DraftEvent::StartIn { settings, prompt } => {
                        this.start_in(draft, (**settings).clone(), prompt.clone(), window, cx);
                    }
                    DraftEvent::OpenSession => this.open_draft_session(draft, window, cx),
                },
            ),
            // A listed draft's row follows its message and project.
            cx.observe(&view, |this, _, cx| this.sync_drafts(cx)),
        ];
        self.drafts.push(OpenDraft {
            view: view.clone(),
            listed: false,
            _subscriptions: subscriptions,
        });
        self.open_draft_session(&view, window, cx);
        self.show(Pane::Draft(view), window, cx);
    }

    /// Shows the draft `id` the sidebar lists.
    fn open_draft(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let view = self
            .drafts
            .iter()
            .find(|open| open.view.entity_id() == id)
            .map(|open| open.view.clone());
        if let Some(view) = view {
            self.show(Pane::Draft(view), window, cx);
        }
    }

    /// Drops the draft `id`, with its message and session.
    fn discard_draft(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.drafts.retain(|open| open.view.entity_id() != id);
        if matches!(&self.pane, Pane::Draft(view) if view.entity_id() == id) {
            self.show(Pane::Empty, window, cx);
        } else {
            self.sync_drafts(cx);
        }
    }

    /// The user left `draft`. With a message, it stays, listed in the
    /// sidebar as the one left last; without, it goes, and with it its
    /// session.
    fn leave_draft(&mut self, draft: &Entity<DraftView>, cx: &App) {
        let Some(ix) = self.drafts.iter().position(|open| open.view == *draft) else {
            return;
        };
        let mut open = self.drafts.remove(ix);
        if draft.read(cx).has_message(cx) {
            open.listed = true;
            self.drafts.push(open);
        }
    }

    /// Lists the drafts left with a message in the sidebar, the one left
    /// last on top.
    fn sync_drafts(&self, cx: &mut Context<'_, Self>) {
        let shown = match &self.pane {
            Pane::Draft(view) => Some(view.entity_id()),
            Pane::Empty | Pane::Session(_) => None,
        };
        let rows = self
            .drafts
            .iter()
            .rev()
            .filter(|open| open.listed)
            .map(|open| {
                let draft = open.view.read(cx);
                DraftRow::new(
                    open.view.entity_id(),
                    draft.kind(),
                    draft.cwd().clone(),
                    draft.title(cx),
                    draft.workspace_index(),
                    draft.added_indices(cx),
                )
            })
            .collect();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_drafts(rows, shown, cx));
    }

    /// `machine`'s drafts.
    fn drafts_on(&self, machine: &MachineId, cx: &App) -> Vec<Entity<DraftView>> {
        self.drafts
            .iter()
            .filter(|open| open.view.read(cx).cwd().machine == *machine)
            .map(|open| open.view.clone())
            .collect()
    }

    /// Lets `machine`'s drafts' sessions go with the connection that held
    /// them, and asks again for each: it opens once the machine connects,
    /// and fails as not connected if it won't. Their messages stay.
    fn restart_drafts(
        &mut self,
        machine: &MachineId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        for draft in self.drafts_on(machine, cx) {
            draft.update(cx, DraftView::disconnected);
            self.open_draft_session(&draft, window, cx);
        }
    }

    /// Drops `machine`'s drafts, as it is gone from the settings.
    fn drop_drafts(
        &mut self,
        machine: &MachineId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let gone = self.drafts_on(machine, cx);
        self.drafts.retain(|open| !gone.contains(&open.view));
        if matches!(&self.pane, Pane::Draft(view) if gone.contains(view)) {
            self.show(Pane::Empty, window, cx);
        } else {
            self.sync_drafts(cx);
        }
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

    /// Find, where no session view has the keyboard (a session view opens
    /// its own find bar): the open session's find bar, or with none open,
    /// the sidebar's search, which finds sessions.
    fn find(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Pane::Session(view) = &self.pane {
            view.update(cx, |view, cx| view.open_find(window, cx));
            return;
        }
        if self.sidebar.read(cx).is_collapsed() {
            self.toggle_sidebar(window, cx);
        }
        let search = self.sidebar.read(cx).search_focus_handle(cx);
        window.focus(&search, cx);
    }

    /// Shows a new session's draft with its project list open.
    fn new_session_in(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.new_session(window, cx);
        if let Pane::Draft(view) = &self.pane {
            view.update(cx, |draft, cx| draft.choose_project(window, cx));
        }
    }

    /// The draft whose session is `id`.
    fn draft_with(&self, id: &SessionKey, cx: &App) -> Option<Entity<DraftView>> {
        self.drafts
            .iter()
            .find(|open| open.view.read(cx).session_key().as_ref() == Some(id))
            .map(|open| open.view.clone())
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
        let opening = self.opening(draft.read(cx), /*first*/ None, cx);
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
            let setup = opening.open(&daemon, &cwd.path).await;
            let opened = this.update_in(cx, |this, window, cx| {
                // A draft dropped meanwhile, or moved to another project,
                // leaves the session unfollowed, and the daemon discards one
                // that saved nothing.
                let draft = draft.upgrade().filter(|draft| {
                    this.is_current(&machine, epoch) && draft.read(cx).awaits(request)
                })?;
                match setup {
                    Ok((setup, dir)) => {
                        this.opened_in(&opening, &machine, &dir, cx);
                        let session = Session::from_setup(setup);
                        let id = SessionKey::new(machine.clone(), session.id().clone());
                        let daemon = daemon.clone();
                        draft.update(cx, |draft, cx| draft.open(daemon, session, dir, window, cx));
                        Some(id)
                    }
                    Err(error) => {
                        draft.update(cx, |draft, cx| draft.fail(error, cx));
                        None
                    }
                }
            });
            let Ok(Some(id)) = opened else {
                return;
            };
            let attached = daemon.attach(&id.id, None).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.attached(&daemon, epoch, &id, attached, window, cx);
            });
        })
        .detach();
    }

    /// Hands the answer to an attach made on connection `epoch` to whoever
    /// holds session `id` now: its draft, or its view once the first
    /// message was sent. With neither left, nothing follows the session,
    /// so the daemon is told. An answer from a connection that ended since
    /// is dropped: the views outlive it, and the next connection follows
    /// them again.
    fn attached(
        &mut self,
        daemon: &Daemon,
        epoch: u64,
        id: &SessionKey,
        attached: Result<Attached, DaemonError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.is_current(&id.machine, epoch) {
            return;
        }
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

    /// Starts `draft`'s first message, `prompt`, in a session of its own,
    /// as [`Self::opening`] says: for a session's draft, in a new worktree
    /// of its directory, or in no project, in a scratch folder named from
    /// `prompt`; for a lead's, as a lead in a folder named so, reaching the
    /// projects its draft picked. Opens it, gives it `settings`, the draft
    /// session's own and its choice for the sessions it starts, follows it,
    /// then shows it and sends `prompt`. Until then the draft stays, with
    /// its own session, so a failure leaves it as it was: only a session
    /// that starts keeps its worktree, and a folder made for it goes again
    /// while it is empty.
    fn start_in(
        &mut self,
        draft: &Entity<DraftView>,
        settings: DraftSettings,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let DraftSettings {
            options,
            started,
            added_dirs,
        } = settings;
        let cwd = draft.read(cx).cwd().clone();
        let machine = cwd.machine.clone();
        // A lead's projects go with its open, so its first turn knows them.
        let opening = self
            .opening(draft.read(cx), Some(&prompt), cx)
            .with_added_dirs(added_dirs);
        let Some((daemon, epoch)) = self.ready(&machine) else {
            return;
        };
        let draft = draft.downgrade();
        cx.spawn_in(window, async move |this, cx| {
            let opened = async {
                let (setup, dir) = opening.open(&daemon, &cwd.path).await?;
                let mut session = Session::from_setup(setup);
                // Nothing follows the session yet, so its events about these
                // changes go nowhere; the answers carry the settings.
                let matched = match (options, session.options().cloned()) {
                    (Some(wanted), Some(current)) => daemon
                        .match_options(session.id(), &wanted, current)
                        .await
                        .map(Some),
                    _ => Ok(None),
                };
                let matched = match (matched, &started) {
                    (Ok(options), Some(wanted)) => daemon
                        .match_started_session_defaults(session.id(), wanted)
                        .await
                        .map(|()| options),
                    (matched, _) => matched,
                };
                let attached = match matched {
                    Ok(options) => {
                        if let Some(options) = options {
                            session.set_options(options);
                        }
                        daemon.attach(session.id(), None).await
                    }
                    Err(error) => Err(error),
                };
                Ok::<_, String>((session, dir, attached))
            }
            .await;
            let abandoned = this.update_in(cx, |this, window, cx| {
                let draft = draft
                    .upgrade()
                    .filter(|draft| this.is_current(&machine, epoch) && draft.read(cx).launching());
                match (opened, draft) {
                    (Ok((mut session, dir, Ok(attached))), Some(draft)) => {
                        this.opened_in(&opening, &machine, &dir, cx);
                        let cwd = Place::new(machine.clone(), attached.workspace());
                        session.attached(attached);
                        this.start_session(&draft, session, &cwd, prompt, window, cx);
                        None
                    }
                    (Ok((session, dir, attached)), draft) => {
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
                        Some(dir)
                    }
                    (Err(error), Some(draft)) => {
                        draft.update(cx, |draft, cx| draft.launch_failed(error, cx));
                        None
                    }
                    (Err(_), None) => None,
                }
            });
            // A folder made for a session that won't use it goes while it
            // is empty.
            if let Ok(Some(dir)) = abandoned {
                opening.unmake(&dir).await;
            }
        })
        .detach();
    }

    /// Opens `draft`'s session, which works in `cwd`, in its own view, in
    /// place of the draft, and sends `prompt` as its first. It shows if the
    /// draft did: a worktree's session may start after the user moved on.
    fn start_session(
        &mut self,
        draft: &Entity<DraftView>,
        session: Session,
        cwd: &Place,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // Sent, the draft is done with, whether or not the session opens.
        let shown = matches!(&self.pane, Pane::Draft(view) if view == draft);
        self.drafts.retain(|open| open.view != *draft);
        let Some(daemon) = self.daemon(&cwd.machine).cloned() else {
            self.sync_drafts(cx);
            return;
        };
        // The project, not a worktree the daemon added in it; a session in
        // no project leaves the last one.
        if !draft.read(cx).in_no_project(cx) {
            self.last_project = Some(draft.read(cx).cwd().clone());
        }
        let view = self.open_new_session(daemon, cwd, session, prompt, window, cx);
        if shown {
            self.show(Pane::Session(view), window, cx);
        } else {
            self.sync_drafts(cx);
        }
    }

    /// Deletes a saved session in its daemon, then drops it here.
    fn delete_session(&mut self, id: SessionKey, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(daemon) = self.daemon(&id.machine).cloned() else {
            self.not_connected(&id.machine, "Couldn't delete the session", window, cx);
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
                    // Its view no longer follows it: follow it again.
                    this.reattach(&id, window, cx);
                }
            });
        })
        .detach();
    }

    /// Saves `title` as session `id`'s title in its daemon, then reads the
    /// saved list again: it holds the new title, or on failure the old one,
    /// which the sidebar showed before the rename. While the machine isn't
    /// connected the user is told, and the sidebar keeps the old title.
    fn rename_session(
        &mut self,
        id: SessionKey,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(daemon) = self.daemon(&id.machine).cloned() else {
            self.not_connected(&id.machine, "Couldn't rename the session", window, cx);
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = daemon.rename_session(&id.id, &title).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = result {
                    window.push_notification(
                        Notification::error(error.to_string()).title("Couldn't rename the session"),
                        cx,
                    );
                }
                this.refresh_sessions(&id.machine, cx);
            });
        })
        .detach();
    }

    /// Saves each of `marks`, a session's mark, for sessions of one family,
    /// in their daemon, one after another, then lists the machine's
    /// sessions again: the list holds each mark as the daemon stamped it,
    /// or, from the first the daemon refused on, the one the session had,
    /// and the user is told why. A list still being read goes unanswered,
    /// as it would put back the marks the sidebar shows these sessions
    /// without. Not connected, the user is told, and the sidebar keeps the
    /// marks it had.
    fn mark_sessions(
        &mut self,
        marks: Vec<(SessionKey, NewSessionMark)>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(machine) = marks.first().map(|(id, _)| id.machine.clone()) else {
            return;
        };
        // A pin moves it too: to the top of the list, or back off it.
        let title = "Couldn't move the session";
        let Some(daemon) = self.daemon(&machine).cloned() else {
            self.not_connected(&machine, title, window, cx);
            return;
        };
        if let Some(connection) = self.connection_mut(&machine) {
            connection.sessions_task = Task::ready(());
        }
        cx.spawn_in(window, async move |this, cx| {
            let mut result = Ok(());
            for (id, mark) in marks {
                result = daemon.mark_session(&id.id, mark).await;
                if result.is_err() {
                    break;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = result {
                    window
                        .push_notification(Notification::error(error.to_string()).title(title), cx);
                }
                this.refresh_sessions(&machine, cx);
            });
        })
        .detach();
    }

    /// Tells the user that what `title` names needs `machine`'s daemon,
    /// which isn't connected: its saved sessions stay listed after the
    /// connection drops, and their rows still offer to open, rename, pin,
    /// move and delete them.
    fn not_connected(
        &self,
        machine: &MachineId,
        title: &'static str,
        window: &mut Window,
        cx: &mut App,
    ) {
        let label = self.machines.read(cx).label(machine);
        window.push_notification(
            Notification::error(format!("Not connected to {label}")).title(title),
            cx,
        );
    }

    /// Drops the open sessions `which` picks; the right panel closes the
    /// tabs of the side chats among them.
    fn drop_sessions(
        &mut self,
        which: impl Fn(&OpenSession, &App) -> bool,
        cx: &mut Context<'_, Self>,
    ) {
        let (dropped, kept): (Vec<OpenSession>, Vec<OpenSession>) =
            std::mem::take(&mut self.sessions)
                .into_iter()
                .partition(|open| which(open, cx));
        self.sessions = kept;
        let dropped: Vec<Entity<SessionView>> = dropped.into_iter().map(|open| open.view).collect();
        self.panel
            .update(cx, |panel, cx| panel.close_side_chats(&dropped, cx));
    }

    /// Drops the open session `id`, if any, with the subagents' sessions
    /// and side chats opened under it, and empties the pane if it showed
    /// one of them. The right panel closes the side chats' tabs.
    fn forget_session(&mut self, id: &SessionKey, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(closed) = self.opened(id).map(|open| open.view.clone()) else {
            return;
        };
        self.drop_sessions(
            |open, cx| open.view == closed || open.view.read(cx).descends_from(&closed, cx),
            cx,
        );
        let gone = match &self.pane {
            Pane::Session(view) => !self.sessions.iter().any(|open| open.view == *view),
            Pane::Empty | Pane::Draft(_) => false,
        };
        if gone {
            self.show(Pane::Empty, window, cx);
        }
        cx.notify();
    }

    /// Follows the session on screen again if the app let it go, and lets
    /// go of each one off screen that can go (see
    /// [`SessionView::can_let_go`]), so its daemon can unload it. One let go
    /// keeps its view, and its sidebar row follows the daemon's jobs until
    /// the view catches up again. Reopening a session from its daemon took
    /// 115 to 310 ms for saved sessions of 12 to 68 MB on a local rho 3.3
    /// daemon, while keeping four of those loaded held about 300 MB of the
    /// daemon's memory, and a session it holds can't be opened in rho's
    /// terminal UI.
    fn sync_following(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let shown = match &self.pane {
            Pane::Session(view) => Some(view),
            Pane::Empty | Pane::Draft(_) => None,
        };
        let mut follow = None;
        let mut let_go = Vec::new();
        for (ix, open) in self.sessions.iter().enumerate() {
            match (&open.let_go, shown == Some(&open.view)) {
                (Some(_), true) => follow = Some(open.id.clone()),
                (None, false) if open.view.read(cx).can_let_go() => {
                    if let Some((daemon, _)) = self.ready(&open.id.machine) {
                        let_go.push((ix, daemon));
                    }
                }
                _ => {}
            }
        }
        for (ix, daemon) in let_go {
            let open = &self.sessions[ix];
            let id = open.id.clone();
            if open.view.read(cx).is_listed() {
                self.sidebar
                    .update(cx, |sidebar, cx| sidebar.forget_live(&id, cx));
            }
            self.sessions[ix].let_go = Some(cx.spawn(async move |_, _| {
                let _ = daemon.detach(&id.id).await;
            }));
        }
        if let Some(id) = follow {
            self.reattach(&id, window, cx);
        }
    }

    /// Shows `pane` in the main area and puts the keyboard in it.
    fn show(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.set_pane(pane, cx);
        self.sync_following(window, cx);
        self.focus_pane(window, cx);
    }

    /// Runs `shortcut` on the right panel's terminal with the keyboard or
    /// under the pointer. With none, the key goes on as if unbound, so a
    /// text field still gets it.
    fn terminal_shortcut(
        &mut self,
        shortcut: TerminalShortcut,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let ran = PanelLayout::get(cx)
            && self.panel.update(cx, |panel, cx| {
                panel.terminal_shortcut(shortcut, window, cx)
            });
        if !ran {
            cx.propagate();
        }
    }

    /// Puts `pane` in the main area, marks its row in the sidebar, and
    /// shows its session in the right panel, leaving the keyboard where it
    /// is. A draft it replaces is left; see [`Self::leave_draft`].
    fn set_pane(&mut self, pane: Pane, cx: &mut Context<'_, Self>) {
        let session = match &pane {
            Pane::Session(view) => Some(view.clone()),
            Pane::Empty | Pane::Draft(_) => None,
        };
        // A subagent's session marks the row of the session it is nested in.
        let active = session.as_ref().map(|view| view.read(cx).root_id(cx));
        let left = std::mem::replace(&mut self.pane, pane);
        if let Pane::Draft(left) = left {
            if !matches!(&self.pane, Pane::Draft(view) if *view == left) {
                self.leave_draft(&left, cx);
            }
        }
        self.sync_drafts(cx);
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_active(active.as_ref(), cx));
        self.panel
            .update(cx, |panel, cx| panel.set_session(session, cx));
        cx.notify();
    }

    /// The right panel shown or hidden: lays it out, and redraws the pane's
    /// title bar, which offers the toggle while it is hidden.
    fn panel_toggled(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        // Hidden with the keyboard in it, the panel would take the keyboard
        // away, and the app's shortcuts with it: give it to the pane.
        if !PanelLayout::get(cx) && self.panel.read(cx).has_keyboard(window, cx) {
            self.focus_pane(window, cx);
        }
        match &self.pane {
            Pane::Session(view) => view.update(cx, |_, cx| cx.notify()),
            Pane::Draft(view) => view.update(cx, |_, cx| cx.notify()),
            Pane::Empty => {}
        }
        cx.notify();
    }

    /// Puts the keyboard in the main pane's composer, unless a dialog such
    /// as settings is open: it keeps the keyboard until it closes.
    fn focus_pane(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let handle = match &self.pane {
            Pane::Draft(view) => view.read(cx).composer_focus_handle(cx),
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
        let workspace = WorkspaceRef::new(&self.workspaces, cwd, cx);
        let remote = self.machines.read(cx).remote_label(&cwd.machine);
        let (auths, prs, projects) = (self.auths.clone(), self.prs.clone(), self.projects.clone());
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
            .with_projects(&projects, window, cx)
            .with_sign_in(auths, window, cx)
            .with_pull_requests(prs, cx)
        });
        self.insert_session(id, view.clone(), window, cx);
        view.update(cx, |view, cx| view.submit(prompt, window, cx));
        view
    }

    /// Opens a saved session, or switches to it if it is already open.
    /// `title` is the listed title, shown until the daemon answers, and
    /// `kind` the listed kind, so a lead shows as one from the start.
    pub(crate) fn open_session(
        &mut self,
        id: SessionKey,
        cwd: PathBuf,
        title: Option<SharedString>,
        kind: Kind,
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
        let Some((daemon, epoch)) = self.ready(&id.machine) else {
            self.not_connected(&id.machine, "Couldn't open the session", window, cx);
            return;
        };
        // The session must exist before the attach: the conversation
        // replays as events that may arrive before the attach's answer.
        let mut session = Session::new(id.id.clone());
        session.set_title(title.map(String::from));
        if kind == Kind::Lead {
            session.set_agent(Some(LEAD_AGENT.to_owned()));
        }
        let cwd = Place::new(id.machine.clone(), cwd);
        let remote = self.machines.read(cx).remote_label(&cwd.machine);
        let workspace = WorkspaceRef::new(&self.workspaces, &cwd, cx);
        let (auths, prs, projects) = (self.auths.clone(), self.prs.clone(), self.projects.clone());
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
            .with_projects(&projects, window, cx)
            .with_sign_in(auths, window, cx)
            .with_pull_requests(prs, cx)
        });
        self.insert_session(id.clone(), view.clone(), window, cx);
        self.show(Pane::Session(view), window, cx);

        cx.spawn_in(window, async move |this, cx| {
            let attached = daemon.attach(&id.id, None).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.attached(&daemon, epoch, &id, attached, window, cx);
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
            Some(view) if !view.read(cx).follows_again() => {
                self.forget_session(&id, window, cx);
            }
            Some(view) => {
                self.show(Pane::Session(view), window, cx);
                return;
            }
            None => {}
        }
        let Some((daemon, epoch)) = self.ready(&id.machine) else {
            self.not_connected(
                &id.machine,
                "Couldn't open the subagent's session",
                window,
                cx,
            );
            return;
        };
        // As for an attach, the session must exist before the observe.
        let mut session = Session::new(id.id.clone());
        session.set_title(Some(title.into()));
        let view = cx.new(|cx| SessionView::subagent(daemon.clone(), session, parent, window, cx));
        self.insert_session(id.clone(), view.clone(), window, cx);
        self.show(Pane::Session(view), window, cx);

        cx.spawn_in(window, async move |this, cx| {
            let observed = daemon.observe(&id.id, None).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.attached(&daemon, epoch, &id, observed, window, cx);
            });
        })
        .detach();
    }

    /// Adds `view` to the open sessions. The sidebar lists it, unless it is
    /// a subagent's or a side chat.
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
                    if view.read(cx).is_listed() {
                        let live = view.read(cx).live();
                        this.sidebar
                            .update(cx, |sidebar, cx| sidebar.set_live(live, cx));
                    }
                    // Off screen, a turn that ended, or the replay that
                    // caught up, may leave it idle.
                    this.sync_following(window, cx);
                }
                // A side chat's isn't listed.
                SessionEvent::Saved => {
                    if !view.read(cx).is_side() {
                        let machine = view.read(cx).key().machine;
                        this.refresh_sessions(&machine, cx);
                    }
                }
                SessionEvent::OpenSubagent { session, title } => {
                    this.open_subagent(view, session.clone(), title.clone(), window, cx);
                }
                // As its row in the sidebar opens it.
                SessionEvent::GoToSession(id) => {
                    let id = SessionKey::new(view.read(cx).key().machine, id.clone());
                    if !this.sidebar.update(cx, |sidebar, cx| sidebar.open(&id, cx)) {
                        let note = Notification::error(
                            "It isn't in the session list: it may have been deleted.",
                        )
                        .title("Couldn't open the session");
                        window.push_notification(note, cx);
                    }
                }
                SessionEvent::Show(target) => {
                    // A side chat shows in the panel, beside its session.
                    let target = target.upgrade().map(|target| {
                        let side = target.read(cx).is_side();
                        match target.read(cx).parent() {
                            Some(parent) if side => parent,
                            _ => target,
                        }
                    });
                    let open = target
                        .filter(|target| this.sessions.iter().any(|open| open.view == *target));
                    if let Some(target) = open {
                        this.show(Pane::Session(target), window, cx);
                    }
                }
                // Only the session on screen shows its title bar, so the
                // new one starts in its project.
                SessionEvent::NewSession => this.new_session(window, cx),
                // And so the panel shows this session's tabs.
                SessionEvent::ShowPullRequest(id) => {
                    PanelLayout::show(cx);
                    this.panel.update(cx, |panel, cx| {
                        panel.show_pull_request(id, window, cx);
                    });
                }
                SessionEvent::ShowBackground => {
                    PanelLayout::show(cx);
                    this.panel
                        .update(cx, |panel, cx| panel.show_background(window, cx));
                }
                SessionEvent::ShowContext => {
                    PanelLayout::show(cx);
                    this.panel
                        .update(cx, |panel, cx| panel.show_context(window, cx));
                }
                SessionEvent::SideChat(prompt) => {
                    this.start_side_chat(view, prompt.clone(), window, cx);
                }
                SessionEvent::Discard => {
                    let id = view.read(cx).key();
                    this.discard_side_chat(&id, window, cx);
                }
            },
        );
        if view.read(cx).is_listed() {
            let live = view.read(cx).live();
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.set_live(live, cx));
        }
        self.sessions.push(OpenSession {
            id,
            view,
            let_go: None,
            _subscription: subscription,
        });
    }

    /// The main pane when no session is open.
    /// The main pane: the session or draft on screen, else the empty one,
    /// or while this machine has no rho, the offer to install it.
    fn render_main(
        &self,
        style: StyleRefinement,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let setup = self
            .installs
            .read(cx)
            .offer(&MachineId::local(), cx)
            .filter(Offer::replaces_connection_error);
        let on_this_machine = match &self.pane {
            Pane::Empty => true,
            Pane::Draft(view) => view.read(cx).request().0.machine.is_local(),
            Pane::Session(_) => false,
        };
        // Panes without a title bar move the window from their top.
        let strip = window_chrome::top_strip(window, cx);
        let untitled = |pane: AnyElement| {
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(pane)
                .children(strip)
                .into_any_element()
        };
        match (&self.pane, setup) {
            // This machine has no rho, so nothing here can work yet.
            (_, Some(offer)) if on_this_machine => {
                untitled(render_setup(&self.installs, &offer, cx))
            }
            (Pane::Session(view), _) => view.clone().cached(style).into_any_element(),
            (Pane::Draft(view), _) => view.clone().cached(style).into_any_element(),
            (Pane::Empty, _) => untitled(Self::render_no_session(cx).into_any_element()),
        }
    }

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
    /// The pull requests the right panel reads, for UI tests.
    pub(crate) fn pull_requests(&self) -> &Entity<PullRequests> {
        &self.prs
    }
    /// The forge CLIs found on each machine, for UI tests.
    pub(crate) fn forges(&self) -> &Entity<Forges> {
        &self.forges
    }
    /// The session the main pane shows, for UI tests.
    pub(crate) fn active_session(&self) -> Option<Entity<SessionView>> {
        match &self.pane {
            Pane::Session(view) => Some(view.clone()),
            Pane::Empty | Pane::Draft(_) => None,
        }
    }
    /// The new session's draft the main pane shows, for UI tests.
    pub(crate) fn draft(&self) -> Option<Entity<DraftView>> {
        match &self.pane {
            Pane::Draft(view) => Some(view.clone()),
            Pane::Empty | Pane::Session(_) => None,
        }
    }
    /// How `machine`'s connection is doing, for UI tests.
    pub(crate) fn machine_status(&self, machine: &MachineId, cx: &App) -> Option<MachineStatus> {
        self.machines.read(cx).status(machine).cloned()
    }
    /// What the views offer for `machine`'s rho, for UI tests.
    pub(crate) fn rho_offer(&self, machine: &MachineId, cx: &App) -> Option<Offer> {
        self.installs.read(cx).offer(machine, cx)
    }
    /// The user's projects, for UI tests.
    pub(crate) fn projects(&self, cx: &App) -> Vec<Place> {
        self.projects.read(cx).roots().cloned().collect()
    }
    /// `machine`'s home directory, as its daemon said, for UI tests.
    pub(crate) fn workspaces_home(&self, machine: &MachineId, cx: &App) -> Option<PathBuf> {
        self.workspaces.read(cx).home(machine)
    }
    /// The files the right panel's Diff surface shows, if it is open, for
    /// UI tests.
    pub(crate) fn diff_paths(&self, cx: &App) -> Option<Vec<String>> {
        self.panel.read(cx).diff_paths(cx)
    }
    /// The file the right panel's Files surface has open, and the text it
    /// shows, for UI tests.
    pub(crate) fn open_file(&self, cx: &App) -> Option<(String, String)> {
        self.panel.read(cx).open_file(cx)
    }
    /// The text the right panel's Brief surface shows, if it is open, for
    /// UI tests.
    pub(crate) fn brief_text(&self, cx: &App) -> Option<String> {
        self.panel.read(cx).brief_text(cx)
    }
    /// Why the right panel's Context surface's latest read found nothing,
    /// the context's tokens it last found, and how many reads have landed,
    /// if it is open, for UI tests.
    pub(crate) fn context_read(
        &self,
        cx: &App,
    ) -> Option<(Option<rho_ui_core::ReportError>, Option<u64>, usize)> {
        self.panel.read(cx).context_read(cx)
    }
    /// The right panel's tabs, in order, for UI tests.
    pub(crate) fn panel_tabs(&self, cx: &App) -> Vec<crate::right_panel::Surface> {
        self.panel.read(cx).tab_order()
    }
    /// The right panel's terminal, for UI tests.
    pub(crate) fn terminal(&self, cx: &App) -> Option<Entity<crate::right_panel::TerminalView>> {
        self.panel.read(cx).terminal(cx)
    }
    /// The side chat the right panel shows, for UI tests.
    pub(crate) fn side_chat(&self, cx: &App) -> Option<Entity<SessionView>> {
        self.panel.read(cx).side_chat()
    }
    /// The panes of the right panel's terminal tab on screen, for UI tests.
    pub(crate) fn terminal_panes(&self, cx: &App) -> usize {
        self.panel.read(cx).terminal_panes(cx)
    }
    /// The sidebar's search, for UI tests.
    pub(crate) fn sidebar_search(&self, cx: &App) -> FocusHandle {
        self.sidebar.read(cx).search_focus_handle(cx)
    }
}

impl Render for RhoApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
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
            .on_action(|_: &ToggleRightPanel, _, cx| PanelLayout::toggle(cx))
            .on_action(cx.listener(|this, _: &FindInTranscript, window, cx| this.find(window, cx)))
            // A text field with no search of its own, such as a draft's
            // composer, passes the same key up as Kit's.
            .on_action(cx.listener(|this, _: &Search, window, cx| this.find(window, cx)))
            .on_action(
                cx.listener(|this, shortcut: &TerminalShortcut, window, cx| {
                    this.terminal_shortcut(*shortcut, window, cx);
                }),
            )
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // First, so it sees scroll events before any scroll area.
            .child(Kinetic::hook(&self.kinetic));

        let pane_style = || StyleRefinement::default().flex_1().min_w_0().h_full();
        let sidebar_width = self.sidebar.read(cx).width(cx);
        let rem = f32::from(window.rem_size());
        let row = f32::from(window.viewport_size().width) / rem;
        let main = self.render_main(pane_style(), window, cx);
        root.child(
            h_flex()
                .size_full()
                .items_stretch()
                // A drag of the right panel's edge, wherever the pointer
                // goes; the panel's width is its distance from the right.
                .on_drag_move(cx.listener(
                    |this, event: &DragMoveEvent<ResizeRightPanel>, window, cx| {
                        let rem = f32::from(window.rem_size());
                        let bounds = event.bounds;
                        let width = f32::from(bounds.right() - event.event.position.x) / rem;
                        let row = f32::from(bounds.size.width) / rem;
                        let sidebar = this.sidebar.read(cx).width(cx).0;
                        PanelLayout::resize(width, row, sidebar, cx);
                    },
                ))
                .on_drop(|_: &ResizeRightPanel, _, cx| PanelLayout::save_width(cx))
                // A drag of the open sidebar's edge: its width is the
                // pointer's distance from the left, leaving the pane its
                // room beside the right panel.
                .on_drag_move(cx.listener(
                    |this, event: &DragMoveEvent<ResizeSidebar>, window, cx| {
                        let rem = f32::from(window.rem_size());
                        let bounds = event.bounds;
                        let width = f32::from(event.event.position.x - bounds.left()) / rem;
                        let row = f32::from(bounds.size.width) / rem;
                        let sidebar = this.sidebar.read(cx).width(cx).0;
                        let panel = if PanelLayout::get(cx) {
                            PanelLayout::width(row, sidebar, cx).0
                        } else {
                            0.
                        };
                        let space = row - panel - MIN_PANE_REMS;
                        this.sidebar
                            .update(cx, |sidebar, _| sidebar.resize(width, space));
                        cx.notify();
                    },
                ))
                .on_drop(cx.listener(|this, _: &ResizeSidebar, _, cx| {
                    this.sidebar.read(cx).save_width();
                }))
                .child(
                    self.sidebar.clone().cached(
                        StyleRefinement::default()
                            .w(sidebar_width)
                            .h_full()
                            .flex_shrink_0(),
                    ),
                )
                .child(main)
                .when(PanelLayout::get(cx), |this| {
                    this.child(
                        self.panel.clone().cached(
                            StyleRefinement::default()
                                .w(PanelLayout::width(row, sidebar_width.0, cx))
                                .h_full()
                                .flex_shrink_0(),
                        ),
                    )
                }),
        )
        .children(window_chrome::drag_areas(cx))
        .children(window_chrome::caption_buttons(window, cx))
        .into_any_element()
    }
}
