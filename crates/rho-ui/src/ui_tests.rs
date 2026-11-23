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
    Modifiers, MouseButton, MouseExitEvent, Pixels, PlatformInput, SharedString, Size, Window,
    WindowBounds, WindowOptions,
};
use rho_ui_core::{
    agent_settings::SettingValue,
    fake_daemon::{
        report, subagent_result, write_program, FakeDaemon, Script, TuiJob, KEY_AUTH, LOGIN_AUTH,
        REFUSED_KEY, SUBAGENT_TASK, TUI_SESSION, UNCHECKED_KEY, WORKFLOW_ERROR,
    },
    Attachment, BackgroundKind, BackgroundStatus, Checkout, Choices, Daemon, DaemonTarget, Delay,
    Entry, ModelRef, PermissionMode, PromptKey, ReasoningLevel, RestartCost, RetryReason,
    RhoInstall, RunState, Session, SessionId, SessionOptions, ToolStatus, WorktreeVerdict,
};

use gpui_kit::component::ActiveTheme as _;

use crate::{
    app::{LaunchConfig, RhoApp},
    forge::ForgeCli,
    machines::{MachineId, MachineStatus, Place, Reach, RemoteMachine, SessionKey},
    projects::Projects,
    pull_requests::{BranchId, CheckStatus, Found, Lookup, PrLinks, RepoId, Target},
    rho_install::Offer,
    right_panel::{Surface, TerminalShell, TerminalView, PROGRAM_POLL},
    settings::{Appearance, Settings, SmoothScroll, UiScale},
    shelf::Shelves,
    sidebar::{SavedFilter, SidebarLayout, SidebarState},
    store::{Key, StateDb},
    turn_times::TurnTimes,
    unseen::Unseen,
};

mod live;
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
    use std::os::unix::fs::PermissionsExt as _;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-shell");
    std::fs::create_dir_all(&dir).expect("make the test shell's folder");
    let path = dir.join("sh");
    let script = "#!/bin/sh\n\
        export PS1='$ '\n\
        if [ -n \"$BASH_VERSION\" ]; then exec /bin/sh --noediting; fi\n\
        exec /bin/sh\n";
    std::fs::write(&path, script).expect("write the test shell");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the test shell executable");
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
}

impl Harness {
    /// Opens the app at 1200x800 against a fake daemon holding `script`.
    /// The fake's saved sessions count as started in rho-ui, so they list
    /// as active.
    fn start(name: &'static str, script: Script) -> Self {
        Self::start_with(name, script, Shelves::owning(["s1", "old"]))
    }

    /// Closes the app and opens it again on the same daemon and state
    /// database, as a restart does, with `shelves` as its settled and
    /// snoozed state.
    fn reopen(self, name: &'static str, shelves: Shelves) -> Self {
        self.reopen_with(name, shelves, Projects::default())
    }

    /// [`Self::reopen`] with `projects` as the user's projects.
    fn reopen_with(mut self, name: &'static str, shelves: Shelves, projects: Projects) -> Self {
        let (target, daemon, state) = (self.target.clone(), self.daemon.take(), self.state.clone());
        drop(self);
        let machines = Machines {
            shelves,
            projects,
            ..Machines::default()
        };
        Self::open_on(name, target, daemon, machines, state)
    }

    /// [`Self::start`] with the given settled and snoozed state.
    fn start_with(name: &'static str, script: Script, shelves: Shelves) -> Self {
        let daemon = FakeDaemon::start(script);
        Self::open(
            name,
            daemon.target(),
            Some(daemon),
            shelves,
            Projects::default(),
        )
    }

    /// Opens the app at 1200x800, reaching its daemon as `target` says.
    fn open(
        name: &'static str,
        target: DaemonTarget,
        daemon: Option<FakeDaemon>,
        shelves: Shelves,
        projects: Projects,
    ) -> Self {
        Self::open_with(
            name,
            target,
            daemon,
            Machines {
                shelves,
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
            shelves,
            projects,
            mut settings,
            remote_sockets,
            workspace,
            rho,
        } = machines;
        let rho = rho.map(|rho| RhoInstall::Here { rho });
        // Never the user's gh, which would ask GitHub about their account.
        if settings.local_cli.get(ForgeCli::Gh).is_empty() {
            settings
                .local_cli
                .set(ForgeCli::Gh, "/nonexistent/gh".to_owned());
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
        };
        let sidebar = SidebarState {
            shelves,
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
                    // In-memory marks, projects and layout: tests never
                    // touch the user's state database.
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
        let (target, id, text) = (self.target.clone(), SessionId::from(id), text.to_owned());
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

    /// Whether this machine's session `id` is open in the app.
    fn is_open(&mut self, id: &str) -> bool {
        let (app, key) = (
            self.app.clone(),
            SessionKey::new(MachineId::local(), SessionId::from(id)),
        );
        self.cx.update(|cx| app.read(cx).is_open(&key))
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
    shelves: Shelves,
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

/// Sidebar, new session draft, prompt, approval, tool card, reply, idle.
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

    // Saved once prompted, and listed as rho-ui's own.
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

/// A questionnaire of two questions, one taking the user's own text and
/// one taking several choices, is answered one question at a time.
#[test]
fn a_questionnaire_is_answered_one_question_at_a_time() {
    let mut ui = Harness::start("questions", Script::default());
    new_session_with(&mut ui, "questions");
    ui.wait_until("the first question", |window, _, _| {
        is_visible(window, "question-lang-rust") && is_visible(window, "approval-decline")
    });
    ui.act(|window, _| {
        assert!(
            !is_visible(window, "question-checks-fmt"),
            "one question at a time"
        );
        assert!(!is_visible(window, "input-timeout"), "no timeout was set");
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
    assert_eq!(ui.fake().holds(), 0, "showing the question holds nothing");

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
    assert_eq!(ui.fake().holds(), 1, "a request is held once");
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
        Shelves::default(),
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
/// details on hover.
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

    ui.act(|window, cx| window.hover("session-local-old", cx));
    ui.click("snooze");
    ui.click("snooze-4h");
    ui.wait_until("old on the snoozed list", |window, _, _| {
        !is_visible(window, "session-local-old") && is_visible(window, "toggle-snoozed")
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

/// An opened shelf shows its newest 25 sessions, Show more adds the rest,
/// and folding it goes back to 25.
#[test]
fn a_long_shelf_shows_25_sessions_at_a_time() {
    use gpui_kit::{point, ScrollDelta};
    let script = Script {
        past_sessions: 30,
        ..Script::default()
    };
    // Nobody's: all 32 sessions start settled.
    let mut ui = Harness::start_with("shelf-pages", script, Shelves::default());
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
    ui.shot("pinned");

    ui.act(|window, cx| window.right_click("session-local-s1", cx));
    click_label(&mut ui, "Copy");
    click_label(&mut ui, "Session ID");
    let copied = ui
        .cx
        .update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("s1"));
}

/// Sessions from elsewhere start settled. A draft makes no session until
/// its first message, and leaving it drops it. A session can be deleted.
#[test]
fn drafts_are_free_outside_sessions_start_settled_and_sessions_delete() {
    let mut ui = Harness::start_with("drafts", Script::default(), Shelves::default());
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

    // A draft that gets a message becomes rho-ui's own session.
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
    ui.wait_until("the new session listed as active", |window, _, _| {
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
    let machines = Machines {
        shelves: Shelves::owning(["s1", "old"]),
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
    ui.cx.advance_clock(Duration::from_secs(31));
    ui.wait_until("the restart offer", |window, _, _| {
        is_visible(window, "restart-rho-local")
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

/// A daemon that needs a restart offers one, which asks first: one too old
/// for rho-ui's protocol restarts with `rho daemon restart`, and rho-ui
/// connects once the Rho installed there is new enough, and until then says
/// to update it rather than offer the restart again; one of another build
/// restarts through the protocol, and comes back as rho-ui's own.
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
    ui.cx.advance_clock(Duration::from_secs(31));
    ui.wait_until("the upgrade it waits on", |window, _, _| {
        is_visible(window, "restart-rho-local")
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
/// the session is not rho-ui's. Opening it attaches mid-turn, and the
/// composer steers the turn: a steer waits above the box until the model
/// reads it, and can be taken back until then.
#[test]
fn a_job_started_elsewhere_is_listed_attached_and_steered() {
    let script = Script {
        tui_job: TuiJob::Running,
        ..Script::default()
    };
    let mut ui = Harness::start_with("job", script, Shelves::default());
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
        assert!(
            is_visible(window, "session-local-tui-1"),
            "steered here, so the session is rho-ui's and stays listed"
        );
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
    let mut ui = Harness::start_with("steer-turn", script, Shelves::default());
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
    let shelves = Shelves::owning([TUI_SESSION, "s1", "old"]);
    let mut ui = Harness::start_with("done", script, shelves);
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

/// A session left off screen stays followed while something in it would
/// be lost: a message being written, or background work, which the row
/// waits on rather than showing Done. Once idle it is let go: the daemon
/// sees no follower, the row keeps its Done, and opening it again replays
/// it. A session with nothing under way is let go as soon as it is left.
#[test]
fn a_left_session_is_let_go_once_idle() {
    let mut ui = Harness::start("let-go", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("session-local-s1");
    ui.wait_for_daemon("s1 attached", |daemon| daemon.followers("s1") == 1);
    ui.type_in("half a thought");
    ui.click("session-local-old");
    ui.wait_for_daemon("old attached", |daemon| daemon.followers("old") == 1);
    assert!(ui.is_open("s1"), "s1 holds a message being written");

    ui.click("session-local-s1");
    assert_eq!(ui.composer_text(), "half a thought", "the same view");
    ui.focus_composer();
    ui.act(|window, cx| window.press("secondary-a", cx));
    ui.send("background");
    ui.wait_for_session("the turn ended, its process running", |session| {
        session.state() == &RunState::Idle && !session.background().is_empty()
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.wait_for_daemon("old let go", |daemon| daemon.followers("old") == 0);
    assert!(ui.is_open("s1"), "s1 waits on its process");
    ui.act(|window, _| {
        assert!(is_visible(window, "session-local-old"), "old's row stays");
        assert!(is_visible(window, "session-local-s1-background"));
        assert!(!is_visible(window, "session-local-s1-done"), "not done yet");
    });
    ui.shot("waiting");

    ui.fake().finish_background(&SessionId::from("s1"), 0);
    ui.wait_until("the daemon's turn about the exit", |window, _, _| {
        is_visible(window, "session-local-s1-busy")
    });
    ui.send_from_elsewhere("s1", "thanks", Send::Steer);
    ui.wait_for_label("session-local-s1-done", "Done");
    ui.wait_for_daemon("s1 let go", |daemon| daemon.followers("s1") == 0);
    assert!(!ui.is_open("s1"));
    ui.act(|window, _| {
        assert!(
            is_visible(window, "session-local-s1-done"),
            "Done outlives the view"
        );
    });

    ui.click("session-local-s1");
    ui.wait_for_daemon("s1 attached again", |daemon| {
        daemon.attaches("s1") == 2 && daemon.followers("s1") == 1
    });
    ui.wait_for_session("the conversation replayed", |session| {
        session.entries().len() > 1
    });
    ui.shot("reopened");
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
#[test]
fn a_session_adds_a_directory_from_its_title_bar() {
    let dir = tempfile::tempdir().expect("a directory to add");
    let mut ui = Harness::start("added-dirs", Script::default());
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
    let mut ui = Harness::open_with(
        "pictures",
        daemon.target(),
        Some(daemon),
        Machines {
            shelves: Shelves::owning(["s1", "old"]),
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
    let wake: &'static str = Box::leak(format!("wake-{wake}").into_boxed_str());
    ui.act(|window, _| assert!(is_visible(window, wake), "the wake row shows"));
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

/// The description of [`StubGh`]'s PR, with a diagram.
const PR_BODY: &str = "Adds **widgets**.\n\n```mermaid\nflowchart LR\n  A --> B\n```\n";

/// What [`StubGh`]'s API answers: PR 12 on `feature`, with a diagram in its
/// description, a failed check and a passed one, a comment and an
/// approval, an open thread and a resolved one.
fn stub_answer() -> String {
    let summary = |checks: serde_json::Value| {
        serde_json::json!({
            "number": 12, "title": "Add widgets", "url": "https://github.com/acme/widgets/pull/12",
            "state": "OPEN", "isDraft": false, "isCrossRepository": false,
            "updatedAt": "2026-11-15T00:00:00Z", "headRefOid": "abc",
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": checks } }] },
        })
    };
    let check = |name: &str, conclusion: &str| {
        serde_json::json!({
            "__typename": "CheckRun", "name": name, "status": "COMPLETED",
            "conclusion": conclusion, "detailsUrl": null,
            "checkSuite": { "workflowRun": { "workflow": { "name": "CI" } } },
        })
    };
    // A comment and an approval; an open thread and a resolved one.
    let post = |login: &str, at: &str, body: &str, url: &str| {
        serde_json::json!({
            "author": { "login": login }, "body": body, "createdAt": at, "url": url
        })
    };
    let thread = |id: &str, resolved: bool, path: &str, line: u32, body: &str| {
        let url = format!("https://github.com/acme/widgets/pull/12#{id}");
        serde_json::json!({
            "id": id, "isResolved": resolved, "isOutdated": false, "path": path, "line": line,
            "comments": {
                "totalCount": 1,
                "nodes": [post("octocat", "2026-11-14T23:00:00Z", body, &url)],
            },
        })
    };
    let mut detail = summary(serde_json::json!({
        "state": "FAILURE",
        "contexts": {
            "totalCount": 2,
            "nodes": [check("linux", "SUCCESS"), check("macos", "FAILURE")],
        },
    }));
    let extra = serde_json::json!({
        "body": PR_BODY, "author": { "login": "octocat" },
        "headRefName": "feature", "baseRefName": "main",
        "additions": 10, "deletions": 2, "changedFiles": 3, "reviewDecision": "APPROVED",
        "mergeable": "MERGEABLE", "repository": {
            "mergeCommitAllowed": true, "squashMergeAllowed": true, "rebaseMergeAllowed": false,
            "viewerDefaultMergeMethod": "SQUASH",
        },
        "comments": { "totalCount": 1, "nodes": [post(
            "hubot", "2026-11-14T22:00:00Z",
            "Looks good, one nit on the error path.",
            "https://github.com/acme/widgets/pull/12#c1",
        )] },
        "reviews": { "totalCount": 1, "nodes": [{
            "author": { "login": "monalisa" }, "body": "Ship it.", "state": "APPROVED",
            "submittedAt": "2026-11-14T23:30:00Z",
            "url": "https://github.com/acme/widgets/pull/12#r1",
        }] },
        "reviewThreads": { "totalCount": 2, "nodes": [
            thread("t1", false, "src/lib.rs", 7, "Should this be `pub(crate)`?"),
            thread("t2", true, "src/widgets/render.rs", 42, "Typo."),
        ] },
    });
    if let (Some(detail), serde_json::Value::Object(extra)) = (detail.as_object_mut(), extra) {
        detail.extend(extra);
    }
    let mut linked = summary(serde_json::json!({ "state": "FAILURE" }));
    linked["title"] = serde_json::json!("Linked widgets");
    serde_json::json!({ "data": {
        "rateLimit": { "remaining": 4999, "resetAt": "2026-11-15T01:00:00Z" },
        "r0": {
            "defaultBranchRef": { "name": "main" },
            "b0": { "nodes": [summary(serde_json::json!({ "state": "FAILURE" }))] },
            "p0": detail,
            // The same PR linked by hand, by another title.
            "l0": linked,
        },
    } })
    .to_string()
}

/// A checkout of `acme/widgets` on GitHub, on branch `feature`, and a gh
/// signed in to github.com whose API answers with PR 12 on `feature`, a
/// failed check and a passed one, and counts its queries.
struct StubGh {
    _checkout: tempfile::TempDir,
    repo: PathBuf,
    bin: tempfile::TempDir,
}

impl StubGh {
    fn new() -> Self {
        let checkout = tempfile::tempdir().unwrap();
        let repo = rho_ui_core::platform::canonicalize(checkout.path()).unwrap();
        let git = |args: &[&str]| crate::local::test::git(&repo, args);
        git(&["init", "-q", "-b", "main"]);
        git(&["remote", "add", "origin", "git@github.com:acme/widgets.git"]);
        git(&["commit", "-q", "--allow-empty", "-m", "x"]);
        git(&["checkout", "-q", "-b", "feature"]);

        let bin = tempfile::tempdir().unwrap();
        let queries = bin.path().join("queries");
        let answer = stub_answer();
        std::fs::write(bin.path().join("answer.json"), answer).unwrap();
        let gh = bin.path().join("gh");
        let script = format!(
            "#!/bin/sh\ncase $1 in\n  --version) echo 'gh version 2.99.0 (2026-09-01)' ;;\n  \
             auth) echo '{{\"hosts\":{{\"github.com\":[{{\"state\":\"success\",\"active\":true,\"host\":\"github.com\",\"login\":\"octocat\"}}]}}}}' ;;\n  \
             api) echo x >> '{}'; cat '{}' ;;\n  \
             pr) printf '%s\\n' \"$@\" > '{}' ;;\nesac\n",
            queries.display(),
            bin.path().join("answer.json").display(),
            bin.path().join("comment").display(),
        );
        write_program(&gh, &script);
        StubGh {
            _checkout: checkout,
            repo,
            bin,
        }
    }

    fn program(&self) -> PathBuf {
        self.bin.path().join("gh")
    }

    /// How many API queries it answered.
    fn asked(&self) -> usize {
        std::fs::read_to_string(self.bin.path().join("queries"))
            .map_or(0, |text| text.lines().count())
    }

    /// The arguments of its last `gh pr ...`, one a line.
    fn acted(&self) -> Option<String> {
        std::fs::read_to_string(self.bin.path().join("comment")).ok()
    }

    /// Edits its API's answer.
    fn edit_answer(&self, edit: impl FnOnce(&mut serde_json::Value)) {
        let path = self.bin.path().join("answer.json");
        let mut answer = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        edit(&mut answer);
        std::fs::write(&path, answer.to_string()).unwrap();
    }
}

/// Opens the Pull request surface on `gh`'s checkout once the session's
/// first turn ended, so that end reads nothing, and waits for PR 12 and
/// its checks.
fn open_pr_session(gh: &StubGh, name: &'static str) -> Harness {
    let mut settings = Settings::default();
    settings
        .local_cli
        .set(ForgeCli::Gh, gh.program().display().to_string());
    let daemon = FakeDaemon::start(Script::default());
    let mut ui = Harness::open_with(
        name,
        daemon.target(),
        Some(daemon),
        Machines {
            shelves: Shelves::owning(["s1"]),
            settings,
            workspace: Some(gh.repo.clone()),
            ..Machines::default()
        },
    );
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("background");
    ui.wait_for_session("the turn to end", |session| {
        session.state() == &RunState::Idle
    });
    ui
}

/// Opens the Pull request surface over a session made by
/// [`open_pr_session`].
fn open_pr_surface(gh: &StubGh, name: &'static str) -> Harness {
    let mut ui = open_pr_session(gh, name);
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.click("open-surface-pull-request");
    ui.wait_until("the PR and its checks", |window, _, _| {
        is_visible(window, "pull-request-check-1")
    });
    ui
}

/// The Pull request surface shows the PR of the session's branch, read
/// through gh: one query finds it, one more reads its detail, then one
/// every 30 seconds while it shows, and none while the panel is hidden or
/// GitHub's limit is nearly spent, which it says.
#[test]
#[cfg_attr(windows, ignore = "runs a fake gh written as a POSIX sh script")]
fn the_pull_request_surface_shows_the_branchs_pr() {
    let gh = StubGh::new();
    let asked = || gh.asked();
    let mut ui = open_pr_surface(&gh, "pull-request");
    let branch = BranchId {
        repo: RepoId::from_url("git@github.com:acme/widgets.git").unwrap(),
        branch: "feature".into(),
    };
    let statuses = ui.cx.update(|cx| {
        let prs = ui.app.read(cx).pull_requests().read(cx);
        let Lookup::Found(Found::Pr(summary)) = prs.lookup(&Target::Branch(branch.clone())) else {
            panic!("PR 12");
        };
        let detail = prs.detail(&summary.id).expect("its detail");
        detail
            .checks
            .iter()
            .map(|check| check.status)
            .collect::<Vec<_>>()
    });
    assert_eq!(statuses, [CheckStatus::Failed, CheckStatus::Passed]);
    assert_eq!(
        ui.label("pull-request-title").as_deref(),
        Some("Add widgets")
    );
    assert_eq!(asked(), 2, "one query found the PR, one read its detail");
    // Its description draws its diagram, as a message does.
    let diagram = SharedString::from(format!(
        "diagram-{}",
        PR_BODY.find("```mermaid").expect("a diagram")
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

    // On screen, it reads again every 30 seconds; its checks done, the
    // PR's detail isn't read again.
    ui.cx.advance_clock(Duration::from_secs(30));
    ui.wait_until("the next poll", |_, _, _| asked() == 3);
    // A daemon reconnecting forgets the gh it found; the poll that finds
    // it being checked again runs once it is.
    let forges = ui.cx.update(|cx| ui.app.read(cx).forges().clone());
    ui.cx.update(|cx| {
        forges.update(cx, |forges, cx| forges.forget(&MachineId::local(), cx));
    });
    ui.cx.advance_clock(Duration::from_secs(30));
    ui.wait_until("a poll after gh's check", |_, _, _| asked() == 4);

    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.cx.advance_clock(Duration::from_secs(120));
    ui.frame();
    assert_eq!(asked(), 4, "a hidden panel polls nothing");
    ui.act(|window, cx| window.press("secondary-alt-b", cx));
    ui.wait_until("a read as the panel shows", |_, _, _| asked() == 5);

    // Down to GitHub's last points, polling waits for the limit to reset
    // and says so over the PR it read.
    gh.edit_answer(|answer| {
        answer["data"]["rateLimit"] =
            serde_json::json!({ "remaining": 10, "resetAt": "2099-01-01T00:00:00Z" });
    });
    ui.click("pull-request-refresh");
    ui.wait_until("the pause", |window, _, _| {
        is_visible(window, "pull-request-trouble") && is_visible(window, "pull-request-title")
    });
    assert_eq!(asked(), 6);
    ui.shot("paused");
    ui.cx.advance_clock(Duration::from_secs(120));
    ui.frame();
    assert_eq!(asked(), 6, "a paused poll asks nothing");
}

/// An active session's row marks its PR, failing checks and all, read for
/// the sidebar by one query of summaries, then again every five minutes.
#[test]
#[cfg_attr(windows, ignore = "runs a fake gh written as a POSIX sh script")]
fn a_sidebar_row_marks_its_pr() {
    let gh = StubGh::new();
    let mut ui = open_pr_session(&gh, "pull-request-row");
    let key = ui.cx.update(|cx| {
        let app = ui.app.read(cx);
        app.active_session().expect("a session").read(cx).live().id
    });
    let mark = SharedString::from(format!("session-{key}-pr"));
    ui.wait_until("the row's PR", |window, _, _| {
        is_visible(window, mark.clone())
    });
    assert_eq!(
        ui.label(mark.clone()).as_deref(),
        Some("#12, checks failing")
    );
    assert_eq!(gh.asked(), 1, "one query, without the PR's detail");
    ui.shot("mark");

    ui.cx.advance_clock(Duration::from_secs(300));
    ui.wait_until("the next read", |_, _, _| gh.asked() == 2);
}

/// PRs linked by hand each open a tab beside the branch's, named for its
/// number, and stay linked across a restart's read of the state database;
/// closing a linked PR's tab, or Unlink, unlinks it. What names no PR says
/// so.
#[test]
#[cfg_attr(windows, ignore = "runs a fake gh written as a POSIX sh script")]
fn pull_requests_link_by_hand() {
    let gh = StubGh::new();
    let mut ui = open_pr_surface(&gh, "pull-request-link");
    let key = ui.cx.update(|cx| {
        let app = ui.app.read(cx);
        app.active_session().expect("a session").read(cx).live().id
    });
    let linked = |ui: &mut Harness| {
        ui.cx.update(|cx| {
            let links = cx.global::<PrLinks>().get(&key);
            links.iter().map(|id| id.number).collect::<Vec<_>>()
        })
    };
    let link = |ui: &mut Harness, text: &str| {
        ui.click("pull-request-link");
        ui.act(|window, cx| {
            window.press("secondary-a", cx);
            window.input(text, cx);
            window.press("enter", cx);
        });
    };
    link(&mut ui, "widgets");
    ui.wait_until("why that's no PR", |window, _, _| {
        is_visible(window, "pull-request-link-error")
    });
    ui.shot("form");
    ui.act(|window, cx| window.press("secondary-a", cx));

    link(&mut ui, "acme/widgets#12");
    ui.wait_until("the linked PR in a tab of its own", |window, _, _| {
        is_visible(window, "panel-tab-pull-request-2")
            && window
                .try_find("pull-request-title")
                .is_some_and(|title| title.label() == Some("Linked widgets"))
    });
    link(&mut ui, "acme/widgets#13");
    ui.wait_until("a second linked PR's tab", |window, _, _| {
        is_visible(window, "panel-tab-pull-request-3")
    });
    assert_eq!(ui.label("panel-tab-pull-request").as_deref(), Some("#12"));
    assert_eq!(ui.label("panel-tab-pull-request-2").as_deref(), Some("#12"));
    assert_eq!(ui.label("panel-tab-pull-request-3").as_deref(), Some("#13"));
    assert_eq!(linked(&mut ui), [12, 13]);
    let reloaded = PrLinks::load(&ui.state);
    let reloaded: Vec<u32> = reloaded.get(&key).iter().map(|id| id.number).collect();
    assert_eq!(reloaded, [12, 13], "a restart reads them back in order");
    ui.shot("linked");

    ui.click("panel-tab-pull-request");
    ui.wait_until("the branch's PR, still first", |window, _, _| {
        window
            .try_find("pull-request-title")
            .is_some_and(|title| title.label() == Some("Add widgets"))
    });
    ui.act(|window, cx| window.hover("panel-tab-pull-request-2", cx));
    ui.click("close-panel-tab-pull-request-2");
    ui.act(|window, _| assert!(!is_visible(window, "panel-tab-pull-request-2")));
    assert_eq!(linked(&mut ui), [13], "closing its tab unlinked it");

    ui.click("panel-tab-pull-request-3");
    ui.click("pull-request-unlink");
    ui.wait_until("the branch's PR again", |window, _, _| {
        !is_visible(window, "panel-tab-pull-request-3")
            && window
                .try_find("pull-request-title")
                .is_some_and(|title| title.label() == Some("Add widgets"))
    });
    assert_eq!(linked(&mut ui), Vec::<u32>::new());
}

/// Two sessions on one branch each have a Pull request tab of the same PR;
/// switching between them, either way, leaves the one on screen polling,
/// whatever order the panel tells their surfaces in.
#[test]
#[cfg_attr(windows, ignore = "runs a fake gh written as a POSIX sh script")]
fn sessions_on_one_branch_each_watch_their_pr() {
    let gh = StubGh::new();
    let mut ui = open_pr_surface(&gh, "pull-request-two-sessions");
    let active = |ui: &mut Harness| {
        ui.cx.update(|cx| {
            let app = ui.app.read(cx);
            app.active_session().expect("a session").read(cx).live().id
        })
    };
    let first = active(&mut ui);
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("background");
    ui.wait_for_session("the second session's turn to end", |session| {
        session.state() == &RunState::Idle
    });
    let second = active(&mut ui);
    assert_ne!(first, second);
    ui.click("open-surface-pull-request");
    ui.wait_until("the second session's PR", |window, _, _| {
        is_visible(window, "pull-request-check-1")
    });
    let watching = |ui: &mut Harness| {
        ui.cx.update(|cx| {
            let prs = ui.app.read(cx).pull_requests().read(cx);
            prs.poster(cx).is_ok()
        })
    };
    for session in [&first, &second, &first] {
        let row = SharedString::from(format!("session-{session}"));
        ui.act(|window, cx| window.click(row, cx));
        ui.wait_until("its PR on screen", |window, _, _| {
            is_visible(window, "pull-request-check-1")
        });
        assert_eq!(&active(&mut ui), session);
        assert!(watching(&mut ui), "the PR on screen is watched");
    }
}

/// The PR's conversation: its comments and reviews, its threads on the
/// code, open ones open and resolved ones closed until clicked; and a
/// comment posted through the session machine's gh, after which the PR
/// is read again to show it.
#[test]
#[cfg_attr(windows, ignore = "runs a fake gh written as a POSIX sh script")]
fn a_pull_request_conversation_takes_a_comment() {
    let gh = StubGh::new();
    let mut ui = open_pr_surface(&gh, "pull-request-conversation");
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
    ui.click("pull-request-comment");
    ui.wait_until("the comment", |_, _, _| gh.acted().is_some());
    assert_eq!(
        gh.acted().as_deref(),
        Some("pr\ncomment\n12\n--repo\ngithub.com/acme/widgets\n--body\nThanks!\n")
    );
    ui.wait_until("the PR read again", |_, _, _| gh.asked() == 3);

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
/// confirmed, through `gh pr merge` without deleting the branch; then it is
/// read again.
#[test]
#[cfg_attr(windows, ignore = "runs a fake gh written as a POSIX sh script")]
fn a_pull_request_merges_once_confirmed() {
    let gh = StubGh::new();
    let mut ui = open_pr_surface(&gh, "pull-request-merge");
    ui.shot("open");
    ui.click("pull-request-merge");
    ui.wait_until("the confirmation", |window, _, _| {
        is_visible(window, "pull-request-merge-confirm")
    });
    assert_eq!(gh.acted(), None, "one click merges nothing");
    ui.shot("confirm");
    ui.click("pull-request-merge-confirm");
    ui.wait_until("the merge", |_, _, _| gh.acted().is_some());
    assert_eq!(
        gh.acted().as_deref(),
        Some(
            "pr\nmerge\n12\n--repo\ngithub.com/acme/widgets\n--squash\n--match-head-commit\nabc\n"
        )
    );
    ui.wait_until("the PR read again", |_, _, _| gh.asked() == 3);
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
    let mut ui = Harness::open_with(
        "diff",
        daemon.target(),
        Some(daemon),
        Machines {
            shelves: Shelves::owning(["s1"]),
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
    let mut ui = Harness::open_with(
        "files",
        daemon.target(),
        Some(daemon),
        Machines {
            shelves: Shelves::owning(["s1"]),
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

    let mut ui = ui.reopen("right-panel-resize", Shelves::owning(["s1"]));
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

    let mut ui = ui.reopen("sidebar-resize", Shelves::owning(["s1"]));
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

    let mut ui = ui.reopen(
        "subagent-restarted",
        Shelves::owning(["s1", "old", parent.as_str()]),
    );
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

/// A card's body is Markdown, and a subagent's holds its whole result, so
/// a long one shows its first lines, with a button to show the rest and
/// fold it again.
#[test]
fn a_long_card_shows_its_first_lines_until_opened() {
    let mut ui = Harness::start("card", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("subagent");
    ui.wait_for_session("the subagent running", |session| {
        session.state() == &RunState::Idle && !session.background().is_empty()
    });
    let parent = ui.session().id().clone();
    ui.fake().finish_subagent(&parent);
    ui.wait_for_session("the subagent's result", |session| {
        session
            .entries()
            .iter()
            .any(|entry| matches!(entry, Entry::Notice(_)))
    });
    let card = ui
        .session()
        .entries()
        .iter()
        .position(|entry| matches!(entry, Entry::Notice(n) if n.description() == Some(subagent_result().as_str())))
        .expect("the parent shows the subagent's result");
    let card: &'static str = format!("notice-{card}").leak();
    let toggle: &'static str = format!("{card}-toggle").leak();
    ui.wait_until("the result cut short", |window, _, _| {
        is_visible(window, toggle)
    });
    let height = move |window: &Window| bounds(window, card).map(|card| card.size.height);
    let short = ui
        .cx
        .update_window(ui.window, |_, window, _| height(window))
        .expect("window is open")
        .expect("the card is drawn");
    assert!(short < px(300.), "{short:?} tall, cut short");
    ui.shot("result-short");
    ui.click(toggle);
    ui.wait_until("the whole result, taller", |window, _, _| {
        height(window).is_some_and(|whole| whole > short + px(100.))
    });
    ui.shot("result-whole");
    ui.click(toggle);
    ui.wait_until("the result cut short again", |window, _, _| {
        height(window).is_some_and(|again| again < short + px(100.))
    });
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
    let mut ui = ui.reopen("images-replayed", Shelves::owning(["s1", "old", "new-1"]));
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
    let mut ui = Harness::start_with("image-order", script, Shelves::default());
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
    let mut ui = Harness::open(
        "project",
        daemon.target(),
        Some(daemon),
        Shelves::owning(["s1", "old"]),
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

    // Ctrl+Shift+N in the draft goes to the search, which filters.
    ui.act(|window, cx| {
        window.press("secondary-shift-n", cx);
        window.input("/w/b", cx);
    });
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
    let mut ui = Harness::open(
        "project-add",
        daemon.target(),
        Some(daemon),
        Shelves::owning(["s1", "old"]),
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
        let projects = ui.app.read(cx).projects(cx).to_vec();
        assert_eq!(projects, [Place::local("/w/a"), Place::local(&clone)]);
    });
}

/// In a git project, the project list offers a new worktree, made only
/// when the first message goes: switching to New worktree and back makes
/// none and keeps the draft's session, and a folder git doesn't track works
/// in Local without forgetting the choice. The message then starts a session
/// in a new worktree with the draft's settings, and runs there. New drafts
/// keep the last choice, after a restart too.
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
        Shelves::default(),
        Projects::of([main.clone(), "/w/a".into()]),
    );
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.click("new-session");
    ui.wait_until("the project", |window, _, _| is_visible(window, main_row));
    ui.click(main_row);
    let in_checkout = |dir: &Path, checkout: Checkout| {
        let dir = dir.to_path_buf();
        move |_: &Window, cx: &App, app: &RhoApp| {
            app.draft().is_some_and(|draft| {
                let draft = draft.read(cx);
                draft.cwd().path == dir
                    && draft.session_key().is_some()
                    && draft.checkout(cx) == checkout
            })
        }
    };
    ui.wait_until(
        "the draft in the project",
        in_checkout(&main, Checkout::Local),
    );
    let draft_session = |ui: &mut Harness| {
        ui.cx.update(|cx| {
            let draft = ui.app.read(cx).draft().expect("a draft");
            draft.read(cx).session_key()
        })
    };
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

    // With Plan mode set in the draft, the first message starts a session
    // in a new worktree, in Plan mode.
    ui.click("config-mode");
    click_label(&mut ui, "Plan");
    ui.wait_until("Plan mode in the draft", |_, cx, app| {
        app.draft()
            .and_then(|draft| draft.read(cx).session().and_then(permission_mode))
            == Some(PermissionMode::Plan)
    });
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
        })
    });
    assert_eq!(ui.fake().worktrees(), 1);
    ui.shot("running");

    // After a restart, a new draft in the project starts in a new worktree.
    let mut ui = ui.reopen_with(
        "worktree-reopened",
        Shelves::default(),
        Projects::of([main.clone()]),
    );
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.click("new-session");
    ui.wait_until("the project", |window, _, _| is_visible(window, main_row));
    ui.click(main_row);
    ui.wait_until("remembered", in_checkout(&main, Checkout::NewWorktree));
    let _ = std::fs::remove_dir_all(&dir);
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

/// Clicks the menu item labelled `label`, not a sidebar row's part that
/// shares it, such as a machine's label.
fn click_menu_item(ui: &mut Harness, label: &str) {
    let center = ui
        .cx
        .update_window(ui.window, |_, window, _| {
            window
                .find_all_by_label(label)
                .into_iter()
                .find(|item| !format!("{:?}", item.path()).contains("sessions"))
                .map(|item| item.bounds().center())
        })
        .expect("window is open")
        .unwrap_or_else(|| panic!("no menu item labelled {label:?}"));
    ui.act(|window, cx| {
        window.pointer_down(center, MouseButton::Left, cx);
        window.pointer_up(center, MouseButton::Left, cx);
    });
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
    let mut shelves = Shelves::owning(["s1", "old"]);
    for id in ["s1", "old"] {
        shelves.own(&SessionKey::new(there.clone(), SessionId::from(id)));
    }
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
            shelves,
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
    let mut shelves = Shelves::owning(["s1", "old"]);
    shelves.own(&SessionKey::new(
        MachineId::remote("box"),
        SessionId::from("s1"),
    ));
    let mut ui = Harness::open_with(
        "add-machine",
        local.target(),
        Some(local),
        Machines {
            shelves,
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
    let mut ui = Harness::open_with(
        "source-control",
        daemon.target(),
        Some(daemon),
        Machines {
            shelves: Shelves::owning(["s1"]),
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
    let mut ui = Harness::open(
        "rho-settings",
        fake.target(),
        None,
        Shelves::owning(["s1", "old"]),
        Projects::default(),
    );
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
