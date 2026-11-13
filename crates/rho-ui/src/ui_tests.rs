//! Headless UI tests: the real app in an offscreen window, driven by
//! simulated clicks and keys, against rho-ui-core's scripted daemon
//! (`fake_daemon`: rho-protocol's real server side over a real socket).
//!
//! Each test saves screenshots to `target/ui-shots/` for a person to review.
//! Rendering needs a working Vulkan driver (WGPU); a software one such as
//! Mesa lavapipe works.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::test::{TestEventExt as _, TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    px, size, AnyWindowHandle, App, AppContext as _, Bounds, Entity, HeadlessAppContext,
    MouseButton, Pixels, Window, WindowBounds, WindowOptions,
};
use rho_ui_core::{
    fake_daemon::{FakeDaemon, Script, TuiJob, TUI_SESSION},
    BackgroundStatus, Checkout, ConfigKind, Daemon, DaemonTarget, Entry, RunState, Session,
    SessionId, ToolStatus,
};

use gpui_kit::component::ActiveTheme as _;

use crate::{
    app::{LaunchConfig, RhoApp},
    projects::Projects,
    settings::{Appearance, Settings, UiScale},
    shelf::Shelves,
    turn_times::TurnTimes,
};

mod live;

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
}

impl Harness {
    /// Opens the app at 1200x800 against a fake daemon holding `script`.
    /// The fake's saved sessions count as started in rho-ui, so they list
    /// as active.
    fn start(name: &'static str, script: Script) -> Self {
        Self::start_with(name, script, Shelves::owning(["s1", "old"]))
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
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(crate::Assets),
            gpui_kit::platform::current_headless_renderer,
        );
        // The daemon's answers arrive on a socket: its I/O thread wakes
        // tasks, which the deterministic scheduler only accepts this way.
        cx.allow_parking();
        cx.update(crate::init);
        let launch = LaunchConfig {
            workspace: std::env::temp_dir(),
            daemon: target.clone(),
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
                    // In-memory marks and projects: tests never touch the
                    // user's state files.
                    |window, cx| cx.new(|cx| RhoApp::new(launch, shelves, projects, window, cx)),
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
                    .prompt(&id, &text)
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
        let app = self.app.clone();
        self.act(|window, cx| {
            if let Some(view) = app.read(cx).active_session() {
                let composer = view.read(cx).composer_focus_handle(cx);
                window.focus(&composer, cx);
            }
            window.input(text, cx);
            window.press("enter", cx);
        });
    }

    /// Saves the window as `target/ui-shots/<test>-<step>.png`.
    fn shot(&mut self, step: &str) -> PathBuf {
        self.frame();
        let image = self
            .cx
            .capture_screenshot(self.window)
            .expect("offscreen rendering needs a working Vulkan driver");
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
        is_visible(window, "session-s1") && is_visible(window, "session-old")
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
        assert_eq!(window.find("session-new-1").label(), Some("list files"));
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
        is_visible(window, "session-new-1")
    });

    // The finished turn's working folds behind "Worked for"; inside, a
    // lone tool call is one line, and opening it shows its output.
    ui.click("worked-0");
    ui.wait_until("the turn's working", |window, _, _| {
        is_visible(window, "step-1")
    });
    ui.act(|window, cx| window.within("step-1").click("step-line", cx));
    ui.wait_until("the tool's output", |window, _, _| {
        is_visible(window, "details-1")
    });
    ui.shot("done");
}

/// A sent message renders as markdown; dragging across it selects the
/// text, and copy puts it on the clipboard.
#[test]
fn a_sent_message_can_be_selected_and_copied() {
    let mut ui = Harness::start("select", Script::default());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-s1")
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

/// Picking a saved session attaches to it, which replays its conversation.
#[test]
fn opening_a_saved_session_replays_its_history() {
    let mut ui = Harness::start("resume", Script::default());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-old")
    });
    ui.click("session-old");
    ui.wait_for_session("the replayed history", |session| {
        matches!(session.entries(), [Entry::Agent(m)] if m.text() == "replayed")
            && !session.config().is_empty()
    });
    ui.shot("replayed");

    // The attach's answer carries the title and the settings.
    ui.click("session-s1");
    ui.wait_for_session("the titled session", |session| {
        session.title() == Some("first") && !session.config().is_empty()
    });
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
        is_visible(window, "session-old")
    });
    ui.click("session-old");
    ui.wait_until("the session settings", |window, _, _| {
        is_visible(window, "config-panel") && is_visible(window, "config-mode")
    });
    ui.shot("composer");
    // Footer buttons sit on the composer's own color; hover must still show.
    ui.act(|window, cx| window.hover("config-mode", cx));
    ui.shot("mode-hover");

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
    ui.wait_until(
        "the button to name the model and reasoning",
        |window, _, _| window.find("config-panel").label() == Some("Huge · High"),
    );
    ui.shot("changed");
}

/// A new session's draft already has the session's settings, and the session
/// keeps what was set there.
#[test]
fn a_draft_sets_the_session_before_the_first_message() {
    let mut ui = Harness::start("draft-settings", Script::default());
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.click("new-session");
    ui.wait_until("the draft's settings", |window, _, _| {
        is_visible(window, "draft-view") && is_visible(window, "config-panel")
    });
    ui.shot("draft");

    ui.click("config-panel");
    ui.wait_until("the reasoning levels", |window, _, _| {
        is_visible(window, "config-thought_level-high")
    });
    ui.click("config-thought_level-high");
    ui.wait_until("the button to show it", |window, _, _| {
        window.find("config-panel").label() == Some("Small · High")
    });
    ui.act(|window, cx| window.press("escape", cx));

    ui.act(|window, cx| {
        window.input("list files", cx);
        window.press("enter", cx);
    });
    ui.wait_for_session("the first prompt in the draft's session", |session| {
        session.id().as_str() == "new-1"
            && !session.entries().is_empty()
            && setting(session, "thought_level").as_deref() == Some("high")
    });
}

/// Settle and snooze from a row's hover buttons and its context menu, and
/// bring sessions back from the opened Settled shelf, whose rows show their
/// details on hover.
#[test]
fn settle_and_snooze_move_sessions_between_lists() {
    let mut ui = Harness::start("shelves", Script::default());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-s1") && is_visible(window, "session-old")
    });

    ui.act(|window, cx| window.hover("session-s1", cx));
    ui.shot("hover");
    ui.click("settle");
    ui.wait_until("s1 on the settled list", |window, _, _| {
        !is_visible(window, "session-s1") && is_visible(window, "toggle-settled")
    });
    ui.shot("settled-entry");

    ui.click("toggle-settled");
    ui.wait_until(
        "the settled shelf under the active sessions",
        |window, _, _| is_visible(window, "session-s1") && is_visible(window, "session-old"),
    );
    ui.act(|window, cx| window.hover("session-s1", cx));
    // The card opens after a short delay, on the scheduler's clock.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the details card", |window, _, _| {
        is_visible(window, "session-card")
    });
    ui.shot("settled-shelf");

    // Restoring the last settled session empties and folds the shelf.
    ui.click("restore");
    ui.wait_until("s1 back on the active list", |window, _, _| {
        is_visible(window, "session-s1")
            && is_visible(window, "session-old")
            && !is_visible(window, "toggle-settled")
    });

    ui.act(|window, cx| window.hover("session-old", cx));
    ui.click("snooze");
    ui.click("snooze-4h");
    ui.wait_until("old on the snoozed list", |window, _, _| {
        !is_visible(window, "session-old") && is_visible(window, "toggle-snoozed")
    });

    // The context menu offers the same moves.
    ui.act(|window, cx| window.right_click("session-s1", cx));
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
        !is_visible(window, "session-s1") && is_visible(window, "toggle-settled")
    });
    ui.shot("both-shelves");
}

/// Sessions from elsewhere start settled, and with nothing active the folded
/// shelf sits at the bottom of the sidebar. A draft makes no session until
/// its first message, and leaving it drops it. A session can be deleted.
#[test]
fn drafts_are_free_outside_sessions_start_settled_and_sessions_delete() {
    let mut ui = Harness::start_with("drafts", Script::default(), Shelves::default());
    // Below the list, the sidebar has only its footer (about 50 px).
    ui.wait_until("the settled entry at the bottom", |window, _, _| {
        bounds(window, "toggle-settled").is_some_and(|header| header.bottom() > px(700.))
            && !is_visible(window, "session-s1")
    });
    ui.shot("settled-by-default");
    let folded = ui
        .cx
        .update_window(ui.window, |_, window, _| bounds(window, "toggle-settled"))
        .expect("window is open")
        .expect("the settled header is drawn");

    // Leaving an unused draft drops it and stops following its session,
    // which, never prompted, the daemon doesn't list.
    ui.click("new-session");
    ui.wait_until("the draft's session", |window, _, _| {
        is_visible(window, "draft-view") && is_visible(window, "config-panel")
    });
    ui.click("toggle-settled");
    // Opening it lifts the header just enough for its rows to fit below.
    ui.wait_until("the settled shelf", |window, _, _| {
        let (Some(header), Some(last)) = (
            bounds(window, "toggle-settled"),
            bounds(window, "session-old"),
        ) else {
            return false;
        };
        header.top() < folded.top() && last.top() > header.bottom() && last.bottom() > px(700.)
    });
    ui.shot("settled-opened");
    ui.click("session-s1");
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
    ui.wait_until("the new session listed as active", |window, _, _| {
        is_visible(window, "session-new-2")
    });
    ui.act(|window, _| {
        assert!(
            !is_visible(window, "session-new-1"),
            "the unused draft's session isn't listed"
        );
    });

    ui.act(|window, cx| window.right_click("session-new-2", cx));
    click_label(&mut ui, "Delete…");
    ui.shot("delete-confirm");
    click_label(&mut ui, "Delete");
    ui.wait_until("the session gone", |window, _, app| {
        !is_visible(window, "session-new-2") && app.active_session().is_none()
    });
}

/// Clicks the element labelled `label`, such as a menu item or dialog button.
fn click_label(ui: &mut Harness, label: &str) {
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
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "open-settings")
    });
    ui.click("open-settings");
    ui.wait_until("the settings dialog", |window, _, _| {
        is_visible(window, "setting-ui-font")
    });
    // Past the dialog's entrance transition.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.shot("open");
    ui.act(|window, _| {
        let panel = bounds(window, "settings-view").expect("settings are shown");
        let viewport = window.viewport_size();
        assert!(
            panel.size.width < viewport.width && panel.size.height < viewport.height,
            "settings {panel:?} fill the {viewport:?} window"
        );
        assert!(
            is_visible(window, "new-session"),
            "the sidebar shows behind settings"
        );
        // Kit's dialog surface, which the panel fills.
        let dialog = bounds(window, "surface").expect("the settings dialog");
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
/// step, each of which opens into its details; an edit shows Kit's diff.
#[test]
fn finished_work_folds_and_opens_in_levels() {
    use gpui_kit::{point, ScrollDelta};

    let script = Script {
        replay_turns: 1,
        ..Script::default()
    };
    let mut ui = Harness::start("work", script);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-old")
    });
    // An earlier run of rho-ui timed this turn, so it shows its time.
    let took = Duration::from_secs(80);
    ui.cx
        .update(|cx| cx.global_mut::<TurnTimes>().record("u0", took));
    ui.click("session-old");
    // Entries: reply, question, thought, read, search, command, edit, answer.
    ui.wait_until("the turn folded behind one line", |window, _, _| {
        is_visible(window, "worked-0") && !is_visible(window, "work-2")
    });
    assert_eq!(turn_took(&mut ui, 0), Some(took), "Worked for 1m 20s");
    ui.shot("worked");

    ui.click("worked-0");
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
    // A heredoc's line shows its first line, as tall as any other step's.
    ui.act(|window, _| {
        let height =
            |window: &mut Window, step| window.within(step).find("step-line").bounds().size.height;
        assert_eq!(height(window, "step-5"), height(window, "step-4"));
    });

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
    ui.wait_until("the whole command above its output", |window, _, _| {
        bounds(window, "command-5")
            .zip(bounds(window, "output-5"))
            .is_some_and(|(command, output)| command.bottom() <= output.top())
    });
    ui.shot("command");
    ui.act(|window, cx| window.within("step-5").click("step-line", cx));

    ui.act(|window, cx| window.within("step-3").click("step-line", cx));
    ui.wait_until("the read's output", |window, _, _| {
        is_visible(window, "details-3")
    });
    ui.act(|window, _| {
        assert!(
            window.try_find("command-3").is_none(),
            "a read's title is all on its line"
        );
    });

    // The output scrolls inside its box, and the transcript stays put.
    ui.act(|window, cx| {
        let up = ScrollDelta::Pixels(point(px(0.), px(400.)));
        window.within("step-4").scroll("step-line", up, cx);
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
            window.find("output-text-3").bounds().top()
        });
        text.expect("window is open")
    };
    let (transcript, text) = (top(&mut ui), text_y(&mut ui));
    ui.act(|window, cx| {
        let down = ScrollDelta::Pixels(point(px(0.), px(-100.)));
        window.scroll("output-3", down, cx);
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
        is_visible(window, "session-old")
    });
    ui.click("session-old");
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
    ui.click("worked-1");
    ui.wait_until("the slow turn's tool call", |window, _, _| {
        is_visible(window, "step-9")
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
        is_visible(window, "session-old")
    });
    let took = Duration::from_secs(80);
    ui.cx
        .update(|cx| cx.global_mut::<TurnTimes>().record("u0", took));
    ui.click("session-old");
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
        assert!(is_visible(window, "worked-0"), "the old turn stays folded");
        assert!(!is_visible(window, "work-2"), "its rows stay hidden");
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

/// A wheel notch glides over several frames instead of jumping.
#[test]
fn a_wheel_notch_glides() {
    use gpui_kit::{point, InputEvent as _, MouseMoveEvent, ScrollDelta, ScrollEvent};

    let script = Script {
        replay_turns: 20,
        ..Script::default()
    };
    let mut ui = Harness::start("glide", script);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-old")
    });
    ui.click("session-old");
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

    let mut frames = 0;
    let mut moved_early = false;
    while frames < 200 {
        std::thread::sleep(Duration::from_millis(4));
        let ran = ui
            .cx
            .update_window(ui.window, |_, window, cx| {
                let ran = window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
                ran
            })
            .expect("window is open");
        frames += 1;
        if frames == 3 {
            moved_early = top(&mut ui) != before;
        }
        if ran == 0 {
            break;
        }
    }
    assert!(moved_early, "the glide starts within a few frames");
    assert!(frames > 10, "and spreads over many: {frames}");
    assert_ne!(top(&mut ui), before, "the transcript scrolled");

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
        is_visible(window, "retry-daemon")
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
        is_visible(window, "session-tui-1") && !is_visible(window, "session-s1")
    });
    ui.shot("listed");

    ui.click("session-tui-1");
    ui.wait_for_session("the turn under way", |session| {
        session.state() == &RunState::Running
            && matches!(session.entries().last(), Some(Entry::Agent(m)) if m.text() == "Reading the parser.")
    });
    assert_eq!(ui.session().id().as_str(), TUI_SESSION);
    ui.act(|window, _| {
        assert_eq!(window.find("session-title").label(), Some("from the tui"));
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
        window
            .try_find("steer-0")
            .and_then(|steer| steer.label().map(str::to_owned))
            == Some("Queued: wait".to_owned())
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
            is_visible(window, "session-tui-1"),
            "steered here, so the session is rho-ui's and stays listed"
        );
    });
    ui.shot("steered");
}

/// Frame-time probe, not a pass/fail test: 300 saved sessions and a long
/// resumed transcript, then hovering sidebar rows and wheel-scrolling the
/// transcript. Run with
/// `cargo test -p rho-ui --release frame_times -- --ignored --nocapture`.
#[test]
#[ignore = "measurement; run on demand"]
fn frame_times() {
    use gpui_kit::{
        point, InputEvent as _, MouseMoveEvent, Pixels, Point, ScrollDelta, ScrollEvent,
    };

    // `TestEventExt::pointer_move` forces two full redraws around the
    // event; the probe wants only the work the event itself causes.
    fn move_pointer(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
        window.dispatch_event(
            MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: window.modifiers(),
            }
            .to_platform_input(),
            cx,
        );
    }

    let sessions: usize = std::env::var("PERF_SESSIONS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(300);
    let script = Script {
        sessions,
        replay_turns: std::env::var("PERF_REPLAY")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(100),
        ..Script::default()
    };
    let owned = std::iter::once("old".to_owned()).chain((0..sessions).map(|n| format!("bulk-{n}")));
    let mut ui = Harness::start_with("perf", script, Shelves::owning(owned));
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-old")
    });
    ui.click("session-old");
    ui.wait_for_session("the replayed history", |session| {
        session.state() == &RunState::Idle && !session.entries().is_empty()
    });
    ui.shot("loaded");

    let report = |name: &str, mut samples: Vec<Duration>| {
        samples.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        println!(
            "{name:>14}: p50 {:6.2} ms  p90 {:6.2} ms  max {:6.2} ms  (n={})",
            ms(samples[samples.len() / 2]),
            ms(samples[samples.len() * 9 / 10]),
            ms(samples[samples.len() - 1]),
            samples.len()
        );
    };
    let measure = |ui: &mut Harness, step: &dyn Fn(usize, &mut Window, &mut App)| {
        (0..60)
            .map(|n| {
                ui.cx
                    .update_window(ui.window, |_, window, cx| {
                        let start = Instant::now();
                        step(n, window, cx);
                        // A plain draw, as the platform does: clean cached
                        // views are reused. `render_frame` would force every
                        // view to rebuild.
                        window.draw(cx).clear(cx);
                        start.elapsed()
                    })
                    .expect("window is open")
            })
            .collect::<Vec<_>>()
    };

    report(
        "full redraw",
        measure(&mut ui, &|_, window, _| window.refresh()),
    );
    report("idle frame", measure(&mut ui, &|_, _, _| {}));
    report(
        "sidebar hover",
        measure(&mut ui, &|n, window, cx| {
            let row = f32::from(u8::try_from(n % 7).expect("under 7"));
            // One row per step across the seven on screen, so every step
            // moves the hover.
            let y = 120.0 + row * crate::sidebar::ROW_REMS * 16.0;
            move_pointer(window, point(px(100.), px(y)), cx);
        }),
    );
    report(
        "transcript wheel",
        measure(&mut ui, &|n, window, cx| {
            let dy = if n < 30 { 40.0 } else { -40.0 };
            let position = point(px(700.), px(400.));
            move_pointer(window, position, cx);
            window.dispatch_event(
                ScrollEvent {
                    position,
                    delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
        }),
    );
}

/// The theme setting switches between rho-ui's light and dark themes,
/// keeping the UI scale. Screenshots of the same busy screen in both modes,
/// for a person to judge: a turn's steps, a diff, code, notices, an
/// approval.
#[test]
fn the_theme_setting_switches_light_and_dark() {
    use gpui_kit::component::Theme;

    use crate::settings::Appearance;

    let script = Script {
        replay_turns: 1,
        cards: true,
        ..Script::default()
    };
    let mut ui = Harness::start("theme", script);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-old")
    });
    ui.click("session-old");
    ui.wait_until("the turn folded behind one line", |window, _, _| {
        is_visible(window, "worked-0")
    });
    ui.click("worked-0");
    ui.wait_until("the steps", |window, _, _| is_visible(window, "work-2"));
    ui.act(|window, cx| window.within("work-2").click("work-summary", cx));
    ui.wait_until("a line per step", |window, _, _| {
        is_visible(window, "step-6")
    });
    ui.act(|window, cx| window.within("step-6").click("step-line", cx));
    ui.wait_until("the edit's diff", |window, _, _| {
        is_visible(window, "details-6")
    });
    // What each theme's file sets: its background and link colors.
    let color = |hex: &str| gpui_kit::component::try_parse_color(hex).expect("a color");
    let show = |ui: &mut Harness, step: &str| {
        for (appearance, background, link) in [
            (Appearance::Dark, "#161618", "#9db4ec"),
            (Appearance::Light, "#fcfcfd", "#3a5fc0"),
        ] {
            let shown = ui.cx.update(|cx| {
                let mut settings = cx.global::<Settings>().clone();
                settings.appearance = appearance;
                settings.ui_scale = UiScale::from_percent(125).expect("125% is on offer");
                settings.apply(cx);
                let theme = Theme::global(cx);
                (theme.background, theme.link, theme.font_size)
            });
            ui.frame();
            let want = (color(background), color(link), px(20.));
            assert_eq!(shown, want, "{appearance:?} keeps the scale");
            // Shots at the default scale, to compare with the other tests'.
            ui.cx.update(|cx| {
                let mut settings = cx.global::<Settings>().clone();
                settings.ui_scale = UiScale::default();
                settings.apply(cx);
            });
            ui.shot(&format!("{step}-{}", appearance.name()));
        }
    };
    // The diff sits above the fold: bring its row to the top.
    let app = ui.app.clone();
    let scroller = ui.cx.update(|cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        view.read(cx).transcript().scroller().clone()
    });
    ui.cx
        .update(|cx| scroller.update(cx, |scroller, cx| scroller.scroll_to_item(2, cx)));
    show(&mut ui, "diff");
    ui.prompt_from_elsewhere("old", "list files");
    ui.wait_until("the approval", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.cx.update(|cx| {
        scroller.update(
            cx,
            gpui_kit::component::message_scroller::MessageScrollerState::scroll_to_end,
        );
    });
    show(&mut ui, "approval");

    ui.click("open-settings");
    ui.wait_until("the settings dialog", |window, _, _| {
        is_visible(window, "setting-ui-font")
    });
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.shot("settings-light");
}

/// Whether the session view shows an error: a failed turn, or a request
/// that failed.
fn shows_an_error(window: &Window) -> bool {
    is_visible(window, "turn-error") || is_visible(window, "session-error")
}

/// A turn ends while the process it started runs on, which is listed above
/// the composer. Stopping it lists it as stopped while the daemon's own turn
/// tells the model, which shows like any turn and takes a steer; the
/// process leaves the list as that turn ends. A process that exits by
/// itself shows its exit status the same way.
#[test]
fn background_work_is_listed_stopped_and_handed_to_the_daemons_turn() {
    let mut ui = Harness::start("background", Script::default());
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-s1")
    });
    ui.click("new-session");
    ui.wait_until("the draft", |window, _, _| is_visible(window, "draft-view"));
    ui.send("background");
    ui.wait_for_session("the turn to end with its process running", |session| {
        session.state() == &RunState::Idle && !session.background().is_empty()
    });
    assert_eq!(
        ui.label("background-0").as_deref(),
        Some("sleep 30: Running")
    );
    ui.shot("running");

    ui.click("background-stop-0");
    ui.wait_for_session("the daemon's turn about the stop", |session| {
        session.state() == &RunState::Running
            && session.background()[0].status() == BackgroundStatus::Cancelled
    });
    ui.act(|window, _| {
        assert_eq!(
            window.find("background-0").label(),
            Some("sleep 30: Stopped"),
            "the list says how it ended"
        );
        assert!(!is_visible(window, "background-stop-0"), "nothing to stop");
        assert!(
            is_visible(window, "composer-stop"),
            "the turn shows as running"
        );
    });
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
    assert_eq!(
        ui.label("background-0").as_deref(),
        Some("sleep 30: Exited 0")
    );
    ui.shot("exited");
    ui.send("ok");
    ui.wait_for_session("the list to empty", |session| {
        session.state() == &RunState::Idle && session.background().is_empty()
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
        is_visible(window, "session-s1")
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
        (
            view.composer_text(cx),
            view.session().held().map(str::to_owned),
        )
    });
    assert_eq!((typed.as_str(), held), ("deleting", None));
}

/// The draft lists the user's projects, not every folder rho has sessions
/// in, the most recently used first. Search filters them, and choosing one
/// opens the draft's session there. The draft's project is checked and the
/// keyboard's row shows only while the search has the keyboard.
#[test]
fn a_new_session_starts_in_one_of_the_users_projects() {
    let launch_row: &'static str = format!("project-{}", std::env::temp_dir().display()).leak();
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
        is_visible(window, "session-s1")
    });

    // Nothing on screen: the draft starts in the launch directory, which
    // is no project, so the list checks none and offers only the two.
    ui.click("new-session");
    ui.wait_until("the draft", draft_in(&std::env::temp_dir()));
    ui.wait_until("the project list", |window, _, _| {
        is_visible(window, "project-/w/a") && is_visible(window, "project-/w/b")
    });
    ui.act(|window, _| {
        let (a, b) = (
            bounds(window, "project-/w/a"),
            bounds(window, "project-/w/b"),
        );
        assert!(
            a.unwrap().origin.y < b.unwrap().origin.y,
            "/w/a has the newest session, so it comes first"
        );
        assert!(!is_visible(window, launch_row), "rho-ui adds no project");
        assert_eq!(checked(window, "project-/w/a"), Some(false));
    });
    ui.cx.update(|cx| {
        let selected = selected_project(cx, ui.app.read(cx));
        assert_eq!(selected, None, "the keyboard is elsewhere");
    });
    ui.shot("list");

    // Hovering a row offers Remove in place of its age, and selects
    // nothing.
    ui.act(|window, cx| window.hover("project-/w/b", cx));
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
        !is_visible(window, "project-/w/a")
            && is_visible(window, "project-/w/b")
            && selected_project(cx, app).as_deref() == Some("project-/w/b")
    });
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_until("the draft in /w/b", draft_in(Path::new("/w/b")));
    ui.wait_until("the search to clear", |window, _, _| {
        is_visible(window, "project-/w/a")
    });
    // Back in the composer: /w/b is checked, and no row looks chosen
    // besides it.
    ui.act(|window, _| {
        assert_eq!(checked(window, "project-/w/b"), Some(true));
        assert_eq!(checked(window, "project-/w/a"), Some(false));
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
        selected_project(cx, app).as_deref() == Some("project-/w/b")
    });
}

/// A typed path to a worktree adds its repository's project, not the
/// worktree, and Remove takes a project off the list without picking it.
#[test]
fn a_typed_worktree_adds_its_project_and_remove_takes_one_off() {
    let dir = std::env::temp_dir().join(format!("rho-ui-projects-{}", std::process::id()));
    let (main, tree) = crate::repo::fake_worktree(&dir);
    let main = main.canonicalize().expect("the checkout exists");
    let main_row: &'static str = format!("project-{}", main.display()).leak();
    let daemon = FakeDaemon::start(Script::default());
    let mut ui = Harness::open(
        "project-add",
        daemon.target(),
        Some(daemon),
        Shelves::owning(["s1", "old"]),
        Projects::of(["/w/b", "/w/a"]),
    );
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-s1")
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
        !is_visible(window, "project-typed") && !is_visible(window, "project-/w/b")
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
        let roots = ui.app.read(cx).projects(cx).to_vec();
        assert_eq!(roots, [PathBuf::from("/w/b"), "/w/a".into(), main.clone()]);
    });

    // Remove takes /w/a off the list without picking it.
    ui.act(|window, cx| window.hover("project-/w/a", cx));
    ui.wait_until("Remove on /w/a", |window, _, _| {
        is_visible(window, "project-remove")
    });
    ui.click("project-remove");
    ui.wait_until("/w/a gone", |window, _, _| {
        !is_visible(window, "project-/w/a")
    });
    ui.wait_until("the draft still in the checkout", draft_in(&main));
    ui.cx.update(|cx| {
        let roots = ui.app.read(cx).projects(cx).to_vec();
        assert_eq!(roots, [PathBuf::from("/w/b"), main.clone()]);
    });
    ui.shot("removed");
    let _ = std::fs::remove_dir_all(&dir);
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
    let main_row: &'static str = format!("project-{}", main.display()).leak();
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
                draft.cwd() == dir && draft.session_id().is_some() && draft.checkout(cx) == checkout
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
            draft.read(cx).session_id().cloned()
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
    ui.click("project-/w/a");
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
    move |_, cx, app| app.draft().is_some_and(|draft| draft.read(cx).cwd() == dir)
}

/// Whether project row `id` shows the draft's check.
fn checked(window: &Window, id: &'static str) -> Option<bool> {
    window.try_find(id).and_then(|element| element.checked())
}

/// The project row the draft's keyboard selection is on, if any.
fn selected_project(cx: &App, app: &RhoApp) -> Option<String> {
    app.draft()?.read(cx).selected_project(cx)
}

/// With more projects than it shows, the list ends on a whole row at any UI
/// scale: none shows cut off at its foot.
#[test]
fn the_project_list_shows_whole_rows() {
    let daemon = FakeDaemon::start(Script::default());
    let projects = ["/w/a", "/w/b", "/w/c", "/w/d", "/w/e", "/w/f", "/w/g"];
    let mut ui = Harness::open(
        "project-rows",
        daemon.target(),
        Some(daemon),
        Shelves::default(),
        Projects::of(projects),
    );
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.click("new-session");
    for scale in [100, 125] {
        ui.cx.update(|cx| {
            let mut settings = cx.global::<Settings>().clone();
            settings.ui_scale = UiScale::from_percent(scale).expect("the scale is on offer");
            settings.apply(cx);
        });
        ui.wait_until("the project list", |window, _, _| {
            is_visible(window, "project-/w/a")
        });
        // /w/a and /w/b have sessions; the rest follow by name.
        ui.act(|window, _| {
            assert!(is_visible(window, "project-/w/e"), "five rows at {scale}%");
            assert!(
                !is_visible(window, "project-/w/f"),
                "no sixth row peeks out at {scale}%"
            );
        });
    }
    ui.shot("scaled");
}
