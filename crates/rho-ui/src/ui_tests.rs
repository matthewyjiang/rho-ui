//! Headless UI tests: the real app in an offscreen window, driven by
//! simulated clicks and keys, against rho-ui-core's scripted daemon
//! (`fake_daemon`: rho-protocol's real server side over a real socket).
//!
//! Each test saves screenshots to `target/ui-shots/` for a person to review.
//! Rendering runs on Mesa lavapipe, Vulkan in software: see
//! [`SELECT_LAVAPIPE`].

use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::test::{TestEventExt as _, TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    px, size, AnyWindowHandle, App, AppContext as _, Bounds, ClipboardEntry, ClipboardItem,
    ElementId, Entity, ExternalPaths, HeadlessAppContext, ImageFormat, MouseButton, Pixels, Window,
    WindowBounds, WindowOptions,
};
use rho_ui_core::{
    agent_settings::SettingValue,
    fake_daemon::{subagent_result, FakeDaemon, Script, TuiJob, SUBAGENT_TASK, TUI_SESSION},
    Attachment, BackgroundKind, BackgroundStatus, Checkout, ConfigKind, Daemon, DaemonTarget,
    Entry, RunState, Session, SessionId, ToolStatus,
};

use gpui_kit::component::ActiveTheme as _;

use crate::{
    app::{LaunchConfig, RhoApp},
    machines::{MachineId, Place, RemoteMachine, SessionKey},
    projects::Projects,
    settings::{Appearance, Settings, UiScale},
    shelf::Shelves,
    sidebar::{SavedFilter, SidebarLayout, SidebarState},
    store::{Key, StateDb},
    turn_times::TurnTimes,
};

mod live;

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

/// Far longer than the fake daemon takes: it answers at once, and its
/// slowest script waits half a second.
const WAIT: Duration = Duration::from_secs(10);

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
    fn reopen(mut self, name: &'static str, shelves: Shelves) -> Self {
        let (target, daemon, state) = (self.target.clone(), self.daemon.take(), self.state.clone());
        drop(self);
        let machines = Machines {
            shelves,
            projects: Projects::default(),
            settings: Settings::default(),
            remote_sockets: Vec::new(),
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
                settings: Settings::default(),
                remote_sockets: Vec::new(),
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
            settings,
            remote_sockets,
        } = machines;
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(crate::Assets),
            gpui_kit::platform::current_headless_renderer,
        );
        // The daemon's answers arrive on a socket: its I/O thread wakes
        // tasks, which the deterministic scheduler only accepts this way.
        cx.allow_parking();
        cx.update(|cx| crate::init(cx, &state));
        cx.update(|cx| settings.apply(cx));
        // A remote machine reaches its fake daemon, never ssh: a host
        // without one gets a socket that isn't there.
        let launch = LaunchConfig {
            workspace: std::env::temp_dir(),
            daemon: target.clone(),
            remote: Rc::new(move |remote: &RemoteMachine| {
                let socket = remote_sockets
                    .iter()
                    .find(|(host, _)| *host == remote.host)
                    .map_or_else(
                        || PathBuf::from("/nonexistent/rho.sock"),
                        |(_, socket)| socket.clone(),
                    );
                DaemonTarget::Socket(socket)
            }),
        };
        let sidebar = SidebarState {
            shelves,
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
        let deadline = Instant::now() + limit;
        loop {
            self.frame();
            let app = self.app.clone();
            let done = self
                .cx
                .update_window(self.window, |_, window, cx| ready(window, cx, app.read(cx)))
                .expect("window is open");
            if done {
                return;
            }
            if Instant::now() >= deadline {
                let seen = self
                    .cx
                    .update_window(self.window, |_, window, _| labels(window))
                    .expect("window is open");
                let shot = self.shot("timeout");
                panic!(
                    "timed out waiting for {what}; labelled elements: {seen:?}; screenshot: {}",
                    shot.display()
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
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
    /// terminal UI: over a connection of its own, which it then closes.
    fn prompt_from_elsewhere(&mut self, id: &str, text: &str) {
        let (target, id, text) = (self.target.clone(), SessionId::from(id), text.to_owned());
        self.cx.update(|cx| {
            cx.spawn(async move |_| {
                let (daemon, _events) = Daemon::connect(target)
                    .await
                    .expect("a second client connects");
                daemon
                    .prompt(&id, &text.into())
                    .await
                    .expect("the daemon takes the other client's prompt");
            })
            .detach();
        });
    }

    /// The fake daemon the app talks to.
    fn fake(&self) -> &FakeDaemon {
        self.daemon.as_ref().expect("the test runs a fake daemon")
    }

    /// The accessibility label of element `id`, once it is drawn.
    fn label(&mut self, id: &'static str) -> Option<String> {
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

    /// Pastes `image` in the composer with Ctrl+V, as from a screenshot
    /// tool.
    fn paste(&mut self, image: gpui_kit::Image) {
        self.paste_entry(ClipboardEntry::Image(image));
    }

    /// Pastes `entry` in the composer with Ctrl+V. In a session, it first
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
            window.press("ctrl-v", cx);
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
struct Machines {
    shelves: Shelves,
    projects: Projects,
    settings: Settings,
    /// The socket each remote machine reaches, by host.
    remote_sockets: Vec<(String, PathBuf)>,
}

/// Every accessibility label in the last frame, for timeout messages.
fn labels(window: &Window) -> Vec<String> {
    window
        .elements()
        .iter()
        .filter_map(|element| element.label().map(str::to_owned))
        .collect()
}

fn is_visible(window: &Window, id: &'static str) -> bool {
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
        countdown.starts_with("rho answers “Language: Rust” in 4:")
            || countdown.starts_with("rho answers “Language: Rust” in 5:00"),
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
/// text, and copy puts it on the clipboard.
#[test]
fn a_sent_message_can_be_selected_and_copied() {
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
        window.press("ctrl-c", cx);
    });
    ui.shot("selected");
    let copied = ui
        .cx
        .update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("list files"));
}

/// The current value of setting `id` on the active session.
fn setting(session: &Session, id: &str) -> Option<String> {
    session
        .config()
        .iter()
        .find(|option| option.id() == id)
        .map(|option| match option.kind() {
            ConfigKind::Select { current, .. } => current.clone(),
            ConfigKind::Toggle(on) => on.to_string(),
        })
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
        is_visible(window, "model-fake/large") && is_visible(window, "config-thought_level-high")
    });
    ui.shot("panel");
    ui.act(|window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    ui.wait_for_session("the new model", |session| {
        setting(session, "model").as_deref() == Some("fake/large")
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
        setting(session, "model").as_deref() == Some("fake/huge")
    });

    // Reasoning and fast mode change in place; the panel stays open.
    ui.click("config-panel");
    ui.wait_until("the reasoning levels", |window, _, _| {
        is_visible(window, "config-thought_level-high")
    });
    ui.click("config-thought_level-high");
    ui.wait_for_session("high reasoning", |session| {
        setting(session, "thought_level").as_deref() == Some("high")
    });
    ui.wait_until("fast mode, enabled again", |window, _, _| {
        window
            .try_find("config-fast_mode")
            .is_some_and(|switch| switch.visible() && switch.disabled() != Some(true))
    });
    ui.click("config-fast_mode");
    ui.wait_for_session("fast mode", |session| {
        setting(session, "fast_mode").as_deref() == Some("true")
    });
    ui.act(|window, cx| window.press("escape", cx));
    ui.wait_until("the panel to close", |window, _, _| {
        !is_visible(window, "config-thought_level-high")
    });

    // The mode menu opens above its button.
    ui.click("config-mode");
    click_label(&mut ui, "Plan");
    ui.wait_for_session("plan mode", |session| {
        setting(session, "mode").as_deref() == Some("plan")
    });
    ui.shot("changed");
}

/// A socket that takes the app's connection but passes it on to `daemon`
/// only once the returned sender sends or drops, so a test can act while
/// the app connects.
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

/// The gear opens settings in a dialog over the sessions; a new scale
/// reaches the theme; Escape closes.
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

    ui.act(|window, cx| window.press("escape", cx));
    // Past the dialog's exit transition.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("settings closed", |window, _, _| {
        !is_visible(window, "setting-ui-font")
    });
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
    ui.prompt_from_elsewhere("old", "list files");
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
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    ui.click("session-local-old");
    ui.wait_for_session("the replayed history", |session| {
        session.state() == &RunState::Idle && session.entries().len() > 20
    });
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

/// A wheel notch glides instead of jumping; `kinetic` owns how it eases.
#[test]
fn a_wheel_notch_glides() {
    use gpui_kit::{point, InputEvent as _, MouseMoveEvent, ScrollDelta, ScrollEvent};

    let script = Script {
        replay_turns: 20,
        ..Script::default()
    };
    let mut ui = Harness::start("glide", script);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-old")
    });
    ui.click("session-local-old");
    ui.wait_for_session("the replayed history", |session| {
        session.state() == &RunState::Idle && session.entries().len() > 20
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
    let position = point(px(700.), px(400.));
    let before = top(&mut ui);
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
                delta: ScrollDelta::Lines(point(0., 3.)),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    });
    assert_eq!(top(&mut ui), before, "the notch doesn't jump");

    // The glide runs on animation frames, which `wait_until` doesn't draw.
    let deadline = Instant::now() + WAIT;
    while top(&mut ui) == before {
        assert!(Instant::now() < deadline, "the glide never scrolled");
        ui.cx
            .update_window(ui.window, |_, window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            })
            .expect("window is open");
        std::thread::sleep(Duration::from_millis(5));
    }

    // A high-resolution wheel's eighth of a notch glides too, rather than
    // passing through as a touchpad would.
    let before = top(&mut ui);
    ui.act(|window, cx| {
        window.dispatch_event(
            ScrollEvent {
                position,
                delta: ScrollDelta::Lines(point(0., 0.375)),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    });
    assert_eq!(
        top(&mut ui),
        before,
        "a fine wheel step doesn't jump either"
    );
}

/// A missing rho program leaves a clear error with a way to retry.
#[test]
fn a_missing_rho_shows_an_error_and_retry() {
    let target = DaemonTarget::Home {
        home: PathBuf::from("/nonexistent/rho-home"),
        rho: PathBuf::from("/nonexistent/rho"),
    };
    let mut ui = Harness::open(
        "missing",
        target,
        None,
        Shelves::default(),
        Projects::default(),
    );
    ui.wait_until("the retry button", |window, _, _| {
        is_visible(window, "retry-local")
    });
    ui.shot("error");
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
        window.press("ctrl-a", cx);
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
        window.press("ctrl-b", cx);
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
        ui.shot(appearance.name());
    }
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
/// surface opens as a tab, follows the session on screen, and stops its
/// work. Closing the last tab goes back to the list.
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

    ui.act(|window, cx| window.press("ctrl-alt-b", cx));
    ui.act(|window, _| {
        assert!(
            is_visible(window, "panel-surface-list"),
            "the list of surfaces"
        );
        assert!(is_visible(window, "planned-surface-terminal"));
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

    // Another session shows its own work: none.
    ui.click("session-local-s1");
    ui.wait_for_session("the saved session", |session| {
        session.title() == Some("first")
    });
    ui.act(|window, _| {
        assert!(is_visible(window, "panel-tab-background"), "the tab stays");
        assert!(!is_visible(window, "panel-ended-0"), "s1 ran no work");
    });

    ui.click("close-panel-tab-background");
    ui.act(|window, _| {
        assert!(!is_visible(window, "panel-tab-background"));
        assert!(is_visible(window, "panel-surface-list"));
    });
    ui.click("toggle-right-panel");
    ui.act(|window, _| assert!(!is_visible(window, "right-panel")));
    assert_eq!(ui.state.get(Key::RightPanel).as_deref(), Some("hidden"));
}

/// Dragging the right panel's edge resizes it, as far as leaves the pane
/// its room; the width outlives a restart, and a double-click on the edge
/// puts the default back.
#[test]
fn the_right_panel_resizes_from_its_edge() {
    let mut ui = Harness::start("right-panel-resize", Script::default());
    ui.act(|window, cx| window.press("ctrl-alt-b", cx));
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
    assert!(short < px(400.), "{short:?} tall, cut short");
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
        assert!(composer_attachments(window).is_empty());
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
    assert_eq!(text.path(), Some(notes.as_path()));
    assert_eq!(linked.text(), None, "a binary file goes by path");
    assert_eq!(linked.path(), Some(report.as_path()));
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
        assert!(composer_attachments(window).is_empty());
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
/// keyboard's row shows only while the search has the keyboard.
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
        window.press("ctrl-shift-n", cx);
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

    // From that session, a new one starts in its project, with the search
    // on the checked row.
    ui.act(|window, cx| window.press("ctrl-shift-n", cx));
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
    let main = main.canonicalize().expect("the checkout exists");
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
    ui.act(|window, cx| window.press("ctrl-shift-n", cx));
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
        window.press("ctrl-shift-n", cx);
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
    let dir = dir.path().canonicalize().expect("it resolves");
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
            shelves: Shelves::default(),
            projects: Projects::of(["/w/a"]),
            settings,
            remote_sockets: Vec::new(),
        },
    );
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.act(|window, cx| window.press("ctrl-shift-n", cx));
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
/// none and keeps the draft's session, and a folder git doesn't track goes
/// back to Local. The message then starts a session in a new worktree with
/// the draft's settings, and runs there.
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

    // /w/a is no git repository, so the draft goes back to Local there.
    ui.click("project-checkout");
    click_label(&mut ui, "New worktree");
    ui.wait_until("New worktree", in_checkout(&main, Checkout::NewWorktree));
    ui.click("project-local:/w/a");
    ui.wait_until(
        "Local in /w/a",
        in_checkout(Path::new("/w/a"), Checkout::Local),
    );
    ui.click(main_row);
    ui.wait_until("Local in the project", in_checkout(&main, Checkout::Local));

    // With Plan mode set in the draft, the first message starts a session
    // in a new worktree, in Plan mode.
    ui.click("project-checkout");
    click_label(&mut ui, "New worktree");
    ui.wait_until("New worktree", in_checkout(&main, Checkout::NewWorktree));
    ui.click("config-mode");
    click_label(&mut ui, "Plan");
    ui.wait_until("Plan mode in the draft", |_, cx, app| {
        app.draft()
            .and_then(|draft| draft.read(cx).session().and_then(|s| setting(s, "mode")))
            .as_deref()
            == Some("plan")
    });
    ui.send("list files");
    ui.wait_until("the session in its worktree, in Plan mode", |_, cx, app| {
        app.active_session().is_some_and(|view| {
            let view = view.read(cx);
            let cwd = view.live().cwd;
            let plan = setting(view.session(), "mode").as_deref() == Some("plan");
            cwd.parent() == Some(Path::new("/rho/worktrees"))
                && cwd.to_string_lossy().contains("/project-")
                && plan
        })
    });
    assert_eq!(ui.fake().worktrees(), 1);
    ui.shot("running");
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
    assert!(
        remote.new_session_var("HOME").is_some(),
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
            projects: Projects::default(),
            settings: Settings::default(),
            remote_sockets: vec![("box".to_owned(), remote_socket)],
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
        .update(|cx| assert!(cx.global::<Settings>().machines.is_empty()));
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
    open_settings_page(&mut ui, "rho");
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
