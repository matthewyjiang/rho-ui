//! Headless UI tests: the real app in an offscreen window, driven by
//! simulated clicks and keys, against the scripted agent in
//! `crates/rho-ui-core/tests/fake_agent.py` (a real process over real stdio).
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
use rho_ui_core::{AgentCommand, ConfigKind, Entry, RunState, Session, ToolStatus};

use gpui_kit::component::ActiveTheme as _;

use crate::{
    app::{LaunchConfig, RhoApp},
    settings::{Settings, UiScale},
    shelf::Shelves,
    turn_times::TurnTimes,
};

/// Long enough for python to start and answer; the fake agent replies at once.
const WAIT: Duration = Duration::from_secs(10);

fn fake_agent() -> AgentCommand {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rho-ui-core/tests/fake_agent.py");
    AgentCommand::new("python3", [script.to_string_lossy().into_owned()])
}

/// One app window and its context.
struct Harness {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    app: Entity<RhoApp>,
    name: &'static str,
}

impl Harness {
    /// Opens the app at 1200x800 with `command` as its agent. The fake
    /// agent's saved sessions count as started in rho-ui, so they list as
    /// active.
    fn start(name: &'static str, command: AgentCommand) -> Self {
        Self::start_with(name, command, Shelves::owning(["s1", "old"]))
    }

    /// [`Self::start`] with the given settled and snoozed state.
    fn start_with(name: &'static str, command: AgentCommand, shelves: Shelves) -> Self {
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(crate::Assets),
            gpui_kit::platform::current_headless_renderer,
        );
        // The agent is a real process: its pipes wake tasks from another
        // thread, which the deterministic scheduler only accepts this way.
        cx.allow_parking();
        cx.update(crate::init);
        let launch = LaunchConfig {
            workspace: std::env::temp_dir(),
            command,
            command_line: "fake agent".to_owned(),
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
                    // In-memory marks: tests never touch the user's state file.
                    |window, cx| cx.new(|cx| RhoApp::new(launch, shelves, window, cx)),
                )
            })
            .expect("open the app window");
        let mut harness = Self {
            cx,
            window,
            app,
            name,
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
        let deadline = Instant::now() + WAIT;
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
    let mut ui = Harness::start("turn", fake_agent());
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
        is_visible(window, "approval-yes")
    });
    assert_eq!(ui.session().state(), &RunState::Running);
    // rho names a session after its first turn; until then the header and
    // the sidebar row both show the first prompt.
    assert_eq!(ui.session().title(), None);
    ui.act(|window, _| {
        assert_eq!(window.find("session-title").label(), Some("list files"));
        assert_eq!(window.find("session-new-1").label(), Some("list files"));
    });
    ui.shot("approval");

    ui.click("approval-yes");
    ui.wait_for_session("the turn to finish", |session| {
        session.state() == &RunState::Idle && session.entries().len() == 3
    });
    let session = ui.session();
    let entries = session.entries();
    assert!(matches!(&entries[0], Entry::User(m) if m.text() == "list files"));
    assert!(
        matches!(&entries[1], Entry::Tool(t) if t.title() == "ls" && t.status() == ToolStatus::Done)
    );
    assert!(matches!(&entries[2], Entry::Agent(m) if m.text() == "you chose yes"));
    ui.act(|window, _| {
        assert!(
            window.try_find("approval-yes").is_none(),
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
    let mut ui = Harness::start("select", fake_agent());
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

/// Picking a saved session replays its history before it can be used.
#[test]
fn opening_a_saved_session_replays_its_history() {
    let mut ui = Harness::start("resume", fake_agent());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-old")
    });
    ui.click("session-old");
    ui.wait_for_session("the replayed history", |session| {
        matches!(session.entries(), [Entry::Agent(m)] if m.text() == "replayed")
            && !session.config().is_empty()
    });
    ui.shot("replayed");

    // Resuming doesn't replay the title; the saved one carries over.
    ui.click("session-s1");
    ui.wait_for_session("the titled session", |session| {
        session.title() == Some("first") && !session.entries().is_empty()
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
/// the reasoning levels and fast mode. Each change reaches the agent.
#[test]
fn composer_settings_change_the_session() {
    let mut ui = Harness::start("settings-panel", fake_agent());
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
    let mut ui = Harness::start("draft-settings", fake_agent());
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
    let mut ui = Harness::start("shelves", fake_agent());
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
    let mut ui = Harness::start_with("drafts", fake_agent(), Shelves::default());
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

    // Leaving an unused draft drops it and closes its session, which,
    // never prompted, the agent doesn't list.
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
        is_visible(window, "approval-yes")
    });
    assert_eq!(
        ui.session().id().as_str(),
        "new-2",
        "the second draft's session"
    );
    ui.click("approval-yes");
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

/// The gear opens settings; a new scale reaches the theme; Escape closes.
#[test]
fn settings_open_apply_and_close() {
    let mut ui = Harness::start("settings", fake_agent());
    ui.wait_until("the sidebar", |window, _, _| {
        is_visible(window, "open-settings")
    });
    ui.click("open-settings");
    ui.wait_until("the settings screen", |window, _, _| {
        is_visible(window, "close-settings") && is_visible(window, "setting-ui-font")
    });
    ui.shot("open");

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

    ui.act(|window, cx| window.press("escape", cx));
    ui.wait_until("the sessions again", |window, _, _| {
        !is_visible(window, "close-settings") && is_visible(window, "new-session")
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
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rho-ui-core/tests/fake_agent.py");
    let command = AgentCommand::new(
        "env",
        [
            "FAKE_REPLAY=1".to_owned(),
            "python3".to_owned(),
            script.to_string_lossy().into_owned(),
        ],
    );
    let mut ui = Harness::start("work", command);
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

    ui.act(|window, cx| window.within("step-3").click("step-line", cx));
    ui.wait_until("the read's output", |window, _, _| {
        is_visible(window, "details-3")
    });
    ui.act(|window, cx| window.within("step-6").click("step-line", cx));
    ui.wait_until("the edit's diff", |window, _, _| {
        is_visible(window, "details-6")
    });
    ui.shot("diff");
}

/// A sent prompt starts a new turn even before the agent echoes it, so the
/// last finished turn stays folded, and a refused prompt times no turn.
#[test]
fn a_new_prompt_leaves_finished_turns_folded() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rho-ui-core/tests/fake_agent.py");
    let command = AgentCommand::new(
        "env",
        [
            "FAKE_REPLAY=1".to_owned(),
            "python3".to_owned(),
            script.to_string_lossy().into_owned(),
        ],
    );
    let mut ui = Harness::start("pending", command);
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-old")
    });
    ui.click("session-old");
    ui.wait_until("the replayed turn, folded", |window, _, _| {
        is_visible(window, "worked-0")
    });

    // The agent echoes "slow" late; until then the old turn stays folded.
    ui.act(|window, cx| {
        window.input("slow", cx);
        window.press("enter", cx);
    });
    ui.act(|window, _| {
        assert!(is_visible(window, "worked-0"), "the old turn stays folded");
        assert!(!is_visible(window, "work-2"), "its rows stay hidden");
    });
    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-yes")
    });
    ui.click("approval-yes");
    ui.wait_for_session("the slow turn to finish", |session| {
        session.state() == &RunState::Idle
    });
    assert_eq!(turn_took(&mut ui, 0), None, "the replayed turn has no time");
    let slow = turn_took(&mut ui, 1).expect("the slow turn was timed here");
    let saved = ui.cx.update(|cx| cx.global::<TurnTimes>().get("m1"));
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

/// A wheel notch glides over several frames instead of jumping.
#[test]
fn a_wheel_notch_glides() {
    use gpui_kit::{point, InputEvent as _, MouseMoveEvent, ScrollDelta, ScrollEvent};

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rho-ui-core/tests/fake_agent.py");
    let command = AgentCommand::new(
        "env",
        [
            "FAKE_REPLAY=20".to_owned(),
            "python3".to_owned(),
            script.to_string_lossy().into_owned(),
        ],
    );
    let mut ui = Harness::start("glide", command);
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

/// A missing agent program leaves a clear error with a way to retry.
#[test]
fn missing_agent_shows_an_error_and_retry() {
    let mut ui = Harness::start("missing", AgentCommand::new("/nonexistent/rho", ["acp"]));
    ui.wait_until("the retry button", |window, _, _| {
        is_visible(window, "retry-agent")
    });
    ui.shot("error");
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

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rho-ui-core/tests/fake_agent.py");
    let sessions: usize = std::env::var("PERF_SESSIONS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(300);
    let command = AgentCommand::new(
        "env",
        [
            format!("FAKE_SESSIONS={sessions}"),
            format!(
                "FAKE_REPLAY={}",
                std::env::var("PERF_REPLAY").unwrap_or_else(|_| "100".to_owned())
            ),
            "python3".to_owned(),
            script.to_string_lossy().into_owned(),
        ],
    );
    let owned = std::iter::once("old".to_owned()).chain((0..sessions).map(|n| format!("bulk-{n}")));
    let mut ui = Harness::start_with("perf", command, Shelves::owning(owned));
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
