//! The app against a real rho daemon and model; see [`live_daemon`].

use std::{path::PathBuf, time::Duration};

use gpui_kit::test::TestWindowExt as _;
use rho_ui_core::{DaemonTarget, Entry, RunState, ToolStatus};

use super::{is_visible, Harness};
use crate::shelf::Shelves;

/// The app against a real rho daemon and model, not a pass/fail unit test:
/// it proves the wire works end to end, and saves screenshots. It starts
/// the daemon through `RHO_UI_LIVE_RHO` (a rho built from the pinned
/// `daemon-v3` commit) in the `RHO_HOME` it inherits, runs a turn of its
/// own through an approval, then attaches to the session
/// `RHO_UI_LIVE_ATTACH` names, one whose turn rho's terminal UI started
/// and left waiting on an approval, answers it, and steers the turn.
/// The README's Test section has the full recipe.
#[test]
#[ignore = "needs a real rho daemon and a signed-in model; see README"]
fn live_daemon() {
    /// A model turn with a tool call, with room for a slow provider.
    const TURN: Duration = Duration::from_secs(180);

    let rho = std::env::var("RHO_UI_LIVE_RHO").expect("RHO_UI_LIVE_RHO names a rho 3 program");
    let target = DaemonTarget::Rho(PathBuf::from(rho));
    let mut ui = Harness::open("live", target, None, Shelves::default());
    ui.wait_within(TURN, "the daemon", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.shot("connected");

    ui.click("new-session");
    ui.wait_within(TURN, "the draft's settings", |window, _, _| {
        is_visible(window, "draft-view") && is_visible(window, "config-panel")
    });
    ui.act(|window, cx| {
        window.input(
            "Run `echo hello-from-rho-ui` with your bash tool, then reply with one short sentence.",
            cx,
        );
        window.press("enter", cx);
    });
    ui.wait_within(TURN, "the approval card", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.shot("approval");
    ui.click("approval-allow-once");
    ui.wait_within(TURN, "the turn to finish", |_, cx, app| {
        app.active_session().is_some_and(|view| {
            let session = view.read(cx).session();
            session.state() == &RunState::Idle
                && matches!(session.entries().last(), Some(Entry::Agent(_)))
        })
    });
    let session = ui.session();
    let ran = session.entries().iter().any(|entry| {
        matches!(entry, Entry::Tool(tool) if tool.status() == ToolStatus::Done
            && tool.title().contains("hello-from-rho-ui"))
    });
    assert!(ran, "the command ran: {:?}", session.entries());
    assert!(session.usage().is_some(), "the daemon reported usage");
    ui.shot("turn-done");

    let Ok(attach) = std::env::var("RHO_UI_LIVE_ATTACH") else {
        return;
    };
    let row: &'static str = Box::leak(format!("session-{attach}").into_boxed_str());
    ui.wait_within(
        TURN,
        "the terminal UI's job in the sidebar",
        |window, _, _| is_visible(window, row),
    );
    ui.shot("job-listed");
    ui.click(row);
    ui.wait_within(TURN, "its open approval", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.shot("job-attached");
    ui.click("approval-allow-once");
    ui.wait_within(TURN, "the turn to run on", |_, cx, app| {
        app.active_session()
            .is_some_and(|view| view.read(cx).session().state() == &RunState::Running)
    });
    ui.act(|window, cx| {
        window.input("When you answer, end with the word STEERED.", cx);
        window.press("enter", cx);
    });
    ui.shot("job-steered");
    ui.wait_within(TURN, "the steered turn to finish", |_, cx, app| {
        app.active_session()
            .is_some_and(|view| view.read(cx).session().state() == &RunState::Idle)
    });
    let session = ui.session();
    let steered = session
        .entries()
        .iter()
        .any(|entry| matches!(entry, Entry::User(message) if message.text().contains("STEERED")));
    assert!(steered, "the steer joined the conversation");
    let Some(Entry::Agent(reply)) = session.entries().last() else {
        panic!("the turn ends in a reply: {:?}", session.entries());
    };
    assert!(
        reply.text().contains("STEERED"),
        "the model read it: {}",
        reply.text()
    );
    ui.shot("job-done");
}
