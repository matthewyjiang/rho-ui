//! The app against a real rho daemon and model; see [`live_daemon`].

use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use rho_ui_core::{BackgroundStatus, DaemonTarget, Entry, RunState, Session, ToolStatus};

use super::{is_visible, labels, Harness, Machines};
use crate::{projects::Projects, shelf::Shelves, store::StateDb};

/// The app against a real rho daemon and model, not a pass/fail unit test:
/// it proves the wire works end to end, and saves screenshots. It starts
/// the daemon through `RHO_UI_LIVE_RHO` (a rho 3 binary) in the
/// `RHO_HOME` it inherits, runs a turn of its
/// own through an approval, then attaches to the session
/// `RHO_UI_LIVE_ATTACH` names, one whose turn rho's terminal UI started
/// and left waiting on an approval, answers it, and steers the turn.
/// docs/testing.md has the full recipe.
#[test]
#[ignore = "needs a real rho daemon and a signed-in model; see docs/testing.md"]
fn live_daemon() {
    /// A model turn with a tool call, with room for a slow provider.
    const TURN: Duration = Duration::from_secs(180);

    let rho = std::env::var("RHO_UI_LIVE_RHO").expect("RHO_UI_LIVE_RHO names a rho 3 program");
    let target = DaemonTarget::rho(rho).expect("RHO_HOME or HOME names the rho home");
    let mut ui = Harness::open(
        "live",
        target,
        None,
        Shelves::default(),
        Projects::default(),
    );
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
        eprintln!("RHO_UI_LIVE_ATTACH is unset: skipping the attach half");
        return;
    };
    let row: &'static str = Box::leak(format!("session-local-{attach}").into_boxed_str());
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

/// A real model's turn, with room for a slow provider.
const BACKGROUND_TURN: Duration = Duration::from_secs(240);

/// Background work against a real rho daemon and model, not a pass/fail
/// unit test: a turn starts two background processes and ends while they
/// run; the list shows them; stopping one lists it as stopped while the
/// daemon's own turn tells the model; a message sent during that turn is
/// taken without an error; the other process exits by itself, shows its
/// exit status, and leaves the list once the daemon's turns end, the last
/// in a reply. Runs as [`live_daemon`] does, through `RHO_UI_LIVE_RHO`; it takes
/// a few minutes.
#[test]
#[ignore = "needs a real rho daemon and a signed-in model; see docs/testing.md"]
fn live_background() {
    let rho = std::env::var("RHO_UI_LIVE_RHO").expect("RHO_UI_LIVE_RHO names a rho 3 program");
    let target = DaemonTarget::rho(rho).expect("RHO_HOME or HOME names the rho home");
    let mut ui = Harness::open(
        "live-bg",
        target,
        None,
        Shelves::default(),
        Projects::default(),
    );
    let long = start_two_processes(&mut ui);
    stop_and_follow_the_daemons_turn(&mut ui, long);
    follow_the_exit(&mut ui);
}

fn idle(session: &Session) -> bool {
    session.state() == &RunState::Idle
}

fn prompts(session: &Session) -> usize {
    let users = session.entries().iter();
    users
        .filter(|entry| matches!(entry, Entry::User(_)))
        .count()
}

/// Runs a turn that starts `sleep 120 && echo first-done` and `sleep 600`,
/// approving whatever it asks. Returns the row of `sleep 600`.
fn start_two_processes(ui: &mut Harness) -> usize {
    ui.wait_within(BACKGROUND_TURN, "the daemon", |window, _, _| {
        is_visible(window, "new-session")
    });
    ui.click("new-session");
    ui.wait_within(BACKGROUND_TURN, "the draft's settings", |window, _, _| {
        is_visible(window, "draft-view") && is_visible(window, "config-panel")
    });
    ui.send(
        "Use your process tool to start two background processes: `sleep 120 && echo first-done` \
         and `sleep 600`. Don't wait for them or check on them. Then reply with one short \
         sentence and end your turn.",
    );
    ui.wait_within(BACKGROUND_TURN, "the prompt's turn", |_, cx, app| {
        app.active_session().is_some_and(|view| {
            let session = view.read(cx).session();
            session.state().is_busy() && !session.entries().is_empty()
        })
    });
    loop {
        ui.wait_within(
            BACKGROUND_TURN,
            "an approval or the turn's end",
            |window, cx, app| {
                is_visible(window, "approval-allow-once")
                    || app
                        .active_session()
                        .is_some_and(|view| idle(view.read(cx).session()))
            },
        );
        if ui.label("approval-allow-once").is_none() && idle(&ui.session()) {
            break;
        }
        ui.click("approval-allow-once");
    }
    let session = ui.session();
    let listed: Vec<_> = session
        .background()
        .iter()
        .map(|work| (work.title().to_owned(), work.status()))
        .collect();
    eprintln!("after the turn: {listed:?}");
    let running = listed
        .iter()
        .filter(|(_, status)| *status == BackgroundStatus::Running);
    assert_eq!(
        running.count(),
        2,
        "the turn ended with both running: {listed:?}"
    );
    ui.shot("turn-ended");
    listed
        .iter()
        .position(|(title, _)| title.contains("600"))
        .expect("the long process is listed")
}

/// Stops row `long`, sees it end as stopped and the daemon's own turn
/// begin, and sends a message during that turn.
fn stop_and_follow_the_daemons_turn(ui: &mut Harness, long: usize) {
    let stop: &'static str = Box::leak(format!("background-stop-{long}").into_boxed_str());
    let sent = prompts(&ui.session());
    ui.click(stop);
    ui.wait_within(BACKGROUND_TURN, "the stopped process", |_, cx, app| {
        app.active_session()
            .is_some_and(|view| ended(view.read(cx).session(), "600", BackgroundStatus::Cancelled))
    });
    ui.click("background-history-button");
    eprintln!("stopped: {:?}", labels_of(ui, ": Stopped"));
    ui.act(|window, cx| window.press("escape", cx));
    ui.shot("stopped");
    // Sent as soon as the daemon's turn begins. Its wake row comes with
    // its first model output, which can come just before it ends.
    ui.wait_within(BACKGROUND_TURN, "the daemon's own turn", |_, cx, app| {
        app.active_session()
            .is_some_and(|view| view.read(cx).session().state().is_busy())
    });
    assert_eq!(
        prompts(&ui.session()),
        sent,
        "no prompt from here started it"
    );
    ui.shot("daemon-turn");
    ui.send("Also end your next reply with the word FOLLOWED.");
    ui.shot("sent-during-daemon-turn");
    ui.wait_within(BACKGROUND_TURN, "the daemon's turn to end", |_, cx, app| {
        app.active_session().is_some_and(|view| {
            let view = view.read(cx);
            idle(view.session()) && view.session().held().is_none()
        })
    });
    ui.act(|window, _| {
        assert!(!is_visible(window, "turn-error"), "no failed turn");
        assert!(!is_visible(window, "session-local-error"), "no error");
    });
    let session = ui.session();
    let followed = session
        .entries()
        .iter()
        .any(|entry| matches!(entry, Entry::User(message) if message.text().contains("FOLLOWED")));
    assert!(followed, "the message sent during the daemon's turn joined");
    let wakes = session
        .entries()
        .iter()
        .filter(|entry| matches!(entry, Entry::Wake));
    assert!(wakes.count() >= 1, "the daemon's turn opened with a wake");
    ui.shot("stop-delivered");
}

/// Waits for `sleep 120` to exit by itself, then for the list to empty and
/// the session to go idle after a reply. It prints how many turns the
/// daemon started and what the last one replied, for a person to check
/// that a turn told the model of the exit: the wakes can't tell which
/// event started them.
fn follow_the_exit(ui: &mut Harness) {
    ui.wait_within(BACKGROUND_TURN, "the exited process", |_, cx, app| {
        app.active_session().is_some_and(|view| {
            ended(
                view.read(cx).session(),
                "first-done",
                BackgroundStatus::Completed,
            )
        })
    });
    ui.click("background-history-button");
    eprintln!("exited: {:?}", labels_of(ui, ": Exited "));
    ui.shot("exited");
    ui.act(|window, cx| window.press("escape", cx));
    ui.wait_within(
        BACKGROUND_TURN,
        "the list to empty and the turn to end",
        |_, cx, app| {
            app.active_session().is_some_and(|view| {
                let session = view.read(cx).session();
                idle(session) && session.background().is_empty()
            })
        },
    );
    let session = ui.session();
    let wakes = session
        .entries()
        .iter()
        .filter(|entry| matches!(entry, Entry::Wake));
    eprintln!("daemon-started turns: {}", wakes.count());
    let Some(Entry::Agent(reply)) = session.entries().last() else {
        panic!("the daemon's turn ends in a reply: {:?}", session.entries());
    };
    eprintln!("the daemon's last turn replied: {}", reply.text());
    ui.shot("done");
}

/// Whether `session` saw work titled with `part` end as `status`.
fn ended(session: &Session, part: &str, status: BackgroundStatus) -> bool {
    session
        .ended_background()
        .iter()
        .any(|work| work.title().contains(part) && work.status() == status)
}

/// Every drawn accessibility label holding `part`.
fn labels_of(ui: &mut Harness, part: &str) -> Vec<String> {
    ui.cx
        .update_window(ui.window, |_, window, _| labels(window))
        .expect("window is open")
        .into_iter()
        .filter(|label| label.contains(part))
        .collect()
}

/// A lead against a real rho daemon and model, not a pass/fail unit test:
/// a lead started from the sidebar makes its folder, starts a worker
/// session in the git repository `RHO_UI_LIVE_REPO` names and ends its
/// turn while the worker sleeps; when the worker's turn ends, the daemon's
/// own turn tells the lead, with no prompt from here. It prints the
/// entries of both turns, what the lead replied and the fold line the
/// sidebar shows, for a person to check. Runs as [`live_daemon`] does,
/// through `RHO_UI_LIVE_RHO`, a rho with the `lead` agent, in a rho home
/// set to `bypass`; it takes a few minutes.
#[test]
#[ignore = "needs a real rho daemon and a signed-in model; see docs/testing.md"]
fn live_lead() {
    /// A lead's turn and its worker's, with room for a slow provider: the
    /// whole test took two minutes when written, the worker's sleep
    /// included.
    const LEAD_TURN: Duration = Duration::from_mins(6);

    let rho = std::env::var("RHO_UI_LIVE_RHO").expect("RHO_UI_LIVE_RHO names a rho 3 program");
    let repo = std::env::var("RHO_UI_LIVE_REPO").expect("RHO_UI_LIVE_REPO names a git repository");
    let target = DaemonTarget::rho(rho).expect("RHO_HOME or HOME names the rho home");
    let machines = Machines {
        rho_home: rho_ui_core::platform::rho_home(),
        ..Machines::default()
    };
    let mut ui = Harness::open_on("live-lead", target, None, machines, StateDb::in_memory());
    ui.wait_within(LEAD_TURN, "the daemon", |window, _, _| {
        is_visible(window, "new-lead")
    });
    ui.click("new-lead");
    ui.wait_within(LEAD_TURN, "the lead's draft", |_, cx, app| {
        app.draft()
            .is_some_and(|draft| draft.read(cx).session_key().is_some())
    });
    ui.send(&format!(
        "Goal: in the git repository at {repo}, add a file HELLO.md whose only line is `hello \
         from a worker`, committed on a new branch. The repository has no remote: push nothing \
         and open no pull request. Use exactly one worker session and ask me nothing. Tell the \
         worker to run `sleep 90` before anything else, so that your turn ends while it works. \
         Once the worker has committed, reply with the name of its branch and stop."
    ));
    ui.wait_within(LEAD_TURN, "the lead's first turn to end", |_, cx, app| {
        app.active_session().is_some_and(|view| {
            let session = view.read(cx).session();
            idle(session) && matches!(session.entries().last(), Some(Entry::Agent(_)))
        })
    });
    let first = ui.session();
    let (sent, said) = (prompts(&first), first.entries().len());
    eprintln!("the lead's turn: {:?}", kinds(&first, 0));
    ui.shot("started");
    ui.wait_within(LEAD_TURN, "the daemon's own turn to end", |_, cx, app| {
        app.active_session().is_some_and(|view| {
            let session = view.read(cx).session();
            let entries = session.entries();
            entries.len() > said && idle(session) && matches!(entries.last(), Some(Entry::Agent(_)))
        })
    });
    let session = ui.session();
    eprintln!("the daemon's turn: {:?}", kinds(&session, said));
    assert_eq!(prompts(&session), sent, "no prompt from here started it");
    let Some(Entry::Agent(reply)) = session.entries().last() else {
        panic!("the daemon's turn ends in a reply: {:?}", session.entries());
    };
    eprintln!("the lead replied: {}", reply.text());
    eprintln!("the sidebar: {:?}", labels_of(&mut ui, " session"));
    ui.shot("told");
}

/// What each of `session`'s entries from `from` on is, to print.
fn kinds(session: &Session, from: usize) -> Vec<&'static str> {
    session.entries()[from..]
        .iter()
        .map(|entry| match entry {
            Entry::User(_) => "user",
            Entry::Agent(_) => "agent",
            Entry::Thought(_) => "thought",
            Entry::Tool(_) => "tool",
            Entry::Notice(_) => "notice",
            Entry::Compaction(_) => "compaction",
            Entry::Surface(_) => "surface",
            Entry::Wake => "wake",
        })
        .collect()
}
