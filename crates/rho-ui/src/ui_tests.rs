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

use gpui_kit::test::{TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    px, size, AnyWindowHandle, App, AppContext as _, Bounds, Entity, HeadlessAppContext, Window,
    WindowBounds, WindowOptions,
};
use rho_ui_core::{AgentCommand, ConfigKind, Entry, RunState, Thread, ToolStatus};

use crate::app::{LaunchConfig, RhoApp};

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
    /// Opens the app at 1200x800 with `command` as its agent.
    fn start(name: &'static str, command: AgentCommand) -> Self {
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(gpui_kit::assets::Assets),
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
                    |window, cx| cx.new(|cx| RhoApp::new(launch, window, cx)),
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

    /// Waits for the active thread to satisfy `ready`.
    fn wait_for_thread(&mut self, what: &str, ready: impl Fn(&Thread) -> bool) {
        self.wait_until(what, |_, cx, app| {
            app.active_thread()
                .is_some_and(|view| ready(view.read(cx).thread()))
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

    /// The active thread's state, cloned out of the app.
    fn thread(&mut self) -> Thread {
        let app = self.app.clone();
        self.cx.update(|cx| {
            app.read(cx)
                .active_thread()
                .expect("a thread is open")
                .read(cx)
                .thread()
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

/// Sidebar, new thread, prompt, approval, tool card, reply, idle.
#[test]
fn new_thread_runs_a_turn_through_an_approval() {
    let mut ui = Harness::start("turn", fake_agent());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-s1") && is_visible(window, "session-old")
    });
    ui.shot("sidebar");

    ui.click("new-thread");
    ui.wait_until("an open thread", |_, _, app| app.active_thread().is_some());
    ui.act(|window, cx| {
        window.input("list files", cx);
        window.press("enter", cx);
    });
    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-yes")
    });
    assert_eq!(ui.thread().state(), &RunState::Running);
    ui.shot("approval");

    ui.click("approval-yes");
    ui.wait_for_thread("the turn to finish", |thread| {
        thread.state() == &RunState::Idle && thread.entries().len() == 3
    });
    let thread = ui.thread();
    let entries = thread.entries();
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

    // Opening the tool card shows its output.
    ui.act(|window, cx| window.within("tool:tool-1").click("toggle-output", cx));
    ui.act(|window, _| {
        assert_eq!(
            window.within("tool:tool-1").find("toggle-output").label(),
            Some("Hide output")
        );
    });
    ui.shot("done");
}

/// Picking a saved session replays its history before it can be used.
#[test]
fn opening_a_saved_session_replays_its_history() {
    let mut ui = Harness::start("resume", fake_agent());
    ui.wait_until("saved sessions in the sidebar", |window, _, _| {
        is_visible(window, "session-old")
    });
    ui.click("session-old");
    ui.wait_for_thread("the replayed history", |thread| {
        matches!(thread.entries(), [Entry::Agent(m)] if m.text() == "replayed")
            && !thread.config().is_empty()
    });
    ui.shot("replayed");
}

/// The model picker sends the change and shows what the agent confirms.
#[test]
fn model_picker_changes_the_session_model() {
    let mut ui = Harness::start("model", fake_agent());
    ui.wait_until("the agent", |window, _, _| is_visible(window, "new-thread"));
    ui.click("new-thread");
    ui.wait_until("the model picker", |window, _, _| {
        is_visible(window, "config-model")
    });
    ui.click("config-model");
    ui.wait_until("the model menu to open", |window, _, _| {
        window.find("config-model").expanded() == Some(true)
    });
    // No screenshot here: the dropdown is open (above), but GPUI Kit 0.9's
    // deferred popover does not show up in the offscreen capture.
    ui.act(|window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    ui.wait_for_thread("the new model", |thread| {
        thread.config().iter().any(|option| {
            option.id() == "model"
                && matches!(option.kind(), ConfigKind::Select { current, .. } if current == "fake/large")
        })
    });
    ui.shot("changed");
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
