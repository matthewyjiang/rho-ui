//! Headless UI tests: the real app in an offscreen window, driven by
//! simulated clicks and keys, against rho-ui-core's scripted daemon
//! (`fake_daemon`: rho-protocol's real server side over a real socket).
//!
//! Each test saves screenshots to `target/ui-shots/` for a person to review.
//! On Linux, rendering runs on Mesa lavapipe, Vulkan in software (see
//! `SELECT_LAVAPIPE`); on macOS, on Metal.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, LazyLock},
    time::Duration,
};

use web_time::Instant;

use gpui_kit::test::{
    ClickOptions, TestEventExt as _, TestInput, TestQueryExt as _, TestWindowExt as _,
};
use gpui_kit::{
    px, size, AnyWindowHandle, App, AppContext as _, Bounds, ClipboardEntry, ClipboardItem,
    ElementId, ElementInputHandler, Entity, ExternalPaths, HeadlessAppContext, ImageFormat,
    IntoElement, Modifiers, MouseButton, MouseExitEvent, Pixels, PlatformInput, Render,
    SharedString, Size, SystemNotification, Window, WindowBounds, WindowOptions,
};
use rho_ui_core::{
    agent_settings::{ModelSelection, SettingValue},
    fake_daemon::{
        report, write_program, Answers, FakeDaemon, PullRequestMethods, Script, SessionLists,
        Started, TuiJob, COMPACTED, KEY_AUTH, LOGIN_AUTH, PULL_REQUEST_BODY, QUESTION_REASON,
        QUESTION_REASON_AT_LENGTH, REFUSED_KEY, SUBAGENT_AGENT, SUBAGENT_MODEL, SUBAGENT_TASK,
        TUI_SESSION, UNCHECKED_KEY, WORKFLOW_ERROR,
    },
    platform, Attachment, BackgroundKind, BackgroundStatus, BranchLookup, Checkout, Choices,
    Compaction, ContextSize, Daemon, DaemonTarget, Delay, Entry, Form, InputRequest,
    LinkedPullRequest, ModelRef, NewSessionMark, PermissionMode, ProcessStream, PromptKey,
    PullRequestId, PullRequestLinkOrigin, PullRequestRead, PullRequestRollup, PullRequestState,
    PullRequestSummary, PullRequestTrouble, ReasoningLevel, ReportError, RestartCost, RetryReason,
    RhoInstall, RunState, Session, SessionId, SessionMark, SessionOptions, SessionPullRequests,
    StartedMode, StartedSessionDefaults, ToolStatus, WorktreeVerdict, LEAD_AGENT,
};

use gpui_kit::component::{ActiveTheme as _, IndexPath, WindowExt as _};

use crate::{
    app::{LaunchConfig, RhoApp},
    attention::Notifier,
    forge::{
        picture::{self, test::serve_pictures},
        Check, ForgeCli,
    },
    leads::Kind,
    machines::{MachineId, MachineStatus, Place, Reach, RemoteMachine, SessionKey},
    projects::Projects,
    pull_requests::{Serving, Shown, Which},
    rho_install::Offer,
    right_panel::{PanelLayout, Surface, TerminalShell, TerminalView, PROGRAM_POLL},
    settings::{Appearance, Settings, SmoothScroll, UiScale},
    sidebar::{SavedFilter, SidebarLayout, SidebarState},
    store::{Key, StateDb},
    title_bar::TOP_ROW_REMS,
    turn_times::TurnTimes,
    unseen::Unseen,
    window_chrome::Chrome,
};

mod cards;
mod configured;
mod live;
mod mcp;
mod plugins;
mod surfaces;

/// Points the Vulkan loader at lavapipe before `main`, while the process has
/// one thread and setting a variable races no one; a variable already set
/// wins.
///
/// GPUI's headless renderer keeps its device for the life of the process and
/// never drops it. On NVIDIA's driver (seen with 550), the device's "[vkps]"
/// thread runs on during `exit`, while `libEGL_nvidia`'s exit handler unloads
/// the code under it: about one run in five ended in SIGSEGV after every test
/// passed. Lavapipe has no such thread, and draws the same screenshots on any
/// machine. The cost is speed: the suite took 4.9 to 7.7s here, against 2.2s
/// on an RTX 3060.
#[cfg(target_os = "linux")]
#[used]
#[link_section = ".init_array"]
static SELECT_LAVAPIPE: extern "C" fn() = {
    extern "C" fn select_lavapipe() {
        const VAR: &str = "VK_LOADER_DRIVERS_SELECT";
        if std::env::var_os(VAR).is_none() {
            std::env::set_var(VAR, "*lvp*");
        }
    }
    select_lavapipe
};

/// The text system the app draws with on this platform.
#[cfg(not(target_os = "macos"))]
fn text_system() -> Arc<dyn gpui_kit::PlatformTextSystem> {
    gpui_kit::platform::current_platform(true).text_system()
}

/// The text system GPUI's Mac platform makes, made alone: the platform
/// itself must be made on the main thread, and tests run on others.
#[cfg(target_os = "macos")]
fn text_system() -> Arc<dyn gpui_kit::PlatformTextSystem> {
    Arc::new(gpui_apple::AppleTextSystem::new())
}

/// GPUI's offscreen renderer for this platform.
#[cfg(not(windows))]
use gpui_kit::platform::current_headless_renderer as headless_renderer;

/// GPUI has no offscreen renderer for Windows; its wgpu one draws there,
/// on Vulkan, which CI gives with Mesa lavapipe.
#[cfg(windows)]
fn headless_renderer() -> gpui_kit::Result<Option<Box<dyn gpui_kit::PlatformHeadlessRenderer>>> {
    let renderer: Box<dyn gpui_kit::PlatformHeadlessRenderer> =
        Box::new(gpui_wgpu::WgpuHeadlessRenderer::new()?);
    Ok(Some(renderer))
}

/// `linux` off macOS, `macos` on it: the terminal's keys, as Ghostty's,
/// differ by more than Cmd for Ctrl, so `secondary-` can't say them.
const fn key(linux: &'static str, macos: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        macos
    } else {
        linux
    }
}

/// Far longer than the fake daemon takes: it answers at once, and its
/// slowest script waits half a second.
const WAIT: Duration = Duration::from_secs(10);

/// `/bin/sh` as dash runs it, whatever shell it is: prompting `$ `, where
/// bash, as on Arch and macOS, prompts `sh-5.3$ `, and without line editing, so the
/// terminal echoes a control key such as Ctrl+B as `^B`. A script named
/// `sh`, so the shell it runs keeps the name the terminal tab shows. Kept
/// in `target/`, beside the screenshots, as a static is never dropped to
/// clean up a temporary folder.
#[cfg(unix)]
static TEST_SHELL: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-shell");
    std::fs::create_dir_all(&dir).expect("make the test shell's folder");
    let path = dir.join("sh");
    let script = "#!/bin/sh\n\
        export PS1='$ '\n\
        if [ -n \"$BASH_VERSION\" ]; then exec /bin/sh --noediting; fi\n\
        exec /bin/sh\n";
    write_program(&path, script);
    path
});

/// `cmd.exe`, the shell a Windows terminal runs; the tests that script a
/// shell's output speak POSIX `sh` and are ignored there.
#[cfg(windows)]
static TEST_SHELL: LazyLock<PathBuf> = LazyLock::new(|| PathBuf::from("cmd.exe"));

/// Writes a `rho` at `path` that answers as rho 3.1.0, with 3.2.0 out:
/// `update --check --json` says whether an update is available with the
/// `sh` expression `available`, `update` creates `updated` beside it, and
/// `daemon restart` creates `restarted`.
fn write_fake_rho(path: &Path, available: &str) {
    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0")
case $1 in
  --version) echo "rho 3.1.0" ;;
  daemon) [ "$2" = restart ] && touch "$dir/restarted" ;;
  update)
    if [ "$2" = --check ]; then
      if {available}; then a=true; else a=false; fi
      printf '{{"current":"3.1.0","latest":"3.2.0","update_available":%s,"method":"script","command":"rho update","needs_terminal":false}}
' "$a"
    else
      touch "$dir/updated"
    fi ;;
esac
"#
    );
    write_program(path, &script);
}

/// Marks `daemon`'s sessions `ids` kept as of now, as a restore in another
/// rho-ui would, so they list as active however long ago the fake daemon
/// dates them.
fn keep(daemon: &FakeDaemon, ids: &[&str]) {
    for id in ids {
        daemon.set_mark(id, &NewSessionMark::Kept);
    }
}

/// One app window and its context.
struct Harness {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    app: Entity<RhoApp>,
    name: &'static str,
    target: DaemonTarget,
    /// Stops when the test ends.
    daemon: Option<FakeDaemon>,
    /// The state database the app saves to, in memory.
    state: StateDb,
    /// The desktop notifications the app posted, in order.
    notifications: Rc<RefCell<Vec<SystemNotification>>>,
}

impl Harness {
    /// Opens the app at 1200x800 against a fake daemon holding `script`.
    /// The fake's saved sessions are kept, so they list as active.
    fn start(name: &'static str, script: Script) -> Self {
        Self::start_with(name, script, &["s1", "old"])
    }

    /// Closes the app and opens it again on the same daemon and state
    /// database, as a restart does. The daemon keeps the sessions' marks.
    fn reopen(self, name: &'static str) -> Self {
        self.reopen_with(name, Projects::default())
    }

    /// [`Self::reopen`] with `projects` as the user's projects.
    fn reopen_with(mut self, name: &'static str, projects: Projects) -> Self {
        let (target, daemon, state) = (self.target.clone(), self.daemon.take(), self.state.clone());
        drop(self);
        let machines = Machines {
            projects,
            ..Machines::default()
        };
        Self::open_on(name, target, daemon, machines, state)
    }

    /// [`Self::start`] with only the sessions `kept` kept ([`keep`]).
    fn start_with(name: &'static str, script: Script, kept: &[&str]) -> Self {
        let daemon = FakeDaemon::start(script);
        keep(&daemon, kept);
        Self::open(name, daemon.target(), Some(daemon), Projects::default())
    }

    /// Opens the app at 1200x800, reaching its daemon as `target` says.
    fn open(
        name: &'static str,
        target: DaemonTarget,
        daemon: Option<FakeDaemon>,
        projects: Projects,
    ) -> Self {
        Self::open_with(
            name,
            target,
            daemon,
            Machines {
                projects,
                ..Machines::default()
            },
        )
    }

    /// [`Self::open`] with `machines`' settings, which may list remote
    /// machines, each reaching a fake daemon's socket in place of SSH.
    fn open_with(
        name: &'static str,
        target: DaemonTarget,
        daemon: Option<FakeDaemon>,
        machines: Machines,
    ) -> Self {
        Self::open_on(name, target, daemon, machines, StateDb::in_memory())
    }

    /// [`Self::open_with`] on the state database `state`.
    fn open_on(
        name: &'static str,
        target: DaemonTarget,
        daemon: Option<FakeDaemon>,
        machines: Machines,
        state: StateDb,
    ) -> Self {
        let Machines {
            projects,
            mut settings,
            remote_sockets,
            workspace,
            rho,
            rho_home,
        } = machines;
        let rho = rho.map(|rho| RhoInstall::Here { rho });
        let notifications: Rc<RefCell<Vec<SystemNotification>>> = Rc::default();
        // Never the user's gh, which would ask GitHub about their account.
        if settings.local_cli.get(ForgeCli::Gh).is_empty() {
            settings
                .local_cli
                .set(ForgeCli::Gh, "/nonexistent/gh".to_owned());
        }
        // Never the user's clone folder, whose projects would join the list.
        if settings.local_clone_folder.is_empty() {
            "/nonexistent/clones".clone_into(&mut settings.local_clone_folder);
        }
        let mut cx = HeadlessAppContext::with_platform(
            text_system(),
            Arc::new(crate::Assets::new("")),
            headless_renderer,
        );
        // The daemon's answers arrive on a socket: its I/O thread wakes
        // tasks, which the deterministic scheduler only accepts this way.
        cx.allow_parking();
        cx.update(|cx| crate::init(cx, &state));
        // Every platform lays the app out alike: the tests of the app's own
        // title bar set theirs.
        cx.update(|cx| cx.set_global(Chrome::System));
        cx.update(|cx| settings.apply(cx));
        // A remote machine reaches its fake daemon, never ssh: a host
        // without one gets a socket that isn't there.
        let launch = LaunchConfig {
            workspace: Some(workspace.unwrap_or_else(std::env::temp_dir)),
            local: Reach {
                daemon: target.clone(),
                rho: rho.clone(),
            },
            remote: Rc::new(move |remote: &RemoteMachine| {
                let socket = remote_sockets
                    .iter()
                    .find(|(host, _)| *host == remote.host)
                    .map_or_else(
                        || PathBuf::from("/nonexistent/rho.sock"),
                        |(_, socket)| socket.clone(),
                    );
                Reach {
                    daemon: DaemonTarget::Socket(socket),
                    rho: rho.clone(),
                }
            }),
            // Not the user's shell, whose files may print or ask things.
            shell: TerminalShell(TEST_SHELL.clone()),
            // Never the user's rho home, where leads would make folders.
            rho_home: Some(rho_home.unwrap_or_else(|| PathBuf::from("/nonexistent/rho"))),
            notifier: Notifier::Kept(Rc::clone(&notifications)),
        };
        let sidebar = SidebarState {
            unseen: Unseen::load(&state),
            layout: SidebarLayout::load(&state),
            filter: SavedFilter::load(&state),
        };
        let (window, app) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: gpui_kit::Point::default(),
                            size: size(px(1200.), px(800.)),
                        })),
                        focus: false,
                        show: false,
                        ..Default::default()
                    },
                    cx,
                    // In-memory projects and layout: tests never touch
                    // the user's state database.
                    |window, cx| cx.new(|cx| RhoApp::new(launch, sidebar, projects, window, cx)),
                )
            })
            .expect("open the app window");
        let mut harness = Self {
            cx,
            window,
            app,
            name,
            target,
            daemon,
            state,
            notifications,
        };
        harness.frame();
        harness
    }

    /// Runs ready tasks and paints a frame.
    fn frame(&mut self) {
        self.cx.run_until_parked();
        self.cx
            .update_window(self.window, |_, window, cx| window.render_frame(cx))
            .expect("window is open");
    }

    /// Runs `act` against the window, then lets its effects settle.
    fn act(&mut self, act: impl FnOnce(&mut Window, &mut App)) {
        self.cx
            .update_window(self.window, |_, window, cx| act(window, cx))
            .expect("window is open");
        self.frame();
    }

    /// Polls `ready` on fresh frames until it holds; panics after [`WAIT`].
    fn wait_until(&mut self, what: &str, ready: impl Fn(&Window, &App, &RhoApp) -> bool) {
        self.wait_within(WAIT, what, ready);
    }

    /// [`Self::wait_until`] with its own limit, for a real model's turns.
    fn wait_within(
        &mut self,
        limit: Duration,
        what: &str,
        ready: impl Fn(&Window, &App, &RhoApp) -> bool,
    ) {
        self.poll(
            limit,
            what,
            |ui| {
                ui.frame();
                let app = ui.app.clone();
                ui.cx
                    .update_window(ui.window, |_, window, cx| ready(window, cx, app.read(cx)))
                    .expect("window is open")
            },
            Self::labels_seen,
        );
    }

    /// Tries `ready` until it holds, 5 ms apart; `ready` draws the frames
    /// it needs. Past `limit`, panics naming `what`, with what `seen` says
    /// is there instead and a screenshot.
    fn poll(
        &mut self,
        limit: Duration,
        what: &str,
        mut ready: impl FnMut(&mut Self) -> bool,
        seen: impl FnOnce(&mut Self) -> String,
    ) {
        let deadline = Instant::now() + limit;
        while !ready(self) {
            if Instant::now() >= deadline {
                let seen = seen(self);
                let shot = self.shot("timeout");
                panic!(
                    "timed out waiting for {what}; {seen}; screenshot: {}",
                    shot.display()
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Every accessibility label in the window, for [`Self::poll`]'s
    /// failure message.
    fn labels_seen(&mut self) -> String {
        let seen = self
            .cx
            .update_window(self.window, |_, window, _| labels(window))
            .expect("window is open");
        format!("labelled elements: {seen:?}")
    }

    /// Waits for the right panel's terminal to show `ready` lines.
    fn wait_for_terminal(&mut self, what: &str, ready: impl Fn(&[String]) -> bool) {
        let lines = |ui: &mut Self| {
            let app = ui.app.clone();
            ui.cx.update(|cx| {
                let terminal = app.read(cx).terminal(cx)?;
                Some(terminal.update(cx, |terminal, _| terminal.text()))
            })
        };
        self.poll(
            WAIT,
            what,
            |ui| {
                ui.frame();
                lines(ui).as_deref().is_some_and(&ready)
            },
            |ui| format!("the terminal shows {:?}", lines(ui)),
        );
    }

    /// A harness named `name` whose session's terminal runs its shell
    /// and has the keyboard.
    fn with_terminal(name: &'static str) -> Self {
        let mut ui = Self::start(name, Script::default());
        ui.click("new-session");
        ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
        ui.send("background");
        ui.wait_for_session("the session", |session| session.state() == &RunState::Idle);
        ui.act(|window, cx| window.press("secondary-alt-b", cx));
        ui.click("open-surface-terminal");
        ui.wait_for_terminal("the shell's prompt", |lines| {
            lines.iter().any(|line| line.starts_with('$'))
        });
        ui.click("terminal");
        ui
    }

    /// Waits for element `id` to be labelled `want`, moving the clock on
    /// for timers that poll, such as a terminal tab's program.
    fn wait_for_label(&mut self, id: &'static str, want: &str) {
        self.poll(
            WAIT,
            &format!("{id} to say {want:?}"),
            |ui| {
                ui.cx.advance_clock(PROGRAM_POLL);
                ui.frame();
                ui.label(id).as_deref() == Some(want)
            },
            |ui| format!("it says {:?}", ui.label(id)),
        );
    }

    /// The number of panes in the right panel's terminal tab on screen.
    fn terminal_panes(&mut self) -> usize {
        let app = self.app.clone();
        self.cx.update(|cx| app.read(cx).terminal_panes(cx))
    }

    /// Moves the pointer to the middle of terminal pane `pane`.
    fn hover_pane(&mut self, pane: usize) {
        self.act(|window, cx| {
            let center = window
                .within(("terminal-pane", pane))
                .find("terminal")
                .bounds()
                .center();
            window.pointer_move(center, None, cx);
        });
    }

    /// Puts the keyboard in the active session's composer.
    fn focus_composer(&mut self) {
        let app = self.app.clone();
        self.act(|window, cx| {
            let view = app.read(cx).active_session().expect("a session shows");
            let composer = view.read(cx).composer_focus_handle(cx);
            window.focus(&composer, cx);
        });
    }

    /// The right panel's terminal, once its shell runs.
    fn terminal(&mut self) -> Entity<TerminalView> {
        let app = self.app.clone();
        self.cx
            .update(|cx| app.read(cx).terminal(cx))
            .expect("the terminal's shell runs")
    }

    /// The terminal's screen, a line a row.
    fn terminal_lines(&mut self) -> Vec<String> {
        let terminal = self.terminal();
        self.cx
            .update(|cx| terminal.update(cx, |terminal, _| terminal.text()))
    }

    /// The row of the terminal's line `text`, and where its cell at
    /// `column` is drawn.
    fn terminal_cell(&mut self, text: &str, column: u16) -> (usize, Bounds<Pixels>) {
        let terminal = self.terminal();
        let lines = self.terminal_lines();
        self.cx.update(|cx| {
            let row = lines
                .iter()
                .position(|line| line == text)
                .unwrap_or_else(|| panic!("no line {text:?} in {lines:?}"));
            let cell = terminal
                .read(cx)
                .cell_bounds(column, u16::try_from(row).expect("a screen has few rows"))
                .expect("the terminal is drawn");
            (row, cell)
        })
    }

    /// Clicks the terminal as `options` say, at window position `at`.
    fn click_terminal(&mut self, at: gpui_kit::Point<Pixels>, options: ClickOptions) {
        self.act(|window, cx| {
            let offset = at - window.find("terminal").bounds().origin;
            window.click_with_options("terminal", options.with_offset(offset), cx);
        });
    }

    /// Presses the terminal's copy key, Ctrl+Shift+C (Cmd+C on macOS), and
    /// returns the clipboard's text.
    fn copy_from_terminal(&mut self) -> Option<String> {
        self.act(|window, cx| window.press(key("ctrl-shift-c", "cmd-c"), cx));
        self.cx
            .update(|cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
    }

    /// Runs `act` as an input method on the terminal.
    fn input_method<R>(&mut self, act: impl FnOnce(&mut TestInput<'_>, &mut App) -> R) -> R {
        let terminal = self.terminal();
        let mut result = None;
        self.act(|window, cx| {
            let bounds = window.find("terminal").bounds();
            let handler = ElementInputHandler::new(bounds, terminal);
            result = Some(act(&mut TestInput::new(handler, window), cx));
        });
        result.expect("the input method ran")
    }

    /// Opens the fake daemon's saved session "old", and waits for its
    /// replay of more than 20 entries to catch up.
    fn open_old_session(&mut self) {
        self.wait_until("saved sessions", |window, _, _| {
            is_visible(window, "session-local-old")
        });
        self.click("session-local-old");
        self.wait_for_session("the replayed history", |session| {
            !session.is_replaying() && session.entries().len() > 20
        });
    }

    /// Waits for the active session to satisfy `ready`.
    fn wait_for_session(&mut self, what: &str, ready: impl Fn(&Session) -> bool) {
        self.wait_until(what, |_, cx, app| {
            app.active_session()
                .is_some_and(|view| ready(view.read(cx).session()))
        });
    }

    fn click(&mut self, id: &'static str) {
        self.act(|window, cx| window.click(id, cx));
    }

    /// Clicks turn header `id`, and lets its working spring open or shut:
    /// a frame starts the spring, then the clock jumps past its settling.
    fn toggle_turn(&mut self, id: &'static str) {
        self.click(id);
        self.frame();
        self.cx.advance_clock(Duration::from_secs(1));
    }

    /// Sends `text` to session `id` as another client would, such as rho's
    /// terminal UI: over a connection of its own, which it then closes. A
    /// session with a turn under way takes it as a steer.
    fn send_from_elsewhere(&mut self, id: &str, text: &str, how: Send) {
        self.send_on(self.target.clone(), id, text, how);
    }

    /// [`Self::send_from_elsewhere`] to the daemon at `target`.
    fn send_on(&mut self, target: DaemonTarget, id: &str, text: &str, how: Send) {
        let (id, text) = (SessionId::from(id), text.to_owned());
        self.cx.update(|cx| {
            cx.spawn(async move |_| {
                let (daemon, _events) = Daemon::connect(target)
                    .await
                    .expect("a second client connects");
                match how {
                    Send::Prompt => daemon
                        .prompt(&id, &text.into(), &PromptKey::generate())
                        .await
                        .map(drop),
                    Send::Steer => daemon.steer(&id, &text).await.map(drop),
                }
                .expect("the daemon takes the other client's message");
            })
            .detach();
        });
    }

    /// Gives another window the focus, as the user does on leaving rho-ui
    /// for another app. The app's window must have had it first for GPUI to
    /// say it lost it.
    fn blur(&mut self) {
        self.act(|window, _| window.activate_window());
        let (elsewhere, _) = self
            .cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        focus: false,
                        show: false,
                        ..Default::default()
                    },
                    cx,
                    |_, cx| cx.new(|_| Elsewhere),
                )
            })
            .expect("open another window");
        self.cx
            .update_window(elsewhere, |_, window, _| window.activate_window())
            .expect("the other window is open");
        self.frame();
    }

    /// The fake daemon the app talks to.
    fn fake(&self) -> &FakeDaemon {
        self.daemon.as_ref().expect("the test runs a fake daemon")
    }

    /// Waits for the fake daemon to be `ready`, as once it has a request.
    fn wait_for_daemon(&mut self, what: &str, ready: impl Fn(&FakeDaemon) -> bool) {
        self.poll(
            WAIT,
            what,
            |ui| {
                ui.frame();
                ready(ui.fake())
            },
            Self::labels_seen,
        );
    }

    /// The accessibility label of element `id`, once it is drawn.
    fn label(&mut self, id: impl Into<ElementId>) -> Option<String> {
        self.cx
            .update_window(self.window, |_, window, _| {
                window
                    .try_find(id)
                    .filter(gpui_kit::test::ElementSnapshot::visible)
                    .and_then(|element| element.label().map(str::to_owned))
            })
            .expect("window is open")
    }

    /// Types `text` in the composer and presses Enter. In a session, it
    /// first moves the keyboard to the composer, as a click in it would.
    fn send(&mut self, text: &str) {
        self.type_in(text);
        self.act(|window, cx| window.press("enter", cx));
    }

    /// Types `text` in the composer. In a session, it first moves the
    /// keyboard to the composer, as a click in it would.
    fn type_in(&mut self, text: &str) {
        let app = self.app.clone();
        self.act(|window, cx| {
            if let Some(view) = app.read(cx).active_session() {
                let composer = view.read(cx).composer_focus_handle(cx);
                window.focus(&composer, cx);
            }
            window.input(text, cx);
        });
    }

    /// What the active session's composer holds.
    fn composer_text(&mut self) -> String {
        let app = self.app.clone();
        self.cx.update(|cx| {
            let view = app.read(cx).active_session().expect("a session is open");
            view.read(cx).composer_text(cx)
        })
    }

    /// Pastes `image` in the composer with Ctrl+V (Cmd+V on macOS), as from a screenshot
    /// tool.
    fn paste(&mut self, image: gpui_kit::Image) {
        self.paste_entry(ClipboardEntry::Image(image));
    }

    /// Pastes `entry` in the composer with Ctrl+V (Cmd+V on macOS). In a session, it first
    /// moves the keyboard to the composer.
    fn paste_entry(&mut self, entry: ClipboardEntry) {
        let app = self.app.clone();
        self.act(|window, cx| {
            if let Some(view) = app.read(cx).active_session() {
                let composer = view.read(cx).composer_focus_handle(cx);
                window.focus(&composer, cx);
            }
            cx.write_to_clipboard(ClipboardItem {
                entries: vec![entry],
            });
            window.press("secondary-v", cx);
        });
    }

    /// Saves the window as `target/ui-shots/<test>-<step>.png`.
    fn shot(&mut self, step: &str) -> PathBuf {
        self.frame();
        let image = self
            .cx
            .capture_screenshot(self.window)
            .expect("offscreen rendering needs Mesa lavapipe (Vulkan in software)");
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-shots");
        std::fs::create_dir_all(&dir).expect("create the screenshot directory");
        let path = dir.join(format!("{}-{step}.png", self.name));
        image.save(&path).expect("save the screenshot");
        path
    }

    /// The active session's state, cloned out of the app.
    fn session(&mut self) -> Session {
        let app = self.app.clone();
        self.cx.update(|cx| {
            app.read(cx)
                .active_session()
                .expect("a session is open")
                .read(cx)
                .session()
                .clone()
        })
    }
}

/// What [`Harness::open_with`] opens the app with besides its daemon.
#[derive(Default)]
struct Machines {
    projects: Projects,
    settings: Settings,
    /// The socket each remote machine reaches, by host.
    remote_sockets: Vec<(String, PathBuf)>,
    /// Where new sessions start on this machine; the temporary directory
    /// when `None`.
    workspace: Option<PathBuf>,
    /// The `rho` every machine runs to check for updates, install or
    /// update; with `None`, rho-ui leaves rho alone, as for any daemon
    /// reached at a socket.
    rho: Option<PathBuf>,
    /// This machine's rho home, where leads keep their folders; one that
    /// isn't there, so none can be made, when `None`.
    rho_home: Option<PathBuf>,
}

/// Another app's window, which [`Harness::blur`] gives the focus.
struct Elsewhere;

impl Render for Elsewhere {
    fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<'_, Self>) -> impl IntoElement {
        gpui_kit::Empty
    }
}

/// How [`Harness::send_from_elsewhere`] sends its message.
#[derive(Clone, Copy)]
enum Send {
    Prompt,
    Steer,
}

/// Every accessibility label in the last frame, for timeout messages.
fn labels(window: &Window) -> Vec<String> {
    window
        .elements()
        .iter()
        .filter_map(|element| element.label().map(str::to_owned))
        .collect()
}

fn is_visible(window: &Window, id: impl Into<ElementId>) -> bool {
    window.try_find(id).is_some_and(|element| element.visible())
}

/// Where a visible element is drawn.
fn bounds(window: &Window, id: &'static str) -> Option<Bounds<Pixels>> {
    window
        .try_find(id)
        .filter(gpui_kit::test::ElementSnapshot::visible)
        .map(|element| element.bounds())
}

/// Sidebar, new session draft, prompt, approval, tool card, reply, idle,
/// and the session in another rho-ui's sidebar.
#[test]
fn new_session_runs_a_turn_through_an_approval() {
    let mut ui = Harness::start("turn", Script::default());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-local-s1") && is_visible(window, "session-local-old")
    });
    ui.shot("sidebar");

    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.act(|window, cx| {
        window.input("list files", cx);
        window.press("enter", cx);
    });
    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    assert_eq!(ui.session().state(), &RunState::NeedsAction);
    // rho names a session after its first turn; until then the header and
    // the sidebar row both show the first prompt.
    assert_eq!(ui.session().title(), None);
    ui.act(|window, _| {
        assert_eq!(window.find("session-title").label(), Some("list files"));
        assert_eq!(
            window.find("session-local-new-1").label(),
            Some("list files")
        );
        // The turn's header counts up while it runs.
        let header = window.find("worked-0").label().map(str::to_owned);
        assert!(
            header
                .as_deref()
                .is_some_and(|header| header.starts_with("Working for ")),
            "the running turn's header: {header:?}"
        );
    });
    ui.shot("approval");

    ui.click("approval-allow-once");
    ui.wait_for_session("the turn to finish", |session| {
        session.state() == &RunState::Idle && session.entries().len() == 3
    });
    let session = ui.session();
    let entries = session.entries();
    assert!(matches!(&entries[0], Entry::User(m) if m.text() == "list files"));
    assert!(
        matches!(&entries[1], Entry::Tool(t) if t.title() == "ls" && t.status() == ToolStatus::Done)
    );
    assert!(matches!(&entries[2], Entry::Agent(m) if m.text() == "you chose allow_once"));
    ui.act(|window, _| {
        assert!(
            window.try_find("approval-allow-once").is_none(),
            "the approval card closes"
        );
        assert_eq!(
            window.find("composer-send").disabled(),
            Some(true),
            "send waits for new text"
        );
    });

    // Saved once prompted, and listed.
    ui.wait_until("the new session in the sidebar", |window, _, _| {
        is_visible(window, "session-local-new-1")
    });

    // The finished turn's working folds behind its header.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the working folded", |window, _, _| {
        !is_visible(window, "step-1")
    });

    // Inside, a lone tool call is one line, and opening it shows its output.
    ui.toggle_turn("worked-0");
    ui.wait_until("the turn's working", |window, _, _| {
        is_visible(window, "step-1")
    });
    ui.act(|window, cx| window.within("step-1").click("step-line", cx));
    ui.wait_until("the tool's output", |window, _, _| {
        is_visible(window, "details-1")
    });
    ui.shot("done");

    // The footer's context ring opens its numbers on hover.
    let usage = ui
        .session()
        .usage()
        .cloned()
        .expect("the daemon reported usage");
    assert_eq!((usage.used(), usage.size()), (2_500, Some(10_000)));
    ui.act(|window, cx| {
        let ring = window.find("composer-usage");
        assert_eq!(ring.label(), Some("Context 25% · $0.01"));
        window.pointer_move(ring.bounds().center(), None, cx);
    });
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the usage card", |window, _, _| {
        is_visible(window, "usage-details")
    });
    ui.shot("usage");

    // Another rho-ui on the daemon lists the session as active too: the
    // daemon dates a session, whoever started it.
    let mut ui = ui.reopen("new-session-elsewhere");
    ui.wait_until("the session active in another rho-ui", |window, _, _| {
        is_visible(window, "session-local-new-1")
    });
}

/// Opens a new session and sends it `prompt`.
fn new_session_with(ui: &mut Harness, prompt: &str) {
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.act(|window, cx| {
        window.input(prompt, cx);
        window.press("enter", cx);
    });
}

/// The last thing the agent said in the active session.
fn last_reply(ui: &mut Harness) -> String {
    match ui.session().entries().last() {
        Some(Entry::Agent(reply)) => reply.text().to_owned(),
        other => panic!("the transcript ends in {other:?}"),
    }
}

/// While the provider makes the turn wait, the turn's header says why and
/// for how long, and the agent's plan shows above the composer, folded
/// until its heading is clicked. A response the provider discarded leaves
/// the transcript; the retry's answer shows in its place, and the finished
/// plan goes.
#[test]
fn a_retried_response_replaces_the_discarded_one_and_the_plan_shows() {
    let mut ui = Harness::start("retry", Script::default());
    new_session_with(&mut ui, "flaky");
    ui.wait_for_session("the turn waiting to retry", |session| {
        matches!(
            session.delay(),
            Some(Delay::Retrying {
                reason: RetryReason::RateLimit,
                until: Some(_)
            })
        )
    });
    // The session's state can land after the frame drawn before it.
    ui.wait_until("the header and the todo", |window, _, _| {
        let label = |id| {
            window
                .try_find(id)
                .and_then(|found| found.label().map(str::to_owned))
        };
        label("worked-0").is_some_and(|header| header.contains(" · rate limited, retrying in "))
            && label("plan").as_deref() == Some("Todo 1 of 3")
    });
    ui.act(|window, _| {
        assert!(!is_visible(window, "plan-0"), "the todo starts folded");
    });
    ui.shot("todo-folded");
    ui.click("plan-toggle");
    ui.wait_until("the todo's steps", |window, _, _| {
        is_visible(window, "plan-0")
    });
    ui.shot("todo-open");
    assert!(
        ui.session()
            .entries()
            .iter()
            .all(|entry| !matches!(entry, Entry::Agent(_))),
        "the discarded answer is gone"
    );
    ui.shot("retrying");

    ui.fake().finish_retry(&SessionId::from("new-1"));
    ui.wait_for_session("the retried answer", |session| {
        session.state() == &RunState::Idle && session.delay().is_none()
    });
    assert_eq!(last_reply(&mut ui), "The whole answer");
    ui.wait_until("the todo gone, every step done", |window, _, _| {
        !is_visible(window, "plan")
    });
    ui.shot("answered");
}

/// Why the agent asks the open questionnaire, if it says.
fn asked_reason(ui: &mut Harness) -> Option<String> {
    match ui.session().inputs().first().map(InputRequest::form) {
        Some(Form::Questions(questions)) => questions.reason.clone(),
        other => panic!("no questionnaire is open: {other:?}"),
    }
}

/// A questionnaire of two questions, one taking the user's own text and
/// one taking several choices, is answered one question at a time. Why
/// the agent asks shows above them when it says, and a long reason scrolls
/// inside the card rather than pushing the questions away.
#[test]
fn a_questionnaire_is_answered_one_question_at_a_time() {
    let mut ui = Harness::start("questions", Script::default());
    new_session_with(&mut ui, "questions");
    ui.wait_until("the first question", |window, _, _| {
        is_visible(window, "question-lang-rust") && is_visible(window, "approval-decline")
    });
    assert_eq!(asked_reason(&mut ui), None);
    ui.act(|window, _| {
        assert!(
            !is_visible(window, "question-checks-fmt"),
            "one question at a time"
        );
        assert!(!is_visible(window, "input-timeout"), "no timeout was set");
        assert!(
            window.try_find("question-reason").is_none(),
            "no reason was given"
        );
        // Both choices have a description; only Rust's wraps.
        let height = |id| bounds(window, id).map(|b| b.size.height);
        assert!(
            height("question-lang-rust") > height("question-lang-go"),
            "a long description wraps"
        );
    });
    ui.shot("first");

    // The user's own text answers a single-choice question.
    ui.act(|window, cx| {
        window.click("question-lang-other", cx);
        window.input("Zig", cx);
    });
    ui.click("questions-next");
    ui.wait_until("the second question", |window, _, _| {
        is_visible(window, "question-checks-fmt") && is_visible(window, "questions-submit")
    });
    // The agent's suggestion, Tests, is picked already; Format joins it.
    ui.click("question-checks-fmt");
    ui.shot("second");
    ui.click("questions-submit");

    ui.wait_for_session("the answered turn", |session| {
        session.state() == &RunState::Idle && session.inputs().is_empty()
    });
    assert_eq!(
        last_reply(&mut ui),
        "you answered checks=fmt,test; lang=Zig"
    );
    ui.act(|window, _| {
        assert!(!is_visible(window, "input-card"), "the card closes");
    });

    // A short reason shows whole; a long one scrolls in its box.
    for (prompt, reason, scrolls, shot) in [
        ("questions why", QUESTION_REASON, false, "reason"),
        (
            "questions why at length",
            QUESTION_REASON_AT_LENGTH,
            true,
            "reason-long",
        ),
    ] {
        ui.send(prompt);
        ui.wait_until("the questions under their reason", |window, _, _| {
            is_visible(window, "question-reason") && is_visible(window, "question-lang-rust")
        });
        assert_eq!(asked_reason(&mut ui).as_deref(), Some(reason));
        ui.act(|window, _| {
            let height = |id| bounds(window, id).map(|b| b.size.height);
            let (shown, text) = (height("question-reason"), height("question-reason-text"));
            assert_eq!(text > shown, scrolls, "{prompt}: {text:?} in {shown:?}");
            assert!(
                is_visible(window, "approval-decline"),
                "{prompt}: the answers stay on screen"
            );
        });
        ui.shot(shot);
        ui.click("approval-decline");
        ui.wait_for_session("the declined turn", |session| {
            session.state() == &RunState::Idle && session.inputs().is_empty()
        });
    }
}

/// An answer the daemon refuses leaves the card as the user filled it in,
/// ready to answer again.
#[test]
fn a_refused_answer_keeps_the_card() {
    let mut ui = Harness::start("refused", Script::default());
    new_session_with(&mut ui, "questions");
    ui.wait_until("the first question", |window, _, _| {
        is_visible(window, "question-lang-other")
    });
    ui.act(|window, cx| {
        window.click("question-lang-other", cx);
        window.input("refuse", cx);
    });
    ui.click("questions-next");
    ui.wait_until("the second question", |window, _, _| {
        is_visible(window, "questions-submit")
    });
    ui.click("questions-submit");
    ui.wait_until("the refusal", |window, _, _| {
        is_visible(window, "session-error")
    });
    ui.act(|window, _| {
        assert!(
            is_visible(window, "question-checks-fmt") && is_visible(window, "questions-submit"),
            "the card stays on the question it was on"
        );
    });
    assert_eq!(ui.session().inputs().len(), 1, "the request is still open");
    ui.shot("refused");

    ui.click("approval-decline");
    ui.wait_for_session("the declined turn", |session| {
        session.state() == &RunState::Idle
    });
    assert_eq!(last_reply(&mut ui), "you declined");
}

/// A question with a timeout shows the daemon's fallback and a countdown,
/// until the user's first key holds it.
#[test]
fn a_timed_question_holds_on_the_first_key() {
    let mut ui = Harness::start("timeout", Script::default());
    new_session_with(&mut ui, "questions timeout");
    ui.wait_until("the countdown", |window, _, _| {
        is_visible(window, "input-timeout")
    });
    let countdown = ui.label("input-timeout").unwrap_or_default();
    assert!(
        countdown.starts_with("Rho answers “Language: Rust” in 4:")
            || countdown.starts_with("Rho answers “Language: Rust” in 5:00"),
        "the countdown names the fallback: {countdown:?}"
    );
    ui.shot("countdown");
    assert_eq!(
        ui.fake().session_holds(),
        0,
        "showing the question holds nothing"
    );

    // The card took the keyboard as it opened, so the keys land in it;
    // the second comes before the daemon says the first held it.
    ui.wait_until("the card to take the keyboard", |window, cx, app| {
        app.active_session()
            .is_some_and(|view| !view.read(cx).composer_focus_handle(cx).is_focused(window))
    });
    ui.act(|window, cx| {
        window.press("down", cx);
        window.press("down", cx);
    });
    ui.wait_until("the hold", |window, cx, app| {
        let held = app.active_session().is_some_and(|view| {
            let inputs = view.read(cx).session().inputs();
            inputs.first().is_some_and(|open| open.timeout().is_none())
        });
        held && !is_visible(window, "input-timeout")
    });
    assert_eq!(ui.fake().session_holds(), 1, "a request is held once");
    ui.shot("held");

    ui.click("approval-decline");
    ui.wait_for_session("the declined turn", |session| {
        session.state() == &RunState::Idle
    });
    assert_eq!(last_reply(&mut ui), "you declined");
}

/// A denial carries the user's reason, and a plan approval their feedback.
#[test]
fn denials_and_plan_approvals_carry_text_for_the_model() {
    let mut ui = Harness::start("feedback", Script::default());
    new_session_with(&mut ui, "list files");
    ui.wait_until("the approval", |window, _, _| {
        is_visible(window, "approval-allow-once") && is_visible(window, "input-text")
    });
    // Enter in the text box denies, with the text as the reason.
    ui.act(|window, cx| {
        window.click("input-text", cx);
        window.input("not in this repo", cx);
    });
    ui.shot("reason");
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_for_session("the denied turn", |session| {
        session.state() == &RunState::Idle
    });
    assert_eq!(last_reply(&mut ui), "you chose deny: not in this repo");

    ui.send("plan");
    ui.wait_until("the plan approval", |window, _, _| {
        is_visible(window, "approval-approve-auto") && is_visible(window, "input-text")
    });
    ui.act(|window, cx| {
        window.click("input-text", cx);
        window.input("tests first", cx);
    });
    ui.shot("plan");
    ui.click("approval-approve-auto");
    ui.wait_for_session("the approved plan", |session| {
        session.state() == &RunState::Idle
    });
    assert_eq!(last_reply(&mut ui), "you chose approve_plan: tests first");
}

/// A sent message renders as markdown; dragging across it selects the
/// text, which copy puts on the clipboard and "Add to prompt" quotes above
/// the composer, leaving the text box as it was.
#[test]
fn a_sent_message_can_be_selected_copied_and_quoted() {
    let mut ui = Harness::start("select", Script::default());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.act(|window, cx| {
        window.input("list **files**", cx);
        window.press("enter", cx);
    });
    ui.wait_until("the sent message", |window, _, _| {
        is_visible(window, "user-0")
    });

    ui.act(|window, cx| {
        let text = bounds(window, "user-0").expect("the message is drawn");
        let y = text.center().y;
        window.drag(
            gpui_kit::point(text.left() + px(1.), y),
            gpui_kit::point(text.right() + px(20.), y),
            cx,
        );
        window.press("secondary-c", cx);
    });
    ui.shot("selected");
    let copied = ui
        .cx
        .update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("list files"));

    ui.wait_until("the add to prompt button", |window, _, _| {
        is_visible(window, "add-to-prompt")
    });
    ui.click("add-to-prompt");
    ui.wait_until("the quote above the composer", |window, _, _| {
        is_visible(window, "composer-quote-1") && !is_visible(window, "add-to-prompt")
    });
    assert_eq!(
        ui.composer_text(),
        "",
        "the quote stays out of the text box"
    );
    // The button put the keyboard in the composer.
    ui.act(|window, cx| window.input("explain", cx));
    ui.shot("quoted");
    let app = ui.app.clone();
    let prompt = ui.cx.update(|cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        view.read(cx).composer_prompt(cx)
    });
    assert_eq!(prompt.text(), "> list files\n\nexplain");

    ui.click("composer-quote-1-remove");
    ui.wait_until("the quote removed", |window, _, _| {
        !is_visible(window, "composer-quote-1")
    });
}

/// One click on a code block's copy button puts its code, and only its
/// code, on the clipboard.
#[test]
fn a_code_block_copies_in_one_click() {
    let script = Script {
        replay_turns: 1,
        ..Script::default()
    };
    let mut ui = Harness::start("copy-code", script);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    ui.click("session-local-old");
    ui.wait_for_session("the replayed reply", |session| {
        session.state() == &RunState::Idle
            && matches!(session.entries().last(), Some(Entry::Agent(_)))
    });
    let reply = ui.cx.update(|cx| {
        let view = ui.app.read(cx).active_session().expect("a session is open");
        let last = view.read(cx).session().entries().len() - 1;
        SharedString::from(format!("reply-{last}"))
    });
    ui.wait_until("the reply's code block", |window, _, _| {
        window.try_find("copy-code").is_some()
    });
    ui.act(|window, cx| window.within(reply.clone()).hover("copy-code", cx));
    ui.shot("hovered");
    ui.act(|window, cx| window.within(reply.clone()).click("copy-code", cx));
    ui.shot("copied");
    let copied = ui
        .cx
        .update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        copied.as_deref(),
        Some("fn main() {\n    println!(\"hi\");\n}")
    );
}

fn permission_mode(session: &Session) -> Option<PermissionMode> {
    session
        .options()
        .and_then(SessionOptions::permission_mode)
        .map(Choices::current)
}

/// The mode the sessions `session` starts open in, once it has said.
fn started_mode(session: &Session) -> Option<StartedMode> {
    session
        .started_session_defaults()
        .map(StartedSessionDefaults::mode)
}

/// The composer's settings: the mode menu on the left, and a button naming
/// the model and reasoning that opens the model list (with search) beside
/// the reasoning levels and fast mode. Each change reaches the daemon.
#[test]
fn composer_settings_change_the_session() {
    let mut ui = Harness::start("settings-panel", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    ui.click("session-local-old");
    ui.wait_until("the session settings", |window, _, _| {
        is_visible(window, "config-panel") && is_visible(window, "config-mode")
    });
    ui.shot("composer");

    // Arrows start at the model in use; Enter picks and closes the panel.
    ui.click("config-panel");
    ui.wait_until("the model list", |window, _, _| {
        is_visible(window, "model-fake/large") && is_visible(window, "config-reasoning-high")
    });
    ui.shot("panel");
    ui.act(|window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    ui.wait_for_session("the new model", |session| {
        session
            .options()
            .is_some_and(|options| options.model().current() == &ModelRef::new("fake", "large"))
    });
    ui.wait_until("the panel to close", |window, _, _| {
        !is_visible(window, "model-fake/large")
    });

    // Search narrows the list by name or value.
    ui.click("config-panel");
    ui.wait_until("the model list", |window, _, _| {
        is_visible(window, "model-fake/small")
    });
    ui.act(|window, cx| window.input("huge", cx));
    ui.wait_until("only the match", |window, _, _| {
        is_visible(window, "model-fake/huge") && !is_visible(window, "model-fake/small")
    });
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_for_session("the searched model", |session| {
        session
            .options()
            .is_some_and(|options| options.model().current() == &ModelRef::new("fake", "huge"))
    });
    // Closed before the click below, which would otherwise close it.
    ui.wait_until("the panel to close", |window, _, _| {
        !is_visible(window, "model-fake/huge")
    });

    // Reasoning and fast mode change in place; the panel stays open.
    ui.click("config-panel");
    ui.wait_until("the reasoning levels", |window, _, _| {
        is_visible(window, "config-reasoning-high")
    });
    ui.click("config-reasoning-high");
    ui.wait_for_session("high reasoning", |session| {
        session
            .options()
            .and_then(SessionOptions::reasoning)
            .map(Choices::current)
            == Some(ReasoningLevel::High)
    });
    ui.wait_until("fast mode, enabled again", |window, _, _| {
        window
            .try_find("config-fast_mode")
            .is_some_and(|switch| switch.visible() && switch.disabled() != Some(true))
    });
    ui.click("config-fast_mode");
    ui.wait_for_session("fast mode", |session| {
        session.options().and_then(SessionOptions::fast_mode) == Some(true)
    });
    ui.act(|window, cx| window.press("escape", cx));
    ui.wait_until("the panel to close", |window, _, _| {
        !is_visible(window, "config-reasoning-high")
    });

    // The mode menu opens above its button.
    ui.click("config-mode");
    click_label(&mut ui, "Plan");
    ui.wait_for_session("plan mode", |session| {
        permission_mode(session) == Some(PermissionMode::Plan)
    });
    ui.shot("changed");
}

/// The settings panel chooses the mode and model of the sessions a session
/// starts. A mode the daemon reports as unknown warns, and leaves the model
/// open to change: one the daemon refuses goes back, with its reason. A
/// change made while a turn runs says it waits for the turn, and applies as
/// the turn ends.
#[test]
fn the_settings_panel_chooses_for_the_sessions_it_starts() {
    let mut ui = Harness::start("started-sessions", Script::default());
    let old = SessionId::from("old");
    ui.fake().save_unreadable_started_choice(&old);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    ui.click("session-local-old");
    let started = |session: &Session| session.started_session_defaults().cloned();
    let enabled = |window: &Window, id: &'static str| {
        window
            .try_find(id)
            .is_some_and(|control| control.visible() && control.disabled() != Some(true))
    };
    let label = |window: &Window, id: &'static str| {
        window
            .try_find(id)
            .and_then(|button| button.label().map(str::to_owned))
    };
    let unknown = StartedSessionDefaults::new(StartedMode::Unknown, None);
    ui.wait_for_session("the unknown mode", |session| {
        started(session).as_ref() == Some(&unknown)
    });
    ui.wait_until("the session settings, open to change", |window, _, _| {
        enabled(window, "config-panel")
    });
    ui.click("config-panel");
    ui.wait_until("the warning, the model open to change", |window, _, _| {
        is_visible(window, "config-started-unreadable") && enabled(window, "config-started-model")
    });
    ui.shot("unknown");

    // The fake keeps a saved choice it can't read, and refuses a model.
    ui.click("config-started-model");
    click_menu_item(&mut ui, "Large");
    ui.wait_until("the refusal, the model as it was", |window, _, _| {
        is_visible(window, "session-error")
            && enabled(window, "config-started-model")
            && label(window, "config-started-model").as_deref() == Some("Default")
    });
    assert_eq!(started(&ui.session()), Some(unknown));

    ui.click("config-started-mode");
    click_menu_item(&mut ui, "Plan");
    let plan = StartedMode::Chosen(PermissionMode::Plan);
    ui.wait_for_session("plan for the sessions it starts", |session| {
        started(session) == Some(StartedSessionDefaults::new(plan, None))
    });
    ui.wait_until("the warning and the refusal gone", |window, _, _| {
        !is_visible(window, "config-started-unreadable")
            && !is_visible(window, "session-error")
            && enabled(window, "config-started-model")
    });
    ui.click("config-started-model");
    click_menu_item(&mut ui, "Large");
    let large = Some(ModelRef::new("fake", "large"));
    ui.wait_for_session("large for the sessions it starts", |session| {
        started(session) == Some(StartedSessionDefaults::new(plan, large.clone()))
    });
    ui.wait_until("the controls naming the choice", |window, _, _| {
        label(window, "config-started-mode").as_deref() == Some("Plan")
            && label(window, "config-started-model").as_deref() == Some("Large")
    });
    ui.shot("chosen");

    // While a turn runs, the daemon holds a change until the turn ends.
    ui.act(|window, cx| window.press("escape", cx));
    ui.wait_until("the panel to close", |window, _, _| {
        !is_visible(window, "config-started-mode")
    });
    ui.send("flaky");
    ui.wait_for_session("the turn under way", |session| session.state().is_busy());
    ui.click("config-panel");
    ui.wait_until("the sessions it starts", |window, _, _| {
        enabled(window, "config-started-mode")
    });
    ui.click("config-started-mode");
    click_menu_item(&mut ui, "Supervised");
    ui.wait_until("the change waiting for the turn", |window, _, _| {
        is_visible(window, "config-started-waiting")
    });
    assert_eq!(
        started(&ui.session()),
        Some(StartedSessionDefaults::new(plan, large.clone()))
    );
    ui.shot("waiting");
    ui.fake().finish_retry(&old);
    let supervised = StartedMode::Chosen(PermissionMode::Supervised);
    ui.wait_for_session("supervised once the turn ended", |session| {
        started(session) == Some(StartedSessionDefaults::new(supervised, large.clone()))
    });
    ui.wait_until("no change waiting", |window, _, _| {
        !is_visible(window, "config-started-waiting")
    });
}

/// A socket that takes the app's connection but passes it on to `daemon`
/// only once the returned sender sends or drops, so a test can act while
/// the app connects.
#[cfg(unix)]
fn gated_socket(dir: &Path, daemon: &DaemonTarget) -> (DaemonTarget, std::sync::mpsc::Sender<()>) {
    use std::os::unix::net::{UnixListener, UnixStream};
    let DaemonTarget::Socket(real) = daemon.clone() else {
        panic!("a fake daemon listens on a socket");
    };
    let path = dir.join("gated.sock");
    let listener = UnixListener::bind(&path).expect("bind the gated socket");
    let (open, gate) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((app, _)) = listener.accept() else {
            return;
        };
        let _ = gate.recv();
        let daemon = UnixStream::connect(&real).expect("reach the fake daemon");
        let (mut app_in, mut daemon_out) = (
            app.try_clone().expect("clone the app's end"),
            daemon.try_clone().expect("clone the daemon's end"),
        );
        std::thread::spawn(move || std::io::copy(&mut app_in, &mut daemon_out));
        let (mut daemon_in, mut app_out) = (daemon, app);
        let _ = std::io::copy(&mut daemon_in, &mut app_out);
    });
    (DaemonTarget::Socket(path), open)
}

/// A new session asked for while this machine connects waits for the
/// daemon, then opens, rather than failing as not connected.
#[test]
// The gate proxies a Unix socket.
#[cfg(unix)]
fn a_draft_opened_while_connecting_waits_for_the_daemon() {
    let daemon = FakeDaemon::start(Script::default());
    let dir = tempfile::tempdir().expect("a temporary directory");
    let (target, open) = gated_socket(dir.path(), &daemon.target());
    let mut ui = Harness::open(
        "draft-connecting",
        target,
        Some(daemon),
        Projects::default(),
    );
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.act(|window, _| {
        assert!(!is_visible(window, "draft-error"), "it waits, not fails");
    });
    drop(open);
    ui.wait_until("the draft's session", |window, cx, app| {
        is_visible(window, "config-panel")
            && app
                .draft()
                .is_some_and(|draft| draft.read(cx).session_key().is_some())
    });
}

/// Settle and snooze from a row's hover buttons and its context menu, and
/// bring sessions back from the opened Settled shelf, whose rows show their
/// details on hover. Each move is saved as the session's mark in the daemon.
#[test]
fn settle_and_snooze_move_sessions_between_lists() {
    let mut ui = Harness::start("shelves", Script::default());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-local-s1") && is_visible(window, "session-local-old")
    });

    ui.act(|window, cx| window.hover("session-local-s1", cx));
    ui.shot("hover");
    ui.click("settle");
    ui.wait_until("s1 on the settled list", |window, _, _| {
        !is_visible(window, "session-local-s1") && is_visible(window, "toggle-settled")
    });
    ui.wait_for_daemon("s1 marked done", |fake| {
        matches!(fake.mark("s1"), Some(SessionMark::Done { .. }))
    });
    ui.shot("settled-entry");

    ui.click("toggle-settled");
    ui.wait_until(
        "the settled shelf under the active sessions",
        |window, _, _| {
            is_visible(window, "session-local-s1") && is_visible(window, "session-local-old")
        },
    );
    ui.act(|window, cx| window.hover("session-local-s1", cx));
    // The card opens after a short delay, on the scheduler's clock.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the details card", |window, _, _| {
        is_visible(window, "session-card")
    });
    ui.shot("settled-shelf");

    // Restoring the last settled session empties and folds the shelf.
    ui.click("restore");
    ui.wait_until("s1 back on the active list", |window, _, _| {
        is_visible(window, "session-local-s1")
            && is_visible(window, "session-local-old")
            && !is_visible(window, "toggle-settled")
    });
    // Kept, not cleared: a clear would leave it settled by its age.
    ui.wait_for_daemon("s1 marked kept", |fake| {
        matches!(fake.mark("s1"), Some(SessionMark::Kept { .. }))
    });

    ui.act(|window, cx| window.hover("session-local-old", cx));
    ui.click("snooze");
    ui.click("snooze-4h");
    ui.wait_until("old on the snoozed list", |window, _, _| {
        !is_visible(window, "session-local-old") && is_visible(window, "toggle-snoozed")
    });
    ui.wait_for_daemon("old marked snoozed", |fake| {
        matches!(fake.mark("old"), Some(SessionMark::Snoozed { .. }))
    });

    // The context menu offers the same moves.
    ui.act(|window, cx| window.right_click("session-local-s1", cx));
    ui.shot("menu");
    let settle = ui
        .cx
        .update_window(ui.window, |_, window, _| {
            // The hover buttons have a Settle too; take the menu's.
            window
                .find_all_by_label("Settle")
                .into_iter()
                .find(|item| item.path().contains(&"popup-menu".into()))
                .map(|item| item.bounds().center())
        })
        .expect("window is open")
        .expect("the context menu has a Settle item");
    ui.act(|window, cx| {
        window.pointer_down(settle, MouseButton::Left, cx);
        window.pointer_up(settle, MouseButton::Left, cx);
    });
    ui.wait_until("s1 settled from the menu", |window, _, _| {
        !is_visible(window, "session-local-s1") && is_visible(window, "toggle-settled")
    });
    ui.shot("both-shelves");
}

/// A mark another rho-ui, or the daemon, gives a session the app follows
/// moves its row at once, and has the app read the machine's sessions
/// again, so that a list read before the mark can't put the old one back.
#[test]
fn a_mark_set_elsewhere_moves_a_followed_session() {
    // `open_old_session` waits for a replay of more than 20 entries.
    let script = Script {
        replay_turns: 20,
        ..Script::default()
    };
    let mut ui = Harness::start("mark-elsewhere", script);
    ui.open_old_session();
    let lists = ui.fake().detailed_lists();
    ui.fake().set_mark("old", &NewSessionMark::Done);
    ui.wait_until("old on the settled list", |window, _, _| {
        !is_visible(window, "session-local-old") && is_visible(window, "toggle-settled")
    });
    ui.wait_for_daemon("the sessions listed again", |fake| {
        fake.detailed_lists() > lists
    });
}

/// A daemon from before marks lists its sessions by age alone, and a move
/// it can't save is undone with an error.
#[test]
fn a_daemon_from_before_marks_refuses_a_move() {
    let script = Script {
        session_lists: SessionLists::Plain,
        ..Script::default()
    };
    let mut ui = Harness::start_with("marks-unserved", script, &[]);
    ui.wait_until("the settled entry", |window, _, _| {
        is_visible(window, "toggle-settled")
    });
    ui.click("toggle-settled");
    ui.wait_until("the settled shelf", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    let notifications = |ui: &mut Harness| {
        ui.frame();
        ui.cx
            .update_window(ui.window, |_, window, cx| window.notifications(cx).len())
            .expect("window is open")
    };
    let before = notifications(&mut ui);
    ui.act(|window, cx| window.hover("session-local-s1", cx));
    ui.click("restore");
    ui.poll(
        WAIT,
        "the error",
        |ui| notifications(ui) > before,
        Harness::labels_seen,
    );
    // Folded, the shelf hides s1 once the daemon's list puts it back.
    ui.click("toggle-settled");
    ui.wait_until("s1 settled again", |window, _, _| {
        !is_visible(window, "session-local-s1") && is_visible(window, "toggle-settled")
    });
    assert_eq!(ui.fake().mark("s1"), None);
}

/// An opened shelf shows its newest 25 sessions, Show more adds the rest,
/// and folding it goes back to 25.
#[test]
fn a_long_shelf_shows_25_sessions_at_a_time() {
    use gpui_kit::{point, ScrollDelta};
    let script = Script {
        past_sessions: 30,
        ..Script::default()
    };
    // Long quiet: all 32 sessions start settled.
    let mut ui = Harness::start_with("shelf-pages", script, &[]);
    ui.wait_until("the settled entry", |window, _, _| {
        is_visible(window, "toggle-settled")
    });
    ui.click("toggle-settled");
    let to_end = |ui: &mut Harness| {
        let down = ScrollDelta::Pixels(point(px(0.), px(-5000.)));
        ui.act(|window, cx| window.scroll("sidebar", down, cx));
    };
    ui.wait_until("the settled shelf", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    to_end(&mut ui);
    // s1, old, then past-0 to past-22.
    ui.wait_until("25 sessions, then Show more", |window, _, _| {
        is_visible(window, "session-local-past-22") && is_visible(window, "more-settled")
    });
    ui.act(|window, _| {
        assert!(window.try_find("session-local-past-23").is_none());
    });
    ui.shot("first-page");

    ui.click("more-settled");
    to_end(&mut ui);
    ui.wait_until("the rest, with no Show more", |window, _, _| {
        is_visible(window, "session-local-past-29") && !is_visible(window, "more-settled")
    });
    ui.shot("all");

    let up = ScrollDelta::Pixels(point(px(0.), px(5000.)));
    ui.act(|window, cx| window.scroll("sidebar", up, cx));
    ui.wait_until("the shelf's header", |window, _, _| {
        is_visible(window, "toggle-settled")
    });
    ui.click("toggle-settled");
    ui.click("toggle-settled");
    ui.wait_until("the settled shelf", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    to_end(&mut ui);
    ui.wait_until("25 sessions again", |window, _, _| {
        is_visible(window, "more-settled") && window.try_find("session-local-past-23").is_none()
    });
}

/// The row menu renames a session in place and saves the title in its
/// daemon, Escape leaving it as it was; pins a session above newer ones;
/// and copies a session's ID.
#[test]
fn the_row_menu_renames_pins_and_copies_a_session() {
    let mut ui = Harness::start("row-menu", Script::default());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-local-s1") && is_visible(window, "session-local-old")
    });

    ui.act(|window, cx| window.right_click("session-local-s1", cx));
    ui.shot("menu");
    click_label(&mut ui, "Rename…");
    ui.act(|window, cx| {
        window.input("never saved", cx);
        window.press("escape", cx);
    });
    assert_eq!(ui.label("session-local-s1").as_deref(), Some("first"));

    ui.act(|window, cx| window.right_click("session-local-old", cx));
    click_label(&mut ui, "Rename…");
    ui.act(|window, cx| window.input("Parser work", cx));
    ui.shot("renaming");
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_for_daemon("the title saved", |fake| {
        fake.title("old").as_deref() == Some("Parser work")
    });
    assert_eq!(
        ui.label("session-local-old").as_deref(),
        Some("Parser work")
    );
    assert_eq!(ui.fake().title("s1").as_deref(), Some("first"));

    let above = |window: &Window, top: &'static str, below: &'static str| {
        bounds(window, top)
            .zip(bounds(window, below))
            .is_some_and(|(top, below)| top.origin.y < below.origin.y)
    };
    ui.act(|window, _| {
        assert!(
            above(window, "session-local-s1", "session-local-old"),
            "newest first"
        );
    });
    ui.act(|window, cx| window.right_click("session-local-old", cx));
    click_label(&mut ui, "Pin");
    ui.wait_until("the pinned session on top", |window, _, _| {
        above(window, "session-local-old", "session-local-s1")
    });
    ui.wait_for_daemon("old marked pinned", |fake| {
        matches!(fake.mark("old"), Some(SessionMark::Pinned { .. }))
    });
    ui.shot("pinned");

    ui.act(|window, cx| window.right_click("session-local-s1", cx));
    click_label(&mut ui, "Copy");
    click_label(&mut ui, "Session ID");
    let copied = ui
        .cx
        .update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("s1"));
}

/// Snoozing a session that started others saves the mark of each in the
/// daemon, so another rho-ui shows the whole family snoozed. Pinning the
/// snoozed starter brings the family back, as Restore does.
#[test]
fn a_move_marks_a_whole_family_in_the_daemon() {
    let fake = FakeDaemon::start(Script::default());
    fake.start_session("s1", "w1", "Write the parser", Started::Idle);
    keep(&fake, &["s1", "old"]);
    let target = fake.target();
    let mut ui = Harness::open("family-move", target, Some(fake), Projects::default());
    ui.wait_until("the starter over its fold", |window, _, _| {
        is_visible(window, "session-local-s1-fold")
    });
    ui.act(|window, cx| window.hover("session-local-s1", cx));
    ui.click("snooze");
    ui.click("snooze-4h");
    ui.wait_until("the family on the snoozed list", |window, _, _| {
        !is_visible(window, "session-local-s1") && is_visible(window, "toggle-snoozed")
    });
    ui.wait_for_daemon("s1 and w1 marked snoozed", |fake| {
        ["s1", "w1"]
            .iter()
            .all(|id| matches!(fake.mark(id), Some(SessionMark::Snoozed { .. })))
    });

    ui.click("toggle-snoozed");
    ui.act(|window, cx| window.right_click("session-local-s1", cx));
    click_label(&mut ui, "Pin");
    ui.wait_for_daemon("s1 pinned and w1 kept", |fake| {
        matches!(fake.mark("s1"), Some(SessionMark::Pinned { .. }))
            && matches!(fake.mark("w1"), Some(SessionMark::Kept { .. }))
    });
    ui.click("session-local-s1-fold");
    ui.wait_until("the family back on the active list", |window, _, _| {
        is_visible(window, "session-local-w1") && !is_visible(window, "toggle-snoozed")
    });
}

/// Sessions a session started fold under it, behind a line that counts
/// them: closed at first, opened by a click, and opened by itself when one
/// of them comes on screen from elsewhere. A row under the fold opens its
/// session as any row does, and one rho settled on its own, as it does when
/// its pull request merges, shows there while it is on screen.
#[test]
fn sessions_a_session_started_fold_under_it() {
    let fake = FakeDaemon::start(Script::default());
    // First, so the daemon dates it no later than it stamps the mark.
    fake.start_session("s1", "w0", "Merge the lexer", Started::Idle);
    fake.set_mark("w0", &NewSessionMark::Done);
    fake.start_session("s1", "w1", "Write the parser", Started::Idle);
    fake.start_session("s1", "w2", "Test the parser", Started::Idle);
    fake.start_session("s1", "w3", "Review the parser", Started::Working);
    keep(&fake, &["s1", "old"]);
    let target = fake.target();
    let mut ui = Harness::open("started", target, Some(fake), Projects::default());
    let started = ["session-local-w1", "session-local-w2", "session-local-w3"];
    ui.wait_until("the fold, closed, under the starter", |window, _, _| {
        is_visible(window, "session-local-s1-fold")
            && started.iter().all(|id| !is_visible(window, *id))
    });
    ui.wait_for_label("session-local-s1-fold", "3 sessions · 1 working");

    ui.click("session-local-s1-fold");
    ui.wait_until("the sessions it started", |window, _, _| {
        started.iter().all(|id| is_visible(window, *id))
    });
    ui.shot("open");

    ui.click("session-local-w1");
    ui.wait_for_session("the started session open", |session| {
        session.id().as_str() == "w1"
    });

    // Closed with one of its sessions on screen, it stays closed.
    ui.click("session-local-s1-fold");
    ui.wait_until("the fold closed", |window, _, _| {
        started.iter().all(|id| !is_visible(window, *id))
    });
    ui.click("session-local-old");
    ui.wait_for_session("another session open", |session| {
        session.id().as_str() == "old"
    });
    // Opened from elsewhere, as from its lead's Work.
    let app = ui.app.clone();
    ui.act(|window, cx| {
        app.update(cx, |app, cx| {
            let w0 = SessionKey::local("w0");
            app.open_session(w0, PathBuf::from("/w/a"), None, Kind::Session, window, cx);
        });
    });
    ui.wait_until(
        "the fold open on the session on screen",
        |window, cx, app| {
            let on_screen = app.active_session().map(|view| view.read(cx).live().id);
            is_visible(window, "session-local-w0") && on_screen == Some(SessionKey::local("w0"))
        },
    );
    ui.shot("merged-on-screen");
}

/// Starts a lead, "new-2", from the sidebar with the first message "Fix
/// the parser!", in a rho home of its own, which the lead's folder is
/// made in: keep it for as long as the test reads that folder.
fn start_lead(shots: &'static str) -> (Harness, tempfile::TempDir) {
    let rho_home = tempfile::tempdir().expect("a rho home");
    let daemon = FakeDaemon::start(Script::default());
    let machines = Machines {
        rho_home: Some(rho_home.path().to_path_buf()),
        ..Machines::default()
    };
    let mut ui = Harness::open_with(shots, daemon.target(), Some(daemon), machines);
    ui.wait_until("the sidebar", |window, _, _| is_visible(window, "new-lead"));
    ui.click("new-lead");
    ui.wait_until("the lead's draft", |_, cx, app| {
        app.draft()
            .is_some_and(|draft| draft.read(cx).session_key().is_some())
    });
    ui.send("Fix the parser!");
    ui.wait_for_session("the lead", |session| session.id().as_str() == "new-2");
    (ui, rho_home)
}

/// A lead starts as rho's lead agent in a folder of its own, named from its
/// first message, made in the rho home's leads, and lists under the Leads
/// header. A new session from it starts in a project, not in its folder.
#[test]
fn a_lead_starts_in_a_folder_of_its_own_and_lists_apart() {
    let (mut ui, rho_home) = start_lead("leads");
    let leads = platform::canonicalize(&rho_home.path().join("leads")).expect("the leads root");
    let folder = leads.join("fix-the-parser");
    assert_eq!(
        (ui.fake().agent("new-2"), ui.fake().cwd("new-2")),
        (Some(LEAD_AGENT.to_owned()), Some(folder.clone()))
    );
    assert!(folder.is_dir(), "the lead's folder is made");
    let app = ui.app.clone();
    ui.act(|_, cx| {
        let lead = app.read(cx).active_session().expect("the lead's view");
        assert_eq!(lead.read(cx).composer_placeholder(), Kind::Lead);
    });
    ui.wait_until("the lead under the header", |window, _, _| {
        is_visible(window, "leads-header") && is_visible(window, "session-local-new-2")
    });
    ui.shot("listed");

    ui.click("new-session");
    ui.wait_until("a session's draft", |_, _, app| app.draft().is_some());
    let app = ui.app.clone();
    ui.act(|_, cx| {
        let draft = app.read(cx).draft().expect("the draft");
        let draft = draft.read(cx);
        assert_eq!(draft.kind(), Kind::Session);
        assert!(
            !draft.cwd().path.starts_with(&leads),
            "a session's draft works outside the leads: {:?}",
            draft.cwd().path
        );
    });
}

/// No project in the picker starts a session in a scratch folder of its
/// own, named from its first message, in the rho home's scratch, in its
/// checkout with no worktree. It is in no project: its sidebar row and
/// title bar name none, and a new session from it starts in a project.
#[test]
fn a_session_in_no_project_starts_in_a_scratch_folder() {
    let rho_home = tempfile::tempdir().expect("a rho home");
    let daemon = FakeDaemon::start(Script::default());
    let machines = Machines {
        projects: Projects::of(["/w/a"]),
        rho_home: Some(rho_home.path().to_path_buf()),
        ..Machines::default()
    };
    let mut ui = Harness::open_with("scratch", daemon.target(), Some(daemon), machines);
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "new-session")
    });
    let scratch = platform::canonicalize(rho_home.path())
        .expect("the rho home")
        .join("scratch");
    ui.click("new-session");
    ui.wait_until("No project offered", |window, _, _| {
        checked(window, "project-none") == Some(false)
    });
    ui.shot("picker");

    ui.click("project-none");
    ui.wait_until("the draft in the scratch root", |window, cx, app| {
        checked(window, "project-none") == Some(true)
            && app.draft().is_some_and(|draft| {
                let draft = draft.read(cx);
                draft.cwd().path == scratch && draft.session_key().is_some()
            })
    });
    ui.send("Try a quick thing");
    // The draft's own sessions, in /tmp then in the root, came first.
    ui.wait_for_session("the scratch session", |session| {
        session.id().as_str() == "new-3"
    });
    let folder = scratch.join("try-a-quick-thing");
    assert_eq!(ui.fake().cwd("new-3"), Some(folder.clone()));
    assert!(folder.is_dir(), "its folder is made");
    assert_eq!(ui.fake().worktrees(), 0, "no worktree");
    ui.wait_until("its row", |window, _, _| {
        is_visible(window, "session-local-new-3")
    });
    // The title bar reads its title alone.
    let names = ui.label("session-local-new-3-project-names");
    assert_eq!(names.as_deref(), Some(""), "the row names no project");
    assert_eq!(
        ui.label("title-project-names"),
        None,
        "nor does the title bar"
    );
    let app = ui.app.clone();
    ui.act(|_, cx| {
        let view = app.read(cx).active_session().expect("the session's view");
        assert!(view.read(cx).in_no_project(cx));
    });
    ui.shot("session");

    ui.act(|window, cx| {
        let view = app.read(cx).active_session().expect("the session's view");
        let composer = view.read(cx).composer_focus_handle(cx);
        window.focus(&composer, cx);
        window.press("secondary-n", cx);
    });
    ui.wait_until("a new draft in a project", |window, cx, app| {
        checked(window, "project-none") == Some(false)
            && app.draft().is_some_and(|draft| {
                let draft = draft.read(cx);
                !draft.in_no_project(cx) && draft.cwd().path == std::env::temp_dir()
            })
    });
}

/// A project picked in a lead's draft stays in the draft, not on the
/// draft's own session, which is not the lead's; a pick can be taken out
/// again. The lead that the first message starts reaches the picks from
/// its first turn, and its title bar and sidebar row name them, not its
/// own folder, with their badges stacked.
#[test]
fn a_leads_draft_hands_its_projects_to_the_lead() {
    let rho_home = tempfile::tempdir().expect("a rho home");
    let projects = tempfile::tempdir().expect("a projects folder");
    let [rho, rho_ui] = ["rho", "rho-ui"].map(|name| {
        let dir = projects.path().join(name);
        std::fs::create_dir(&dir).expect("a project");
        dir
    });
    let daemon = FakeDaemon::start(Script::default());
    let machines = Machines {
        projects: Projects::of([&rho_ui, &rho]),
        rho_home: Some(rho_home.path().to_path_buf()),
        ..Machines::default()
    };
    let mut ui = Harness::open_with("lead-projects", daemon.target(), Some(daemon), machines);
    ui.wait_until("the sidebar", |window, _, _| is_visible(window, "new-lead"));

    ui.click("new-lead");
    ui.wait_until("the lead's draft open", |_, cx, app| {
        app.draft()
            .is_some_and(|draft| draft.read(cx).session_key().is_some())
    });
    ui.click("added-dirs-button");
    ui.wait_until("the project offered", |window, _, _| {
        is_visible(window, "add-project-0")
    });
    ui.shot("offered");
    ui.click("add-project-0");
    ui.wait_until("the project picked", |window, _, _| {
        is_visible(window, "added-dir-0") && !is_visible(window, "add-project-1")
    });
    assert_eq!(ui.fake().added_dirs("new-1"), Some(Vec::new()));

    ui.click("added-dir-remove-0");
    ui.wait_until("the project offered again", |window, _, _| {
        is_visible(window, "add-project-0") && !is_visible(window, "added-dir-0")
    });
    ui.click("add-project-0");
    ui.wait_until("the project picked again", |window, _, _| {
        is_visible(window, "added-dir-0")
    });
    ui.click("add-project-0");
    ui.wait_until("both projects picked", |window, _, _| {
        is_visible(window, "added-dir-1")
    });

    let app = ui.app.clone();
    ui.act(|window, cx| {
        let draft = app.read(cx).draft().expect("the lead's draft");
        let composer = draft.read(cx).composer_focus_handle(cx);
        window.focus(&composer, cx);
    });
    ui.send("Fix the parser!");
    let both = vec![rho, rho_ui];
    ui.wait_for_session("the lead, reaching the projects", |session| {
        session.id().as_str() == "new-2" && session.added_dirs() == both
    });
    assert_eq!(ui.fake().added_dirs("new-2"), Some(both));

    let named = "rho + rho-ui";
    ui.wait_until("the lead's row names its projects", |window, _, _| {
        window
            .try_find("session-local-new-2-project-names")
            .is_some_and(|names| names.label() == Some(named))
    });
    assert_eq!(ui.label("title-project-names").as_deref(), Some(named));
    ui.wait_until("the lead's turn shows", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.click("toggle-settled");
    ui.shot("projects");
    let width = |ui: &mut Harness| {
        let stack = ui.cx.update_window(ui.window, |_, window, _| {
            window.find("title-projects").bounds()
        });
        stack.expect("window is open")
    };
    let stacked = width(&mut ui);
    ui.act(|window, cx| window.pointer_move(stacked.center(), None, cx));
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.shot("projects-fanned");
    let fanned = width(&mut ui);
    assert!(
        fanned.size.width > stacked.size.width,
        "hovered, the badges fan apart"
    );
}

/// A lead's panel starts on Work, beside Brief. Work lists the sessions
/// the lead started and the PRs linked to it, its finished part folded
/// until its header opens it, and a session's row opens that session,
/// whose panel offers neither. Coming back, the finished part is as the
/// user left it. Brief reads the lead's `brief.md` again as its turn ends.
#[test]
fn a_leads_panel_shows_its_work_and_brief() {
    let (mut ui, _rho_home) = start_lead("lead-view");
    ui.wait_until("the lead's tabs", |_, cx, app| {
        app.panel_tabs(cx) == [Surface::Work, Surface::Brief]
    });
    let app = ui.app.clone();
    ui.act(|_, cx| {
        assert_eq!(
            app.read(cx).brief_text(cx),
            None,
            "no brief before the lead writes one"
        );
    });

    // In its first turn the lead writes its brief, starts a session that
    // works and one that has finished its turn, and links a PR open and one
    // merged.
    let lead = SessionKey::local("new-2");
    let folder = ui.fake().cwd("new-2").expect("the lead's folder");
    let brief = "# Fix the parser\n\nOne session writes it, one tests it.\n";
    std::fs::write(folder.join("brief.md"), brief).expect("write the brief");
    ui.fake()
        .start_session("new-2", "w1", "Write the parser", Started::Working);
    ui.fake()
        .start_session("new-2", "w2", "Test the parser", Started::Idle);
    let open = linked_pr(
        "github.com",
        14,
        "Ship the parser",
        PullRequestLinkOrigin::Agent,
    );
    let mut merged = linked_pr(
        "github.com",
        15,
        "Fix the lexer",
        PullRequestLinkOrigin::Agent,
    );
    if let Some(summary) = &mut merged.summary {
        summary.state = PullRequestState::Merged;
    }
    ui.fake()
        .edit_pull_requests(&lead.id, |set| set.linked.extend([open, merged]));
    // Its turn ends after the brief is written, which the panel reads.
    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.click("approval-allow-once");
    ui.wait_for_session("the lead's turn to end", |session| {
        session.state() == &RunState::Idle
    });

    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.wait_until("Work showing, beside Brief", |window, cx, app| {
        is_visible(window, "lead-work") && app.panel_tabs(cx) == [Surface::Work, Surface::Brief]
    });
    let items = [
        "lead-work-item-local-w1",
        "lead-work-item-local-w2",
        "lead-work-item-acme/widgets#14",
        "lead-work-finished",
    ];
    let finished = "lead-work-item-acme/widgets#15";
    // The daemon's jobs, polled each second, list the sessions it started.
    advance_until(&mut ui, Duration::from_secs(1), "the lead's work", |ui| {
        ui.cx
            .update_window(ui.window, |_, window, _| {
                items.iter().all(|id| is_visible(window, *id))
            })
            .expect("window is open")
    });
    // The jobs also say which of them works, which the shot shows.
    ui.wait_for_label("session-local-new-2-fold", "2 sessions · 1 working");
    // The merged PR is finished, folded under a header that counts it.
    ui.wait_for_label("lead-work-finished", "Finished · 1");
    ui.act(|window, _| assert!(!is_visible(window, finished), "finished starts folded"));
    ui.shot("work");

    // Its header opens the finished part, folds it, and opens it again.
    for shows in [true, false, true] {
        ui.click("lead-work-finished");
        ui.wait_until("the finished part to open or fold", |window, _, _| {
            is_visible(window, finished) == shows
        });
    }
    ui.shot("work-finished-open");

    ui.click("panel-tab-brief");
    ui.wait_until("the lead's brief", |window, cx, app| {
        is_visible(window, "brief-text") && app.brief_text(cx).as_deref() == Some(brief)
    });
    ui.shot("brief");

    ui.click("panel-tab-work");
    ui.click("lead-work-item-local-w1");
    ui.wait_for_session("the started session open", |session| {
        session.id().as_str() == "w1"
    });
    ui.wait_until(
        "its panel, offering no lead's surfaces",
        |window, cx, app| {
            app.panel_tabs(cx).is_empty()
                && is_visible(window, "open-surface-files")
                && !is_visible(window, "open-surface-work")
                && !is_visible(window, "open-surface-brief")
        },
    );

    // Back on the lead, its finished part is still open.
    ui.click("session-local-new-2");
    ui.wait_for_session("the lead open", |session| session.id().as_str() == "new-2");
    ui.wait_until("Work, its finished part open", |window, _, _| {
        is_visible(window, "lead-work") && is_visible(window, finished)
    });
}

/// Sessions quiet for long start settled. A draft makes no session until
/// its first message, and leaving it drops it. A session can be deleted.
#[test]
fn drafts_are_free_quiet_sessions_start_settled_and_sessions_delete() {
    let mut ui = Harness::start_with("drafts", Script::default(), &[]);
    ui.wait_until("the settled entry", |window, _, _| {
        is_visible(window, "toggle-settled") && !is_visible(window, "session-local-s1")
    });
    ui.shot("settled-by-default");

    // Leaving an unused draft drops it and stops following its session,
    // which, never prompted, the daemon doesn't list.
    ui.click("new-session");
    ui.wait_until("the draft's session", |window, _, _| {
        is_visible(window, "draft-view") && is_visible(window, "config-panel")
    });
    ui.click("toggle-settled");
    ui.wait_until("the settled shelf", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    ui.shot("settled-opened");
    ui.click("session-local-s1");
    ui.wait_until("the saved session", |window, _, app| {
        !is_visible(window, "draft-view") && app.active_session().is_some()
    });

    // A draft that gets a message becomes a session.
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.act(|window, cx| {
        window.input("list files", cx);
        window.press("enter", cx);
    });
    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    assert_eq!(
        ui.session().id().as_str(),
        "new-2",
        "the second draft's session"
    );
    ui.click("approval-allow-once");
    // Once its turn ends, so the row stays put for the right click.
    ui.wait_for_session("the turn to end", |session| {
        session.state() == &RunState::Idle
    });
    ui.wait_until("the new session listed", |window, _, _| {
        is_visible(window, "session-local-new-2")
    });
    ui.act(|window, _| {
        assert!(
            !is_visible(window, "session-local-new-1"),
            "the unused draft's session isn't listed"
        );
    });

    ui.act(|window, cx| window.right_click("session-local-new-2", cx));
    click_label(&mut ui, "Delete…");
    ui.shot("delete-confirm");
    click_label(&mut ui, "Delete");
    ui.wait_until("the session gone", |window, _, app| {
        !is_visible(window, "session-local-new-2") && app.active_session().is_none()
    });
}

/// A draft left with a message waits at the top of the sidebar with its
/// session: a click brings it back as it was, and Send starts it in that
/// session. Discard drops one; an empty draft is never listed.
#[test]
fn a_draft_left_with_a_message_waits_in_the_sidebar() {
    let mut ui = Harness::start("draft-left", Script::default());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    let (first, session) = leave_a_draft(&mut ui, "fix the flaky test");
    assert_eq!(
        ui.label(first.clone()).as_deref(),
        Some("fix the flaky test")
    );
    let (second, _) = leave_a_draft(&mut ui, "rename the crate");
    ui.act(|window, cx| window.hover(second.clone(), cx));
    ui.shot("listed");

    ui.click("discard-draft");
    ui.wait_until("the second draft gone", |window, _, _| {
        !is_visible(window, second.clone()) && is_visible(window, first.clone())
    });

    ui.act(|window, cx| window.click(first.clone(), cx));
    ui.wait_until("the first draft back as it was", |_, cx, app| {
        app.draft().is_some_and(|draft| {
            let draft = draft.read(cx);
            draft.composer_prompt(cx).text() == "fix the flaky test"
                && draft.session_key().as_ref() == Some(&session)
        })
    });
    ui.shot("back");
    // New session leaves a draft with a message listed, for a fresh one.
    ui.click("new-session");
    ui.wait_until("a fresh draft", |window, cx, app| {
        app.draft()
            .is_some_and(|draft| draft.read(cx).composer_prompt(cx).is_empty())
            && is_visible(window, first.clone())
    });
    ui.act(|window, cx| window.click(first.clone(), cx));
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_until("the draft started in its session", |window, cx, app| {
        app.active_session()
            .is_some_and(|view| view.read(cx).key() == session)
            && !is_visible(window, first.clone())
    });

    ui.click("new-session");
    let empty = draft_row(&mut ui);
    ui.click("session-local-s1");
    ui.wait_until("the empty draft gone, unlisted", |window, _, app| {
        app.draft().is_none() && !is_visible(window, empty.clone())
    });
}

/// Opens a draft, types `text` in it once its session opens, and leaves
/// it for session s1. Returns its sidebar row's element ID and its session.
fn leave_a_draft(ui: &mut Harness, text: &str) -> (SharedString, SessionKey) {
    ui.click("new-session");
    ui.wait_until("the draft's session", |_, cx, app| {
        app.draft()
            .is_some_and(|draft| draft.read(cx).session_key().is_some())
    });
    ui.type_in(text);
    let row = draft_row(ui);
    let app = ui.app.clone();
    let session = ui.cx.update(|cx| {
        let draft = app.read(cx).draft().expect("the draft is on screen");
        draft.read(cx).session_key().expect("its session is open")
    });
    ui.click("session-local-s1");
    ui.wait_until("the draft listed", |window, _, app| {
        app.active_session().is_some() && is_visible(window, row.clone())
    });
    (row, session)
}

/// The element ID the sidebar gives the draft on screen.
fn draft_row(ui: &mut Harness) -> SharedString {
    let app = ui.app.clone();
    ui.cx.update(|cx| {
        let draft = app.read(cx).draft().expect("the draft is on screen");
        format!("draft-{}", draft.entity_id()).into()
    })
}

/// Clicks the element labelled `label`, such as a menu item or dialog
/// button, once it shows.
fn click_label(ui: &mut Harness, label: &str) {
    ui.wait_until(&format!("something labelled {label:?}"), |window, _, _| {
        window.try_find_by_label(label).is_some()
    });
    let center = ui
        .cx
        .update_window(ui.window, |_, window, _| {
            window
                .try_find_by_label(label)
                .map(|item| item.bounds().center())
        })
        .expect("window is open")
        .unwrap_or_else(|| panic!("nothing labelled {label:?}"));
    ui.act(|window, cx| {
        window.pointer_down(center, MouseButton::Left, cx);
        window.pointer_up(center, MouseButton::Left, cx);
    });
}

/// Clicks the open menu's item labelled `label`, as [`click_label`] does,
/// not something else that shares the label, such as a sidebar row's
/// machine or a row of the model list.
fn click_menu_item(ui: &mut Harness, label: &str) {
    let item = |window: &Window| {
        window
            .find_all_by_role(gpui_kit::Role::MenuItem)
            .into_iter()
            .find(|item| item.label() == Some(label))
            .map(|item| item.bounds().center())
    };
    ui.wait_until(
        &format!("a menu item labelled {label:?}"),
        |window, _, _| item(window).is_some(),
    );
    let center = ui
        .cx
        .update_window(ui.window, |_, window, _| item(window))
        .expect("window is open")
        .unwrap_or_else(|| panic!("no menu item labelled {label:?}"));
    ui.act(|window, cx| {
        window.pointer_down(center, MouseButton::Left, cx);
        window.pointer_up(center, MouseButton::Left, cx);
    });
}

/// Every item in the macOS menu bar works where it's chosen: the window
/// handles its action, else the app does. An item nothing handles shows
/// greyed out and does nothing. Items that edit text work in a text box.
/// Choosing Settings opens the settings, as a menu item dispatches it.
#[cfg(target_os = "macos")]
#[test]
fn the_menu_bar_offers_only_what_works() {
    use gpui_kit::OwnedMenuItem;

    let mut ui = Harness::start("menu-bar", Script::default());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    // The draft's composer has the keyboard, so the Edit items apply.
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.act(|window, _| window.activate_window());
    let menus = ui.cx.update(|cx| cx.get_menus()).expect("a menu bar");
    let mut items = 0;
    for menu in &menus {
        for item in &menu.items {
            let OwnedMenuItem::Action { name, action, .. } = item else {
                continue;
            };
            items += 1;
            let works = ui.cx.update(|cx| cx.is_action_available(action.as_ref()));
            assert!(works, "{} > {name} does nothing here", menu.name);
        }
    }
    assert!(items > 10, "the menu bar lists its items, not {items}");

    let settings = menus
        .iter()
        .flat_map(|menu| &menu.items)
        .find_map(|item| match item {
            OwnedMenuItem::Action { name, action, .. } if name == "Settings…" => {
                Some(action.boxed_clone())
            }
            _ => None,
        })
        .expect("Settings in the menu bar");
    ui.cx.update(|cx| cx.dispatch_action(settings.as_ref()));
    ui.wait_until("the settings dialog", |window, _, _| {
        is_visible(window, "setting-ui-font")
    });
}

/// The gear opens settings in a dialog over the sessions; a new scale
/// reaches the theme; About opens; Escape closes.
#[test]
fn settings_open_apply_and_close() {
    let mut ui = Harness::start("settings", Script::default());
    open_settings(&mut ui);
    ui.shot("open");
    ui.act(|window, _| {
        let panel = bounds(window, "settings-view").expect("settings are shown");
        let viewport = window.viewport_size();
        assert_eq!(
            panel.size.height,
            viewport.height * 0.75,
            "settings take three quarters of the window's height"
        );
        assert!(
            is_visible(window, "new-session"),
            "the sidebar shows behind settings"
        );
        // Kit's dialog surface, which the panel fills.
        let dialog = bounds(window, "surface").expect("the settings dialog");
        assert_eq!(
            dialog.size.width,
            viewport.width * 0.75,
            "and three quarters of its width"
        );
        let above = dialog.top();
        let below = viewport.height - dialog.bottom();
        assert!(
            (above - below).abs() <= px(1.),
            "settings sit {above:?} from the top and {below:?} from the bottom"
        );
    });

    ui.cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.ui_scale = UiScale::from_percent(125).expect("125% is on offer");
        settings.apply(cx);
        assert_eq!(cx.theme().font_size, px(20.), "16 px at 125%");
        // Markdown code blocks read the Base theme, not GPUI Kit's.
        let code = gpui_kit::base::Theme::global(cx)
            .tokens
            .typography
            .mono_md
            .size;
        assert_eq!(code, px(16.25), "13 px at 125%");
    });
    ui.shot("scaled");

    ui.cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.appearance = Appearance::Light;
        settings.apply(cx);
    });
    ui.shot("light");

    open_settings_page(&mut ui, "About");
    ui.wait_until("the About page", |window, _, _| {
        is_visible(window, "about-source")
    });
    ui.shot("about");

    ui.act(|window, cx| window.press("escape", cx));
    // Past the dialog's exit transition.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("settings closed", |window, _, _| {
        !is_visible(window, "setting-ui-font")
    });
}

/// Waits for the dialog whose button is labelled `ok`, then saves a
/// screenshot named `name` once it has faded in.
fn shot_dialog(ui: &mut Harness, ok: &str, name: &str) {
    ui.wait_until(&format!("a dialog with {ok:?}"), |window, _, _| {
        window.try_find_by_label(ok).is_some()
    });
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.shot(name);
}

/// Opens Settings from the gear, past the dialog's entrance.
fn open_settings(ui: &mut Harness) {
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "open-settings")
    });
    ui.click("open-settings");
    ui.wait_until("the settings dialog", |window, _, _| {
        is_visible(window, "setting-ui-font")
    });
    // The dialog fades in.
    ui.cx.advance_clock(Duration::from_secs(1));
}

/// Shows Settings page `page`, one with titled groups, once it is listed.
/// Kit's page rows aren't labelled for tests; the page's submenu toggle
/// is, at the row's right end, so this clicks the row left of it.
fn open_settings_page(ui: &mut Harness, page: &str) {
    let toggle = format!("Toggle {page} submenu");
    ui.wait_until(&format!("the {page} page listed"), |window, _, _| {
        !window.find_all_by_label(&toggle).is_empty()
    });
    let center = ui
        .cx
        .update_window(ui.window, |_, window, _| {
            window
                .find_all_by_label(&toggle)
                .first()
                .map(|toggle| toggle.bounds().center() - gpui_kit::point(px(60.), px(0.)))
        })
        .expect("window is open")
        .expect("the page is listed");
    ui.act(|window, cx| {
        window.pointer_down(center, MouseButton::Left, cx);
        window.pointer_up(center, MouseButton::Left, cx);
    });
}

/// How long the active session's turn `turn` took, as "Worked for" shows.
fn turn_took(ui: &mut Harness, turn: usize) -> Option<Duration> {
    let app = ui.app.clone();
    ui.cx.update(|cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        view.read(cx).transcript().took(turn)
    })
}

/// A finished turn folds its working behind "Worked". Inside, a run of
/// tool calls and thoughts is one summary line, which opens into a line per
/// step, each of which opens into what rho's card for it shows: a search's
/// matches, a command's output, an edit's diff. What the model read and no
/// person needs, such as a read file or an edit's receipt, stays hidden.
#[test]
fn finished_work_folds_and_opens_in_levels() {
    use gpui_kit::{point, ScrollDelta};

    let script = Script {
        replay_turns: 1,
        ..Script::default()
    };
    let mut ui = Harness::start("work", script);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    // An earlier run of rho-ui timed this turn, so it shows its time.
    let took = Duration::from_secs(80);
    ui.cx
        .update(|cx| cx.global_mut::<TurnTimes>().record("u0", took));
    ui.click("session-local-old");
    // Entries: reply, question, thought, read, search, command, edit, answer.
    ui.wait_until("the turn folded behind one line", |window, _, _| {
        is_visible(window, "worked-0") && !is_visible(window, "work-2")
    });
    assert_eq!(turn_took(&mut ui, 0), Some(took), "Worked for 1m 20s");
    ui.shot("worked");

    ui.toggle_turn("worked-0");
    ui.wait_until("the steps as one summary line", |window, _, _| {
        is_visible(window, "work-2") && !is_visible(window, "step-3")
    });
    ui.shot("folded");

    ui.act(|window, cx| window.within("work-2").click("work-summary", cx));
    ui.wait_until("a line per step", |window, _, _| {
        ["step-2", "step-3", "step-4", "step-5", "step-6"]
            .into_iter()
            .all(|step| is_visible(window, step))
            && !is_visible(window, "details-3")
    });
    ui.shot("steps");

    ui.act(|window, cx| window.within("step-5").click("step-line", cx));
    ui.wait_until("the command's details", |window, _, _| {
        is_visible(window, "details-5")
    });
    // It opens above the pinned bottom, so scroll up to it, over the edit's
    // line just below.
    ui.act(|window, cx| {
        // Pixels, as a touchpad scrolls: a wheel's lines glide over frames.
        let up = ScrollDelta::Pixels(point(px(0.), px(400.)));
        window.within("step-6").scroll("step-line", up, cx);
    });
    ui.wait_until("the command and its output", |window, _, _| {
        is_visible(window, "command-5") && is_visible(window, "output-5")
    });
    ui.shot("command");
    ui.act(|window, cx| window.within("step-5").click("step-line", cx));

    ui.act(|window, cx| window.within("step-4").click("step-line", cx));
    ui.wait_until("the search's matches", |window, _, _| {
        is_visible(window, "details-4")
    });
    ui.act(|window, _| {
        assert!(
            window.try_find("command-4").is_none(),
            "a search's label is all on its line"
        );
    });
    ui.shot("search");

    // The output scrolls inside its box, and the transcript stays put.
    ui.act(|window, cx| {
        let up = ScrollDelta::Pixels(point(px(0.), px(400.)));
        window.within("step-5").scroll("step-line", up, cx);
    });
    let top = |ui: &mut Harness| {
        let app = ui.app.clone();
        ui.cx.update(|cx| {
            app.read(cx)
                .active_session()
                .expect("a session is open")
                .read(cx)
                .scroll_top(cx)
        })
    };
    let text_y = |ui: &mut Harness| {
        let text = ui.cx.update_window(ui.window, |_, window, _| {
            window.find("output-text-4").bounds().top()
        });
        text.expect("window is open")
    };
    let (transcript, text) = (top(&mut ui), text_y(&mut ui));
    ui.act(|window, cx| {
        let down = ScrollDelta::Pixels(point(px(0.), px(-100.)));
        window.scroll("output-4", down, cx);
    });
    assert_eq!(text_y(&mut ui), text - px(100.), "the output scrolled");
    assert_eq!(top(&mut ui), transcript, "the transcript did not");
    ui.act(|window, cx| window.within("step-6").click("step-line", cx));
    // It opens under the fold, as the transcript was scrolled up.
    ui.act(|window, cx| {
        let down = ScrollDelta::Pixels(point(px(0.), px(-2000.)));
        window.within("step-6").scroll("step-line", down, cx);
    });
    ui.wait_until("the edit's diff", |window, _, _| {
        is_visible(window, "details-6")
    });
    ui.shot("diff");
}

/// A result delivered after the reply, while the daemon's turn still runs,
/// opens a turn of its own, as it will when the conversation replays: the
/// turn it ends gets its time then, and the new one runs on.
#[test]
fn a_result_after_the_reply_ends_the_turn_it_joined() {
    let mut ui = Harness::start("late-result", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    let id = SessionId::from("new-1");
    let says = |session: &Session, text: &str| matches!(session.entries().last(), Some(Entry::Agent(reply)) if reply.text() == text);
    ui.send("background");
    ui.wait_for_session("the process to run", |session| {
        session.state() == &RunState::Idle && !session.background().is_empty()
    });
    ui.send("subagent");
    ui.wait_for_session("the subagent to run", |session| {
        session.state() == &RunState::Idle && session.background().len() == 2
    });
    // The daemon's turn about the exit answers and runs on.
    ui.fake().finish_background(&id, 0);
    ui.wait_for_session("the daemon's turn's reply", |session| {
        session.state() == &RunState::Running && says(session, "`sleep 30` exited with 0.")
    });
    assert_eq!(turn_took(&mut ui, 2), None, "the daemon's turn runs");

    ui.fake().finish_subagent(&id);
    ui.wait_for_session("the reply to the subagent's result", |session| {
        says(session, "The subagent found the parser.")
    });
    let session = ui.session();
    let wakes = session
        .entries()
        .iter()
        .filter(|entry| matches!(entry, Entry::Wake))
        .count();
    assert_eq!(wakes, 2, "the result opens a turn of its own");
    assert_eq!(session.state(), &RunState::Running);
    assert!(
        turn_took(&mut ui, 2).is_some(),
        "the turn it ended is timed"
    );
    assert_eq!(turn_took(&mut ui, 3), None, "the new turn runs");
    ui.shot("split");
}

/// Each turn rho began itself, as a lead's are, opens with its wake and
/// folds only its own working, so every reply shows: live, and once the
/// conversation replays after a daemon restart, which marks no turn starts.
#[test]
fn every_wake_turn_keeps_its_reply_after_a_replay() {
    let mut ui = Harness::start("wake-replies", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    // A prompt: a read and a reply.
    ui.send("pictures");
    ui.wait_for_session("the prompt's reply", |session| {
        session.state() == &RunState::Idle && replies(session) == 1
    });
    let id = SessionId::from("new-1");
    // Two turns of the daemon's own: a card, a read and a reply each.
    for (reply, n) in [("First result in.", 2), ("Second result in.", 3)] {
        ui.fake().deliver(&id, reply);
        ui.wait_for_session("the daemon's reply", |session| {
            session.state() == &RunState::Idle && replies(session) == n
        });
    }
    // Entries: prompt, read, reply; card, wake, read, reply; card, wake,
    // read, reply. Each turn's reply shows under its folded working, a
    // read's step row: the prompt's at the top, the daemon's two at the
    // bottom. The clock jumps past the folds' springs.
    let shows_three_turns = |ui: &mut Harness, when: &str| {
        let session = ui.session();
        let wakes: Vec<usize> = session
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(ix, entry)| matches!(entry, Entry::Wake).then_some(ix))
            .collect();
        assert_eq!(wakes, [4, 8], "{when}: each daemon turn opens with a wake");
        let shows_turn =
            |window: &Window, (worked, work, reply): (&'static str, &'static str, &'static str)| {
                assert!(
                    is_visible(window, worked),
                    "{when}: {worked} heads its turn"
                );
                assert!(!is_visible(window, work), "{when}: {work} is folded");
                assert!(is_visible(window, reply), "{when}: {reply} shows");
            };
        let app = ui.app.clone();
        let scroller = ui.cx.update(|cx| {
            let view = app.read(cx).active_session().expect("a session is open");
            view.read(cx).transcript().scroller().clone()
        });
        ui.cx
            .update(|cx| scroller.update(cx, |scroller, cx| scroller.scroll_to_item(0, cx)));
        ui.frame();
        ui.cx.advance_clock(Duration::from_secs(1));
        ui.frame();
        ui.act(|window, _| shows_turn(window, ("worked-0", "step-1", "reply-2")));
        ui.cx.update(|cx| {
            scroller.update(
                cx,
                gpui_kit::component::message_scroller::MessageScrollerState::scroll_to_end,
            );
        });
        ui.frame();
        ui.cx.advance_clock(Duration::from_secs(1));
        ui.frame();
        ui.act(|window, _| {
            shows_turn(window, ("worked-1", "step-5", "reply-6"));
            shows_turn(window, ("worked-2", "step-9", "reply-10"));
            for wake in ["wake-4", "wake-8"] {
                assert!(
                    window.try_find(wake).is_none(),
                    "{when}: the card above {wake} says why rho went on, so it draws no row"
                );
            }
        });
        session.entries().to_vec()
    };
    let live = shows_three_turns(&mut ui, "live");
    ui.shot("live");

    ui.fake().restart();
    ui.wait_for_daemon("new-1 attached after the restart", |fake| {
        fake.attaches("new-1") == 2
    });
    ui.wait_for_session("the replay", |session| !session.is_replaying());
    let replayed = shows_three_turns(&mut ui, "replayed");
    ui.shot("replayed");
    assert_eq!(replayed, live, "the replay shows what live did");
}

/// How many replies `session` shows.
fn replies(session: &Session) -> usize {
    session
        .entries()
        .iter()
        .filter(|entry| matches!(entry, Entry::Agent(_)))
        .count()
}

/// A wake with no card right above it keeps its line, so its turn doesn't
/// start unexplained: live, rho compacts a full context before it delivers
/// the result's card, so the wake opens above the compaction. A plain
/// notice right above it sits on the rail as a card does, but says nothing
/// of why rho went on, so the wake keeps its line under it too.
#[test]
fn a_wake_with_no_card_above_keeps_its_line() {
    let mut ui = Harness::start("bare-wake", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("pictures");
    let idle_after =
        |session: &Session, n: usize| session.state() == &RunState::Idle && replies(session) == n;
    ui.wait_for_session("the prompt's reply", |session| idle_after(session, 1));
    let id = SessionId::from("new-1");
    ui.fake().deliver_after_compacting(&id, "Result in.");
    ui.wait_for_session("the daemon's reply", |session| idle_after(session, 2));
    let session = ui.session();
    let wake = session
        .entries()
        .iter()
        .position(|entry| matches!(entry, Entry::Wake))
        .expect("the daemon's turn opens with a wake");
    assert!(
        matches!(session.entries()[wake - 1], Entry::Agent(_))
            && matches!(session.entries()[wake + 1], Entry::Compaction(_))
            && matches!(session.entries()[wake + 2], Entry::Notice(_)),
        "the compaction comes between the wake and its card: {:?}",
        session.entries()
    );
    let wake = SharedString::from(format!("wake-{wake}"));
    ui.wait_until("the wake's line", |window, _, _| {
        is_visible(window, wake.clone())
    });
    ui.act(|window, _| {
        assert!(
            is_visible(window, "worked-1"),
            "the daemon's turn has its own header"
        );
    });
    // Past the folds' springs.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.frame();
    ui.shot("line");

    ui.fake().notice(&id, "Switched to the fallback model.");
    ui.wait_for_session("the notice", |session| {
        matches!(session.entries().last(), Some(Entry::Notice(_)))
    });
    ui.fake().deliver_after_compacting(&id, "Result in again.");
    ui.wait_for_session("the second daemon reply", |session| idle_after(session, 3));
    let session = ui.session();
    let wake = session
        .entries()
        .iter()
        .rposition(|entry| matches!(entry, Entry::Wake))
        .expect("the second daemon turn opens with a wake");
    assert!(
        matches!(&session.entries()[wake - 1], Entry::Notice(notice) if !notice.is_card()),
        "the plain notice sits right above the wake: {:?}",
        session.entries()
    );
    let wake = SharedString::from(format!("wake-{wake}"));
    ui.wait_until("the wake's line under the notice", |window, _, _| {
        is_visible(window, wake.clone())
    });
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.frame();
    ui.shot("notice");
}

/// A step shows what rho's terminal UI shows for it, once: an edit's line
/// names the file and what changed, and it opens into the diff alone, not
/// the receipt the model read; a read's line says how much it read, and the
/// file it read was for the model.
#[test]
fn a_step_shows_its_card_not_what_the_model_read() {
    let script = Script {
        replay_turns: 1,
        ..Script::default()
    };
    let mut ui = Harness::start("cards", script);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    ui.click("session-local-old");
    ui.wait_until("the turn folded", |window, _, _| {
        is_visible(window, "worked-0")
    });
    ui.toggle_turn("worked-0");
    ui.wait_until("the steps as one summary line", |window, _, _| {
        is_visible(window, "work-2")
    });
    ui.act(|window, cx| window.within("work-2").click("work-summary", cx));
    ui.wait_until("a line per step", |window, _, _| {
        is_visible(window, "step-6")
    });

    let session = ui.session();
    let tool = |ix: usize| match session.entries().get(ix) {
        Some(Entry::Tool(tool)) => tool,
        other => panic!("entry {ix} is a tool call, not {other:?}"),
    };
    assert_eq!(tool(3).label(), "read_file(src/0.rs)");
    let edit = tool(6);
    assert_eq!(edit.label(), "str_replace(src/0.rs)");
    let changed = rho_ui_core::ToolFact::Changed {
        added: 2,
        removed: 1,
    };
    assert_eq!(
        edit.card().map(rho_ui_core::ToolCard::facts),
        Some(&[changed][..]),
        "its line says what changed"
    );

    ui.act(|window, cx| window.within("step-3").click("step-line", cx));
    ui.act(|window, cx| window.within("step-6").click("step-line", cx));
    ui.wait_until("the edit's diff", |window, _, _| {
        is_visible(window, "details-6")
    });
    ui.act(|window, _| {
        assert!(
            window.try_find("details-3").is_none(),
            "a read has nothing to open"
        );
        assert!(
            window.try_find("output-6").is_none(),
            "an edit shows its diff alone, not the receipt the model read"
        );
    });
    ui.shot("edit");

    // A script shows as its card's code, not what it printed for the model.
    assert!(matches!(
        tool(7).card().map(rho_ui_core::ToolCard::body),
        Some(rho_ui_core::ToolBody::Code { language, .. }) if language == "python"
    ));
    ui.act(|window, cx| window.within("step-6").click("step-line", cx));
    ui.act(|window, cx| window.within("step-7").click("step-line", cx));
    ui.wait_until("the script", |window, _, _| is_visible(window, "output-7"));
    ui.shot("script");
}

/// A sent prompt starts a new turn even before the daemon echoes it, so the
/// last finished turn stays folded, and a refused prompt times no turn.
#[test]
fn a_new_prompt_leaves_finished_turns_folded() {
    let script = Script {
        replay_turns: 1,
        ..Script::default()
    };
    let mut ui = Harness::start("pending", script);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    ui.click("session-local-old");
    ui.wait_until("the replayed turn, folded", |window, _, _| {
        is_visible(window, "worked-0")
    });

    // The daemon takes "slow" late; until then the old turn stays folded.
    ui.act(|window, cx| {
        window.input("slow", cx);
        window.press("enter", cx);
    });
    ui.act(|window, _| {
        assert!(is_visible(window, "worked-0"), "the old turn stays folded");
        assert!(!is_visible(window, "work-2"), "its rows stay hidden");
    });
    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.click("approval-allow-once");
    ui.wait_for_session("the slow turn to finish", |session| {
        session.state() == &RunState::Idle
    });
    assert_eq!(turn_took(&mut ui, 0), None, "the replayed turn has no time");
    let slow = turn_took(&mut ui, 1).expect("the slow turn was timed here");
    let saved = ui.cx.update(|cx| cx.global::<TurnTimes>().get("u-turn-1"));
    assert_eq!(saved, Some(slow), "saved under its prompt for a replay");

    // A refused prompt starts no turn, so it times none.
    ui.act(|window, cx| {
        window.input("fail", cx);
        window.press("enter", cx);
    });
    ui.wait_for_session("the refusal", |session| {
        matches!(session.state(), RunState::Failed(_))
    });
    assert_eq!(turn_took(&mut ui, 0), None, "the refusal times no old turn");

    // Each turn has its own "Worked" line: opening the second opens only it.
    ui.toggle_turn("worked-1");
    ui.wait_until("the slow turn's tool call", |window, _, _| {
        is_visible(window, "step-10")
    });
    ui.act(|window, _| {
        assert!(!is_visible(window, "work-2"), "the old turn stays folded");
    });
    ui.shot("second-open");
}

/// A turn another client starts in a session open here is a turn of its
/// own: the turn before it stays folded and keeps its time. Its approval
/// shows here too, without taking the keyboard from a message being typed,
/// whose Enter would otherwise approve.
#[test]
fn a_turn_started_elsewhere_is_its_own_turn_and_leaves_typing_alone() {
    let script = Script {
        replay_turns: 1,
        ..Script::default()
    };
    let mut ui = Harness::start("elsewhere", script);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    let took = Duration::from_secs(80);
    ui.cx
        .update(|cx| cx.global_mut::<TurnTimes>().record("u0", took));
    ui.click("session-local-old");
    ui.wait_until("the replayed turn, folded", |window, _, _| {
        is_visible(window, "worked-0")
    });

    ui.act(|window, cx| window.input("half a thought", cx));
    ui.send_from_elsewhere("old", "list files", Send::Prompt);
    ui.wait_until("the other client's approval", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    let typing = ui
        .cx
        .update_window(ui.window, |_, window, cx| {
            let view = ui.app.read(cx).active_session().expect("a session is open");
            let composer = view.read(cx).composer_focus_handle(cx);
            composer.is_focused(window)
        })
        .expect("window is open");
    assert!(typing, "the keyboard stays in the composer");
    ui.act(|window, _| {
        assert!(!is_visible(window, "work-2"), "the old turn stays folded");
    });
    ui.shot("approval");

    ui.click("approval-allow-once");
    ui.wait_for_session("the other client's turn to finish", |session| {
        session.state() == &RunState::Idle
            && matches!(session.entries().last(), Some(Entry::Agent(m)) if m.text() == "you chose allow_once")
    });
    assert_eq!(
        turn_took(&mut ui, 0),
        Some(took),
        "the old turn keeps its time"
    );
    assert!(turn_took(&mut ui, 1).is_some(), "the new turn has its own");
    let view = ui.app.clone();
    let typed = ui.cx.update(|cx| {
        let view = view.read(cx).active_session().expect("a session is open");
        view.read(cx).composer_text(cx)
    });
    assert_eq!(typed, "half a thought", "the message being typed is intact");
}

/// Cmd/Ctrl+F finds text in the session's transcript, folded turns
/// included: moving to a match inside one opens the fold and scrolls to
/// it, and Escape closes the bar and clears the marks, leaving the fold
/// open. With no session open, it goes to the sidebar's search.
#[test]
fn find_steps_through_matches_into_folded_work() {
    use crate::transcript::find::{Count, Field};

    let script = Script {
        replay_turns: 3,
        ..Script::default()
    };
    let mut ui = Harness::start("find", script);
    // With no session open, it finds sessions: the sidebar's search.
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.act(|window, cx| window.press("secondary-f", cx));
    let app = ui.app.clone();
    ui.act(|window, cx| {
        let search = app.read(cx).sidebar_search(cx);
        assert!(
            search.is_focused(window),
            "the sidebar's search has the keyboard"
        );
    });
    ui.open_old_session();
    ui.wait_until("every turn folded", |window, _, _| {
        is_visible(window, "worked-2") && !is_visible(window, "details-2")
    });
    let entries = ui.session().entries().to_vec();
    // The current match's entry, by its first line or as a thought.
    let kind = |current: Option<(usize, Field)>| {
        let (entry, field) = current?;
        let what = match &entries[entry] {
            Entry::Agent(message) => message.text().lines().next()?.to_owned(),
            Entry::Thought(_) => format!("thought {entry}"),
            _ => String::new(),
        };
        Some((what, field))
    };
    let app = ui.app.clone();
    let state = move |ui: &mut Harness| {
        ui.cx.update(|cx| {
            let view = app.read(cx).active_session().expect("a session is open");
            let view = view.read(cx);
            let transcript = view.transcript();
            (
                transcript.find_count(),
                transcript.find_current(),
                view.scroll_top(cx).0,
            )
        })
    };

    ui.act(|window, cx| window.press("secondary-f", cx));
    ui.wait_until("the find bar", |window, _, _| {
        is_visible(window, "find-bar")
    });
    ui.act(|window, cx| window.input("WALK", cx));
    // In each turn: the thought's "walker" and "walk", the search's
    // `src/n/walk.rs`, the command's `walk(n)`, and the answer's "walks";
    // not the address of the thought's link, which doesn't show. The first
    // match from the top of the view on is current: turn 1's answer, near
    // the end.
    ui.wait_for_label("find-count", "10 of 15");
    let (_, current, bottom) = state(&mut ui);
    assert_eq!(
        kind(current),
        Some(("## Answer 1".to_owned(), Field::Part(0)))
    );
    ui.shot("found");

    // Back to the first match, in turn 0's thought: its working, its run
    // of steps and the thought's details open, and the transcript
    // scrolls up to it.
    for _ in 0..9 {
        ui.act(|window, cx| window.press("shift-enter", cx));
    }
    ui.wait_for_label("find-count", "1 of 15");
    // Turn 0's question is entry 1, and its thought entry 2.
    ui.wait_until("the thought, open", |window, _, _| {
        is_visible(window, "details-2")
    });
    let (_, current, top) = state(&mut ui);
    assert_eq!(kind(current), Some(("thought 2".to_owned(), Field::Body)));
    assert!(top < bottom, "scrolled up from item {bottom} to {top}");
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.shot("in-fold");

    // It wraps around to the last match, and back.
    ui.act(|window, cx| window.press("shift-enter", cx));
    ui.wait_for_label("find-count", "15 of 15");
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_for_label("find-count", "1 of 15");

    // A phrase that reads across bold in a thought, "The **tree** walk",
    // is found as it reads. The shortcut selects the query to type over.
    ui.act(|window, cx| window.press("secondary-f", cx));
    ui.act(|window, cx| window.input("tree walk", cx));
    ui.wait_for_label("find-count", "1 of 3");
    let (_, current, _) = state(&mut ui);
    assert_eq!(kind(current), Some(("thought 2".to_owned(), Field::Body)));
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.shot("across-bold");

    // Escape closes it from outside its field too, rather than stopping
    // the turn as it does with no bar open.
    let app = ui.app.clone();
    ui.act(|window, cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        let composer = view.read(cx).composer_focus_handle(cx);
        window.focus(&composer, cx);
    });
    ui.act(|window, cx| window.press("escape", cx));
    ui.wait_until("the bar gone", |window, _, _| {
        !is_visible(window, "find-bar")
    });
    let (count, current, _) = state(&mut ui);
    assert_eq!((count, current), (Count::NoQuery, None), "no matches left");
    ui.act(|window, cx| {
        assert!(is_visible(window, "details-2"), "the fold stays open");
        let view = app.read(cx).active_session().expect("a session is open");
        let composer = view.read(cx).composer_focus_handle(cx);
        assert!(
            composer.is_focused(window),
            "the keyboard is back in the composer"
        );
    });
}

/// The sidebar's search, which matches titles, hands its query on to the
/// search of every saved session's conversation, which follows what is
/// typed in it. Opening a hit opens its session with the find bar on the
/// message that matched: turn 0's answer, scrolled up to, not the thought
/// before it that find also matches (search doesn't look in thoughts),
/// nor the match nearest the end, where the session opens.
#[test]
fn search_opens_a_session_on_its_match() {
    let script = Script {
        replay_turns: 3,
        ..Script::default()
    };
    let mut ui = Harness::start("search", script);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    // No title holds "walk".
    ui.act(|window, cx| window.press("secondary-f", cx));
    ui.act(|window, cx| window.input("walk", cx));
    ui.wait_until("the hand-off", |window, _, _| {
        is_visible(window, "search-sessions") && !is_visible(window, "session-local-old")
    });
    ui.shot("hand-off");
    ui.click("search-sessions");
    // Past the wait for typing to settle.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("a hit", |window, _, _| {
        is_visible(window, IndexPath::new(0))
    });
    ui.shot("hits");
    // Typed in the palette: no message holds "walkzz", and then "walk"
    // again.
    ui.act(|window, cx| window.input("zz", cx));
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("no hit", |window, _, _| {
        !is_visible(window, IndexPath::new(0))
    });
    for _ in 0..2 {
        ui.act(|window, cx| window.press("backspace", cx));
    }
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the hit again", |window, _, _| {
        is_visible(window, IndexPath::new(0))
    });

    ui.act(|window, cx| window.press("enter", cx));
    // Each turn's thought, search, command and answer match "walk" (see
    // `find_steps_through_matches_into_folded_work`): turn 0's answer is
    // the fifth.
    ui.wait_for_label("find-count", "5 of 15");
    let app = ui.app.clone();
    let current = ui.cx.update(|cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        view.read(cx).transcript().find_current()
    });
    let entries = ui.session().entries().to_vec();
    let entry = current.map(|(entry, _)| &entries[entry]);
    assert!(
        matches!(entry, Some(Entry::Agent(message)) if message.text().starts_with("## Answer 0")),
        "the current match is turn 0's answer, not {entry:?}"
    );
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.shot("opened");
}

/// A saved session's replay goes into the session before any of its rows
/// are built: the transcript doesn't grow row by row as history arrives,
/// which drew every event and made a long session slow to open. It is
/// built once, and opens at its end.
#[test]
fn a_replayed_session_builds_its_transcript_once() {
    let script = Script {
        replay_turns: 20,
        ..Script::default()
    };
    let mut ui = Harness::start("replay-once", script);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    // Clicked without running tasks, so no event is applied before the
    // scroller is watched.
    ui.cx
        .update_window(ui.window, |_, window, cx| {
            window.click("session-local-old", cx);
        })
        .expect("window is open");
    let app = ui.app.clone();
    let counts: Rc<RefCell<Vec<usize>>> = Rc::default();
    let _watch = ui.cx.update(|cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        let scroller = view.read(cx).transcript().scroller().clone();
        let counts = Rc::clone(&counts);
        cx.observe(&scroller, move |scroller, cx| {
            counts.borrow_mut().push(scroller.read(cx).item_count());
        })
    });
    ui.wait_for_session("the replayed history", |session| {
        !session.is_replaying() && session.entries().len() > 20
    });
    let (built, last) = ui.cx.update(|cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        let view = view.read(cx);
        let built = view.transcript().scroller().read(cx).item_count();
        (built, view.session().entries().len() - 1)
    });
    let counts = counts.borrow().clone();
    assert!(
        !counts.is_empty() && counts.iter().all(|&count| count == built),
        "the list went from empty to all {built} items at once: {counts:?}"
    );
    let reply = SharedString::from(format!("reply-{last}"));
    ui.wait_until("the last reply", |window, _, _| {
        is_visible(window, reply.clone())
    });
}

/// A long reply scrolled away and back draws whole on the first frame it
/// shows again, so the transcript above it doesn't jump.
#[test]
fn a_long_reply_scrolled_back_keeps_its_height() {
    let script = Script {
        replay_turns: 20,
        long_reply: true,
        ..Script::default()
    };
    let mut ui = Harness::start("long-reply", script);
    ui.open_old_session();
    let app = ui.app.clone();
    let (view, last) = ui.cx.update(|cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        let last = view.read(cx).session().entries().len() - 1;
        (view, last)
    });
    let reply = gpui_kit::SharedString::from(format!("reply-{last}"));
    let height = |ui: &mut Harness| {
        ui.cx
            .update_window(ui.window, |_, window, _| {
                window
                    .try_find(reply.clone())
                    .filter(gpui_kit::test::ElementSnapshot::visible)
                    .map(|element| f32::from(element.bounds().size.height))
            })
            .expect("window is open")
    };
    ui.frame();
    let whole = height(&mut ui).expect("the long reply shows at the end");
    assert!(whole > 1000., "the reply is drawn whole: {whole}px");

    let scroller = ui
        .cx
        .update(|cx| view.read(cx).transcript().scroller().clone());
    ui.cx
        .update(|cx| scroller.update(cx, |scroller, cx| scroller.scroll_to_item(0, cx)));
    ui.frame();
    assert_eq!(height(&mut ui), None, "the reply is out of the drawn range");

    // One frame, with no time for a parse in the background.
    ui.cx.update(|cx| {
        scroller.update(
            cx,
            gpui_kit::component::message_scroller::MessageScrollerState::scroll_to_end,
        );
    });
    ui.cx
        .update_window(ui.window, |_, window, cx| window.render_frame(cx))
        .expect("window is open");
    assert_eq!(height(&mut ui), Some(whole), "the reply comes back whole");
}

/// A long reply draws in parts, each one of the scroller's items, which it
/// takes as it streams in. The turn after it shows under it all the same.
#[test]
fn a_long_reply_streams_in_as_parts() {
    let mut ui = Harness::start("parts", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("report");
    ui.wait_for_session("the report", |session| {
        session.state() == &RunState::Idle
            && matches!(session.entries().last(), Some(Entry::Agent(_)))
    });
    let entries = ui.session().entries().len();
    assert!(
        matches!(ui.session().entries().last(), Some(Entry::Agent(reply)) if reply.text() == report()),
        "the whole report arrived"
    );
    let items = |ui: &mut Harness| {
        let app = ui.app.clone();
        ui.cx.update(|cx| {
            let view = app.read(cx).active_session().expect("a session is open");
            let scroller = view.read(cx).transcript().scroller().clone();
            scroller.read(cx).item_count()
        })
    };
    // The prompt is one item, and each of the report's parts another:
    // streamed in, it parts where it does whole.
    let parts = items(&mut ui) - 1;
    assert!(parts > 2, "the report is in parts: {parts}");
    assert_eq!(parts, crate::parts::split(&report()).len());
    let end = SharedString::from(format!("reply-{}-{}", entries - 1, parts - 1));
    ui.wait_until("the report's last part", |window, _, _| {
        is_visible(window, end.clone())
    });
    ui.shot("report");

    // A selection runs from one part into the next, as through one text:
    // from the last line of the part before the last, to the end of the
    // last part's first line.
    let before = SharedString::from(format!("reply-{}-{}", entries - 1, parts - 2));
    ui.act(|window, cx| {
        let from = window.find(before.clone()).bounds();
        let to = window.find(end.clone()).bounds();
        window.drag(
            gpui_kit::point(from.left() + px(1.), to.top() - px(20.)),
            gpui_kit::point(to.right() - px(1.), to.top() + px(10.)),
            cx,
        );
        window.press("secondary-c", cx);
    });
    ui.shot("selected");
    let copied = ui
        .cx
        .update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .expect("the selection was copied");
    assert!(
        copied.contains("could not reach.") && copied.ends_with("first of 5"),
        "the end of one part and the start of the next: {copied:?}"
    );

    ui.send("figures");
    ui.wait_for_session("the next reply", |session| {
        session.state() == &RunState::Idle && session.entries().len() == entries + 2
    });
    assert_eq!(items(&mut ui), 1 + parts + 2, "a prompt and a short reply");
    let prompt = SharedString::from(format!("user-{entries}"));
    let reply = SharedString::from(format!("reply-{}", entries + 1));
    ui.wait_until("the next turn under the report", |window, _, _| {
        let top = |id: &SharedString| {
            window
                .try_find(id.clone())
                .filter(gpui_kit::test::ElementSnapshot::visible)
                .map(|element| element.bounds().top())
        };
        top(&prompt)
            .zip(top(&reply))
            .is_some_and(|(prompt, reply)| prompt < reply)
    });
}

/// A wheel notch glides instead of jumping; `kinetic` owns how it eases.
#[test]
fn a_wheel_notch_glides() {
    use gpui_kit::{point, InputEvent as _, MouseMoveEvent, ScrollDelta, ScrollEvent};

    let script = Script {
        replay_turns: 20,
        ..Script::default()
    };
    let mut ui = Harness::start("glide", script);
    ui.open_old_session();
    let top = |ui: &mut Harness| {
        let app = ui.app.clone();
        ui.cx.update(|cx| {
            app.read(cx)
                .active_session()
                .expect("a session is open")
                .read(cx)
                .scroll_top(cx)
        })
    };
    let position = point(px(700.), px(400.));
    // Turns the wheel by `lines` over the transcript.
    let wheel = |ui: &mut Harness, lines: f32| {
        ui.act(|window, cx| {
            window.dispatch_event(
                MouseMoveEvent {
                    position,
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                ScrollEvent {
                    position,
                    delta: ScrollDelta::Lines(point(0., lines)),
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
        });
    };
    let before = top(&mut ui);
    wheel(&mut ui, 3.);
    assert_eq!(top(&mut ui), before, "the notch doesn't jump");

    // The glide runs on animation frames, which `wait_until` doesn't draw.
    ui.poll(
        WAIT,
        "the glide to scroll",
        |ui| {
            if top(ui) != before {
                return true;
            }
            ui.cx
                .update_window(ui.window, |_, window, cx| {
                    window.simulate_next_frame(cx);
                    window.draw(cx).clear(cx);
                })
                .expect("window is open");
            false
        },
        |_| format!("the transcript stayed scrolled to {before:?}"),
    );

    // A high-resolution wheel's eighth of a notch glides too, rather than
    // passing through as a touchpad would.
    let before = top(&mut ui);
    wheel(&mut ui, 0.375);
    assert_eq!(
        top(&mut ui),
        before,
        "a fine wheel step doesn't jump either"
    );

    // A desktop that asks for reduced motion, as Windows does with its
    // animations off, stops the glide only while the setting follows it.
    for (reduce_motion, smooth_scroll, glides) in [
        (true, SmoothScroll::System, false),
        (true, SmoothScroll::On, true),
        (false, SmoothScroll::Off, false),
    ] {
        ui.cx.update(|cx| {
            cx.set_reduce_motion(reduce_motion);
            let mut settings = cx.global::<Settings>().clone();
            settings.smooth_scroll = smooth_scroll;
            settings.apply(cx);
        });
        // A click, here on the sidebar's empty part, stops the glide under
        // way.
        ui.act(|window, cx| {
            let empty = point(px(120.), px(500.));
            window.pointer_down(empty, MouseButton::Left, cx);
            window.pointer_up(empty, MouseButton::Left, cx);
        });
        let before = top(&mut ui);
        wheel(&mut ui, 3.);
        assert_eq!(
            top(&mut ui) == before,
            glides,
            "{smooth_scroll:?}, with reduced motion {reduce_motion}"
        );
    }
}

/// With no rho on this machine, the main pane offers to install it in
/// place of the connection's error, whose Retry couldn't help.
#[test]
fn a_missing_rho_offers_its_install() {
    let target = DaemonTarget::Home {
        home: PathBuf::from("/nonexistent/rho-home"),
        rho: PathBuf::from("/nonexistent/rho"),
    };
    let machines = Machines {
        rho: Some(PathBuf::from("/nonexistent/rho")),
        ..Machines::default()
    };
    let mut ui = Harness::open_with("missing", target, None, machines);
    ui.wait_until("the install offer", |window, cx, app| {
        is_visible(window, "setup-install-rho")
            && app.rho_offer(&MachineId::local(), cx) == Some(Offer::Install)
    });
    ui.act(|window, _| {
        assert!(
            !is_visible(window, "retry-local"),
            "the install offer stands in for the connection's error"
        );
    });
    ui.shot("install");
}

/// An update found by the check installs from the sidebar, and once the
/// daemon finds the new program waiting on work, Restart now restarts it.
/// rho-ui follows its open sessions again, the one on screen and the one
/// with work under way, so the daemon carries on its turn.
#[test]
#[cfg_attr(windows, ignore = "runs a fake rho written as a POSIX sh script")]
fn an_update_installs_then_restarts_the_daemon_on_request() {
    let dir = tempfile::tempdir().expect("a folder for the fake rho");
    let rho = dir.path().join("rho");
    write_fake_rho(&rho, r#"[ ! -e "$dir/updated" ]"#);
    let script = Script {
        tui_job: TuiJob::Running,
        ..Script::default()
    };
    let daemon = FakeDaemon::start(script);
    keep(&daemon, &["s1", "old"]);
    let machines = Machines {
        rho: Some(rho),
        ..Machines::default()
    };
    let mut ui = Harness::open_with("update", daemon.target(), Some(daemon), machines);
    ui.wait_until("the update offer", |window, _, _| {
        is_visible(window, "update-rho-local")
    });
    ui.shot("offered");

    ui.click("update-rho-local");
    ui.wait_until(
        "the update installed and checked again",
        |window, cx, app| {
            app.rho_offer(&MachineId::local(), cx).is_none()
                && !is_visible(window, "update-rho-local")
        },
    );
    assert!(dir.path().join("updated").exists(), "rho update ran");

    ui.wait_until("the job started elsewhere", |window, _, _| {
        is_visible(window, "session-local-tui-1")
    });
    ui.click("session-local-tui-1");
    ui.wait_for_daemon("tui-1 attached", |daemon| daemon.attaches(TUI_SESSION) == 1);
    ui.click("session-local-s1");
    ui.wait_for_daemon("s1 attached", |daemon| daemon.attaches("s1") == 1);
    ui.fake().replace_program("3.2.0", 2);
    // Its status is asked now and then, on the app's clock.
    advance_until(&mut ui, UPGRADE_STEP, "the restart offer", |ui| {
        ui.cx
            .update_window(ui.window, |_, window, _| {
                is_visible(window, "restart-rho-local")
            })
            .expect("window is open")
    });
    ui.shot("waiting");

    ui.click("restart-rho-local");
    click_label(&mut ui, "Restart");
    ui.wait_for_daemon("the restart", |daemon| daemon.restarts() == 1);
    ui.cx.advance_clock(Duration::from_secs(2));
    ui.wait_until(
        "the restarted daemon waits on nothing",
        |window, cx, app| {
            app.rho_offer(&MachineId::local(), cx).is_none()
                && !is_visible(window, "restart-rho-local")
        },
    );
    ui.wait_for_daemon("the sessions followed again", |daemon| {
        daemon.attaches("s1") == 2 && daemon.attaches(TUI_SESSION) == 2
    });
    assert_eq!(ui.session().id().as_str(), "s1", "s1 is back on screen");
    ui.shot("restarted");
}

/// An open session keeps its view while the daemon stops sending its
/// events. Fallen behind, it carries on where it was scrolled to. Across a
/// restart, its composer keeps what was typed and its transcript holds
/// each entry once; and a prompt the restarting daemon couldn't take goes
/// again once it is back, and runs once. A new session's first message
/// cut off by a restart, which leaves no session to follow, goes back in
/// the composer.
#[test]
fn an_open_session_survives_lag_and_restarts() {
    let script = Script {
        replay_turns: 20,
        ..Script::default()
    };
    let mut ui = Harness::start("resume", script);
    ui.open_old_session();
    let app = ui.app.clone();
    let view = ui
        .cx
        .update(|cx| app.read(cx).active_session().expect("a session is open"));
    let same_view = |ui: &mut Harness| {
        ui.cx
            .update(|cx| app.read(cx).active_session() == Some(view.clone()))
    };
    let scroller = ui
        .cx
        .update(|cx| view.read(cx).transcript().scroller().clone());
    ui.cx
        .update(|cx| scroller.update(cx, |scroller, cx| scroller.scroll_to_item(0, cx)));
    ui.frame();
    assert!(ui.label("worked-0").is_some(), "scrolled to the first turn");

    ui.fake().fall_behind(&SessionId::from("old"));
    ui.wait_for_daemon("old attached again", |fake| fake.attaches("old") == 2);
    ui.wait_for_session("old caught up", |session| !session.is_replaying());
    assert!(same_view(&mut ui), "the same view follows old");
    assert!(
        ui.label("worked-0").is_some(),
        "still scrolled to the first turn"
    );

    let entries = ui.session().entries().len();
    ui.type_in("half a thought");
    ui.fake().restart();
    ui.wait_for_daemon("old attached after the restart", |fake| {
        fake.attaches("old") == 3
    });
    ui.wait_until("old caught up", |window, cx, app| {
        app.active_session()
            .is_some_and(|view| !view.read(cx).session().is_replaying())
            && !is_visible(window, "link-notice")
    });
    assert!(same_view(&mut ui), "the same view follows old");
    assert!(
        ui.label("worked-0").is_some(),
        "the whole replay kept the scroll position"
    );
    assert_eq!(ui.composer_text(), "half a thought");
    assert_eq!(ui.session().entries().len(), entries, "each entry once");
    ui.shot("restarted");

    ui.fake().restart_at_next_prompt();
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_for_session("the prompt's turn after the restart", |session| {
        session.inputs().len() == 1
    });
    assert_eq!(ui.fake().restarts(), 2);
    assert_eq!(ui.fake().prompts("old"), 1, "the prompt ran once");
    assert_eq!(ui.composer_text(), "", "the prompt left the composer");
    let sent = ui
        .session()
        .entries()
        .iter()
        .filter(|entry| matches!(entry, Entry::User(user) if user.text() == "half a thought"))
        .count();
    assert_eq!(sent, 1);
    ui.shot("sent-across");

    // A new session's first message, which the restart cuts off before
    // rho saved the session anywhere.
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.fake().restart_at_next_prompt();
    ui.send("list files");
    ui.wait_until("the message back, and why", |window, cx, app| {
        app.active_session()
            .is_some_and(|view| view.read(cx).composer_text(cx) == "list files")
            && is_visible(window, "link-notice")
    });
    assert_eq!(ui.fake().restarts(), 3);
    ui.wait_for_session("no turn", |session| !session.state().is_busy());
    ui.shot("gone");
}

/// A session left with nothing under way is let go, so the daemon can
/// unload it, but its view stays: shown again after the daemon unloaded it,
/// it is there at once, as it was scrolled and with what was typed, while
/// the app follows it again. A session left with background work running
/// stays followed.
#[test]
fn a_left_session_is_let_go_and_shown_again_at_once() {
    let script = Script {
        replay_turns: 20,
        ..Script::default()
    };
    let mut ui = Harness::start("let-go", script);
    ui.open_old_session();
    let app = ui.app.clone();
    let view = ui
        .cx
        .update(|cx| app.read(cx).active_session().expect("a session is open"));
    let scroller = ui
        .cx
        .update(|cx| view.read(cx).transcript().scroller().clone());
    ui.cx
        .update(|cx| scroller.update(cx, |scroller, cx| scroller.scroll_to_item(0, cx)));
    ui.frame();
    ui.type_in("half a thought");

    ui.click("session-local-s1");
    // Until s1's attach answers and its replay catches up, Enter sends
    // nothing.
    ui.wait_for_session("s1 caught up", |session| {
        session.id().as_str() == "s1" && !session.is_replaying()
    });
    ui.wait_for_daemon("old let go", |fake| fake.followers("old") == 0);
    ui.act(|window, _| {
        assert!(is_visible(window, "session-local-old"), "old's row stays");
    });
    ui.send("background");
    ui.wait_for_session("the turn ended, its process running", |session| {
        session.state() == &RunState::Idle && !session.background().is_empty()
    });

    ui.fake().unload(&SessionId::from("old"));
    ui.click("session-local-old");
    assert!(
        ui.cx
            .update(|cx| app.read(cx).active_session() == Some(view.clone())),
        "the same view"
    );
    assert!(ui.label("worked-0").is_some(), "shown as it was, at once");
    ui.wait_for_daemon("old followed again", |fake| {
        fake.attaches("old") == 2 && fake.followers("old") == 1
    });
    ui.wait_for_session("old caught up", |session| !session.is_replaying());
    assert!(ui.label("worked-0").is_some(), "still scrolled there");
    assert_eq!(ui.composer_text(), "half a thought");
    assert_eq!(ui.fake().followers("s1"), 1, "s1 waits on its process");
    ui.shot("shown-again");
}

/// A daemon that needs a restart offers one, which asks first: one too old
/// for rho-ui's protocol restarts with `rho daemon restart`, and rho-ui
/// connects once the Rho installed there is new enough, and until then says
/// to update it rather than offer the restart again; one of another build
/// warns until hidden, restarts through the protocol, and comes back as
/// rho-ui's own.
#[test]
#[cfg_attr(windows, ignore = "runs a fake rho written as a POSIX sh script")]
fn a_daemon_that_needs_a_restart_restarts_once_confirmed() {
    let dir = tempfile::tempdir().expect("a folder for the fake rho");
    let rho = dir.path().join("rho");
    write_fake_rho(&rho, "false");
    let daemon = FakeDaemon::start(Script::default());
    let cost = RestartCost {
        in_progress: 2,
        clients: 1,
    };
    // `rho daemon restart` starts the same old daemon at first.
    daemon.speak_older_protocol(cost, &dir.path().join("never"));
    let machines = Machines {
        rho: Some(rho),
        ..Machines::default()
    };
    let mut ui = Harness::open_with("older-daemon", daemon.target(), Some(daemon), machines);
    let local = MachineId::local();
    ui.wait_until("the restart offered", |window, cx, app| {
        is_visible(window, "restart-daemon-local")
            && app.machine_status(&local, cx)
                == Some(MachineStatus::OlderDaemon { cost: Some(cost) })
    });
    ui.shot("older");
    ui.click("restart-daemon-local");
    shot_dialog(&mut ui, "Restart", "older-confirm");
    click_label(&mut ui, "Restart");
    // Past the dialog's exit, so the next one is the only one.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the installed Rho found older too", |window, cx, app| {
        app.machine_status(&local, cx) == Some(MachineStatus::OlderRho)
            && is_visible(window, "retry-local")
            && !is_visible(window, "restart-daemon-local")
    });
    assert!(
        dir.path().join("restarted").exists(),
        "rho daemon restart ran"
    );
    ui.shot("older-rho");

    // Rho is updated there; Retry finds the old daemon again, and a
    // restart now runs the new Rho, another build still.
    std::fs::remove_file(dir.path().join("restarted")).expect("the marker");
    ui.fake()
        .speak_older_protocol(cost, &dir.path().join("restarted"));
    ui.fake().run_another_build(cost);
    ui.click("retry-local");
    ui.wait_until("the restart offered again", |window, _, _| {
        is_visible(window, "restart-daemon-local")
    });
    ui.click("restart-daemon-local");
    click_label(&mut ui, "Restart");
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("connected after the restart", |_, cx, app| {
        matches!(
            app.machine_status(&local, cx),
            Some(MachineStatus::Ready {
                another_build: true,
                ..
            })
        )
    });
    assert_eq!(
        ui.fake().refusals(),
        3,
        "the restarted daemon took the hello"
    );

    ui.wait_until("the other build's restart offered", |window, _, _| {
        is_visible(window, "restart-daemon-local")
    });
    ui.shot("another-build");
    // The warning's X drops it, with its restart, until the setting is
    // back. Kit's Alert doesn't register its X: a 20 px box in the top
    // right corner, inside the small alert's 12 px by 8 px padding.
    ui.act(|window, cx| {
        let alert = window.find("skew-local");
        let offset = gpui_kit::point(alert.bounds().size.width - px(22.), px(18.));
        window.click_with_options("skew-local", ClickOptions::new().with_offset(offset), cx);
    });
    ui.wait_until("the warning hidden", |window, cx, _| {
        !is_visible(window, "skew-local") && !cx.global::<Settings>().warn_other_builds
    });
    ui.cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.warn_other_builds = true;
        settings.apply(cx);
    });
    ui.wait_until("the warning back", |window, _, _| {
        is_visible(window, "skew-local") && is_visible(window, "restart-daemon-local")
    });
    ui.click("restart-daemon-local");
    shot_dialog(&mut ui, "Restart", "another-build-confirm");
    click_label(&mut ui, "Restart");
    ui.wait_for_daemon("the restart in place", |daemon| daemon.restarts() == 1);
    ui.wait_until("back as rho-ui's build", |window, cx, app| {
        !is_visible(window, "restart-daemon-local")
            && matches!(
                app.machine_status(&local, cx),
                Some(MachineStatus::Ready {
                    another_build: false,
                    ..
                })
            )
    });
    ui.shot("restarted");
}

/// Stop asks first, then stops the daemon, and rho-ui doesn't start it
/// again by itself, though it expected the daemon to restart into an
/// upgrade; Start does.
#[test]
fn a_stopped_daemon_stays_stopped_until_started() {
    let mut ui = Harness::start("stop", Script::default());
    let local = MachineId::local();
    ui.wait_until("connected", |_, cx, app| {
        matches!(
            app.machine_status(&local, cx),
            Some(MachineStatus::Ready { .. })
        )
    });
    ui.fake().replace_program("3.2.0", 1);
    ui.fake().set_in_progress(1);
    advance_until(&mut ui, UPGRADE_STEP, "the upgrade it waits on", |ui| {
        ui.cx
            .update_window(ui.window, |_, window, _| {
                is_visible(window, "restart-rho-local")
            })
            .expect("window is open")
    });

    open_settings(&mut ui);
    open_settings_page(&mut ui, "About");
    ui.wait_until("the daemon's controls", |window, _, _| {
        is_visible(window, "about-stop-local")
    });
    ui.shot("about");
    ui.click("about-stop-local");
    shot_dialog(&mut ui, "Stop", "confirm");
    click_label(&mut ui, "Stop");
    ui.wait_until("the daemon stopped", |window, cx, app| {
        is_visible(window, "about-start-local")
            && app.machine_status(&local, cx) == Some(MachineStatus::Stopped)
    });
    assert_eq!(ui.fake().stops(), 1);
    let hellos = ui.fake().hellos();
    // Past every poll that could have connected again.
    ui.cx.advance_clock(Duration::from_secs(61));
    ui.frame();
    assert_eq!(ui.fake().hellos(), hellos, "nothing connected by itself");
    assert_eq!(ui.fake().restarts(), 0, "nor restarted it");

    ui.act(|window, cx| window.press("escape", cx));
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the sidebar's Start", |window, _, _| {
        is_visible(window, "start-daemon-local")
    });
    ui.shot("stopped");
    ui.click("start-daemon-local");
    ui.wait_until("connected again", |_, cx, app| {
        matches!(
            app.machine_status(&local, cx),
            Some(MachineStatus::Ready { .. })
        )
    });
    assert_eq!(ui.fake().hellos(), hellos + 1);
}

/// A turn rho's terminal UI started shows in the sidebar as running, though
/// its session was long quiet. Opening it attaches mid-turn, and the
/// composer steers the turn: a steer waits above the box until the model
/// reads it, and can be taken back until then.
#[test]
fn a_job_started_elsewhere_is_listed_attached_and_steered() {
    let script = Script {
        tui_job: TuiJob::Running,
        ..Script::default()
    };
    let mut ui = Harness::start_with("job", script, &[]);
    ui.wait_until("the running job among settled sessions", |window, _, _| {
        is_visible(window, "session-local-tui-1") && !is_visible(window, "session-local-s1")
    });
    ui.shot("listed");

    ui.click("session-local-tui-1");
    ui.wait_for_session("the turn under way", |session| {
        session.state() == &RunState::Running
            && matches!(session.entries().last(), Some(Entry::Agent(m)) if m.text() == "Reading the parser.")
    });
    assert_eq!(ui.session().id().as_str(), TUI_SESSION);
    ui.act(|window, _| {
        assert!(
            is_visible(window, "composer-stop"),
            "the turn can be stopped"
        );
    });

    // The fake's model never reads "wait", so it stays queued.
    ui.act(|window, cx| {
        window.input("wait", cx);
        window.press("enter", cx);
    });
    ui.wait_until("the queued steer", |window, _, _| {
        is_visible(window, "steer-0")
    });
    ui.shot("queued");
    ui.click("unsteer-0");
    ui.wait_until("the steer back in the composer", |window, cx, app| {
        let view = app.active_session().expect("a session is open");
        !is_visible(window, "steer-0") && view.read(cx).composer_text(cx) == "wait"
    });

    ui.act(|window, cx| {
        window.press("secondary-a", cx);
        window.input("keep the old API", cx);
        window.press("enter", cx);
    });
    ui.wait_for_session("the steered turn to finish", |session| {
        session.state() == &RunState::Idle
            && matches!(session.entries().last(), Some(Entry::Agent(m)) if m.text() == "steered: keep the old API")
    });
    let session = ui.session();
    let steer = &session.entries()[session.entries().len() - 2];
    assert!(
        matches!(steer, Entry::User(m) if m.text() == "keep the old API"),
        "the steer joined the conversation: {steer:?}"
    );
    ui.act(|window, _| {
        assert!(!is_visible(window, "steer-0"), "nothing is left queued");
    });
    ui.shot("steered");
}

/// Whether turn header `id` shows, counting up.
fn working(window: &Window, id: &'static str) -> bool {
    window
        .try_find(id)
        .filter(gpui_kit::test::ElementSnapshot::visible)
        .and_then(|found| found.label().map(|label| label.starts_with("Working for ")))
        .unwrap_or(false)
}

/// A steer opens a turn of its own, as the session's replay shows it: the
/// turn it joined stops there, timed, and the running header moves under
/// the steer, so one header counts up, not two.
#[test]
fn a_steer_moves_the_running_header_under_it() {
    let script = Script {
        tui_job: TuiJob::Running,
        ..Script::default()
    };
    let mut ui = Harness::start_with("steer-turn", script, &[]);
    ui.wait_until("the running job", |window, _, _| {
        is_visible(window, "session-local-tui-1")
    });
    ui.click("session-local-tui-1");
    ui.wait_for_session("the turn under way", |session| {
        session.state() == &RunState::Running
    });
    // Resumed, the header springs open on the test's clock.
    ui.frame();
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the running header", |window, _, _| {
        working(window, "worked-0")
    });
    ui.act(|window, cx| {
        window.input("keep going", cx);
        window.press("enter", cx);
    });
    ui.wait_for_session("the steer in the conversation", |session| {
        matches!(session.entries().last(), Some(Entry::User(m)) if m.text() == "keep going")
    });
    ui.wait_until("one running header, under the steer", |window, _, _| {
        working(window, "worked-1") && !working(window, "worked-0")
    });
    let joined = turn_took(&mut ui, 0).expect("the steered turn is timed");
    assert!(
        joined >= Duration::from_secs(90),
        "the steered turn ran from its start, 90 s before the test: {joined:?}"
    );
    assert_eq!(turn_took(&mut ui, 1), None, "the steer's turn still runs");
    ui.shot("steered");

    ui.click("composer-stop");
    ui.wait_for_session("the turn stopped", |session| !session.state().is_busy());
    assert!(
        turn_took(&mut ui, 1).is_some_and(|took| took < joined),
        "the steer's turn is timed from the steer"
    );
}

/// A turn that ends while its session is off screen leaves the row Done
/// until the session is opened.
#[test]
fn a_turn_finished_off_screen_is_done_until_opened() {
    let script = Script {
        tui_job: TuiJob::Running,
        ..Script::default()
    };
    let mut ui = Harness::start("done", script);
    ui.wait_until("the running job among the sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
            && window
                .try_find("session-local-tui-1-busy")
                .and_then(|status| status.label().map(|label| label.starts_with("Working")))
                .unwrap_or(false)
    });
    // Only once the sidebar has seen the turn at work: a turn that ends
    // before the first poll of the jobs was never under way here.
    ui.send_from_elsewhere(TUI_SESSION, "wrap up", Send::Steer);
    // The sidebar learns of it from the daemon's jobs, which it polls.
    ui.wait_for_label("session-local-tui-1-done", "Done");
    ui.shot("done");

    ui.click("session-local-tui-1");
    ui.wait_until("the session open and the row seen", |window, _, app| {
        app.active_session().is_some() && !is_visible(window, "session-local-tui-1-done")
    });
}

/// A turn that ends on a remote machine while rho-ui is in the background
/// counts in the window's title and, a burst's wait later, sends one
/// notification naming its session; a click on it brings the window back
/// and opens the session, which clears the count. With the window focused,
/// the session on screen asking for an approval sends nothing, though the
/// title counts it.
#[test]
fn a_turn_finished_in_the_background_notifies_and_opens_on_a_click() {
    let local = FakeDaemon::start(Script::default());
    let remote = FakeDaemon::start(Script {
        tui_job: TuiJob::Running,
        ..Script::default()
    });
    let remote_target = remote.target();
    let DaemonTarget::Socket(remote_socket) = remote.target() else {
        panic!("a fake daemon listens on a socket");
    };
    let mut settings = Settings::default();
    settings.machines = vec![RemoteMachine::new("box")];
    let mut ui = Harness::open_with(
        "notify",
        local.target(),
        Some(local),
        Machines {
            settings,
            remote_sockets: vec![("box".to_owned(), remote_socket)],
            ..Machines::default()
        },
    );
    ui.wait_until("the box's job at work", |window, _, _| {
        window
            .try_find("session-box-tui-1-busy")
            .and_then(|status| status.label().map(|label| label.starts_with("Working")))
            .unwrap_or(false)
    });
    ui.blur();
    ui.send_on(remote_target.clone(), TUI_SESSION, "wrap up", Send::Steer);
    ui.wait_for_label("session-box-tui-1-done", "Done");
    let count = |ui: &mut Harness| ui.cx.update(|cx| ui.app.read(cx).attention_count());
    assert_eq!(count(&mut ui), 1, "the title counts the Done session");
    // The burst's wait, on the test's clock.
    ui.cx.advance_clock(Duration::from_secs(2));
    let posted = Rc::clone(&ui.notifications);
    ui.wait_until("one notification", |_, _, _| posted.borrow().len() == 1);
    let tag = posted.borrow()[0].tag.clone();
    assert_eq!(
        tag, "session-box-tui-1",
        "it opens the session that finished"
    );
    ui.shot("background");

    let app = ui.app.clone();
    ui.act(|window, cx| app.update(cx, |app, cx| app.notification_clicked(&tag, window, cx)));
    ui.wait_until("the session open and seen", |window, cx, app| {
        app.active_session()
            .is_some_and(|view| view.read(cx).key().id.as_str() == TUI_SESSION)
            && window.is_window_active()
            && !is_visible(window, "session-box-tui-1-done")
    });
    assert_eq!(count(&mut ui), 0, "nothing is left to count");

    // A prompt from elsewhere asks to run `ls`: the session on screen
    // needs the user, who has the window.
    ui.send_on(remote_target, TUI_SESSION, "once more", Send::Prompt);
    ui.wait_until("the approval on screen", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.cx.advance_clock(Duration::from_secs(2));
    ui.frame();
    assert_eq!(
        posted.borrow().len(),
        1,
        "nothing more is sent while focused"
    );
    assert_eq!(count(&mut ui), 1, "the title counts it all the same");
    // The box's daemon lasts the test.
    drop(remote);
}

/// A project with an origin remote gets a button beside its name in the
/// title bar that opens the repository's page; `repo::web_url` owns which
/// page. Headless GPUI records no opened URL, so this checks the button.
#[test]
fn a_projects_title_bar_links_its_repository() {
    let checkout = tempfile::tempdir().unwrap();
    let repo = rho_ui_core::platform::canonicalize(checkout.path()).unwrap();
    let git = |args: &[&str]| crate::local::test::git(&repo, args);
    git(&["init", "-q", "-b", "main"]);
    git(&["remote", "add", "origin", "git@github.com:acme/widgets.git"]);
    let daemon = FakeDaemon::start(Script::default());
    let mut ui = Harness::open_with(
        "title-repo",
        daemon.target(),
        Some(daemon),
        Machines {
            workspace: Some(repo.clone()),
            ..Machines::default()
        },
    );
    ui.click("new-session");
    ui.wait_until("the repository's link", |window, _, _| {
        is_visible(window, "draft-view") && is_visible(window, "title-repo")
    });
    ui.act(|window, cx| window.hover("title-repo", cx));
    ui.shot("hover");
}

/// The title bar lists the directories a session reaches besides its
/// workspace, and adds one: a path the daemon refuses says why and stays
/// in the box to fix, and one it takes is listed, as the session has it.
/// One of the user's projects is added with a click.
#[test]
fn a_session_adds_a_directory_from_its_title_bar() {
    let dir = tempfile::tempdir().expect("a directory to add");
    let project = tempfile::tempdir().expect("a project to add");
    let daemon = FakeDaemon::start(Script::default());
    keep(&daemon, &["s1", "old"]);
    let mut ui = Harness::open(
        "added-dirs",
        daemon.target(),
        Some(daemon),
        Projects::of([project.path()]),
    );
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("session-local-s1");
    ui.wait_for_daemon("s1 attached", |daemon| daemon.followers("s1") == 1);
    ui.click("added-dirs-button");
    let missing = dir.path().join("missing");
    ui.act(|window, cx| {
        window.input(&missing.to_string_lossy(), cx);
        window.press("enter", cx);
    });
    ui.wait_until("the refusal", |window, _, _| {
        is_visible(window, "added-dirs-error")
    });
    ui.shot("refused");

    ui.act(|window, cx| {
        window.press("secondary-a", cx);
        window.input(&dir.path().to_string_lossy(), cx);
        window.press("enter", cx);
    });
    ui.wait_for_session("the directory added", |session| {
        session.added_dirs() == [dir.path()]
    });
    ui.wait_until("it is listed", |window, _, _| {
        is_visible(window, "added-dir-0") && !is_visible(window, "added-dirs-error")
    });
    ui.shot("added");

    ui.click("add-project-0");
    let mut both = vec![dir.path().to_path_buf(), project.path().to_path_buf()];
    // In the daemon's order.
    both.sort();
    ui.wait_for_session("the project added", |session| session.added_dirs() == both);
    ui.wait_until("it is listed, and no longer offered", |window, _, _| {
        is_visible(window, "added-dir-1") && !is_visible(window, "add-project-0")
    });
}

/// The sidebar collapses to a rail and opens again, with Ctrl+B or its
/// toggle, and the state database remembers which. The toggle and Settings
/// stay where they were at any UI scale, the pane takes the width, the
/// keyboard leaves the search it hides, and the rail still starts a session
/// and shows one at work.
#[test]
fn the_sidebar_collapses_to_a_rail_and_back() {
    let script = Script {
        tui_job: TuiJob::Running,
        ..Script::default()
    };
    let mut ui = Harness::start("rail", script);
    ui.wait_until("sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-local-s1") && is_visible(window, "session-local-tui-1")
    });
    ui.click("session-local-s1");
    ui.wait_for_session("the open session", |session| {
        session.title() == Some("first")
    });
    // Where the toggle, Settings and the pane's title are drawn.
    let places = |ui: &mut Harness| {
        ui.cx
            .update_window(ui.window, |_, window, _| {
                ["toggle-sidebar", "open-settings", "session-title"]
                    .map(|id| bounds(window, id).unwrap_or_else(|| panic!("{id} is drawn")))
            })
            .expect("window is open")
    };
    let [toggle, settings, title] = places(&mut ui);
    assert_eq!(
        ui.label("toggle-sidebar").as_deref(),
        Some("Collapse sidebar")
    );

    // From the sidebar's search, which the rail doesn't show.
    let app = ui.app.clone();
    ui.act(|window, cx| {
        let search = app.read(cx).sidebar_search(cx);
        window.focus(&search, cx);
        window.press("secondary-b", cx);
    });
    ui.wait_until("the rail", |window, _, _| {
        is_visible(window, "sidebar-rail") && !is_visible(window, "session-local-s1")
    });
    ui.act(|window, cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        assert!(
            view.read(cx).composer_focus_handle(cx).is_focused(window),
            "the keyboard goes to the composer"
        );
    });
    assert_eq!(
        ui.state.get(Key::SidebarLayout).as_deref(),
        Some("collapsed")
    );
    let [rail_toggle, rail_settings, rail_title] = places(&mut ui);
    assert_eq!(rail_toggle, toggle, "the toggle stays put");
    assert_eq!(rail_settings, settings, "Settings stays put");
    assert!(
        rail_title.left() < title.left() - px(200.),
        "the pane takes the sidebar's width: {title:?} became {rail_title:?}"
    );
    assert_eq!(
        ui.label("toggle-sidebar").as_deref(),
        Some("Expand sidebar")
    );
    // The terminal UI's turn runs on, so the rail shows it.
    assert_eq!(
        ui.label("rail-working").as_deref(),
        Some("1 session working")
    );
    ui.shot("collapsed");

    ui.click("new-session");
    ui.wait_until("a draft beside the rail", |window, _, _| {
        is_visible(window, "draft-view") && is_visible(window, "sidebar-rail")
    });

    // At another scale and theme the rail and the open sidebar still line
    // up, as both place their buttons in rems.
    ui.cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.appearance = Appearance::Light;
        settings.ui_scale = UiScale::from_percent(125).expect("125% is on offer");
        settings.apply(cx);
    });
    ui.frame();
    let [scaled_rail_toggle, scaled_rail_settings, _] = places(&mut ui);
    ui.shot("collapsed-light");
    ui.click("toggle-sidebar");
    ui.wait_until("the open sidebar", |window, _, _| {
        is_visible(window, "session-local-s1") && !is_visible(window, "sidebar-rail")
    });
    assert_eq!(ui.state.get(Key::SidebarLayout).as_deref(), Some("open"));
    let [scaled_toggle, scaled_settings, _] = places(&mut ui);
    assert_eq!(
        scaled_toggle, scaled_rail_toggle,
        "the toggle stays put at 125%"
    );
    assert_eq!(
        scaled_settings, scaled_rail_settings,
        "Settings stays put at 125%"
    );
    assert_eq!(
        ui.label("toggle-sidebar").as_deref(),
        Some("Collapse sidebar")
    );
    ui.shot("open-light");
}

/// A reply's Mermaid diagram and `$$` formula draw as pictures inside the
/// reply's measure, in the theme's colors, and redraw when it changes. The
/// reply's text stays as the model wrote it.
#[test]
fn a_reply_draws_its_diagram_and_formula_in_the_theme() {
    use crate::settings::Appearance;

    let mut ui = Harness::start("figures", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("figures");
    ui.wait_for_session("the reply", |session| {
        session.state() == &RunState::Idle
            && matches!(session.entries().last(), Some(Entry::Agent(_)))
    });
    let reply = match ui.session().entries().last() {
        Some(Entry::Agent(message)) => message.text().to_owned(),
        other => panic!("the reply is last: {other:?}"),
    };
    // Each figure is named for where its source starts in the reply.
    let id = |kind: &str, source: &str| -> &'static str {
        let start = reply.find(source).expect("the reply holds the figure");
        Box::leak(format!("{kind}-{start}").into_boxed_str())
    };
    let diagram = id("diagram", "```mermaid\nsequenceDiagram");
    let wide = id("diagram", "```mermaid\nflowchart LR");
    let formula = id("formula", "$$");
    for appearance in [Appearance::Dark, Appearance::Light] {
        ui.cx.update(|cx| {
            let mut settings = cx.global::<Settings>().clone();
            settings.appearance = appearance;
            settings.apply(cx);
        });
        ui.wait_until("the diagrams and the formula", |window, _, _| {
            is_visible(window, diagram) && is_visible(window, wide) && is_visible(window, formula)
        });
        ui.act(|window, _| {
            for figure in [diagram, wide, formula] {
                let column = window.find(figure).bounds();
                let shown = window.within(figure).find("picture").bounds();
                assert!(
                    shown.size.height > px(0.)
                        && shown.left() >= column.left()
                        && shown.right() <= column.right(),
                    "{figure} fits the reply's column {column:?}: {shown:?}"
                );
            }
            // Wider than the column, it scales down to fill it, keeping
            // its proportions: a row of boxes stays a thin strip.
            let column = window.find(wide).bounds();
            let shown = window.within(wide).find("picture").bounds().size;
            assert!(
                (shown.width - column.size.width).abs() < px(1.),
                "the wide diagram fills the column: {shown:?}"
            );
            assert!(shown.height < shown.width / 4., "{shown:?}");
        });
        // Drawn again in each theme: the formula's ink stands out from the
        // background toward the theme's text color, which a drawing kept
        // from the other theme's doesn't.
        let (background, foreground) = ui
            .cx
            .update(|cx| (cx.theme().background.l, cx.theme().foreground.l));
        let (shown, scale) = ui
            .cx
            .update_window(ui.window, |_, window, _| {
                let shown = window.within(formula).find("picture").bounds();
                (shown, f64::from(window.scale_factor()))
            })
            .expect("window is open");
        let image = ui
            .cx
            .capture_screenshot(ui.window)
            .expect("the window renders offscreen");
        let span = |from: Pixels, to: Pixels| f64::from(from)..f64::from(to);
        let (across, down) = (
            span(shown.left(), shown.right()),
            span(shown.top(), shown.bottom()),
        );
        let ink = image
            .enumerate_pixels()
            .filter(|(x, y, _)| {
                across.contains(&(f64::from(*x) / scale)) && down.contains(&(f64::from(*y) / scale))
            })
            .map(|(_, _, pixel)| {
                let [r, g, b, _] = pixel.0.map(|channel| f32::from(channel) / 255.);
                let lightness = f32::midpoint(r.max(g).max(b), r.min(g).min(b));
                (lightness - background) / (foreground - background)
            })
            .fold(0., f32::max);
        assert!(
            ink > 0.5,
            "the formula is drawn in the {} theme's text color: its inkiest pixel is {ink} of the way from the background",
            appearance.name()
        );
        ui.shot(appearance.name());
    }
}

/// A reply's images by path come from the session's folder, at their own
/// size within the reply's column, and one that isn't there says so in its
/// place. A tool's image shows in its details. A click opens a picture in
/// the lightbox at its own size, under its caption; it zooms in and fits
/// again, and Escape closes it.
#[test]
fn pictures_show_in_place_and_open_in_the_lightbox() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let folder = rho_ui_core::platform::canonicalize(dir.path()).expect("it resolves");
    std::fs::write(
        folder.join("plot.png"),
        crate::attachments::png(640, 320, RED),
    )
    .expect("write the picture");
    let daemon = FakeDaemon::start(Script::default());
    keep(&daemon, &["s1", "old"]);
    let mut ui = Harness::open_with(
        "pictures",
        daemon.target(),
        Some(daemon),
        Machines {
            workspace: Some(folder),
            ..Machines::default()
        },
    );
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("pictures");
    ui.wait_for_session("the reply", |session| {
        session.state() == &RunState::Idle
            && matches!(session.entries().last(), Some(Entry::Agent(_)))
    });
    let reply = match ui.session().entries().last() {
        Some(Entry::Agent(message)) => message.text().to_owned(),
        other => panic!("the reply is last: {other:?}"),
    };
    // Each picture's row is named for where its paragraph starts.
    let row = |alt: &str| -> &'static str {
        let start = reply
            .find(&format!("![{alt}"))
            .expect("the reply holds the picture");
        Box::leak(format!("image-{start}-0").into_boxed_str())
    };
    let (plot, chart) = (row("The plot"), row("A missing chart"));
    ui.wait_until(
        "the plot, and the chart that isn't there",
        |window, _, _| is_visible(window, "picture") && is_visible(window, "picture-failed"),
    );
    ui.act(|window, _| {
        let column = window.find(plot).bounds();
        let shown = window.within(plot).find("picture").bounds();
        assert!(
            (shown.size.width - px(640.)).abs() < px(1.)
                && (shown.size.height - px(320.)).abs() < px(1.),
            "the plot shows at its own size: {shown:?}"
        );
        assert!(
            shown.left() >= column.left() && shown.right() <= column.right(),
            "inside the reply's column {column:?}: {shown:?}"
        );
        assert!(window.within(chart).try_find("picture-failed").is_some());
    });
    ui.shot("reply");

    ui.cx.advance_clock(Duration::from_secs(1));
    ui.toggle_turn("worked-0");
    ui.wait_until("the turn's working", |window, _, _| {
        is_visible(window, "step-1")
    });
    ui.act(|window, cx| window.within("step-1").click("step-line", cx));
    ui.wait_until("the tool's image", |window, _, _| {
        is_visible(window, "step-1-image-0")
    });
    ui.shot("tool");

    let shown = |ui: &mut Harness| {
        ui.cx
            .update_window(ui.window, |_, window, _| {
                window.find("lightbox-picture").bounds().size
            })
            .expect("window is open")
    };
    let is_about = |size: Size<Pixels>, width: f32, height: f32| {
        (size.width - px(width)).abs() < px(1.) && (size.height - px(height)).abs() < px(1.)
    };
    ui.act(|window, cx| window.within(plot).click("picture", cx));
    ui.wait_until("the lightbox", |window, _, _| {
        is_visible(window, "lightbox-picture") && is_visible(window, "lightbox-caption")
    });
    // The dialog fades in.
    ui.cx.advance_clock(Duration::from_secs(1));
    let fitted = shown(&mut ui);
    assert!(is_about(fitted, 640., 320.), "at its own size: {fitted:?}");
    ui.shot("lightbox");
    ui.click("lightbox-zoom-in");
    let zoomed = shown(&mut ui);
    assert!(is_about(zoomed, 960., 480.), "zoomed in: {zoomed:?}");
    ui.shot("zoomed");
    ui.act(|window, cx| window.press("0", cx));
    let fitted = shown(&mut ui);
    assert!(is_about(fitted, 640., 320.), "fitted again: {fitted:?}");
    ui.act(|window, cx| window.press("escape", cx));
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the lightbox closed", |window, _, _| {
        !is_visible(window, "lightbox")
    });
}

/// Whether the session view shows an error: a failed turn, or a request
/// that failed.
fn shows_an_error(window: &Window) -> bool {
    is_visible(window, "turn-error") || is_visible(window, "session-error")
}

/// A turn ends while the process it started runs on, which is listed above
/// the composer. Stopping it takes it off that list, and the header's ended
/// work says it was stopped, while the daemon's own turn tells the model,
/// which shows like any turn and takes a steer. A process that exits by
/// itself is listed in the ended work too.
#[test]
fn background_work_is_listed_stopped_and_handed_to_the_daemons_turn() {
    let mut ui = Harness::start("background", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("background");
    ui.wait_for_session("the turn to end with its process running", |session| {
        session.state() == &RunState::Idle && !session.background().is_empty()
    });
    ui.act(|window, _| {
        assert!(is_visible(window, "background-0"), "the process is listed");
    });
    ui.shot("running");

    ui.click("background-stop-0");
    ui.wait_for_session("the daemon's turn about the stop", |session| {
        session.state() == &RunState::Running
            && session.background()[0].status() == BackgroundStatus::Cancelled
    });
    ui.act(|window, _| {
        assert!(!is_visible(window, "background"), "nothing runs");
        assert!(
            is_visible(window, "composer-stop"),
            "the turn shows as running"
        );
    });
    ui.click("background-history-button");
    ui.act(|window, _| {
        assert!(
            is_visible(window, "background-ended-0"),
            "the stopped process is in the ended work"
        );
    });
    ui.shot("stopped-history");
    ui.act(|window, cx| window.press("escape", cx));
    let session = ui.session();
    assert!(
        matches!(session.entries().last(), Some(Entry::Agent(m)) if m.text() == "`sleep 30` was stopped."),
        "the daemon's turn shows in the transcript: {:?}",
        session.entries()
    );
    let wake = session
        .entries()
        .iter()
        .position(|entry| matches!(entry, Entry::Wake))
        .expect("the daemon's turn opens with a wake");
    let wake = SharedString::from(format!("wake-{wake}"));
    ui.act(|window, _| {
        assert!(
            window.try_find(wake).is_none(),
            "the stop's card says why rho went on, so the wake draws no row"
        );
        assert!(
            is_visible(window, "worked-1"),
            "the daemon's turn has its own header"
        );
    });
    ui.shot("stopped");

    // A message sent during the daemon's turn steers it, which ends it.
    ui.send("thanks");
    ui.wait_for_session("the daemon's turn to end", |session| {
        session.state() == &RunState::Idle && session.background().is_empty()
    });
    ui.act(|window, _| {
        assert!(!is_visible(window, "background"), "the list is gone");
        assert!(
            is_visible(window, "background-history-button"),
            "ended work outlives the daemon's list"
        );
        assert!(!shows_an_error(window));
    });
    assert!(
        turn_took(&mut ui, 1).is_some(),
        "the daemon's turn is its own"
    );
    ui.shot("delivered");

    ui.send("background");
    ui.wait_for_session("another process", |session| {
        session.state() == &RunState::Idle && !session.background().is_empty()
    });
    ui.fake().finish_background(&SessionId::from("new-1"), 0);
    ui.wait_for_session("the daemon's turn about the exit", |session| {
        session.state() == &RunState::Running
            && session.background()[0].status() == BackgroundStatus::Completed
    });
    ui.act(|window, _| assert!(!is_visible(window, "background"), "it left the list"));
    ui.click("background-history-button");
    ui.act(|window, _| {
        assert!(
            is_visible(window, "background-ended-0") && is_visible(window, "background-ended-1"),
            "both processes are in the ended work"
        );
    });
    ui.shot("exited");
}

/// Line `ix` of the process output on screen.
fn process_line(ix: usize) -> ElementId {
    ElementId::from(("process-line", ix))
}

/// A background process's row says how long it has run and been quiet,
/// and opens its output in the right panel's Background surface, which
/// reads on while the process runs: what it writes next shows, a line on
/// stderr marked. Once it ended and its last output is in, nothing is read
/// again; Back shows the lists, whose ended row opens the output once more.
#[test]
fn a_background_process_output_is_followed_until_it_ends() {
    let mut ui = Harness::start("process-output", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("background");
    ui.wait_for_session("the turn to end with its process running", |session| {
        session.state() == &RunState::Idle && !session.background().is_empty()
    });
    let session = SessionId::from("new-1");
    ui.fake()
        .write_process(&session, ProcessStream::Stdout, "Compiling rho-ui\nCompil");
    // It ran a while, and has been quiet long enough for its row to say.
    ui.fake().age_process(
        &session,
        Duration::from_secs(4 * 60 + 12),
        Duration::from_secs(90),
    );
    ui.poll(
        WAIT,
        "the row's time",
        |ui| {
            ui.cx.advance_clock(Duration::from_secs(1));
            ui.frame();
            ui.label("background-0")
                .is_some_and(|label| label.starts_with("sleep 30: Running · 4m 1"))
                && ui
                    .label("background-0")
                    .is_some_and(|label| label.contains("· quiet 1m 3"))
        },
        |ui| format!("the row says {:?}", ui.label("background-0")),
    );
    ui.shot("rows");

    ui.click("background-0");
    ui.wait_until("the output in the panel", |window, _, _| {
        is_visible(window, "process-output") && is_visible(window, process_line(1))
    });
    assert_eq!(
        ui.label(process_line(0)).as_deref(),
        Some("Compiling rho-ui")
    );

    // What it writes next is read on the output's own poll: the rest of
    // the line it began, then a line on stderr.
    ui.fake()
        .write_process(&session, ProcessStream::Stdout, "ing rho-ui-core\n");
    ui.fake()
        .write_process(&session, ProcessStream::Stderr, "warning: unused\n");
    let lines = |ui: &mut Harness| [1, 2].map(|ix| ui.label(process_line(ix)));
    ui.poll(
        WAIT,
        "the next output",
        |ui| {
            ui.cx.advance_clock(Duration::from_millis(500));
            ui.frame();
            lines(ui)
                == [
                    Some("Compiling rho-ui-core".into()),
                    Some("warning: unused".into()),
                ]
        },
        |ui| format!("lines 1 and 2 say {:?}", lines(ui)),
    );
    ui.shot("following");

    ui.fake().finish_background(&session, 0);
    ui.poll(
        WAIT,
        "the exit",
        |ui| {
            ui.cx.advance_clock(Duration::from_secs(1));
            ui.frame();
            ui.label("process-output-status")
                .is_some_and(|status| status.starts_with("Exited 0"))
        },
        |ui| format!("it says {:?}", ui.label("process-output-status")),
    );
    ui.shot("exited");
    ui.click("process-output-back");
    ui.act(|window, _| {
        assert!(
            !is_visible(window, "process-output"),
            "Back shows the lists"
        );
        assert!(is_visible(window, "panel-ended-0"));
    });
    // Nothing runs and the last output is in: the clock moving on reads
    // nothing. Following it again reads once, and no more.
    for _ in 0..10 {
        ui.cx.advance_clock(Duration::from_secs(1));
        ui.frame();
    }
    let reads = ui.fake().process_reads();
    ui.click("panel-ended-0");
    ui.wait_until("the output again", |window, _, _| {
        is_visible(window, process_line(2))
    });
    for _ in 0..10 {
        ui.cx.advance_clock(Duration::from_secs(1));
        ui.frame();
    }
    assert_eq!(
        ui.fake().process_reads(),
        reads + 1,
        "an ended process's output is read once"
    );
}

/// The right panel shows and hides with Ctrl+Alt+B or its toggle, and the
/// state database remembers which. It lists its surfaces; the Background
/// surface opens as a tab and stops its work. Each session has its own
/// tabs: another shows none, and coming back shows its own again. Closing
/// the last tab goes back to the list.
#[test]
fn the_right_panel_opens_a_surface_that_follows_the_session() {
    let mut ui = Harness::start("right-panel", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.act(|window, _| {
        assert!(!is_visible(window, "right-panel"), "hidden until shown");
        assert!(
            is_visible(window, "toggle-right-panel"),
            "the title bar offers it"
        );
    });
    ui.send("background");
    ui.wait_for_session("the turn to end with its process running", |session| {
        session.state() == &RunState::Idle && !session.background().is_empty()
    });

    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.act(|window, _| {
        assert!(
            is_visible(window, "panel-surface-list"),
            "the list of surfaces"
        );
        assert!(is_visible(window, "open-surface-terminal"));
        assert!(
            !is_visible(window, "panel-surfaces"),
            "no + while the list is on screen"
        );
    });
    assert_eq!(ui.state.get(Key::RightPanel).as_deref(), Some("shown"));
    ui.shot("list");

    ui.click("open-surface-background");
    ui.act(|window, _| {
        assert!(
            is_visible(window, "panel-tab-background"),
            "it opens as a tab"
        );
        assert!(
            is_visible(window, "panel-background-0"),
            "the running process is listed"
        );
    });
    ui.shot("background");
    ui.click("panel-background-stop-0");
    ui.wait_until(
        "the stopped process in the panel's ended work",
        |window, cx, app| {
            let stopped = app.active_session().is_some_and(|view| {
                view.read(cx).session().background()[0].status() == BackgroundStatus::Cancelled
            });
            stopped
                && is_visible(window, "panel-ended-0")
                && !is_visible(window, "panel-background")
        },
    );
    ui.shot("stopped");

    // Another session has tabs of its own: none yet.
    let key = ui.cx.update(|cx| {
        let app = ui.app.read(cx);
        app.active_session().expect("a session").read(cx).live().id
    });
    ui.click("session-local-s1");
    ui.wait_for_session("the saved session", |session| {
        session.title() == Some("first")
    });
    ui.act(|window, _| {
        assert!(!is_visible(window, "panel-tab-background"), "not s1's tab");
        assert!(is_visible(window, "panel-surface-list"));
    });
    let row = SharedString::from(format!("session-{key}"));
    ui.act(|window, cx| window.click(row, cx));
    ui.wait_until("the first session's tab again", |window, _, _| {
        is_visible(window, "panel-tab-background") && is_visible(window, "panel-ended-0")
    });

    ui.act(|window, cx| window.hover("panel-tab-background", cx));
    ui.click("close-panel-tab-background");
    ui.act(|window, _| {
        assert!(!is_visible(window, "panel-tab-background"));
        assert!(is_visible(window, "panel-surface-list"));
    });
    ui.click("toggle-right-panel");
    ui.act(|window, _| assert!(!is_visible(window, "right-panel")));
    assert_eq!(ui.state.get(Key::RightPanel).as_deref(), Some("hidden"));
}

/// `/side` forks the session into a side chat in the right panel: the
/// fork asks its own approval and answers there, and the session itself
/// takes no prompt. Closing the tab deletes the fork, after stopping its
/// turn if one waits; the panel's list starts another.
#[test]
fn a_side_chat_forks_the_session_into_the_right_panel() {
    let mut ui = Harness::start("side-chat", Script::default());
    new_session_with(&mut ui, "list files");
    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.click("approval-allow-once");
    ui.wait_for_session("the turn to finish", |session| {
        session.state() == &RunState::Idle && session.entries().len() == 3
    });

    ui.send("/side why ls");
    ui.wait_until("the side chat's approval", |window, cx, app| {
        let asked = app
            .side_chat(cx)
            .is_some_and(|view| view.read(cx).session().state() == &RunState::NeedsAction);
        asked && is_visible(window, "panel-tab-side-chat")
    });
    assert_eq!(ui.composer_text(), "", "the question left the composer");
    // The question went once the forked conversation was all there, so
    // its turn is the one running, not the turn it forked: the forked
    // turn's header, once drawn, says how long it worked.
    let forked = |ui: &mut Harness| {
        ui.cx
            .update_window(ui.window, |_, window, _| {
                let header = window.within("right-panel").try_find("worked-0")?;
                header.label().map(str::to_owned)
            })
            .expect("window is open")
    };
    ui.poll(
        WAIT,
        "the forked turn's header",
        |ui| {
            ui.frame();
            forked(ui).is_some_and(|label| label.starts_with("Worked for "))
        },
        |ui| format!("it says {:?}", forked(ui)),
    );
    ui.shot("approval");
    ui.act(|window, cx| {
        window
            .within("right-panel")
            .click("approval-allow-once", cx);
    });
    let app = ui.app.clone();
    ui.wait_until("the side chat's answer", |_, cx, _| {
        app.read(cx).side_chat(cx).is_some_and(|view| {
            let session = view.read(cx).session();
            session.state() == &RunState::Idle
                && matches!(session.entries().last(), Some(Entry::Agent(m)) if m.text() == "you chose allow_once")
        })
    });
    let side = ui.cx.update(|cx| {
        let view = app.read(cx).side_chat(cx).expect("a side chat shows");
        view.read(cx).session().clone()
    });
    // It forked the conversation, then asked its own question.
    assert!(matches!(&side.entries()[0], Entry::User(m) if m.text() == "list files"));
    assert!(matches!(&side.entries()[3], Entry::User(m) if m.text() == "why ls"));
    assert_eq!(ui.session().entries().len(), 3, "the session is untouched");
    assert_eq!(
        (ui.fake().prompts("new-1"), ui.fake().prompts("fork-1")),
        (1, 1)
    );
    ui.shot("answered");

    ui.act(|window, cx| window.hover("panel-tab-side-chat", cx));
    ui.click("close-panel-tab-side-chat");
    ui.wait_for_daemon("the fork deleted", |fake| !fake.has_session("fork-1"));
    ui.act(|window, _| assert!(is_visible(window, "panel-surface-list")));

    // One closed while its turn waits on an approval goes too.
    ui.send("/btw and then");
    ui.wait_until("the second side chat's approval", |window, cx, app| {
        let asked = app
            .side_chat(cx)
            .is_some_and(|view| view.read(cx).session().state() == &RunState::NeedsAction);
        asked && is_visible(window, "panel-tab-side-chat-2")
    });
    ui.act(|window, cx| window.hover("panel-tab-side-chat-2", cx));
    ui.click("close-panel-tab-side-chat-2");
    ui.wait_for_daemon("the waiting fork deleted", |fake| {
        !fake.has_session("fork-2")
    });

    ui.click("open-surface-side-chat");
    ui.wait_until("another side chat", |window, cx, app| {
        app.side_chat(cx).is_some() && is_visible(window, "panel-tab-side-chat-3")
    });
    assert!(ui.fake().has_session("fork-3"));
    assert_eq!(ui.fake().prompts("new-1"), 1, "the session took no prompt");
}

/// With Windows' window buttons drawn over the top row's right end, the
/// control at that end still takes clicks: the title bar's panel toggle,
/// then the panel's own.
#[test]
fn windows_buttons_leave_the_top_rows_controls_clickable() {
    let mut ui = Harness::start("windows-chrome", Script::default());
    ui.cx.update(|cx| {
        let chrome = Chrome::windows(cx);
        cx.set_global(chrome);
    });
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("session-local-s1");
    ui.wait_until("the session and the window buttons", |window, _, _| {
        is_visible(window, "session-title") && is_visible(window, "window-close")
    });
    ui.shot("session");

    ui.click("toggle-right-panel");
    ui.act(|window, _| assert!(is_visible(window, "right-panel"), "shown"));
    ui.shot("panel");
    ui.click("toggle-right-panel");
    ui.act(|window, _| assert!(!is_visible(window, "right-panel"), "hidden"));
    assert_eq!(ui.state.get(Key::RightPanel).as_deref(), Some("hidden"));
}

/// On macOS the traffic lights take the sidebar's top row, and draw over
/// the app where a click can't reach it: the rail's toggle goes under that
/// row, and still opens the sidebar.
#[test]
fn macs_traffic_lights_leave_the_rails_toggle_clickable() {
    let mut ui = Harness::start("mac-chrome", Script::default());
    ui.cx.update(|cx| cx.set_global(Chrome::Mac));
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("toggle-sidebar");
    ui.wait_until("the rail", |window, _, _| {
        is_visible(window, "sidebar-rail")
    });
    ui.act(|window, _| {
        let toggle = bounds(window, "toggle-sidebar").expect("the rail's toggle is drawn");
        let lights = window.rem_size() * TOP_ROW_REMS;
        assert!(
            toggle.top() >= lights,
            "{toggle:?} is under the lights' row"
        );
    });
    ui.shot("rail");

    ui.click("toggle-sidebar");
    ui.act(|window, _| assert!(!is_visible(window, "sidebar-rail"), "open again"));
    assert_eq!(ui.state.get(Key::SidebarLayout).as_deref(), Some("open"));
    ui.shot("open");
}

/// The + in the right panel's header opens a menu of the surfaces without
/// a tab. Dragging one tab onto another moves it there, and a tab shows ×,
/// in place of its icon, only while hovered.
#[test]
fn the_right_panels_tabs_open_from_a_menu_move_and_close() {
    let mut ui = Harness::start("panel-tabs", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("session-local-s1");
    ui.wait_for_session("the saved session", |session| {
        session.title() == Some("first")
    });
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.click("open-surface-background");

    // + opens a menu over the open surface, not the list in its place.
    ui.click("panel-surfaces");
    ui.shot("menu");
    click_label(&mut ui, "Diff");
    ui.act(|window, _| {
        assert!(is_visible(window, "panel-tab-diff"), "Diff opens as a tab");
        assert!(!is_visible(window, "panel-surface-list"));
        assert!(
            is_visible(window, "panel-surfaces"),
            "Terminal is left to add"
        );
    });
    // Dragging Diff onto Background work puts it first.
    let (from, to) = ui
        .cx
        .update_window(ui.window, |_, window, _| {
            let from = bounds(window, "panel-tab-diff").expect("the Diff tab");
            let to = bounds(window, "panel-tab-background").expect("the Background tab");
            (from.center(), to.center())
        })
        .expect("window is open");
    ui.act(|window, cx| window.drag(from, to, cx));
    ui.wait_until("Diff moved first", |_, cx, app| {
        app.panel_tabs(cx) == [Surface::Diff, Surface::Background]
    });
    ui.shot("reordered");
    ui.act(|window, cx| window.hover("panel-tab-diff", cx));
    ui.click("close-panel-tab-diff");
    ui.act(|window, _| assert!(!is_visible(window, "panel-tab-diff")));

    // Background work slid under the pointer; move it off the tabs.
    ui.act(|window, cx| window.hover("toggle-right-panel", cx));
    ui.act(|window, _| {
        assert!(
            !is_visible(window, "close-panel-tab-background"),
            "× waits for a hover"
        );
    });
    ui.act(|window, cx| window.hover("panel-tab-background", cx));
    ui.shot("hover-tab");
    ui.click("close-panel-tab-background");
    ui.act(|window, _| assert!(is_visible(window, "panel-surface-list")));
}

/// A gh for the tests that act on a PR: it writes the arguments of each
/// `gh pr ...` it runs, and notes each `gh auth status`. It reads nothing:
/// the daemon reads PRs.
struct StubGh {
    bin: tempfile::TempDir,
}

impl StubGh {
    /// One signed in to github.com as octocat.
    fn new() -> Self {
        Self::with_hosts(
            r#"{"github.com":[{"state":"success","active":true,"host":"github.com","login":"octocat"}]}"#,
        )
    }

    /// One signed in nowhere, so it can't comment.
    fn signed_out() -> Self {
        Self::with_hosts("{}")
    }

    /// One whose `gh auth status --json hosts` gives `hosts`.
    fn with_hosts(hosts: &str) -> Self {
        let bin = tempfile::tempdir().unwrap();
        let script = format!(
            "#!/bin/sh\ncase $1 in\n  --version) echo 'gh version 2.99.0 (2026-09-01)' ;;\n  \
             auth) echo checked >> '{checks}'; echo '{{\"hosts\":{hosts}}}' ;;\n  \
             pr) printf '%s\\n' \"$@\" > '{acted}' ;;\nesac\n",
            checks = bin.path().join("checks").display(),
            acted = bin.path().join("acted").display(),
        );
        write_program(&bin.path().join("gh"), &script);
        StubGh { bin }
    }

    /// How many times its accounts were checked.
    fn checks(&self) -> usize {
        std::fs::read_to_string(self.bin.path().join("checks"))
            .map_or(0, |checks| checks.lines().count())
    }

    /// Settings that act through this gh on this machine.
    fn settings(&self) -> Settings {
        let mut settings = Settings::default();
        let program = self.bin.path().join("gh");
        settings
            .local_cli
            .set(ForgeCli::Gh, program.display().to_string());
        settings
    }

    /// The arguments of its last `gh pr ...`, one a line.
    fn acted(&self) -> Option<String> {
        std::fs::read_to_string(self.bin.path().join("acted")).ok()
    }
}

/// The session on screen.
fn active_key(ui: &mut Harness) -> SessionKey {
    let app = ui.app.clone();
    ui.cx.update(|cx| {
        let view = app.read(cx).active_session().expect("a session shows");
        view.read(cx).live().id
    })
}

/// The summary of the branch's PR in `set`.
fn branch_pr(set: &mut SessionPullRequests) -> &mut PullRequestSummary {
    match set.branch.as_mut().map(|branch| &mut branch.lookup) {
        Some(BranchLookup::Found { pull_request }) => pull_request.as_mut(),
        _ => panic!("the branch has a PR"),
    }
}

/// How each `show` the fake daemon got for `session` read, oldest first.
fn shows(ui: &Harness, session: &SessionKey) -> Vec<PullRequestRead> {
    ui.fake().pull_request_shows(&session.id)
}

/// How many `show`s for `session` read GitHub afresh.
fn fresh_shows(fake: &FakeDaemon, session: &SessionKey) -> usize {
    let shows = fake.pull_request_shows(&session.id);
    shows
        .iter()
        .filter(|read| **read == PullRequestRead::Fresh)
        .count()
}

/// The numbers of the PRs the fake daemon links to `session`, in order.
fn linked_numbers(fake: &FakeDaemon, session: &SessionKey) -> Vec<u32> {
    let set = fake.pull_requests(&session.id);
    set.linked.iter().map(|link| link.id.number).collect()
}

/// How far to move the clock at a time for a daemon's status to be asked
/// again: past the app's status poll. The status is asked from the jobs
/// loop, whose next timer starts only once the last status answers, which
/// may be after a step.
const UPGRADE_STEP: Duration =
    Duration::from_secs(crate::app::UPGRADE_POLL.as_secs() + crate::app::JOB_POLL.as_secs());

/// Moves the clock on by `step` at a time until `ready` holds: for a
/// timer that starts as an answer lands, which may still be on its way.
fn advance_until(
    ui: &mut Harness,
    step: Duration,
    what: &str,
    mut ready: impl FnMut(&mut Harness) -> bool,
) {
    ui.poll(
        WAIT,
        what,
        |ui| {
            ui.cx.advance_clock(step);
            ui.frame();
            ready(ui)
        },
        Harness::labels_seen,
    );
}

/// A new session on this machine, its first turn ended, whose branch the
/// daemon then finds PR 12 on, as `open` gives the session it. Returns
/// its key.
fn open_pr_session(
    name: &'static str,
    settings: Settings,
    open: impl FnOnce(&FakeDaemon, &SessionId),
) -> (Harness, SessionKey) {
    let daemon = FakeDaemon::start(Script::default());
    keep(&daemon, &["s1"]);
    let mut ui = Harness::open_with(
        name,
        daemon.target(),
        Some(daemon),
        Machines {
            settings,
            ..Machines::default()
        },
    );
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("background");
    ui.wait_for_session("the turn to end", |session| {
        session.state() == &RunState::Idle
    });
    let key = active_key(&mut ui);
    open(ui.fake(), &key.id);
    (ui, key)
}

/// Opens the Pull request surface over a session made by
/// [`open_pr_session`], and waits for PR 12 and its checks.
fn open_pr_surface(name: &'static str, settings: Settings) -> (Harness, SessionKey) {
    let pictures = serve_pictures();
    open_pr_surface_with(name, settings, |fake, session| {
        fake.open_pull_request(session, &pictures);
    })
}

/// [`open_pr_surface`], the session given its PR as `open` does.
fn open_pr_surface_with(
    name: &'static str,
    settings: Settings,
    open: impl FnOnce(&FakeDaemon, &SessionId),
) -> (Harness, SessionKey) {
    let (mut ui, key) = open_pr_session(name, settings, open);
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.click("open-surface-pull-request");
    ui.wait_until("the PR and its checks", |window, _, _| {
        is_visible(window, "pull-request-check-1")
    });
    (ui, key)
}

/// The Pull request surface shows the PR the daemon found on the session's
/// branch, with its people's pictures. While it shows, it asks the daemon
/// again every 30 seconds, so the daemon keeps reading it, and nothing
/// while the panel is hidden; a change the daemon reports redraws it at
/// once. A trouble the daemon reports shows over the PR it read last.
#[test]
fn the_pull_request_surface_shows_the_branchs_pr() {
    let (mut ui, key) = open_pr_surface("pull-request", Settings::default());
    // What [`FakeDaemon::open_pull_request`] opened.
    let id = PullRequestId::new("github.com", "acme", "widgets", 12);
    let set = ui.fake().pull_requests(&key.id);
    ui.cx.update(|cx| {
        let prs = ui.app.read(cx).pull_requests().read(cx);
        assert_eq!(prs.of(&key), Some(&set), "the daemon's set, whole");
        let shown = Shown {
            session: key.clone(),
            which: Which::Branch,
        };
        assert_eq!(prs.summary(&shown).map(|summary| &summary.id), Some(&id));
        assert!(prs.detail(&key.machine, &id).is_some(), "its detail");
    });
    assert_eq!(
        ui.label("pull-request-title").as_deref(),
        Some("Add widgets")
    );
    // Those who posted show their pictures, fetched from where GitHub
    // serves each.
    ui.wait_until("the authors' pictures", |_, cx, app| {
        let prs = app.pull_requests().read(cx);
        let detail = prs.detail(&key.machine, &id).expect("its detail");
        let comments = detail.threads.iter().flat_map(|thread| &thread.comments);
        detail.posts.iter().chain(comments).all(|post| {
            let url = post.avatar_url.as_deref().expect("the author's picture");
            prs.avatar(url).is_some()
        })
    });
    // Its description draws its diagram, as a message does.
    let diagram = SharedString::from(format!(
        "diagram-{}",
        PULL_REQUEST_BODY.find("```mermaid").expect("a diagram")
    ));
    ui.wait_until("the description's diagram", |window, _, _| {
        is_visible(window, diagram.clone())
    });
    ui.act(|window, _| {
        let column = window.find("pull-request-body").bounds();
        let shown = window.within(diagram.clone()).find("picture").bounds();
        assert!(
            shown.size.height > px(0.)
                && shown.left() >= column.left()
                && shown.right() <= column.right(),
            "the diagram fits the surface {column:?}: {shown:?}"
        );
    });
    ui.shot("pr");

    // On screen, it asks again within the daemon's 30 seconds.
    let asked = shows(&ui, &key).len();
    ui.cx.advance_clock(Duration::from_secs(30));
    ui.wait_for_daemon("the next show", |fake| {
        fake.pull_request_shows(&key.id).len() > asked
    });
    assert_eq!(shows(&ui, &key).last(), Some(&PullRequestRead::Cached));

    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    let asked = shows(&ui, &key).len();
    ui.cx.advance_clock(Duration::from_secs(120));
    ui.frame();
    assert_eq!(shows(&ui, &key).len(), asked, "a hidden panel asks nothing");
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.wait_for_daemon("a show as the panel shows", |fake| {
        fake.pull_request_shows(&key.id).len() > asked
    });

    // A change the daemon reports redraws the PR with no wait.
    ui.fake().edit_pull_requests(&key.id, |set| {
        "Add widgets, renamed".clone_into(&mut branch_pr(set).title);
    });
    ui.wait_until("the new title", |window, _, _| {
        window
            .try_find("pull-request-title")
            .is_some_and(|title| title.label() == Some("Add widgets, renamed"))
    });

    // The daemon paused its reads: it says so over the PR it read last.
    ui.fake().edit_pull_requests(&key.id, |set| {
        let branch = set.branch.as_mut().expect("the branch");
        branch.trouble = Some(PullRequestTrouble::Paused {
            host: "github.com".to_owned(),
            remaining: 10,
            floor: 50,
            // 2099-01-01.
            resets_at: 4_070_908_800,
        });
    });
    ui.wait_until("the pause", |window, _, _| {
        is_visible(window, "pull-request-trouble") && is_visible(window, "pull-request-title")
    });
    ui.shot("paused");
}

/// A PR's images show in its description and comments, fetched from
/// where their addresses say, each once: a Markdown screenshot scaled to
/// the surface's measure, and an `<img>` upload at the width its tag
/// says. One that doesn't load says so in its place, with its address. A
/// click opens one in the lightbox.
#[test]
fn a_pull_requests_images_show_and_open_in_the_lightbox() {
    let pictures = picture::test::serve();
    let (mut ui, key) = open_pr_surface_with(
        "pull-request-images",
        Settings::default(),
        |fake, session| {
            fake.open_pull_request_with_images(session, &pictures.address);
        },
    );
    let id = PullRequestId::new("github.com", "acme", "widgets", 12);
    let body = ui.cx.update(|cx| {
        let prs = ui.app.read(cx).pull_requests().read(cx);
        prs.detail(&key.machine, &id)
            .expect("its detail")
            .body
            .clone()
    });
    // Each picture's row is named for where its paragraph or tag starts.
    let row = |starts: &str| -> SharedString {
        let start = body.find(starts).expect("the description holds it");
        format!("image-{start}-0").into()
    };
    let (widget, icon, missing) = (row("![The widget"), row("<img"), row("![A missing"));
    let tracker = row("![A tracker");
    // The comment's image is the description's icon.
    let post = SharedString::from("pull-request-post-0");
    // Whether each `(within, element)` is drawn.
    let drawn = |ui: &mut Harness, wanted: &[(&SharedString, &'static str)]| {
        ui.frame();
        ui.cx
            .update_window(ui.window, |_, window, _| {
                wanted
                    .iter()
                    .all(|(within, id)| window.within((*within).clone()).try_find(*id).is_some())
            })
            .expect("window is open")
    };
    let wanted = [
        (&widget, "picture"),
        (&icon, "picture"),
        // With its address, as a link, to try again.
        (&missing, "picture-link"),
        (&missing, "picture-retry"),
        // On a host rho-ui doesn't fetch from: a link alone.
        (&tracker, "picture-link"),
        (&post, "picture"),
    ];
    ui.poll(
        WAIT,
        "the description's and the comment's images",
        |ui| drawn(ui, &wanted),
        Harness::labels_seen,
    );
    ui.act(|window, _| {
        assert!(window
            .within(tracker.clone())
            .try_find("picture-retry")
            .is_none());
        let column = window.find(widget.clone()).bounds();
        let shown = window.within(widget.clone()).find("picture").bounds();
        assert!(
            shown.size.width < px(1200.)
                && (shown.size.width - column.size.width).abs() < px(1.)
                && (shown.size.width - shown.size.height * 2.).abs() < px(2.),
            "the screenshot is scaled to the surface's measure {column:?}: {shown:?}"
        );
        let shown = window.within(icon.clone()).find("picture").bounds();
        assert!(
            (shown.size.width - px(160.)).abs() < px(1.)
                && (shown.size.height - px(160.)).abs() < px(1.),
            "the upload shows at its tag's width: {shown:?}"
        );
    });
    // Each fetched once.
    let fetched = |path: &str| {
        let requests = pictures.requests();
        requests.iter().filter(|(asked, _)| asked == path).count()
    };
    let paths = ["/widget-1200x600.png", "/icon-320x320.png", "/gone.png"];
    assert_eq!(paths.map(fetched), [1; 3]);
    ui.shot("images");

    ui.act(|window, cx| window.within(widget.clone()).click("picture", cx));
    ui.wait_until("the lightbox", |window, _, _| {
        is_visible(window, "lightbox-picture")
    });
    // The dialog fades in.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.act(|window, _| {
        let shown = window.find("lightbox-picture").bounds().size;
        assert!(
            shown.width > px(600.) && (shown.width - shown.height * 2.).abs() < px(2.),
            "the screenshot, larger: {shown:?}"
        );
    });
    ui.shot("lightbox");
    ui.act(|window, cx| window.press("escape", cx));
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.act(|window, cx| {
        let down = gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-360.)));
        window.scroll("pull-request-body", down, cx);
    });
    ui.shot("missing");
}

/// A row marks its session's PR, failing checks and all, for a session
/// rho-ui lists but doesn't follow, so no event tells of it: listing
/// finds it, and listing again within a minute finds its change. Once
/// its agent watches the PR, the row says so, as the session will wake by
/// itself. A settled session, which no row lists, has its PR listed once
/// it is on screen: its title bar marks the PR with the panel hidden,
/// where a click shows it in the panel.
#[test]
fn a_sidebar_row_marks_its_pr() {
    let daemon = FakeDaemon::start(Script::default());
    let (old, settled) = (SessionKey::local("old"), SessionKey::local("s1"));
    let pictures = serve_pictures();
    daemon.open_pull_request(&old.id, &pictures);
    daemon.open_pull_request(&settled.id, &pictures);
    keep(&daemon, &["old"]);
    let mut ui = Harness::open(
        "pull-request-row",
        daemon.target(),
        Some(daemon),
        Projects::default(),
    );
    let mark = SharedString::from(format!("session-{old}-pr"));
    ui.wait_until("the row's PR", |window, _, _| {
        is_visible(window, mark.clone())
    });
    assert_eq!(
        ui.label(mark.clone()).as_deref(),
        Some("#12, checks failing")
    );
    assert_eq!(
        shows(&ui, &old),
        Vec::<PullRequestRead>::new(),
        "a list found it, not a show"
    );
    ui.shot("mark");

    let lists = ui.fake().pull_request_lists();
    ui.fake().edit_pull_requests(&old.id, |set| {
        branch_pr(set).rollup = PullRequestRollup::Passing;
    });
    advance_until(
        &mut ui,
        Duration::from_secs(60),
        "the checks passing",
        |ui| ui.label(mark.clone()).as_deref() == Some("#12, checks passing"),
    );
    let rollup = ui.cx.update(|cx| {
        let prs = ui.app.read(cx).pull_requests().read(cx);
        prs.mark(&old).map(|mark| mark.pr.rollup)
    });
    assert_eq!(rollup, Some(PullRequestRollup::Passing));
    assert!(ui.fake().pull_request_lists() > lists);

    // Each agent watches its PR, which links it.
    let watch = |set: &mut SessionPullRequests| {
        let pr = branch_pr(set).clone();
        let mut link = LinkedPullRequest::new(
            pr.id.clone(),
            pr.url.clone(),
            PullRequestLinkOrigin::Agent,
            1_767_312_100,
        );
        link.watched = true;
        link.summary = Some(pr);
        set.linked.push(link);
    };
    ui.fake().edit_pull_requests(&old.id, watch);
    ui.fake().edit_pull_requests(&settled.id, watch);
    let status = SharedString::from(format!("session-{old}-watching"));
    advance_until(&mut ui, Duration::from_secs(60), "the row's watch", |ui| {
        ui.label(status.clone()).is_some()
    });
    let (watching, marked) = ui.cx.update(|cx| {
        let prs = ui.app.read(cx).pull_requests().read(cx);
        (
            prs.watching().contains(&old),
            prs.mark(&old).map(|mark| mark.watched),
        )
    });
    assert_eq!((watching, marked), (true, Some(true)));
    ui.shot("watching");

    let marked = |ui: &mut Harness, session: &SessionKey| {
        ui.cx.update(|cx| {
            let prs = ui.app.read(cx).pull_requests().read(cx);
            prs.mark(session).map(|mark| mark.watched)
        })
    };
    assert_eq!(marked(&mut ui, &settled), None, "no row listed it");
    ui.click("toggle-settled");
    let row = SharedString::from(format!("session-{settled}"));
    ui.wait_until("the settled session", |window, _, _| {
        is_visible(window, row.clone())
    });
    ui.act(|window, cx| window.click(row, cx));
    ui.wait_until("the title bar's PR", |window, cx, _| {
        is_visible(window, "title-pr") && !PanelLayout::get(cx)
    });
    assert_eq!(marked(&mut ui, &settled), Some(true));
    ui.shot("title");
    ui.click("title-pr");
    ui.wait_until("the PR in the panel", |window, cx, _| {
        PanelLayout::get(cx) && is_visible(window, "pull-request-title")
    });
    assert_eq!(
        ui.label("pull-request-title").as_deref(),
        Some("Add widgets")
    );
}

/// A PR linked to the session by `origin`, read as open with passing
/// checks, titled `title`, at `host`.
fn linked_pr(
    host: &str,
    number: u32,
    title: &str,
    origin: PullRequestLinkOrigin,
) -> LinkedPullRequest {
    let id = PullRequestId::new(host, "acme", "widgets", number);
    let url = format!("https://{host}/acme/widgets/pull/{number}");
    let mut link = LinkedPullRequest::new(id.clone(), url.clone(), origin, 1_767_312_100);
    link.summary = Some(PullRequestSummary::new(
        id,
        title,
        url,
        PullRequestState::Open,
        PullRequestRollup::Passing,
        "def",
        "2026-11-16T00:00:00Z",
    ));
    link
}

/// What the Pull request tab says, as `#12`.
fn pr_tab_label(ui: &mut Harness) -> Option<String> {
    ui.label("panel-tab-pull-request")
}

/// Whether the Pull request surface shows the PR titled `title`.
fn shows_pr(window: &Window, title: &str) -> bool {
    window
        .try_find("pull-request-title")
        .is_some_and(|shown| shown.label() == Some(title))
}

/// Picks the session's PR whose menu item says `label` from the Pull
/// request surface's menu.
fn pick_pr(ui: &mut Harness, label: &str) {
    ui.click("pull-request-pick");
    click_menu_item(ui, label);
}

/// Links go to the daemon, and the one Pull request tab follows them: what
/// names no PR shows the daemon's refusal; a PR linked by hand shows at
/// once, in that tab; its menu switches between the session's PRs, the
/// agent's too, which never takes the tab from the one shown; Unlink
/// unlinks the one shown and shows the branch's again.
#[test]
fn pull_requests_link_by_hand() {
    let (mut ui, key) = open_pr_surface("pull-request-link", Settings::default());
    let link = |ui: &mut Harness, text: &str| {
        ui.click("pull-request-link");
        ui.act(|window, cx| {
            window.press("secondary-a", cx);
            window.input(text, cx);
            window.press("enter", cx);
        });
    };
    let linked = |ui: &Harness| linked_numbers(ui.fake(), &key);
    link(&mut ui, "widgets");
    ui.wait_until("why that's no PR", |window, _, _| {
        is_visible(window, "pull-request-link-error")
    });
    assert_eq!(linked(&ui), Vec::<u32>::new());
    ui.shot("form");
    ui.act(|window, cx| window.press("secondary-a", cx));

    // The branch's own PR, linked, is still the branch's: there is nothing
    // to switch to.
    link(&mut ui, "acme/widgets#12");
    ui.wait_until("the form closed", |window, _, _| {
        !is_visible(window, "pull-request-link-input")
    });
    assert_eq!(linked(&ui), [12]);
    ui.act(|window, _| assert!(!is_visible(window, "pull-request-pick")));

    link(&mut ui, "#13");
    ui.wait_until("#13 in the same tab", |window, _, _| {
        window
            .try_find("panel-tab-pull-request")
            .is_some_and(|tab| tab.label() == Some("#13"))
    });
    assert_eq!(linked(&ui), [12, 13]);
    let tabs = ui.cx.update(|cx| ui.app.read(cx).panel_tabs(cx));
    assert_eq!(tabs, [Surface::PullRequest], "one tab for every PR");

    // The agent links and watches a PR: it joins the menu, and the tab
    // stays on the PR the user chose.
    let mut agents = linked_pr(
        "github.com",
        14,
        "Agent's widgets",
        PullRequestLinkOrigin::Agent,
    );
    agents.watched = true;
    let id = agents.id.clone();
    ui.fake()
        .edit_pull_requests(&key.id, |set| set.linked.push(agents));
    ui.wait_until("the agent's link", |_, cx, app| {
        let prs = app.pull_requests().read(cx);
        prs.of(&key).is_some_and(|set| set.link(&id).is_some())
    });
    assert_eq!(pr_tab_label(&mut ui).as_deref(), Some("#13"));
    pick_pr(&mut ui, "acme/widgets#14  Agent's widgets");
    ui.wait_until("the agent's PR", |window, _, _| {
        shows_pr(window, "Agent's widgets")
    });
    assert_eq!(pr_tab_label(&mut ui).as_deref(), Some("#14"));
    ui.shot("watched");

    ui.click("pull-request-unlink");
    ui.wait_until("the branch's PR again", |window, _, _| {
        shows_pr(window, "Add widgets")
    });
    assert_eq!(pr_tab_label(&mut ui).as_deref(), Some("#12"));
    assert_eq!(linked(&ui), [12, 13]);
}

/// A session whose branch has no PR but which links some, as an agent's
/// watches leave behind, opens one Pull request tab on the PR linked last,
/// not an empty tab beside a tab per link. The title bar's mark shows its
/// PR in that tab, asking the daemon for that PR alone, not first for the
/// one the tab showed. Closing the tab closes just it, and unlinks
/// nothing.
#[test]
fn a_branch_without_a_pr_shows_the_pr_linked_last() {
    let mut ui = Harness::start_with("pull-request-no-branch-pr", Script::default(), &["s1"]);
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("background");
    ui.wait_for_session("the turn to end", |session| {
        session.state() == &RunState::Idle
    });
    let key = active_key(&mut ui);
    // Its workspace is in no git repository: its branch has no PR.
    ui.fake().edit_pull_requests(&key.id, |set| {
        let agent = PullRequestLinkOrigin::Agent;
        set.linked
            .push(linked_pr("github.com", 13, "Old widgets", agent));
        set.linked
            .push(linked_pr("github.com", 14, "New widgets", agent));
    });

    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.click("open-surface-pull-request");
    ui.wait_until("the PR linked last", |window, _, _| {
        shows_pr(window, "New widgets")
    });
    assert_eq!(pr_tab_label(&mut ui).as_deref(), Some("#14"));
    let tabs = ui.cx.update(|cx| ui.app.read(cx).panel_tabs(cx));
    assert_eq!(tabs, [Surface::PullRequest], "one tab for every PR");
    ui.click("pull-request-pick");
    ui.shot("menu");
    click_menu_item(&mut ui, "acme/widgets#13  Old widgets");
    ui.wait_until("the other PR", |window, _, _| {
        shows_pr(window, "Old widgets")
    });

    // With the panel hidden, the title bar marks the PR linked last. A
    // minute on, the daemon's read of the tab's PR is due again.
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    // The picked PR showed from the set at once; its show, or one before
    // it, may still be on its way to the daemon, which counts it on arrival.
    ui.wait_until("the shows answered", |_, cx, app| {
        !app.pull_requests().read(cx).is_showing()
    });
    ui.cx.advance_clock(Duration::from_secs(60));
    ui.frame();
    let asked = ui.fake().pull_requests_shown(&key.id).len();
    ui.click("title-pr");
    ui.wait_until("the marked PR in the panel", |window, cx, _| {
        PanelLayout::get(cx) && shows_pr(window, "New widgets")
    });
    ui.wait_for_daemon("the marked PR asked for", |fake| {
        fake.pull_requests_shown(&key.id).len() > asked
    });
    let new = PullRequestId::new("github.com", "acme", "widgets", 14);
    assert_eq!(
        ui.fake().pull_requests_shown(&key.id)[asked..],
        [Some(new)],
        "only the marked PR is asked for"
    );

    ui.act(|window, cx| window.hover("panel-tab-pull-request", cx));
    ui.click("close-panel-tab-pull-request");
    ui.wait_until("the tab closed", |window, _, _| {
        !is_visible(window, "panel-tab-pull-request")
    });
    assert_eq!(linked_numbers(ui.fake(), &key), [13, 14]);
}

/// An unlink the daemon refuses shows why on the surface, which stays on
/// the PR: Unlink never fails silently.
#[test]
fn a_refused_unlink_shows_on_its_pr() {
    let (mut ui, key) = open_pr_surface("pull-request-unlink-refused", Settings::default());
    // A link on a host the daemon refuses can't be unlinked.
    let elsewhere = linked_pr(
        "ghe.acme.com",
        15,
        "Widgets elsewhere",
        PullRequestLinkOrigin::User,
    );
    ui.fake()
        .edit_pull_requests(&key.id, |set| set.linked.push(elsewhere));
    ui.wait_until("the menu of PRs", |window, _, _| {
        is_visible(window, "pull-request-pick")
    });
    pick_pr(&mut ui, "ghe.acme.com/acme/widgets#15  Widgets elsewhere");
    ui.wait_until("its PR", |window, _, _| {
        shows_pr(window, "Widgets elsewhere")
    });
    ui.act(|window, _| assert!(!is_visible(window, "pull-request-trouble")));
    ui.click("pull-request-unlink");
    ui.wait_until("the refusal", |window, _, _| {
        is_visible(window, "pull-request-trouble")
    });
    ui.act(|window, _| assert!(shows_pr(window, "Widgets elsewhere")));
    assert_eq!(linked_numbers(ui.fake(), &key), [15]);
}

/// Two sessions on one branch each have a Pull request tab of the same PR;
/// switching between them, either way, leaves the daemon asked about the
/// one on screen and not the other, whatever order the panel tells their
/// surfaces in.
#[test]
fn sessions_on_one_branch_each_watch_their_pr() {
    let (mut ui, first) = open_pr_surface("pull-request-two-sessions", Settings::default());
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("background");
    ui.wait_for_session("the second session's turn to end", |session| {
        session.state() == &RunState::Idle
    });
    let second = active_key(&mut ui);
    assert_ne!(first, second);
    ui.fake().open_pull_request(&second.id, &serve_pictures());
    ui.click("open-surface-pull-request");
    ui.wait_until("the second session's PR", |window, _, _| {
        is_visible(window, "pull-request-check-1")
    });
    for (session, other) in [(&first, &second), (&second, &first), (&first, &second)] {
        let asked = shows(&ui, session).len();
        let row = SharedString::from(format!("session-{session}"));
        ui.act(|window, cx| window.click(row, cx));
        ui.wait_for_daemon("a show for the session on screen", |fake| {
            fake.pull_request_shows(&session.id).len() > asked
        });
        ui.wait_until("its PR on screen", |window, _, _| {
            is_visible(window, "pull-request-check-1")
        });
        assert_eq!(&active_key(&mut ui), session);

        let (asked, hidden) = (shows(&ui, session).len(), shows(&ui, other).len());
        advance_until(&mut ui, Duration::from_secs(30), "the show renewed", |ui| {
            shows(ui, session).len() > asked
        });
        assert_eq!(
            shows(&ui, other).len(),
            hidden,
            "the session off screen is not asked about"
        );
    }
}

/// The PR's conversation: its comments and reviews, its threads on the
/// code, open ones open and resolved ones closed until clicked; and a
/// comment posted through the session machine's gh, after which the daemon
/// is asked to read the PR afresh.
#[test]
#[cfg_attr(windows, ignore = "runs a fake gh written as a POSIX sh script")]
fn a_pull_request_conversation_takes_a_comment() {
    let gh = StubGh::new();
    let (mut ui, key) = open_pr_surface("pull-request-conversation", gh.settings());
    // Once the description's diagram replaces its code.
    ui.wait_until("the approval", |window, _, _| {
        is_visible(window, "pull-request-post-1")
    });
    ui.act(|window, _| {
        assert!(
            is_visible(window, "pull-request-thread-0-0"),
            "an open thread shows"
        );
        assert!(
            !is_visible(window, "pull-request-thread-1-0"),
            "a resolved one waits"
        );
    });
    let to_the_end = |window: &mut Window, cx: &mut App| {
        let down = gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-2000.)));
        window.scroll("pull-request-body", down, cx);
    };
    ui.act(to_the_end);
    ui.click("pull-request-thread-1");
    ui.act(|window, _| assert!(is_visible(window, "pull-request-thread-1-0")));

    ui.act(to_the_end);
    ui.click("pull-request-comment-box");
    ui.act(|window, cx| window.input("Thanks!", cx));
    ui.shot("written");
    let fresh = fresh_shows(ui.fake(), &key);
    ui.click("pull-request-comment");
    ui.wait_until("the comment", |_, _, _| gh.acted().is_some());
    assert_eq!(
        gh.acted().as_deref(),
        Some("pr\ncomment\n12\n--repo\ngithub.com/acme/widgets\n--body\nThanks!\n")
    );
    ui.wait_for_daemon("a fresh show after the comment", |fake| {
        fresh_shows(fake, &key) > fresh
    });

    // Hidden with the keyboard in the comment box, the panel gives it
    // back to the pane, where the shortcut that shows it again works.
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.act(|window, _| assert!(!is_visible(window, "pull-request-body")));
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.wait_until("the panel again", |window, _, _| {
        is_visible(window, "pull-request-body")
    });
}

/// An open PR merges the way its repository and the viewer prefer, once
/// confirmed, through `gh pr merge` without deleting the branch, guarded
/// by the head shown; then the daemon is asked to read it afresh.
#[test]
#[cfg_attr(windows, ignore = "runs a fake gh written as a POSIX sh script")]
fn a_pull_request_merges_once_confirmed() {
    let gh = StubGh::new();
    let (mut ui, key) = open_pr_surface("pull-request-merge", gh.settings());
    ui.shot("open");
    ui.click("pull-request-merge");
    ui.wait_until("the confirmation", |window, _, _| {
        is_visible(window, "pull-request-merge-confirm")
    });
    assert_eq!(gh.acted(), None, "one click merges nothing");
    ui.shot("confirm");
    let fresh = fresh_shows(ui.fake(), &key);
    ui.click("pull-request-merge-confirm");
    ui.wait_until("the merge", |_, _, _| gh.acted().is_some());
    assert_eq!(
        gh.acted().as_deref(),
        Some(
            "pr\nmerge\n12\n--repo\ngithub.com/acme/widgets\n--squash\n--match-head-commit\nabc\n"
        )
    );
    ui.wait_for_daemon("a fresh show after the merge", |fake| {
        fresh_shows(fake, &key) > fresh
    });
}

/// A turn's end has the daemon read the PR on screen afresh, but leaves
/// alone the check of a gh that can't comment, which would run `gh auth
/// status` on its machine after every turn; Refresh checks it again, as
/// the user may have just signed in.
#[test]
#[cfg_attr(windows, ignore = "runs a fake gh written as a POSIX sh script")]
fn a_turn_end_reads_the_pr_afresh_and_refresh_checks_gh() {
    let gh = StubGh::signed_out();
    let (mut ui, key) = open_pr_surface("pull-request-gh-check", gh.settings());
    let checked = |cx: &App, app: &RhoApp| {
        let forges = app.forges().read(cx);
        matches!(
            forges.check(&key.machine, ForgeCli::Gh),
            Some(Check::Done(_))
        )
    };
    ui.wait_until("gh checked", |_, cx, app| checked(cx, app));
    let checks = gh.checks();

    let fresh = fresh_shows(ui.fake(), &key);
    ui.send("background");
    ui.wait_for_daemon("a fresh show after the turn", |fake| {
        fresh_shows(fake, &key) > fresh
    });

    ui.click("pull-request-refresh");
    ui.wait_until("gh checked again", |_, cx, app| {
        gh.checks() > checks && checked(cx, app)
    });
    assert_eq!(gh.checks(), checks + 1, "only Refresh checked gh again");
}

/// A daemon from before pull requests is asked once a connection: its
/// rows have no marks and its panel says to update rho, and it is asked
/// again only once it reconnects.
#[test]
fn an_older_daemon_serves_no_pull_requests() {
    let script = Script {
        pull_requests: PullRequestMethods::Unserved,
        ..Script::default()
    };
    let mut ui = Harness::start("pull-request-unserved", script);
    let local = MachineId::local();
    ui.wait_until("the daemon's refusal", |_, cx, app| {
        app.pull_requests().read(cx).serving(&local) == Some(Serving::Unserved)
    });
    assert_eq!(ui.fake().pull_request_lists(), 1);

    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    ui.click("session-local-old");
    ui.wait_for_session("old", |_| true);
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.click("open-surface-pull-request");
    ui.wait_until("the advice to update rho", |window, _, _| {
        is_visible(window, "pull-request-unserved")
    });
    ui.act(|window, _| assert!(!is_visible(window, "session-local-old-pr")));
    ui.shot("panel");

    ui.cx.advance_clock(Duration::from_mins(3));
    ui.frame();
    let old = SessionKey::local("old");
    assert_eq!(ui.fake().pull_request_lists(), 1, "asked once");
    assert_eq!(
        shows(&ui, &old),
        Vec::<PullRequestRead>::new(),
        "and never to show"
    );

    ui.fake().restart();
    ui.wait_for_daemon("a list on the next connection", |fake| {
        fake.pull_request_lists() == 2
    });
}

/// The Diff surface shows the uncommitted changes where the session on
/// screen works, read with this machine's `git`, and reads them again when
/// the session's turn ends.
#[test]
fn the_diff_surface_follows_the_sessions_changes() {
    let repo = tempfile::tempdir().unwrap();
    let repo = rho_ui_core::platform::canonicalize(repo.path()).unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(&repo)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "x"]);
    // A line longer than the panel, which wraps in the screenshot.
    let long = "two words a line, ".repeat(8);
    std::fs::write(repo.join("a.txt"), format!("one\n{long}\n")).unwrap();

    let daemon = FakeDaemon::start(Script::default());
    keep(&daemon, &["s1"]);
    let mut ui = Harness::open_with(
        "diff",
        daemon.target(),
        Some(daemon),
        Machines {
            workspace: Some(repo.clone()),
            ..Machines::default()
        },
    );
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    // The turn ends with its process running, which saves the session.
    ui.send("background");
    ui.wait_for_session("the session", |_| true);
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.click("open-surface-diff");
    ui.wait_until("the session's change", |window, cx, app| {
        app.diff_paths(cx) == Some(vec!["a.txt".to_owned()]) && is_visible(window, "diff-summary")
    });
    ui.shot("changes");

    std::fs::write(repo.join("new.txt"), "new\n").unwrap();
    ui.send("background");
    ui.wait_until("the file the turn added", |_, cx, app| {
        app.diff_paths(cx) == Some(vec!["a.txt".to_owned(), "new.txt".to_owned()])
    });
}

/// The Files surface lists the session's folder as a tree: a click opens a
/// folder, and a click on a file shows its text. A turn that ends reads the
/// open file again, and Back returns to the tree with its folder still
/// open.
#[test]
fn the_files_surface_opens_the_sessions_files() {
    let repo = tempfile::tempdir().unwrap();
    let repo = rho_ui_core::platform::canonicalize(repo.path()).unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(repo.join("README.md"), "# Hi\n").unwrap();

    let daemon = FakeDaemon::start(Script::default());
    keep(&daemon, &["s1"]);
    let mut ui = Harness::open_with(
        "files",
        daemon.target(),
        Some(daemon),
        Machines {
            workspace: Some(repo.clone()),
            ..Machines::default()
        },
    );
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("background");
    ui.wait_for_session("the session", |_| true);
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.click("open-surface-files");
    ui.wait_until("the folder's files", |window, _, _| {
        is_visible(window, "files-entry-src") && is_visible(window, "files-entry-README.md")
    });
    ui.act(|window, _| {
        assert!(
            !is_visible(window, "files-entry-src/main.rs"),
            "folders start shut"
        );
    });
    ui.click("files-entry-src");
    ui.wait_until("the folder open", |window, _, _| {
        is_visible(window, "files-entry-src/main.rs")
    });
    ui.shot("tree");

    ui.click("files-entry-src/main.rs");
    ui.wait_until("the file's text", |_, cx, app| {
        app.open_file(cx) == Some(("src/main.rs".to_owned(), "fn main() {}\n".to_owned()))
    });
    ui.shot("file");

    let edited = "fn main() {\n    println!(\"hi\");\n}\n";
    std::fs::write(repo.join("src/main.rs"), edited).unwrap();
    // The turn ends with its process running, which saves the session.
    ui.send("background");
    ui.wait_until("the file as the turn left it", |_, cx, app| {
        app.open_file(cx) == Some(("src/main.rs".to_owned(), edited.to_owned()))
    });

    ui.click("files-back");
    ui.act(|window, _| {
        assert!(
            is_visible(window, "files-entry-src/main.rs"),
            "the folder stays open"
        );
    });
}

/// The Terminal surface runs a shell in the session's folder: what the
/// keyboard types reaches it once, what it prints shows, and the app's own
/// shortcuts that the shell uses go to the shell. A drag or a double click
/// selects what the copy key copies and a middle click pastes; an input
/// method's composing stays out of the shell until it commits; and a
/// program that asks for the mouse gets a click as an escape sequence.
#[test]
#[cfg_attr(windows, ignore = "scripts the POSIX sh test shell's output")]
fn the_terminal_surface_runs_a_shell() {
    let mut ui = Harness::with_terminal("terminal");
    ui.act(|window, cx| {
        for key in ["e", "c", "h", "o", "space", "r", "h", "o", "enter"] {
            window.press(key, cx);
        }
        // Readline's back-a-character, not the sidebar.
        window.press("ctrl-b", cx);
    });
    ui.wait_for_terminal("the shell's echo", |lines| {
        lines.iter().any(|line| line == "rho")
    });
    assert_ne!(
        ui.state.get(Key::SidebarLayout).as_deref(),
        Some("collapsed"),
        "Ctrl+B went to the shell"
    );
    ui.shot("echo");

    // Drag across the echo's output, from the left of its first cell to
    // the right of its third.
    let (_, first) = ui.terminal_cell("rho", 0);
    let (_, third) = ui.terminal_cell("rho", 2);
    let from = first.left_center() + gpui_kit::point(px(1.), px(0.));
    let to = third.right_center() - gpui_kit::point(px(1.), px(0.));
    ui.act(|window, cx| window.drag(from, to, cx));
    ui.shot("selected");
    assert_eq!(ui.copy_from_terminal().as_deref(), Some("rho"));
    #[cfg(target_os = "linux")]
    assert_eq!(
        ui.cx
            .update(|cx| cx.read_from_primary())
            .and_then(|item| item.text())
            .as_deref(),
        Some("rho"),
        "a selection goes to the primary selection"
    );
    // A middle click pastes it at the prompt. macOS has no primary
    // selection.
    #[cfg(target_os = "linux")]
    {
        let middle = ClickOptions::new().with_button(MouseButton::Middle);
        ui.click_terminal(first.center(), middle);
        ui.wait_for_terminal("the middle click's paste", |lines| {
            lines.iter().any(|line| line == "$ ^Brho")
        });
    }

    // A double click selects the word under it.
    let (_, echo) = ui.terminal_cell("$ echo rho", 3);
    ui.click_terminal(echo.center(), ClickOptions::new().with_count(2));
    assert_eq!(ui.copy_from_terminal().as_deref(), Some("echo"));

    // A dead key's accent, which the platform composes without an input
    // method, gives way to the letter a keystroke then types.
    ui.input_method(|input, cx| input.compose_text(None, "´", None, cx));
    ui.act(|window, cx| window.press("é", cx));
    #[expect(
        clippy::redundant_closure_for_method_calls,
        reason = "the method path isn't general over TestInput's lifetime"
    )]
    let marked = ui.input_method(|input, cx| input.marked_text_range(cx));
    assert_eq!(marked, None, "the accent is gone");

    // An input method composes, then commits a command that turns on the
    // mouse for the program it runs, `cat`. Ctrl+U first: the line still
    // holds the Ctrl+B, any pasted rho and the é from above.
    ui.act(|window, cx| window.press("ctrl-u", cx));
    ui.input_method(|input, cx| input.compose_text(None, "pr", None, cx));
    ui.shot("composing");
    let command = r"printf '\033[?1000hready\n'; cat -v";
    ui.input_method(|input, cx| input.commit_text(command, cx));
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_for_terminal("the command the input method committed", |lines| {
        lines.iter().any(|line| line == "ready")
    });

    // A click reaches `cat` as a press and a release at its cell, which the
    // terminal echoes as typed: X10's `ESC [ M`, then the button, column
    // and row, each plus 32 and counted from 1.
    let column = 4;
    let (row, target) = ui.terminal_cell("ready", column);
    ui.click_terminal(target.center(), ClickOptions::new());
    let at = |n: usize| char::from(u8::try_from(33 + n).expect("a small grid"));
    let report = format!(
        "^[[M {x}{y}^[[M#{x}{y}",
        x = at(usize::from(column)),
        y = at(row)
    );
    ui.wait_for_terminal("the click's report", |lines| lines.contains(&report));
    ui.shot("mouse");
}

/// A selection dragged past the terminal's top or bottom scrolls and
/// selects on while the pointer rests there.
#[test]
#[cfg_attr(windows, ignore = "scripts the POSIX sh test shell's output")]
fn a_terminal_selection_scrolls_past_its_edges() {
    let mut ui = Harness::with_terminal("terminal-autoscroll");
    ui.input_method(|input, cx| input.commit_text("seq 200", cx));
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_for_terminal("the numbers", |lines| {
        lines.iter().any(|line| line == "200")
    });
    let numbers = |ui: &mut Harness| -> Vec<u32> {
        let lines = ui.terminal_lines();
        lines.iter().filter_map(|line| line.parse().ok()).collect()
    };
    let copied_numbers = |ui: &mut Harness| -> Vec<u32> {
        let copied = ui.copy_from_terminal().expect("the drag selected");
        copied
            .lines()
            .filter_map(|line| line.parse().ok())
            .collect()
    };
    // Presses at the left of `number`'s line, drags to `to`, and holds the
    // button there for ten ticks of the frames a shown terminal draws.
    let drag_and_hold = |ui: &mut Harness, number: u32, to: Pixels| {
        let (_, cell) = ui.terminal_cell(&number.to_string(), 0);
        let from = cell.left_center() + gpui_kit::point(px(1.), px(0.));
        let to = gpui_kit::point(from.x, to);
        ui.act(|window, cx| {
            window.pointer_down(from, MouseButton::Left, cx);
            window.pointer_move(to, Some(MouseButton::Left), cx);
        });
        for _ in 0..10 {
            ui.cx.advance_clock(Duration::from_millis(15));
            ui.frame();
        }
        ui.act(|window, cx| window.pointer_up(to, MouseButton::Left, cx));
    };

    // Up, from the panel's tab bar above the terminal.
    let top = numbers(&mut ui)[0];
    drag_and_hold(&mut ui, top + 5, px(1.));
    ui.shot("scrolled-up");
    let first = copied_numbers(&mut ui)[0];
    assert!(
        first < top,
        "the selection starts at {first}, not above {top}"
    );

    // Down, from the window's bottom edge below the terminal.
    let shown = numbers(&mut ui);
    let bottom = *shown.last().expect("numbers show");
    drag_and_hold(&mut ui, shown[5], px(799.));
    ui.shot("scrolled-down");
    let last = *copied_numbers(&mut ui)
        .last()
        .expect("numbers were selected");
    assert!(
        last > bottom,
        "the selection ends at {last}, not below {bottom}"
    );

    // Hidden mid-drag, the terminal stops scrolling soon after, where a
    // second of ticks would scroll 66 rows.
    let top = numbers(&mut ui)[0];
    let (_, cell) = ui.terminal_cell(&(top + 5).to_string(), 0);
    let above = gpui_kit::point(cell.center().x, px(1.));
    ui.act(|window, cx| {
        window.pointer_down(cell.center(), MouseButton::Left, cx);
        window.pointer_move(above, Some(MouseButton::Left), cx);
        window.press("secondary-alt-b", cx);
    });
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.act(|window, cx| {
        window.pointer_up(above, MouseButton::Left, cx);
        window.press("secondary-alt-b", cx);
    });
    let scrolled = top - numbers(&mut ui)[0];
    assert!(
        scrolled <= 10,
        "the hidden terminal scrolled {scrolled} rows"
    );
}

/// The Terminal surface opens as many tabs as asked, each named for the
/// program its shell runs, and splits into panes from a pane's menu or
/// Ghostty's keys. The keys act on the terminal with the keyboard, or
/// else, with the keyboard elsewhere, the one under the pointer, and on
/// nothing with neither, as when the pointer has left the window.
#[test]
#[cfg_attr(windows, ignore = "scripts the POSIX sh test shell's output")]
fn terminal_tabs_split_and_name_their_programs() {
    const SPLIT_DOWN: &str = key("ctrl-shift-e", "cmd-shift-d");
    const CLOSE: &str = key("ctrl-shift-w", "cmd-w");
    let mut ui = Harness::with_terminal("terminal-tabs");
    // Only Linux names a tab for the program on this machine
    // (`foreground_program`); elsewhere it says Terminal.
    #[cfg(target_os = "linux")]
    {
        ui.wait_for_label("panel-tab-terminal", "sh");
        ui.input_method(|input, cx| input.commit_text("sleep 30", cx));
        ui.act(|window, cx| window.press("enter", cx));
        ui.wait_for_label("panel-tab-terminal", "sleep");
        ui.shot("program");
        ui.act(|window, cx| window.press("ctrl-c", cx));
        ui.wait_for_label("panel-tab-terminal", "sh");
    }

    // The pane's menu splits it; the new pane runs a shell of its own and
    // takes the keyboard.
    ui.act(|window, cx| window.right_click("terminal", cx));
    ui.shot("pane-menu");
    click_label(&mut ui, "Split right");
    assert_eq!(ui.terminal_panes(), 2);
    ui.wait_for_terminal("the new pane's prompt", |lines| {
        lines.iter().any(|line| line.starts_with('$'))
    });
    // Typed, not composed: the input method helper finds the terminal by
    // an ID each pane's has.
    ui.act(|window, cx| {
        window.input("echo right", cx);
        window.press("enter", cx);
    });
    ui.wait_for_terminal("the new pane's echo", |lines| {
        lines.iter().any(|line| line == "right")
    });

    // The keyboard outranks the pointer: over the left pane, Ctrl+Shift+E
    // (Cmd+Shift+D on macOS) splits the right one, where the keyboard is, and the new pane below
    // it takes the keyboard.
    ui.hover_pane(0);
    ui.act(|window, cx| window.press(SPLIT_DOWN, cx));
    assert_eq!(ui.terminal_panes(), 3);
    let pane_bounds = |ui: &mut Harness, pane: usize| {
        ui.cx
            .update_window(ui.window, |_, window, _| {
                window
                    .within(("terminal-pane", pane))
                    .find("terminal")
                    .bounds()
            })
            .expect("window is open")
    };
    let (right, below) = (pane_bounds(&mut ui, 1), pane_bounds(&mut ui, 2));
    assert!(
        below.left() == right.left() && below.top() > right.top(),
        "the new pane, at {below:?}, is below the right one, at {right:?}"
    );
    ui.wait_for_terminal("the bottom pane's prompt", |lines| {
        lines.iter().any(|line| line.starts_with('$'))
    });
    ui.shot("split");

    // With the keyboard in the composer, the pointer chooses: Ctrl+Shift+W
    // (Cmd+W on macOS) over the left pane closes it, and the keyboard stays in the composer.
    ui.focus_composer();
    ui.hover_pane(0);
    ui.act(|window, cx| window.press(CLOSE, cx));
    assert_eq!(ui.terminal_panes(), 2);
    let app = ui.app.clone();
    ui.act(|window, cx| {
        let view = app.read(cx).active_session().expect("a session shows");
        assert!(
            view.read(cx).composer_focus_handle(cx).is_focused(window),
            "the keyboard stays in the composer"
        );
    });

    // Ctrl+Alt+Up (Cmd+Alt+Up on macOS) goes from the bottom pane to the one above.
    ui.act(|window, cx| {
        window
            .within(("terminal-pane", 2_usize))
            .click("terminal", cx);
    });
    ui.act(|window, cx| window.press(key("ctrl-alt-up", "cmd-alt-up"), cx));
    ui.wait_for_terminal("the pane above", |lines| {
        lines.iter().any(|line| line == "right")
    });

    // Ctrl+Shift+T (Cmd+T on macOS) opens a second tab, with a shell of
    // its own; closing its only pane closes the tab.
    ui.act(|window, cx| window.press(key("ctrl-shift-t", "cmd-t"), cx));
    ui.act(|window, _| assert!(is_visible(window, "panel-tab-terminal-2")));
    assert_eq!(ui.terminal_panes(), 1);
    ui.wait_for_terminal("the second tab's prompt", |lines| {
        lines.iter().any(|line| line.starts_with('$'))
    });
    ui.act(|window, cx| window.press(CLOSE, cx));
    ui.act(|window, _| assert!(!is_visible(window, "panel-tab-terminal-2")));
    assert_eq!(ui.terminal_panes(), 2, "the first tab shows again");

    // With neither the pointer nor the keyboard on a terminal, the keys do
    // nothing to it: here the pointer left the window over a pane.
    ui.focus_composer();
    ui.hover_pane(1);
    ui.act(|window, cx| {
        let position = window.mouse_position();
        window.dispatch_event(
            PlatformInput::MouseExited(MouseExitEvent {
                position,
                pressed_button: None,
                modifiers: Modifiers::default(),
            }),
            cx,
        );
        window.press(CLOSE, cx);
    });
    assert_eq!(ui.terminal_panes(), 2);
}

/// A shell that exits closes its pane, as a terminal window closes, even
/// with a job it left in the background holding the pseudo-terminal open,
/// and the keyboard goes on to the pane left; the last pane's takes the
/// tab with it.
#[test]
#[cfg_attr(windows, ignore = "scripts the POSIX sh test shell's output")]
fn a_shell_that_exits_closes_its_pane() {
    let mut ui = Harness::with_terminal("terminal-exit");
    ui.act(|window, cx| window.press(key("ctrl-shift-o", "cmd-d"), cx));
    assert_eq!(ui.terminal_panes(), 2);
    ui.wait_for_terminal("the new pane's prompt", |lines| {
        lines.iter().any(|line| line.starts_with('$'))
    });
    // The job outlasts `WAIT`, so a pane that waited for the end of the
    // output would never close.
    ui.act(|window, cx| {
        window.input("sleep 60 & exit", cx);
        window.press("enter", cx);
    });
    ui.wait_until("the exited pane to close", |_, cx, app| {
        app.terminal_panes(cx) == 1
    });
    let app = ui.app.clone();
    ui.act(|window, cx| {
        let terminal = app.read(cx).terminal(cx).expect("a pane is left");
        assert!(
            gpui_kit::Focusable::focus_handle(terminal.read(cx), cx).is_focused(window),
            "the keyboard is in the pane left"
        );
    });
    ui.shot("exited");
    ui.act(|window, cx| {
        window.input("exit", cx);
        window.press("enter", cx);
    });
    ui.wait_until("the tab to close", |window, _, _| {
        !is_visible(window, "panel-tab-terminal")
    });
}

/// The composer's context ring opens the session's context in the right
/// panel: how full it is, where it compacts, what it left out, and what
/// fills it, a source opening to its parts. The daemon reads it between
/// turns only: the ring asks again during a turn, which keeps the numbers
/// shown, and the turn's end reads them again. Coming back to the session
/// after the app let it go reads it before the session is followed again;
/// when that read's answer, which may say it isn't loaded, comes only
/// after the view caught up, a read after replaces it.
#[test]
fn the_context_ring_opens_what_fills_the_context() {
    let mut ui = Harness::start("context", Script::default());
    new_session_with(&mut ui, "list files");
    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.click("approval-allow-once");
    ui.wait_for_session("the turn to finish", |session| {
        session.state() == &RunState::Idle && session.usage().is_some()
    });
    let key = active_key(&mut ui);
    ui.fake().leave_out_native_context(&key.id);

    ui.click("composer-usage");
    ui.wait_until("the context", |window, cx, app| {
        matches!(app.context_read(cx), Some((None, Some(_), _)))
            && is_visible(window, "context-figure")
    });
    ui.act(|window, cx| {
        assert!(PanelLayout::get(cx), "the panel shows");
        assert!(is_visible(window, "panel-tab-context"));
        // What the fake reports: a threshold, a compaction, and two blocks
        // another model wrote.
        for line in ["context-compacted", "context-left-out"] {
            assert!(is_visible(window, line), "{line} shows");
        }
        assert!(
            !is_visible(window, "context-part-system-prompt-0"),
            "a source's parts show once it opens"
        );
    });
    assert_eq!(
        ui.label("context-fill").as_deref(),
        Some("36k of 200k. Compacts at 160k."),
        "the bar reads as it shows"
    );
    ui.shot("context");

    ui.click("context-source-system-prompt");
    // Past the panel's reveal.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the system prompt's parts", |window, _, _| {
        is_visible(window, "context-part-system-prompt-0")
    });
    ui.shot("open");
    ui.act(|window, cx| window.hover("context-compacted", cx));
    // Past the hover card's delay.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the last compaction's details", |window, _, _| {
        is_visible(window, "context-compacted-details")
    });
    ui.shot("compaction");

    // Asked during a turn, it keeps the numbers it read.
    let read = ui.cx.update(|cx| ui.app.read(cx).context_read(cx));
    let Some((_, Some(before), _)) = read else {
        panic!("the context read")
    };
    ui.send("list files");
    ui.wait_until("the second approval", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.click("composer-usage");
    ui.wait_until("the busy read", |window, cx, app| {
        matches!(app.context_read(cx), Some((Some(ReportError::Busy), Some(tokens), _)) if tokens == before)
            && is_visible(window, "context-figure")
    });

    // The turn's end reads it again, grown by its conversation.
    ui.click("approval-allow-once");
    ui.wait_until(
        "the context after the turn",
        |_, cx, app| matches!(app.context_read(cx), Some((None, Some(after), _)) if after > before),
    );

    // The app lets the session go while another shows, so the daemon can
    // unload it. Back on screen, the panel reads before the session is
    // followed again; that read's answer, that the session isn't loaded,
    // mostly lands before the view caught up, which then reads again.
    let_go_and_come_back(&mut ui, &key, Answer::Prompt);
    // When the answer comes only after the view caught up, as from a busy
    // daemon, the view catching up reads again too.
    let_go_and_come_back(&mut ui, &key, Answer::Late);
}

/// When the daemon answers [`let_go_and_come_back`]'s first read.
#[derive(Clone, Copy)]
enum Answer {
    /// As it reads the report.
    Prompt,
    /// Once the view has caught up with the session.
    Late,
}

/// Shows another session until the app lets `key`'s go, then `key`'s
/// again, and waits for a fresh read of its context to land.
fn let_go_and_come_back(ui: &mut Harness, key: &SessionKey, answer: Answer) {
    let read = ui.cx.update(|cx| ui.app.read(cx).context_read(cx));
    let Some((_, _, landed)) = read else {
        panic!("the context read")
    };
    ui.click("session-local-s1");
    ui.wait_for_daemon("the session let go", |fake| {
        fake.followers(key.id.as_str()) == 0
    });
    if let Answer::Late = answer {
        ui.fake().hold(Answers::Reports);
    }
    let row = SharedString::from(format!("session-{key}"));
    ui.act(|window, cx| window.click(row, cx));
    if let Answer::Late = answer {
        // Held, the first read is under way when the view catches up,
        // whichever reached the daemon first. `Reports::read` asks for
        // three reports at once, so two reads hold six: the one as the
        // session came back, and the one the view catching up sends, once
        // its replay has ended.
        ui.wait_for_daemon("a read once the view caught up", |fake| {
            fake.waiting(Answers::Reports) >= 6
        });
        ui.fake().release(Answers::Reports);
    }
    ui.wait_until("the context read once loaded", |window, cx, app| {
        matches!(app.context_read(cx), Some((None, Some(_), now)) if now > landed)
            && is_visible(window, "context-figure")
    });
}

/// Dragging the right panel's edge resizes it, as far as leaves the pane
/// its room; the width outlives a restart, and a double-click on the edge
/// puts the default back.
#[test]
fn the_right_panel_resizes_from_its_edge() {
    let mut ui = Harness::start("right-panel-resize", Script::default());
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    let panel = |ui: &mut Harness| {
        ui.cx
            .update_window(ui.window, |_, window, _| {
                bounds(window, "right-panel").expect("the panel is drawn")
            })
            .expect("window is open")
    };
    let drag_edge = |ui: &mut Harness, to: Pixels| {
        let edge = panel(ui);
        let from = gpui_kit::point(edge.left() + px(2.), edge.center().y);
        ui.act(|window, cx| window.drag(from, gpui_kit::point(to, from.y), cx));
    };
    let before = panel(&mut ui);
    drag_edge(&mut ui, before.left() - px(40.));
    let wider = panel(&mut ui);
    assert!(
        (wider.size.width - before.size.width - px(40.)).abs() < px(3.),
        "the panel follows the pointer: {before:?} became {wider:?}"
    );

    let mut ui = ui.reopen("right-panel-resize");
    assert_eq!(
        panel(&mut ui).size.width,
        wider.size.width,
        "the width is kept"
    );

    drag_edge(&mut ui, px(0.));
    let widest = panel(&mut ui);
    // The open sidebar is 288px wide, and the pane keeps 480px.
    assert!(
        widest.left() >= px(288. + 479.),
        "the pane keeps its room: {widest:?}"
    );
    ui.shot("widest");
    ui.act(|window, cx| {
        window.click_with_options(
            "right-panel-edge",
            gpui_kit::test::ClickOptions::new().with_count(2),
            cx,
        );
    });
    assert_eq!(
        panel(&mut ui).size.width,
        before.size.width,
        "the default width"
    );
}

/// Dragging the open sidebar's edge resizes it, but no narrower than its
/// minimum; the width outlives a restart, and a double-click on the edge
/// puts the default back.
#[test]
fn the_sidebar_resizes_from_its_edge() {
    let mut ui = Harness::start("sidebar-resize", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    let sidebar = |ui: &mut Harness| {
        ui.cx
            .update_window(ui.window, |_, window, _| {
                bounds(window, "sidebar").expect("the sidebar is drawn")
            })
            .expect("window is open")
    };
    let drag_edge = |ui: &mut Harness, to: Pixels| {
        let edge = sidebar(ui);
        let from = gpui_kit::point(edge.right() - px(2.), edge.center().y);
        ui.act(|window, cx| window.drag(from, gpui_kit::point(to, from.y), cx));
    };
    let before = sidebar(&mut ui);
    drag_edge(&mut ui, before.right() + px(40.));
    let wider = sidebar(&mut ui);
    assert!(
        (wider.size.width - before.size.width - px(40.)).abs() < px(3.),
        "the sidebar follows the pointer: {before:?} became {wider:?}"
    );

    let mut ui = ui.reopen("sidebar-resize");
    ui.wait_until("saved sessions again", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    assert_eq!(
        sidebar(&mut ui).size.width,
        wider.size.width,
        "the width is kept"
    );

    drag_edge(&mut ui, px(0.));
    // 14 rem at the default scale.
    assert_eq!(sidebar(&mut ui).size.width, px(224.), "the narrowest");
    ui.act(|window, cx| window.hover("session-local-s1", cx));
    ui.shot("narrowest");
    ui.act(|window, cx| {
        window.click_with_options(
            "sidebar-edge",
            gpui_kit::test::ClickOptions::new().with_count(2),
            cx,
        );
    });
    assert_eq!(
        sidebar(&mut ui).size.width,
        before.size.width,
        "the default width"
    );
}

/// A subagent's row in the background work opens its session, nested under
/// its parent in the title bar: the subagent's messages as they come, and
/// no composer, as only the subagent writes there. The parent's step in the
/// title bar leads back. Once the run ends, its row in the ended work opens
/// the session as saved, and still does after a restart, when neither the
/// daemon's list nor its replay of the parent holds the run.
#[test]
fn a_subagents_session_opens_nested_under_its_parent() {
    let mut ui = Harness::start("subagent", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("subagent");
    ui.wait_for_session("the turn to end with its subagent running", |session| {
        session.state() == &RunState::Idle && !session.background().is_empty()
    });
    let parent = ui.session().id().clone();

    ui.click("background-0");
    ui.wait_for_session("the subagent's session", |session| {
        session.id().as_str() == "sub-1"
            && session
                .entries()
                .iter()
                .any(|entry| matches!(entry, Entry::Agent(m) if m.text() == "Reading the parser."))
    });
    assert_eq!(ui.label("session-title").as_deref(), Some(SUBAGENT_TASK));
    let runs_as = format!("{SUBAGENT_AGENT} · {SUBAGENT_MODEL}");
    assert_eq!(
        ui.label("subagent-runs-as").as_deref(),
        Some(runs_as.as_str()),
        "the agent and model the subagent runs as, not its parent's"
    );
    ui.act(|window, _| {
        assert!(
            is_visible(window, "title-parent-0"),
            "nested under its parent"
        );
        assert!(
            !is_visible(window, "composer-attach"),
            "only the subagent writes"
        );
        assert!(
            !is_visible(window, "session-local-sub-1"),
            "the sidebar lists only the parent"
        );
    });
    // The running turn's header springs open.
    ui.frame();
    ui.cx.advance_clock(Duration::from_secs(1));
    assert_eq!(
        ui.session().state(),
        &RunState::Running,
        "the subagent's run shows as running"
    );
    ui.act(|window, _| assert!(is_visible(window, "worked-0"), "with its turn's header"));
    ui.shot("running");

    ui.fake().finish_subagent(&parent);
    ui.wait_for_session("the subagent's answer", |session| {
        session.state() == &RunState::Idle
            && matches!(session.entries().last(), Some(Entry::Agent(m)) if m.text() == "Found the parser.")
    });
    let app = ui.app.clone();
    ui.wait_until("the end of the observe", |_, cx, _| {
        app.read(cx)
            .active_session()
            .is_some_and(|view| view.read(cx).is_unloaded())
    });
    ui.shot("ended");

    ui.click("title-parent-0");
    ui.wait_for_session("the parent, told by the daemon's turn", |session| {
        *session.id() == parent && session.state() == &RunState::Running
    });
    ui.act(|window, _| {
        assert!(is_visible(window, "composer-attach"));
        assert!(
            !is_visible(window, "title-parent-0"),
            "the parent is on top"
        );
    });
    ui.click("background-history-button");
    ui.act(|window, _| {
        assert!(
            is_visible(window, "background-ended-0"),
            "the ended run is in the ended work"
        );
    });
    ui.click("background-ended-0");
    ui.wait_for_session("the subagent's session as saved", subagent_saved);
    assert_eq!(
        ui.label("subagent-runs-as"),
        Some(format!("{SUBAGENT_AGENT} · {SUBAGENT_MODEL}"))
    );
    ui.shot("reopened");
    restart_and_open_the_ended_run(ui, &parent);
}

/// The daemon's job list says what a session's background list can't: the
/// Background surface nests a workflow's agent runs under it, a failed one
/// with the daemon's reason. The list above the composer stays one row per
/// piece of work.
#[test]
fn a_workflows_agent_runs_nest_under_it_with_why_one_failed() {
    let mut ui = Harness::start("workflow-runs", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("session-local-s1");
    ui.wait_for_session("s1 attached", |session| {
        session.title() == Some("first") && !session.is_replaying()
    });
    ui.fake().start_workflow(&SessionId::from("s1"));
    ui.wait_until("the workflow", |window, _, _| {
        is_visible(window, "background-0")
    });
    // The next listing of the jobs, on the app's clock.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.click("open-surface-background");
    ui.wait_until("the workflow's agent runs", |window, _, _| {
        is_visible(window, "panel-background-0-run-0")
            && is_visible(window, "panel-background-0-run-1")
    });
    assert_eq!(
        ui.label("panel-background-0-run-1-error").as_deref(),
        Some(WORKFLOW_ERROR),
        "the failed run says why, in the daemon's words"
    );
    ui.act(|window, _| {
        assert!(
            !is_visible(window, "panel-background-0-run-0-error"),
            "the queued run hasn't failed"
        );
        assert!(
            !is_visible(window, "background-0-run-0"),
            "the list above the composer doesn't nest runs"
        );
    });
    ui.shot("nested");
}

/// Whether `session` is the subagent's of
/// [`a_subagents_session_opens_nested_under_its_parent`], as saved once its
/// run ended.
fn subagent_saved(session: &Session) -> bool {
    session.id().as_str() == "sub-1"
        && matches!(session.entries().last(), Some(Entry::Agent(m)) if m.text() == "Found the parser.")
}

/// Ends the daemon's turn in `parent`, whose subagent's run ended, so the
/// daemon drops the run; restarts the app, and opens the run from the
/// parent's ended work.
fn restart_and_open_the_ended_run(mut ui: Harness, parent: &SessionId) {
    ui.click("title-parent-0");
    ui.wait_for_session("the parent", |session| session.id() == parent);
    ui.send("thanks");
    ui.wait_for_session("the daemon's turn to end", |session| {
        session.state() == &RunState::Idle && session.background().is_empty()
    });

    let mut ui = ui.reopen("subagent-restarted");
    let row: &'static str =
        Box::leak(format!("session-local-{}", parent.as_str()).into_boxed_str());
    ui.wait_until("the parent in the sidebar", |window, _, _| {
        is_visible(window, row)
    });
    ui.click(row);
    ui.wait_for_session("the parent's replay", |session| {
        session.id() == parent && !session.is_replaying()
    });
    let ended: Vec<_> = ui
        .session()
        .ended_background()
        .iter()
        .map(|work| (work.kind(), work.status(), work.session()))
        .collect();
    assert_eq!(
        ended,
        [(
            BackgroundKind::Subagent,
            BackgroundStatus::Completed,
            Some(SessionId::from("sub-1"))
        )],
        "the run ended as the app saw it"
    );
    ui.click("background-history-button");
    ui.act(|window, _| {
        assert!(
            is_visible(window, "background-ended-0"),
            "the run is in the ended work"
        );
    });
    ui.shot("restarted");
    ui.click("background-ended-0");
    ui.wait_for_session("the subagent's session after the restart", subagent_saved);
}

/// A message the session's turn can't take waits above the composer and
/// goes as a prompt once the turn ends, with no error: one sent while the
/// turn waits on an approval, and one the daemon refused because it had
/// just begun a turn of its own. A busy refusal with no turn behind it is
/// an error, and the message goes back in the composer.
#[test]
fn a_message_the_turn_cannot_take_waits_for_it_to_end() {
    let mut ui = Harness::start("queued", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("list files");
    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    // The approval took the keyboard; `send` clicks back into the box.
    ui.send("and then this");
    ui.wait_until("the held message", |window, _, _| {
        is_visible(window, "held")
    });
    ui.act(|window, _| assert!(!shows_an_error(window)));
    ui.shot("held");

    ui.click("approval-allow-once");
    ui.wait_until("the held message's own turn", |window, _, _| {
        !is_visible(window, "held") && is_visible(window, "approval-allow-once")
    });
    let prompts = |session: &Session| -> Vec<String> {
        let users = session.entries().iter().filter_map(|entry| match entry {
            Entry::User(message) => Some(message.text().to_owned()),
            _ => None,
        });
        users.collect()
    };
    assert_eq!(prompts(&ui.session()), ["list files", "and then this"]);
    ui.click("approval-allow-once");
    ui.wait_for_session("that turn to end", |session| {
        session.state() == &RunState::Idle
    });

    // The daemon refuses this one: its own turn began a moment before.
    ui.send("busy");
    ui.wait_until(
        "the prompt's turn, once the daemon's ended",
        |window, _, _| is_visible(window, "approval-allow-once"),
    );
    ui.act(|window, _| assert!(!shows_an_error(window)));
    let session = ui.session();
    assert_eq!(prompts(&session), ["list files", "and then this", "busy"]);
    assert!(
        session
            .entries()
            .iter()
            .any(|entry| matches!(entry, Entry::Agent(m) if m.text() == "Back to you.")),
        "the daemon's own turn shows: {:?}",
        session.entries()
    );
    ui.shot("after-busy");
    ui.click("approval-allow-once");
    ui.wait_for_session("that turn to end", |session| {
        session.state() == &RunState::Idle
    });

    ui.send("deleting");
    ui.wait_until("the refusal", |window, _, _| {
        is_visible(window, "session-error")
    });
    let view = ui.app.clone();
    let (typed, held) = ui.cx.update(|cx| {
        let view = view.read(cx).active_session().expect("a session is open");
        let view = view.read(cx);
        (view.composer_text(cx), view.session().held().cloned())
    });
    assert_eq!((typed.as_str(), held), ("deleting", None));
}

const RED: [u8; 4] = [220, 60, 60, 255];
const BLUE: [u8; 4] = [60, 110, 220, 255];
const GREEN: [u8; 4] = [60, 170, 90, 255];

/// A pasted PNG of one colour, `width` × `height`.
fn pasted_png(width: u32, height: u32, rgba: [u8; 4]) -> gpui_kit::Image {
    gpui_kit::Image::from_bytes(
        ImageFormat::Png,
        crate::attachments::png(width, height, rgba),
    )
}

/// The bytes of each image in `attachments`.
fn image_bytes(attachments: &[Attachment]) -> Vec<&[u8]> {
    let images = attachments
        .iter()
        .filter_map(|attachment| match attachment {
            Attachment::Image(image) => Some(image.bytes()),
            Attachment::File(_) => None,
        });
    images.collect()
}

/// The composer's attachments on screen, by number.
fn composer_attachments(window: &Window) -> Vec<u64> {
    (1..=10)
        .filter(|n| {
            window
                .try_find(format!("composer-attachment-{n}"))
                .is_some_and(|tile| tile.visible())
        })
        .collect()
}

/// Ctrl+Shift+V pastes text as Ctrl+V does: dictation apps such as Hex send
/// it when set to paste for terminals.
#[test]
fn ctrl_shift_v_pastes_text() {
    let mut ui = Harness::start("paste-text", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("session-local-s1");
    ui.wait_for_session("its commands", |session| !session.commands().is_empty());
    ui.type_in("");
    ui.act(|window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("list the files".into()));
        window.press("ctrl-shift-v", cx);
    });
    assert_eq!(ui.composer_text(), "list the files");
}

/// Shift+Enter and Alt+Enter start a line in the composer, and send
/// nothing.
#[test]
fn shift_and_alt_enter_start_a_line() {
    let mut ui = Harness::start("newline", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("session-local-s1");
    ui.wait_for_session("its commands", |session| !session.commands().is_empty());
    let sent = ui.session().entries().len();
    ui.type_in("one");
    ui.act(|window, cx| {
        window.press("shift-enter", cx);
        window.input("two", cx);
        window.press("alt-enter", cx);
        window.input("three", cx);
    });
    assert_eq!(ui.composer_text(), "one\ntwo\nthree");
    assert_eq!(ui.session().entries().len(), sent, "nothing is sent");
}

/// Pasted images show as tiles above the composer's text. Each comes off
/// with its remove
/// control, or the last with Backspace in the empty text box; an image rho
/// can't take is refused, saying why.
#[test]
fn pasted_images_show_as_removable_tiles() {
    let mut ui = Harness::start("paste", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));

    // A wide image, a tall one, and a square one.
    ui.paste(pasted_png(320, 180, RED));
    ui.paste(pasted_png(90, 160, BLUE));
    ui.paste(pasted_png(120, 120, GREEN));
    ui.wait_until("three tiles", |window, _, _| {
        composer_attachments(window) == [1, 2, 3]
    });
    ui.act(|window, cx| window.hover("composer-attachment-2", cx));
    ui.shot("pasted");

    // The tall one comes off with its remove control...
    let remove =
        |n: u64| ElementId::from((ElementId::from(("composer-attachment-card", n)), "remove"));
    ui.act(|window, cx| window.click(remove(2), cx));
    ui.wait_until("the tall tile gone", |window, _, _| {
        composer_attachments(window) == [1, 3]
    });
    // Backspace edits text before it removes a tile, even at the start...
    ui.act(|window, cx| {
        window.input("hi", cx);
        window.press("home", cx);
        window.press("backspace", cx);
        window.press("end", cx);
        window.press("backspace", cx);
    });
    ui.act(|window, _| assert_eq!(composer_attachments(window), [1, 3], "no tile went"));
    // ...and in the empty text box, takes the last one.
    ui.act(|window, cx| {
        window.press("backspace", cx);
        window.press("backspace", cx);
    });
    ui.wait_until("the square tile gone", |window, _, _| {
        composer_attachments(window) == [1]
    });
    let held = |ui: &mut Harness| {
        let app = ui.app.clone();
        ui.cx.update(|cx| {
            let draft = app.read(cx).draft().expect("the draft is open");
            draft.read(cx).composer_prompt(cx)
        })
    };
    let prompt = held(&mut ui);
    assert_eq!(prompt.text(), "");
    assert_eq!(
        image_bytes(prompt.attachments()),
        [crate::attachments::png(320, 180, RED)],
        "the wide image is the one left"
    );

    // rho takes no BMP; only the start of one is read.
    ui.paste(gpui_kit::Image::from_bytes(
        ImageFormat::Bmp,
        b"BM\0\0".to_vec(),
    ));
    ui.wait_until("the refusal", |window, _, _| {
        is_visible(window, "composer-refusal")
    });
    ui.act(|window, _| assert_eq!(composer_attachments(window), [1], "the rest stay"));
    assert_eq!(held(&mut ui), prompt);
    ui.shot("refused");
}

/// Pasted images go with the text as one message, and the transcript shows
/// them with it, also when the session is opened afresh. While a turn
/// runs, a message with images waits for the turn to end: a steer carries
/// only text. A failed send puts the images back.
#[test]
fn images_go_with_the_message() {
    let mut ui = Harness::start("images", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.paste(pasted_png(320, 180, RED));
    ui.act(|window, cx| {
        window.input("what is this?", cx);
        window.press("enter", cx);
    });

    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    let session = ui.session();
    let Entry::User(sent) = &session.entries()[0] else {
        panic!("the prompt comes first: {:?}", session.entries());
    };
    assert_eq!(sent.text(), "what is this?");
    assert_eq!(
        image_bytes(sent.attachments()),
        [crate::attachments::png(320, 180, RED)],
        "the daemon echoed the image it got"
    );
    ui.act(|window, _| {
        assert!(
            is_visible(window, "user-0-attachment-0"),
            "the transcript shows it"
        );
        assert!(is_visible(window, "user-0"), "with the text under it");
        assert!(
            composer_attachments(window).is_empty(),
            "the composer is empty"
        );
    });

    // The turn waits on an approval, so this can't steer it: it waits.
    ui.paste(pasted_png(160, 90, BLUE));
    ui.send("and this");
    ui.wait_until("the held message", |window, _, _| {
        is_visible(window, "held")
    });
    ui.act(|window, _| {
        assert_eq!(composer_attachments(window), Vec::<u64>::new());
        assert!(!shows_an_error(window));
    });
    ui.shot("held");

    ui.click("approval-allow-once");
    let images_sent = |session: &Session| -> Vec<(String, usize)> {
        let users = session.entries().iter().filter_map(|entry| match entry {
            Entry::User(message) => Some((message.text().to_owned(), message.attachments().len())),
            _ => None,
        });
        users.collect()
    };
    ui.wait_for_session("the held message's own turn", |session| {
        images_sent(session).len() == 2
    });
    let session = ui.session();
    assert_eq!(
        images_sent(&session),
        [("what is this?".to_owned(), 1), ("and this".to_owned(), 1)]
    );
    ui.click("approval-allow-once");
    ui.wait_for_session("that turn to end", |session| {
        session.state() == &RunState::Idle
    });
    ui.shot("sent");

    // The fake refuses "fail": the message comes back whole.
    ui.paste(pasted_png(120, 120, GREEN));
    ui.send("fail");
    ui.wait_until("the refusal", |window, _, _| shows_an_error(window));
    let app = ui.app.clone();
    let back = ui.cx.update(|cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        view.read(cx).composer_prompt(cx)
    });
    assert_eq!(back.text(), "fail");
    assert_eq!(
        image_bytes(back.attachments()),
        [crate::attachments::png(120, 120, GREEN)]
    );
    ui.act(|window, _| assert_eq!(composer_attachments(window).len(), 1, "its tile is back"));

    // Opened afresh, the session replays its messages with their images.
    let mut ui = ui.reopen("images-replayed");
    ui.wait_until("the session in the sidebar", |window, _, _| {
        is_visible(window, "session-local-new-1")
    });
    ui.click("session-local-new-1");
    ui.wait_until("the newest image", |window, _, _| {
        is_visible(window, "user-3-attachment-0")
    });
    assert_eq!(images_sent(&ui.session()), images_sent(&session));
    ui.shot("replayed");
}

/// Files copied in a file manager and pasted attach as what they are: text
/// whole, an image as one, anything else by its path. They go with the
/// text as one message, and the transcript shows them with it. The attach
/// button opens the desktop's file chooser, which reads files the same way.
#[test]
fn attached_files_go_with_the_message() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).expect("write a file to attach");
        path
    };
    let notes = write("notes.md", b"# Notes\n\nShip it.\n");
    let report = write("report.pdf", b"%PDF-1.7\n\x00\xff\xfe binary");
    let shot = write("shot.png", &crate::attachments::png(64, 36, GREEN));

    let mut ui = Harness::start("files", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.act(|window, cx| {
        let attach = window.find("composer-attach");
        assert!(attach.visible() && attach.disabled() != Some(true));
        // The chooser is the desktop's; headless, it never answers.
        window.click("composer-attach", cx);
    });

    let paths = [notes.clone(), report.clone(), shot];
    ui.paste_entry(ClipboardEntry::ExternalPaths(ExternalPaths(
        paths.into_iter().collect(),
    )));
    ui.wait_until("three attachments", |window, _, _| {
        composer_attachments(window) == [1, 2, 3]
    });
    ui.act(|window, cx| window.hover("composer-attachment-2", cx));
    ui.shot("attached");

    ui.act(|window, cx| {
        window.input("review these", cx);
        window.press("enter", cx);
    });
    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    let session = ui.session();
    let Entry::User(sent) = &session.entries()[0] else {
        panic!("the prompt comes first: {:?}", session.entries());
    };
    assert_eq!(sent.text(), "review these");
    let [Attachment::File(text), Attachment::File(linked), Attachment::Image(_)] =
        sent.attachments()
    else {
        panic!(
            "the files and the image, in order: {:?}",
            sent.attachments()
        );
    };
    assert_eq!(text.text(), Some("# Notes\n\nShip it.\n"));
    assert_eq!(text.path(), Some(notes));
    assert_eq!(linked.text(), None, "a binary file goes by path");
    assert_eq!(linked.path(), Some(report));
    ui.act(|window, _| {
        for n in 0..3 {
            let id = format!("user-0-attachment-{n}");
            assert!(
                window
                    .try_find(id.clone())
                    .is_some_and(|card| card.visible()),
                "{id} shows in the transcript"
            );
        }
        assert_eq!(composer_attachments(window), Vec::<u64>::new());
    });
    ui.shot("sent");
}

/// A running turn takes steers but not images, so a message with images
/// waits for the turn to end, and a message sent after it waits with it
/// instead of reaching the model first as a steer.
#[test]
fn a_message_after_a_waiting_image_waits_too() {
    let script = Script {
        tui_job: TuiJob::Running,
        ..Script::default()
    };
    let mut ui = Harness::start_with("image-order", script, &[]);
    ui.wait_until("the running job", |window, _, _| {
        is_visible(window, "session-local-tui-1")
    });
    ui.click("session-local-tui-1");
    ui.wait_for_session("the turn under way", |session| {
        session.state() == &RunState::Running && !session.entries().is_empty()
    });
    ui.paste(pasted_png(160, 90, BLUE));
    ui.send("look at this");
    ui.wait_until("the held message", |window, _, _| {
        is_visible(window, "held")
    });
    ui.send("then fix it");
    ui.wait_for_session("both held", |session| {
        session.held().is_some_and(|held| {
            held.text() == "look at this\nthen fix it" && held.attachments().len() == 1
        })
    });
    let session = ui.session();
    assert!(
        !session
            .entries()
            .iter()
            .any(|entry| matches!(entry, Entry::Agent(m) if m.text().starts_with("steered"))),
        "nothing steered past the held message: {:?}",
        session.entries()
    );
    ui.shot("held");
}

/// The draft lists the user's projects, not every folder rho has sessions
/// in, the most recently used first. Search filters them, and choosing one
/// opens the draft's session there. The draft's project is checked and the
/// keyboard's row shows only while the search has the keyboard. A
/// session's project in its title bar starts a new session there.
#[test]
fn a_new_session_starts_in_one_of_the_users_projects() {
    let launch_row: &'static str =
        format!("project-local:{}", std::env::temp_dir().display()).leak();
    let daemon = FakeDaemon::start(Script::default());
    keep(&daemon, &["s1", "old"]);
    let mut ui = Harness::open(
        "project",
        daemon.target(),
        Some(daemon),
        // Added in this order; /w/a has the newer session.
        Projects::of(["/w/b", "/w/a"]),
    );
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-local-s1")
    });

    // Nothing on screen: the draft starts in the launch directory, which
    // is no project, so the list checks none and offers only the two.
    ui.click("new-session");
    ui.wait_until("the draft", draft_in(&std::env::temp_dir()));
    ui.wait_until("the project list", |window, _, _| {
        is_visible(window, "project-local:/w/a") && is_visible(window, "project-local:/w/b")
    });
    ui.act(|window, _| {
        let (a, b) = (
            bounds(window, "project-local:/w/a"),
            bounds(window, "project-local:/w/b"),
        );
        assert!(
            a.unwrap().origin.y < b.unwrap().origin.y,
            "/w/a has the newest session, so it comes first"
        );
        assert!(!is_visible(window, launch_row), "rho-ui adds no project");
        assert_eq!(checked(window, "project-local:/w/a"), Some(false));
    });
    ui.cx.update(|cx| {
        let selected = selected_project(cx, ui.app.read(cx));
        assert_eq!(selected, None, "the keyboard is elsewhere");
    });
    ui.shot("list");

    // Hovering a row offers Remove in place of its age, and selects
    // nothing.
    ui.act(|window, cx| window.hover("project-local:/w/b", cx));
    ui.wait_until("Remove on the hovered row", |window, _, _| {
        is_visible(window, "project-remove")
    });
    ui.cx
        .update(|cx| assert_eq!(selected_project(cx, ui.app.read(cx)), None));
    ui.shot("hover");

    // Ctrl+Shift+N in the draft goes to the search, selecting the newest
    // project rather than No project, since the draft is in neither; the
    // search filters.
    ui.act(|window, cx| window.press("secondary-shift-n", cx));
    ui.wait_until("the newest project selected", |_, cx, app| {
        selected_project(cx, app).as_deref() == Some("project-local:/w/a")
    });
    ui.act(|window, cx| window.input("/w/b", cx));
    ui.wait_until("the search to filter", |window, cx, app| {
        !is_visible(window, "project-local:/w/a")
            && is_visible(window, "project-local:/w/b")
            && selected_project(cx, app).as_deref() == Some("project-local:/w/b")
    });
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_until("the draft in /w/b", draft_in(Path::new("/w/b")));
    ui.wait_until("the search to clear", |window, _, _| {
        is_visible(window, "project-local:/w/a")
    });
    // Back in the composer: /w/b is checked, and no row looks chosen
    // besides it.
    ui.act(|window, _| {
        assert_eq!(checked(window, "project-local:/w/b"), Some(true));
        assert_eq!(checked(window, "project-local:/w/a"), Some(false));
    });
    ui.cx
        .update(|cx| assert_eq!(selected_project(cx, ui.app.read(cx)), None));
    ui.shot("moved");

    // The session runs in /w/b.
    ui.send("list files");
    ui.wait_until("the session in /w/b", |_, cx, app| {
        app.active_session()
            .is_some_and(|view| view.read(cx).live().cwd == Path::new("/w/b"))
    });

    // The project in that session's title bar starts a new one there.
    ui.act(|window, cx| window.hover("title-project", cx));
    ui.shot("title-hover");
    ui.click("title-project");
    ui.wait_until(
        "a draft in /w/b from the title bar",
        draft_in(Path::new("/w/b")),
    );

    // Ctrl+Shift+N keeps the draft and puts the search on the checked row.
    ui.act(|window, cx| window.press("secondary-shift-n", cx));
    ui.wait_until("a draft in /w/b", draft_in(Path::new("/w/b")));
    ui.wait_until("the keyboard on /w/b", |_, cx, app| {
        selected_project(cx, app).as_deref() == Some("project-local:/w/b")
    });
}

/// A typed path to a worktree adds its repository's project, not the
/// worktree, and Remove takes a project off the list without picking it.
#[test]
fn a_typed_worktree_adds_its_project_and_remove_takes_one_off() {
    let dir = std::env::temp_dir().join(format!("rho-ui-projects-{}", std::process::id()));
    let (main, tree) = crate::repo::fake_worktree(&dir);
    let main = rho_ui_core::platform::canonicalize(&main).expect("the checkout exists");
    let main_row: &'static str = format!("project-local:{}", main.display()).leak();
    let daemon = FakeDaemon::start(Script::default());
    keep(&daemon, &["s1", "old"]);
    let mut ui = Harness::open(
        "project-add",
        daemon.target(),
        Some(daemon),
        Projects::of(["/w/b", "/w/a"]),
    );
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.act(|window, cx| window.press("secondary-shift-n", cx));
    ui.wait_until("the draft", draft_in(&std::env::temp_dir()));

    // A worktree's path offers its repository's project. Typed a key at a
    // time, the path names nothing until its last key; then the project is
    // offered, selected, and Enter adds it and moves there.
    let typed = tree.join("src").display().to_string();
    let (head, last) = typed.split_at(typed.len() - 1);
    ui.act(|window, cx| window.input(head, cx));
    ui.wait_until("nothing to match the partial path", |window, _, _| {
        !is_visible(window, "project-typed") && !is_visible(window, "project-local:/w/b")
    });
    ui.act(|window, cx| window.input(last, cx));
    ui.wait_until("the worktree's project, selected", |window, cx, app| {
        is_visible(window, "project-typed")
            && selected_project(cx, app).as_deref() == Some("project-typed")
    });
    ui.shot("typed");
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_until("the draft in the checkout", draft_in(&main));
    ui.wait_until("the checkout listed and checked", |window, _, _| {
        checked(window, main_row) == Some(true)
    });
    ui.cx.update(|cx| {
        let roots: Vec<PathBuf> = ui
            .app
            .read(cx)
            .projects(cx)
            .iter()
            .map(|root| root.path.clone())
            .collect();
        assert_eq!(roots, [PathBuf::from("/w/b"), "/w/a".into(), main.clone()]);
    });

    // Remove takes /w/a off the list without picking it.
    ui.act(|window, cx| window.hover("project-local:/w/a", cx));
    ui.wait_until("Remove on /w/a", |window, _, _| {
        is_visible(window, "project-remove")
    });
    ui.click("project-remove");
    ui.wait_until("/w/a gone", |window, _, _| {
        !is_visible(window, "project-local:/w/a")
    });
    ui.wait_until("the draft still in the checkout", draft_in(&main));
    ui.cx.update(|cx| {
        let roots: Vec<PathBuf> = ui
            .app
            .read(cx)
            .projects(cx)
            .iter()
            .map(|root| root.path.clone())
            .collect();
        assert_eq!(roots, [PathBuf::from("/w/b"), main.clone()]);
    });
    ui.shot("removed");

    // A path ending in / browses its folder: a subfolder's row goes into
    // it, where its project and its own subfolders show.
    let wt_row: &'static str = format!("project-next:{}", tree.display()).leak();
    let src_row: &'static str = format!("project-next:{}", tree.join("src").display()).leak();
    ui.act(|window, cx| {
        window.press("secondary-shift-n", cx);
        window.input(&format!("{}/", dir.display()), cx);
    });
    ui.wait_until("the folder's subfolders", |window, _, _| {
        is_visible(window, wt_row)
    });
    ui.click(wt_row);
    ui.wait_until("the worktree's project and subfolder", |window, _, _| {
        is_visible(window, main_row) && is_visible(window, src_row) && !is_visible(window, wt_row)
    });
    ui.shot("browsed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A git URL in the project search offers to clone it: the footer's Add
/// project… turns into Clone, and Enter clones the repository into this
/// machine's clone folder, adds the clone to the projects, and starts the
/// draft there.
#[test]
fn a_typed_git_url_clones_into_the_clone_folder() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let dir = rho_ui_core::platform::canonicalize(dir.path()).expect("it resolves");
    for args in [
        &["init", "-q", "-b", "main", "thing"][..],
        &["-C", "thing", "commit", "-q", "--allow-empty", "-m", "x"],
    ] {
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&dir)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }
    let folder = dir.join("clones");
    let mut settings = Settings::default();
    settings.local_clone_folder = folder.display().to_string();
    let daemon = FakeDaemon::start(Script::default());
    let mut ui = Harness::open_with(
        "clone",
        daemon.target(),
        Some(daemon),
        Machines {
            projects: Projects::of(["/w/a"]),
            settings,
            ..Machines::default()
        },
    );
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.act(|window, cx| window.press("secondary-shift-n", cx));
    ui.wait_until("the draft", draft_in(&std::env::temp_dir()));
    let url = format!("file://{}/thing", dir.display());
    ui.act(|window, cx| window.input(&url, cx));
    ui.wait_until("Clone, selected, in the footer too", |window, cx, app| {
        is_visible(window, "project-clone")
            && !is_visible(window, "project-add")
            && selected_project(cx, app).as_deref() == Some("project-clone-url")
    });
    ui.shot("offered");
    ui.act(|window, cx| window.press("enter", cx));
    let clone = folder.join("thing");
    ui.wait_until("the draft in the clone", draft_in(&clone));
    assert!(clone.join(".git").is_dir(), "git cloned it");
    ui.cx.update(|cx| {
        let projects = ui.app.read(cx).projects(cx);
        assert_eq!(projects, [Place::local("/w/a"), Place::local(&clone)]);
    });
}

/// The folders in this machine's clone folder are projects without being
/// added: the list shows them beside the added ones, with no Remove, as
/// they stay while their folder does, and New session finds a folder made
/// since. Choosing one starts the draft there.
#[test]
fn the_clone_folders_folders_are_projects() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let folder = rho_ui_core::platform::canonicalize(dir.path()).expect("it resolves");
    std::fs::create_dir(folder.join("found")).expect("a project folder");
    let row = |name: &str| -> &'static str {
        format!("project-local:{}", folder.join(name).display()).leak()
    };
    let mut settings = Settings::default();
    settings.local_clone_folder = folder.display().to_string();
    let daemon = FakeDaemon::start(Script::default());
    let mut ui = Harness::open_with(
        "found",
        daemon.target(),
        Some(daemon),
        Machines {
            projects: Projects::of(["/w/a"]),
            settings,
            ..Machines::default()
        },
    );
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.click("new-session");
    ui.wait_until("the added and the found project", |window, _, _| {
        is_visible(window, "project-local:/w/a") && is_visible(window, row("found"))
    });
    ui.act(|window, cx| window.hover("project-local:/w/a", cx));
    ui.wait_until("Remove on the added one", |window, _, _| {
        is_visible(window, "project-remove")
    });
    ui.act(|window, cx| window.hover(row("found"), cx));
    ui.wait_until("no Remove on the found one", |window, _, _| {
        !is_visible(window, "project-remove")
    });
    ui.shot("found");

    std::fs::create_dir(folder.join("later")).expect("another project folder");
    ui.click("new-session");
    ui.wait_until("the folder made since", |window, _, _| {
        is_visible(window, row("later"))
    });
    ui.click(row("later"));
    ui.wait_until("the draft in it", draft_in(&folder.join("later")));
}

/// In a git project, the project list offers a new worktree, made only
/// when the first message goes: switching to New worktree and back makes
/// none and keeps the draft's session, and a folder git doesn't track works
/// in Local without forgetting the choice. The message then starts a session
/// in a new worktree with the draft's settings, its choice for the sessions
/// it starts too, and runs there. New drafts keep the last choice, after a
/// restart too.
#[test]
fn a_new_session_can_work_in_a_new_worktree() {
    let dir = std::env::temp_dir().join(format!("rho-ui-worktree-{}", std::process::id()));
    let (main, _) = crate::repo::fake_worktree(&dir);
    let main_row: &'static str = format!("project-local:{}", main.display()).leak();
    let daemon = FakeDaemon::start(Script::default());
    let mut ui = Harness::open(
        "worktree",
        daemon.target(),
        Some(daemon),
        Projects::of([main.clone(), "/w/a".into()]),
    );
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.click("new-session");
    ui.wait_until("the project", |window, _, _| is_visible(window, main_row));
    ui.click(main_row);
    ui.wait_until(
        "the draft in the project",
        in_checkout(&main, Checkout::Local),
    );
    let opened = draft_session(&mut ui);

    // Choosing New worktree, and back, makes no worktree and keeps the
    // draft's session.
    ui.click("project-checkout");
    click_label(&mut ui, "New worktree");
    ui.wait_until(
        "New worktree chosen",
        in_checkout(&main, Checkout::NewWorktree),
    );
    ui.shot("chosen");
    ui.click("project-checkout");
    click_label(&mut ui, "Local");
    ui.wait_until("Local again", in_checkout(&main, Checkout::Local));
    assert_eq!(draft_session(&mut ui), opened, "the draft kept its session");
    assert_eq!(ui.fake().worktrees(), 0, "switching adds no worktree");

    // /w/a is no git repository, so the draft works in Local there, and
    // back in the project, in a new worktree again.
    ui.click("project-checkout");
    click_label(&mut ui, "New worktree");
    ui.wait_until("New worktree", in_checkout(&main, Checkout::NewWorktree));
    ui.click("project-local:/w/a");
    ui.wait_until(
        "Local in /w/a",
        in_checkout(Path::new("/w/a"), Checkout::Local),
    );
    ui.click(main_row);
    ui.wait_until("back in it", in_checkout(&main, Checkout::NewWorktree));

    // With Plan mode set in the draft, and Supervised for the sessions it
    // starts, the first message starts a session in a new worktree with both.
    let supervised = Some(StartedMode::Chosen(PermissionMode::Supervised));
    ui.click("config-mode");
    click_label(&mut ui, "Plan");
    ui.wait_until("Plan mode in the draft", |_, cx, app| {
        app.draft()
            .and_then(|draft| draft.read(cx).session().and_then(permission_mode))
            == Some(PermissionMode::Plan)
    });
    ui.click("config-panel");
    ui.wait_until("the sessions it starts", |window, _, _| {
        is_visible(window, "config-started-mode")
    });
    ui.click("config-started-mode");
    click_menu_item(&mut ui, "Supervised");
    ui.wait_until("Supervised for the sessions it starts", |_, cx, app| {
        app.draft()
            .and_then(|draft| draft.read(cx).session().and_then(started_mode))
            == supervised
    });
    ui.act(|window, cx| window.press("escape", cx));
    ui.send("list files");
    ui.wait_until("the session in its worktree, in Plan mode", |_, cx, app| {
        app.active_session().is_some_and(|view| {
            let view = view.read(cx);
            let cwd = view.live().cwd;
            let plan = permission_mode(view.session()) == Some(PermissionMode::Plan);
            cwd.parent() == Some(Path::new("/rho/worktrees"))
                && cwd
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("project-"))
                && plan
                && started_mode(view.session()) == supervised
        })
    });
    assert_eq!(ui.fake().worktrees(), 1);
    ui.shot("running");

    // After a restart, a new draft in the project starts in a new worktree.
    let mut ui = ui.reopen_with("worktree-reopened", Projects::of([main.clone()]));
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.click("new-session");
    ui.wait_until("the project", |window, _, _| is_visible(window, main_row));
    ui.click(main_row);
    ui.wait_until("remembered", in_checkout(&main, Checkout::NewWorktree));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Whether the app shows a draft whose session is open in `dir`, to start
/// in `checkout`.
fn in_checkout(dir: &Path, checkout: Checkout) -> impl Fn(&Window, &App, &RhoApp) -> bool {
    let dir = dir.to_path_buf();
    move |_, cx, app| {
        app.draft().is_some_and(|draft| {
            let draft = draft.read(cx);
            draft.cwd().path == dir
                && draft.session_key().is_some()
                && draft.checkout(cx) == checkout
        })
    }
}

/// The draft's open session.
fn draft_session(ui: &mut Harness) -> Option<SessionKey> {
    ui.cx.update(|cx| {
        let draft = ui.app.read(cx).draft().expect("a draft");
        draft.read(cx).session_key()
    })
}

/// Whether the app shows a draft for a session in `dir`.
fn draft_in(dir: &Path) -> impl Fn(&Window, &App, &RhoApp) -> bool {
    let dir = dir.to_path_buf();
    move |_, cx, app| {
        app.draft()
            .is_some_and(|draft| draft.read(cx).cwd().path == dir)
    }
}

/// Whether project row `id` shows the draft's check.
fn checked(window: &Window, id: &'static str) -> Option<bool> {
    window.try_find(id).and_then(|element| element.checked())
}

/// The project row the draft's keyboard selection is on, if any.
fn selected_project(cx: &App, app: &RhoApp) -> Option<String> {
    app.draft()?.read(cx).selected_project(cx)
}

/// Typing `/` at the start of a message, or `$` anywhere in it, opens a
/// menu of the session's commands or skills above the composer. Enter or
/// Tab completes the highlighted name and Down picks the next; Enter on a
/// name typed out sends. Escape closes the menu, and only the next Escape
/// stops the turn.
#[test]
fn the_composer_completes_commands_and_skills() {
    let mut ui = Harness::start("completion", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    // A draft offers its session's names too.
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.type_in("$");
    ui.wait_until("the draft's skills", |window, _, _| {
        is_visible(window, "completion-pstack")
    });

    ui.click("session-local-s1");
    ui.wait_for_session("its commands", |session| !session.commands().is_empty());
    let sent = ui.session().entries().len();

    ui.type_in("/");
    ui.act(|window, _| {
        assert!(is_visible(window, "completion-compact"));
        assert!(is_visible(window, "completion-prompt:review"));
        assert!(
            !is_visible(window, "completion-pstack"),
            "skills wait for a $"
        );
    });
    ui.shot("commands");

    // Enter completes a name typed in part, and sends nothing.
    ui.act(|window, cx| {
        window.input("co", cx);
        window.press("enter", cx);
    });
    assert_eq!(ui.composer_text(), "/compact ");
    ui.act(|window, _| {
        assert!(!is_visible(window, "completion-menu"), "the menu closes");
    });
    assert_eq!(ui.session().entries().len(), sent, "nothing was sent");

    // A skill completes anywhere in the message.
    ui.type_in("keep $p");
    ui.act(|window, _| {
        assert!(is_visible(window, "completion-pstack"));
        assert!(is_visible(window, "completion-prove-it-works"));
    });
    ui.shot("skills");
    ui.act(|window, cx| {
        window.press("down", cx);
        window.press("tab", cx);
    });
    assert_eq!(ui.composer_text(), "/compact keep $prove-it-works ");

    // Escape closes the menu; typing on opens it again.
    ui.type_in("$");
    ui.act(|window, cx| window.press("escape", cx));
    ui.act(|window, _| {
        assert!(!is_visible(window, "completion-menu"), "Escape closes it");
    });
    ui.type_in("ps");
    ui.act(|window, _| {
        assert!(is_visible(window, "completion-pstack"), "typing reopens it");
    });

    // A name typed out leaves Enter to send.
    ui.type_in("tack");
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_for_session("the message, as typed", |session| {
        matches!(
            session.entries().get(sent),
            Some(Entry::User(m)) if m.text() == "/compact keep $prove-it-works $pstack"
        )
    });

    ui.wait_until("the turn's approval", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.type_in("$p");
    ui.act(|window, cx| window.press("escape", cx));
    ui.act(|window, _| {
        assert!(!is_visible(window, "completion-menu"), "Escape closes it");
    });
    assert_eq!(
        ui.session().state(),
        &RunState::NeedsAction,
        "the turn runs on"
    );
    ui.act(|window, cx| window.press("escape", cx));
    ui.wait_for_session("the next Escape to stop the turn", |session| {
        session.state() != &RunState::NeedsAction
    });
}

/// A compaction leaves a divider in the transcript saying how far the
/// context shrank. One in the middle of a turn's work stays out of the
/// working that folds when the turn ends.
#[test]
fn a_compaction_shows_as_a_divider() {
    let mut ui = Harness::start("compaction", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("/compact");
    // Entries: prompt, compaction; the reply says it again, so stays out.
    ui.wait_until("the divider", |window, _, _| {
        is_visible(window, "compaction-1")
    });
    assert_eq!(
        ui.label("compaction-1").as_deref(),
        Some("Context compacted · 263 → 65 messages · 247k → 54k tokens"),
        "COMPACTED's counts, tokens shortened"
    );
    ui.wait_for_session("the turn's end", |session| {
        session.state() == &RunState::Idle
    });
    ui.shot("divider");

    ui.send("long");
    // Entries: prompt, read, compaction, read, answer.
    ui.wait_for_session("the long turn's end", |session| {
        session.state() == &RunState::Idle && session.entries().len() == 7
    });
    assert_eq!(
        ui.session().entries()[4],
        Entry::Compaction(Compaction {
            before: ContextSize {
                messages: COMPACTED.previous_messages,
                tokens: COMPACTED.previous_tokens,
            },
            after: ContextSize {
                messages: COMPACTED.current_messages,
                tokens: COMPACTED.current_tokens,
            },
        })
    );
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the work folded, the divider not", |window, _, _| {
        is_visible(window, "worked-1")
            && !is_visible(window, "step-3")
            && !is_visible(window, "step-5")
            && is_visible(window, "compaction-4")
    });
    ui.shot("folded");
}

/// Two machines list their sessions side by side, though their daemons
/// issue the same session IDs, and each row names its machine. A session
/// opens from the daemon it is on, a new session starts on the machine its
/// project is on, and the filter hides a machine. A project with a
/// checkout on each machine lists once, at the chosen machine's checkout,
/// and choosing the other machine moves the draft to its checkout there.
/// A machine that can't be reached says so with Retry while the others
/// work on.
#[test]
fn machines_list_side_by_side() {
    let local = FakeDaemon::start(Script::default());
    let remote = FakeDaemon::start(Script {
        tui_job: TuiJob::Running,
        ..Script::default()
    });
    let DaemonTarget::Socket(remote_socket) = remote.target() else {
        panic!("a fake daemon listens on a socket");
    };
    let mut workstation = RemoteMachine::new("workstation");
    workstation.label = "Workstation".to_owned();
    let mut settings = Settings::default();
    settings.machines = vec![workstation, RemoteMachine::new("gone")];
    let there = MachineId::remote("workstation");
    keep(&local, &["s1", "old"]);
    keep(&remote, &["s1", "old"]);
    let mut projects = Projects::default();
    projects.add(Place::new(there.clone(), "/srv/app"));
    // The same project, named for its folder, on this machine, and one
    // only this machine holds.
    projects.add(Place::local("/w/app"));
    projects.add(Place::local("/w/b"));
    let mut ui = Harness::open_with(
        "machines",
        local.target(),
        Some(local),
        Machines {
            projects,
            settings,
            remote_sockets: vec![
                ("workstation".to_owned(), remote_socket),
                ("gone".to_owned(), PathBuf::from("/nonexistent/rho.sock")),
            ],
            ..Machines::default()
        },
    );
    ui.wait_until("both machines' sessions, and Retry", |window, _, _| {
        is_visible(window, "session-local-s1")
            && is_visible(window, "session-workstation-s1")
            && is_visible(window, "session-workstation-tui-1")
            && is_visible(window, "retry-gone")
    });
    ui.act(|window, _| {
        assert!(
            is_visible(window, "session-workstation-s1-machine")
                && is_visible(window, "session-local-s1-machine"),
            "each row names its machine"
        );
        assert!(
            !is_visible(window, "session-local-tui-1"),
            "only the workstation runs it"
        );
    });
    ui.shot("listed");

    // The workstation's running session replays from its own daemon.
    ui.click("session-workstation-tui-1");
    ui.wait_for_session("the workstation's turn under way", |session| {
        matches!(session.entries().last(), Some(Entry::Agent(m)) if m.text() == "Reading the parser.")
    });
    ui.cx.update(|cx| {
        let view = ui.app.read(cx).active_session().expect("a session is open");
        assert_eq!(view.read(cx).key().machine, there);
    });

    // A new session from there starts there, and moves to the
    // workstation's project.
    let draft_at = |path| draft_at(there.clone(), path);
    ui.click("new-session");
    ui.wait_until("the draft's session on the workstation", draft_at("/w/a"));
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    assert!(
        remote.new_session_var(home).is_some(),
        "the workstation opened it"
    );

    // rho there can't read a file here, so one that would go by its path
    // is refused, from the draft's start.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let (notes, report) = notes_and_report(dir.path());
    ui.paste_entry(ClipboardEntry::ExternalPaths(ExternalPaths(
        std::iter::once(report.clone()).collect(),
    )));
    ui.wait_until("the refusal, in the first draft", |window, _, _| {
        composer_attachments(window).is_empty() && is_visible(window, "composer-refusal")
    });

    wait_for_the_workstations_projects(&mut ui);
    ui.click("project-workstation:/srv/app");
    ui.wait_until(
        "the draft in the workstation's project",
        draft_at("/srv/app"),
    );
    ui.shot("draft-there");

    // Still refused after the move, while a text file goes whole.
    ui.paste_entry(ClipboardEntry::ExternalPaths(ExternalPaths(
        [notes, report].into_iter().collect(),
    )));
    ui.wait_until("the text file, and the refusal", |window, _, _| {
        composer_attachments(window).len() == 1 && is_visible(window, "composer-refusal")
    });

    choose_this_machine(&mut ui);

    // The filter hides the workstation's sessions.
    ui.click("sidebar-filter");
    click_menu_item(&mut ui, "Workstation");
    ui.wait_until("only this machine's sessions", |window, _, _| {
        is_visible(window, "session-local-s1") && !is_visible(window, "session-workstation-s1")
    });
    ui.shot("filtered");
}

/// Waits for the project list of a draft on the workstation: `app` once,
/// at the workstation's checkout, and /w/b dimmed, as only this machine
/// holds it.
fn wait_for_the_workstations_projects(ui: &mut Harness) {
    ui.wait_until(
        "the workstation's checkout, and /w/b dimmed",
        |window, _, _| {
            is_visible(window, "project-workstation:/srv/app")
                && is_visible(window, "project-elsewhere-local:/w/b")
                && !is_visible(window, "project-local:/w/app")
                && !is_visible(window, "project-elsewhere-local:/w/app")
        },
    );
}

/// Chooses this machine in the project list, which moves the draft from
/// the workstation's checkout of `app` to this machine's. Back on the
/// workstation from /w/b, which it doesn't hold, the draft goes to its
/// home folder, so the footer still names where the session runs.
fn choose_this_machine(ui: &mut Harness) {
    ui.click("project-machine");
    click_menu_item(ui, "local");
    ui.wait_until(
        "the draft in this machine's checkout",
        draft_at(MachineId::local(), "/w/app"),
    );
    ui.act(|window, _| {
        assert_eq!(checked(window, "project-local:/w/app"), Some(true));
        assert!(!is_visible(window, "project-workstation:/srv/app"));
    });

    ui.click("project-local:/w/b");
    ui.wait_until("the draft in /w/b", draft_at(MachineId::local(), "/w/b"));
    ui.click("project-machine");
    click_menu_item(ui, "Workstation");
    let home = ui.cx.update(|cx| {
        let app = ui.app.read(cx);
        app.workspaces_home(&MachineId::remote("workstation"), cx)
    });
    let home = home.expect("the workstation's daemon said its home");
    ui.wait_until("the draft in the workstation's home", move |_, cx, app| {
        app.draft().is_some_and(|draft| {
            let draft = draft.read(cx);
            draft.cwd() == &Place::new(MachineId::remote("workstation"), home.clone())
                && draft.session_key().is_some()
        })
    });
    ui.act(|window, _| assert!(is_visible(window, "project-elsewhere-local:/w/b")));
}

/// Whether the draft is open at `path`, with its session made on `machine`.
fn draft_at(
    machine: MachineId,
    path: &'static str,
) -> impl Fn(&Window, &App, &RhoApp) -> bool + 'static {
    move |_, cx, app| {
        app.draft().is_some_and(|draft| {
            let draft = draft.read(cx);
            draft.cwd().path == Path::new(path)
                && draft
                    .session_key()
                    .is_some_and(|key| key.machine == machine)
        })
    }
}

/// Writes a text file, which a message carries whole, and a PDF, which
/// goes by its path, in `dir`.
fn notes_and_report(dir: &Path) -> (PathBuf, PathBuf) {
    let notes = dir.join("notes.md");
    let report = dir.join("report.pdf");
    std::fs::write(&notes, "# Notes\n").expect("write the notes");
    std::fs::write(&report, b"%PDF-1.7\n\x00\xff binary").expect("write the report");
    (notes, report)
}

/// A machine added on the Machines page connects at once, and its sessions
/// list beside this machine's under its host until it has a label; removing
/// it takes them off the list.
#[test]
fn a_machine_added_in_settings_lists_its_sessions() {
    let local = FakeDaemon::start(Script::default());
    let remote = FakeDaemon::start(Script::default());
    let DaemonTarget::Socket(remote_socket) = remote.target() else {
        panic!("a fake daemon listens on a socket");
    };
    keep(&local, &["s1", "old"]);
    keep(&remote, &["s1"]);
    let mut ui = Harness::open_with(
        "add-machine",
        local.target(),
        Some(local),
        Machines {
            remote_sockets: vec![("box".to_owned(), remote_socket)],
            ..Machines::default()
        },
    );
    ui.wait_until("this machine's sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.act(|window, _| {
        assert!(
            !is_visible(window, "session-local-s1-machine"),
            "with one machine, rows name none"
        );
    });

    open_settings(&mut ui);
    open_settings_page(&mut ui, "Machines");
    ui.wait_until("the Machines page", |window, _, _| {
        is_visible(window, "new-machine-host")
    });
    ui.click("add-machine");
    ui.cx.update(|cx| {
        assert!(
            cx.global::<Settings>().machines.is_empty(),
            "an empty host is refused"
        );
    });
    ui.click("new-machine-host");
    ui.act(|window, cx| window.input("box", cx));
    ui.click("add-machine");
    ui.wait_until("the box's sessions", |window, _, _| {
        is_visible(window, "session-box-s1") && is_visible(window, "remove-machine-box")
    });
    ui.cx.update(|cx| {
        let hosts: Vec<String> = cx
            .global::<Settings>()
            .machines
            .iter()
            .map(|machine| machine.host.clone())
            .collect();
        assert_eq!(hosts, ["box"]);
    });
    ui.act(|window, _| {
        assert!(
            is_visible(window, "session-box-s1-machine"),
            "with two machines, rows name theirs"
        );
    });
    ui.shot("added");

    ui.click("remove-machine-box");
    ui.wait_until("the box's sessions gone", |window, _, _| {
        !is_visible(window, "session-box-s1") && is_visible(window, "session-local-s1")
    });
    ui.cx
        .update(|cx| assert_eq!(cx.global::<Settings>().machines, []));
}

/// Settings → Source control shows the gh this machine runs, with the
/// account it is signed in to. A new Program is saved and checked at once,
/// and a token GitHub refused shows as signed out, with no sign-in to copy
/// when the token comes from a variable.
#[test]
#[cfg_attr(windows, ignore = "runs a fake gh written as a POSIX sh script")]
fn source_control_shows_this_machines_gh() {
    let bin = tempfile::tempdir().unwrap();
    let fake_gh = |name: &str, account: &str| {
        let path = bin.path().join(name);
        let script = format!(
            "#!/bin/sh\ncase $1 in\n  --version) echo 'gh version 2.99.0 (2026-09-01)' ;;\n  \
             auth) echo '{{\"hosts\":{{\"github.com\":[{account}]}}}}' ;;\nesac\n"
        );
        write_program(&path, &script);
        path.display().to_string()
    };
    let signed_in = fake_gh(
        "gh",
        r#"{"state":"success","active":true,"host":"github.com","login":"octocat","tokenSource":"keyring","scopes":"repo, workflow","gitProtocol":"ssh"}"#,
    );
    let refused = fake_gh(
        "gh-refused",
        r#"{"state":"error","error":"non-200 OK status code: 401 Unauthorized","active":true,"host":"github.com","login":"","tokenSource":"GH_TOKEN","gitProtocol":"https"}"#,
    );
    let mut settings = Settings::default();
    settings.local_cli.set(ForgeCli::Gh, signed_in);
    let daemon = FakeDaemon::start(Script::default());
    keep(&daemon, &["s1"]);
    let mut ui = Harness::open_with(
        "source-control",
        daemon.target(),
        Some(daemon),
        Machines {
            settings,
            ..Machines::default()
        },
    );
    open_settings(&mut ui);
    open_settings_page(&mut ui, "Source control");
    let account = "source-control-gh-account-0";
    ui.wait_until("gh's account", |window, _, _| {
        window
            .try_find(account)
            .is_some_and(|element| element.label() == Some("@octocat"))
    });
    assert_eq!(
        ui.label("source-control-gh-status").as_deref(),
        Some("gh 2.99.0")
    );
    ui.shot("signed-in");

    ui.click("setting-gh-program");
    ui.act(|window, cx| {
        window.press("secondary-a", cx);
        window.input(&refused, cx);
        window.press("enter", cx);
    });
    ui.wait_until("the refused token", |window, _, _| {
        window
            .try_find(account)
            .is_some_and(|element| element.label() == Some("Not signed in"))
    });
    ui.act(|window, _| {
        assert!(
            window.try_find("source-control-gh-sign-in-0").is_none(),
            "signing in again can't replace a token from GH_TOKEN, so nothing offers it"
        );
    });
    ui.cx.update(|cx| {
        assert_eq!(
            cx.global::<Settings>()
                .cli_path(&MachineId::local(), ForgeCli::Gh),
            refused,
            "the new Program is saved"
        );
    });
    ui.shot("refused");
}

/// rho's own settings show as a page drawn from its daemon's schema. A
/// switch flipped there changes the daemon's `config.toml`, and a hand
/// edit of the file shows up on the open page.
#[test]
fn rho_settings_write_and_follow_the_daemon() {
    let fake = FakeDaemon::start(Script::default());
    keep(&fake, &["s1", "old"]);
    let mut ui = Harness::open("rho-settings", fake.target(), None, Projects::default());
    open_settings(&mut ui);
    open_settings_page(&mut ui, "Rho");
    let compact = "setting-general.auto_compact";
    let switch_is = |on: bool| {
        move |window: &Window, _: &App, _: &RhoApp| {
            window
                .try_find(compact)
                .is_some_and(|switch| switch.visible() && switch.checked() == Some(on))
        }
    };
    ui.wait_until("Auto-compact on, its default", switch_is(true));
    ui.shot("general");

    ui.click(compact);
    ui.wait_until("the switch off", switch_is(false));
    ui.wait_until("the daemon's file turned it off", |_, _, _| {
        fake.agent_setting("general.auto_compact") == Some(SettingValue::Bool(false))
    });

    fake.edit_agent_setting("general.auto_compact", SettingValue::Bool(true));
    ui.wait_until("the hand edit on the page", switch_is(true));
    ui.shot("edited");
}

/// A model setting picks from the models the daemon lists, and its picker
/// follows the daemon's list when a sign-in or a refresh changes it.
#[test]
fn a_model_setting_picks_from_the_daemons_models() {
    let mut ui = Harness::start("model-setting", Script::default());
    open_settings(&mut ui);
    open_settings_page(&mut ui, "Rho");
    let picker = "setting-general.title_model";
    let shows = |label: &'static str| {
        move |window: &Window, _: &App, _: &RhoApp| {
            window
                .try_find(picker)
                .is_some_and(|picker| picker.visible() && picker.value() == Some(label))
        }
    };
    let fake_model = |model: &str| {
        Some(SettingValue::Model(ModelSelection::Rho {
            provider: "fake".to_owned(),
            model: model.to_owned(),
        }))
    };
    ui.wait_until("the default model picked", shows("Small"));

    // Opens the picker and clicks `model`'s row once it is listed.
    let pick = |ui: &mut Harness, model: &str| {
        ui.click(picker);
        let row = format!("setting-model-fake/{model}");
        ui.wait_until(&format!("{model} listed"), |window, _, _| {
            is_visible(window, row.clone())
        });
        // Past the menu's entrance, for the screenshot.
        ui.cx.advance_clock(Duration::from_secs(1));
        ui.shot(&format!("listed-{model}"));
        ui.act(|window, cx| window.click(row, cx));
    };
    pick(&mut ui, "large");
    ui.wait_until("Large picked", shows("Large"));
    ui.wait_for_daemon("the daemon's file holds it", |fake| {
        fake.agent_setting("general.title_model") == fake_model("large")
    });
    ui.shot("picked");

    // A sign-in elsewhere reaches another model.
    ui.fake()
        .set_models(&[("small", "Small"), ("large", "Large"), ("tiny", "Tiny")]);
    pick(&mut ui, "tiny");
    ui.wait_until("the new model picked", shows("Tiny"));
    ui.wait_for_daemon("the daemon's file holds it", |fake| {
        fake.agent_setting("general.title_model") == fake_model("tiny")
    });
}

/// A new session whose model has no credentials opens anyway, signed
/// out, and offers its sign-in above the composer: a key the provider
/// refuses says why and saves nothing; one it couldn't check signs the
/// daemon in, but the card stays to say so until dismissed.
#[test]
fn a_signed_out_session_signs_in_from_its_card() {
    let mut ui = Harness::start(
        "sign-in-card",
        Script {
            signed_out: true,
            ..Script::default()
        },
    );
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the sign-in card", |window, _, _| {
        is_visible(window, "draft-view") && is_visible(window, "sign-in-card")
    });

    ui.click("sign-in-key");
    ui.act(|window, cx| {
        window.input(REFUSED_KEY, cx);
        window.press("enter", cx);
    });
    ui.wait_until("the refusal", |window, _, _| {
        is_visible(window, "sign-in-note")
    });
    assert!(!ui.fake().signed_in(KEY_AUTH));
    ui.shot("refused");

    ui.click("sign-in-key");
    ui.act(|window, cx| window.input(UNCHECKED_KEY, cx));
    ui.click("sign-in-save");
    ui.wait_for_daemon("the key saved", |fake| fake.signed_in(KEY_AUTH));
    ui.wait_until("the card saying it went unchecked", |window, _, _| {
        is_visible(window, "sign-in-dismiss") && is_visible(window, "sign-in-note")
    });
    ui.shot("unchecked");

    ui.click("sign-in-dismiss");
    ui.wait_until("the card gone", |window, _, _| {
        is_visible(window, "draft-view") && !is_visible(window, "sign-in-card")
    });
}

/// Settings → Providers lists only the profiles signed in. Add provider
/// offers the rest: an account's login starts as it is picked, showing
/// its page until the provider approves it; a profile signed out leaves
/// the list, and one picked but not signed in can be removed again.
#[test]
fn providers_add_a_login_and_sign_out() {
    let mut ui = Harness::start("providers", Script::default());
    open_settings(&mut ui);
    open_settings_page(&mut ui, "Providers");
    ui.wait_until("only the signed-in profile listed", |window, _, _| {
        is_visible(window, "providers-fake-api-key-logout")
            && is_visible(window, "providers-add")
            && !is_visible(window, "providers-fake-login")
    });
    ui.shot("listed");

    ui.click("providers-add");
    click_label(&mut ui, "Fake Pro (login)");
    ui.wait_until("the login's page", |window, _, _| {
        is_visible(window, "providers-fake-login-prompt")
    });
    ui.shot("login");
    ui.fake().approve_login();
    ui.wait_until("signed in", |window, _, _| {
        is_visible(window, "providers-fake-login-logout")
            && !is_visible(window, "providers-fake-login-prompt")
    });
    assert!(ui.fake().signed_in(LOGIN_AUTH));

    ui.click("providers-fake-api-key-logout");
    ui.wait_until("the profile off the list", |window, _, _| {
        !is_visible(window, "providers-fake-api-key")
    });
    assert!(!ui.fake().signed_in(KEY_AUTH));

    ui.click("providers-add");
    ui.wait_until("the menu", |window, _, _| {
        window.try_find_by_label("Fake (API key)").is_some()
    });
    ui.shot("menu");
    click_label(&mut ui, "Fake (API key)");
    ui.wait_until("the key's box", |window, _, _| {
        is_visible(window, "providers-fake-api-key-save")
    });
    ui.shot("adding");
    ui.click("providers-fake-api-key-remove");
    ui.wait_until("the profile off the list again", |window, _, _| {
        !is_visible(window, "providers-fake-api-key") && is_visible(window, "providers-add")
    });
}

/// Settings → Worktrees lists the daemon's session worktrees with what a
/// prune would do with each; Remove prunes the merged one on the daemon,
/// which leaves the list, and keeps the rest.
#[test]
fn worktrees_prune_the_merged_ones() {
    let mut ui = Harness::start("worktrees", Script::default());
    let main = || "main".to_owned();
    let fake = ui.fake();
    fake.add_session_worktree(
        Some("s1"),
        "rho/merged",
        WorktreeVerdict::Merged { base: main() },
    );
    fake.add_session_worktree(None, "rho/dirty", WorktreeVerdict::Dirty);
    fake.add_session_worktree(
        None,
        "rho/unmerged",
        WorktreeVerdict::Unmerged { base: main() },
    );
    open_settings(&mut ui);
    open_settings_page(&mut ui, "Worktrees");
    let row = |name: &str| format!("worktree-/rho/worktrees/rho-{name}");
    ui.wait_until("the worktrees listed", |window, _, _| {
        ["merged", "dirty", "unmerged"]
            .into_iter()
            .all(|name| is_visible(window, row(name)))
    });
    // s1's worktree goes by its session's title; the others by branch.
    assert_eq!(
        ui.label(row("merged")).as_deref(),
        Some("first: Merged into main")
    );
    ui.shot("listed");

    ui.click("worktrees-remove");
    ui.wait_until("the prune done", |window, _, _| {
        !is_visible(window, "worktrees-remove")
            && window.find("worktrees-refresh").disabled() != Some(true)
    });
    assert_eq!(ui.fake().session_worktrees(), ["rho/dirty", "rho/unmerged"]);
    ui.act(|window, _| {
        assert!(!is_visible(window, row("merged")), "the removed one leaves");
        assert!(is_visible(window, row("dirty")), "the kept ones stay");
    });
    ui.shot("pruned");
}
