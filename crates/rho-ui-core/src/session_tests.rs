use rho_protocol::{
    FastModeOptions, ModelChoice, ModelOptions, ModelRef, PermissionMode, PermissionModeOptions,
    Replay, StreamId,
};
use serde_json::json;

use super::*;
use crate::{
    input::InputChoice,
    options::{ConfigCategory, ConfigChoice, ConfigKind},
};

fn update(value: serde_json::Value) -> SessionUpdate {
    SessionUpdate(serde_json::from_value(value).expect("a valid session event"))
}

fn options() -> SessionOptions {
    let model = ModelRef {
        provider: "a".to_owned(),
        model: "org/x".to_owned(),
    };
    SessionOptions {
        model: ModelOptions {
            current: model.clone(),
            auth: "key".to_owned(),
            available: vec![ModelChoice {
                model,
                label: "X".to_owned(),
            }],
        },
        reasoning: None,
        fast_mode: Some(FastModeOptions { enabled: true }),
        permission_mode: PermissionModeOptions {
            current: PermissionMode::Plan,
            available: vec![PermissionMode::Auto, PermissionMode::Plan],
        },
        commands: Vec::new(),
        added_dirs: Vec::new(),
    }
}

/// An attach response for a session in `state`, whose replay is `replay`
/// events long.
fn attach_response(state: JobState, replay: u64) -> Attached {
    Attached(Box::new(SessionAttachResponse {
        stream: StreamId::new("stream"),
        workspace: PathBuf::from("/w"),
        replay: Replay::Reset,
        replay_events: replay,
        state,
        turn: None,
        turn_elapsed_ms: None,
        inputs: Vec::new(),
        title: None,
        options: options(),
        background: Vec::new(),
    }))
}

fn session() -> Session {
    Session::following(SessionId::from("s1"))
}

fn texts(session: &Session) -> Vec<(&'static str, String)> {
    session
        .entries()
        .iter()
        .map(|entry| match entry {
            Entry::User(m) => ("user", m.text().to_owned()),
            Entry::Agent(m) => ("agent", m.text().to_owned()),
            Entry::Thought(m) => ("thought", m.text().to_owned()),
            Entry::Tool(t) => ("tool", t.title().to_owned()),
            Entry::Notice(n) => ("notice", n.title().to_owned()),
            Entry::Wake => ("wake", String::new()),
        })
        .collect()
}

/// A real turn from a rho daemon built from `daemon-v3` (bf3af5b): prompt,
/// one bash call, streamed reply.
#[test]
fn recorded_bash_turn_becomes_user_tool_and_agent_rows() {
    let mut session = session();
    for line in include_str!("../tests/fixtures/bash_turn.jsonl").lines() {
        session.apply(update(serde_json::from_str(line).unwrap()));
    }

    let rows = texts(&session);
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[0].0, "user");
    assert!(rows[0].1.starts_with("Run `echo hello-from-trace`"));
    assert_eq!(rows[1].0, "tool");
    assert!(rows[1].1.contains("echo hello-from-trace"), "{}", rows[1].1);
    assert_eq!(rows[2].0, "agent");
    assert!(rows[2].1.contains("hello-from-trace"), "{}", rows[2].1);

    let Entry::Tool(tool) = &session.entries()[1] else {
        unreachable!()
    };
    assert_eq!(tool.status(), ToolStatus::Done);
    assert_eq!(tool.kind(), ToolKind::Execute);
    assert!(
        matches!(&tool.content()[0], ToolContent::Text(out) if out.contains("hello-from-trace"))
    );

    assert_eq!(session.state(), &RunState::Idle);
    let usage = session.usage().unwrap();
    assert!(usage.used() > 0 && usage.size().is_some(), "{usage:?}");
}

/// The replay can reach the UI before the attach response does. Nothing of
/// it may be lost, and the response's own state must not undo the replay's.
#[test]
fn events_before_the_attach_response_apply_after_it() {
    let mut session = Session::new(SessionId::from("s1"));
    let early = session.apply(update(
        json!({"type": "agent_message_chunk", "message": "m1", "text": "hello"}),
    ));
    session.apply(update(json!({"type": "state", "state": "running"})));
    assert_eq!(early, None, "held, so it names no row yet");
    assert!(session.entries().is_empty());
    assert!(session.is_replaying());

    session.attached(attach_response(JobState::Idle, 3));

    assert_eq!(texts(&session), vec![("agent", "hello".to_owned())]);
    assert_eq!(session.state(), &RunState::Running);
    assert!(
        session.is_replaying(),
        "one replayed event is still to come"
    );
    session.apply(update(
        json!({"type": "agent_message_chunk", "message": "m1", "text": " world"}),
    ));
    assert!(!session.is_replaying());
    assert_eq!(texts(&session)[0].1, "hello world");
}

#[test]
fn interleaved_messages_and_tools_keep_first_seen_order() {
    let mut session = session();
    let thought = session.apply(update(
        json!({"type": "thought_chunk", "message": "t1", "text": "thinking"}),
    ));
    let tool = session.apply(update(json!({"type": "tool_call", "id": "c1",
        "title": "read a.rs", "kind": "read", "status": "in_progress"})));
    let message = session.apply(update(
        json!({"type": "agent_message_chunk", "message": "m1", "text": "done"}),
    ));
    assert_eq!((thought, tool, message), (Some(0), Some(1), Some(2)));
    // A late update to the first tool edits it in place.
    let late = session.apply(update(
        json!({"type": "tool_call", "id": "c1", "status": "failed"}),
    ));
    assert_eq!(late, Some(1), "the update names the row it changed");

    assert_eq!(
        texts(&session),
        vec![
            ("thought", "thinking".to_owned()),
            ("tool", "read a.rs".to_owned()),
            ("agent", "done".to_owned()),
        ]
    );
    let Entry::Tool(tool) = &session.entries()[1] else {
        unreachable!()
    };
    assert_eq!(tool.status(), ToolStatus::Failed);
    assert_eq!(tool.title(), "read a.rs", "omitted fields keep their value");
    assert_eq!(tool.kind(), ToolKind::Read);
}

/// The daemon sends an edit as the old and new text; viewers read a patch.
#[test]
fn tool_diffs_become_git_patches() {
    let mut session = session();
    session.apply(update(
        json!({"type": "tool_call", "id": "c1", "title": "edit",
        "content": [
            {"type": "diff", "path": "/w/a.rs", "old_text": "one\ntwo\n", "new_text": "one\n2\n"},
            {"type": "diff", "path": "/w/new.rs", "new_text": "fresh\n"},
        ]}),
    ));
    let Entry::Tool(tool) = &session.entries()[0] else {
        unreachable!()
    };
    assert_eq!(
        tool.content(),
        &[
            ToolContent::Diff {
                path: PathBuf::from("/w/a.rs"),
                patch: "diff --git a/w/a.rs b/w/a.rs\n--- a/w/a.rs\n+++ b/w/a.rs\n\
                        @@ -1,2 +1,2 @@\n one\n-two\n+2\n"
                    .to_owned(),
            },
            ToolContent::Diff {
                path: PathBuf::from("/w/new.rs"),
                patch: "diff --git a/w/new.rs b/w/new.rs\n--- /dev/null\n+++ b/w/new.rs\n\
                        @@ -0,0 +1 @@\n+fresh\n"
                    .to_owned(),
            },
        ]
    );
}

/// A failed turn says why in its `turn_ended`; the `state` after it must not
/// wipe that out.
#[test]
fn a_failed_turn_keeps_its_reason_past_the_job_state() {
    let mut session = session();
    session.apply(update(
        json!({"type": "state", "state": "running", "turn": "t1"}),
    ));
    assert!(session.state().is_busy());
    session.apply(update(json!({"type": "turn_ended", "turn": "t1",
        "outcome": {"type": "failed",
                    "error": {"kind": "provider", "message": "provider failed: invalid response"}}})));
    session.apply(update(
        json!({"type": "state", "state": "failed", "turn": "t1"}),
    ));
    assert_eq!(
        session.state(),
        &RunState::Failed("provider failed: invalid response".to_owned())
    );

    // The next turn clears it, and a turn the daemon's restart cut short,
    // which sends no `turn_ended`, still stops being busy.
    session.apply(update(
        json!({"type": "state", "state": "running", "turn": "t2"}),
    ));
    session.apply(update(
        json!({"type": "state", "state": "interrupted", "turn": "t2"}),
    ));
    assert_eq!(session.state(), &RunState::Idle);
}

#[test]
fn options_become_the_settings_the_ui_lays_out() {
    let mut session = session();
    session.apply(update(
        json!({"type": "options_changed", "options": options()}),
    ));
    let config = session.config();
    let shape: Vec<_> = config
        .iter()
        .map(|option| (option.id(), option.category()))
        .collect();
    assert_eq!(
        shape,
        [
            ("mode", ConfigCategory::Mode),
            ("model", ConfigCategory::Model),
            ("fast_mode", ConfigCategory::Other),
        ],
        "no reasoning option: the model's cannot be configured"
    );
    assert_eq!(
        config[0].current_choice().map(ConfigChoice::name),
        Some("Plan")
    );
    assert_eq!(
        config[1].current_choice().map(|c| (c.value(), c.name())),
        Some(("a/org/x", "X"))
    );
    assert_eq!(config[2].kind(), &ConfigKind::Toggle(true));
}

fn approval(id: u64) -> serde_json::Value {
    json!({"type": "input_requested", "request": {"id": id, "ask": {
        "kind": "approval",
        "source": {"kind": "built_in_tool", "name": "bash"},
        "operation": {"kind": "run_process", "program": "cargo", "args": ["test"], "cwd": "/w"},
        "reason": "runs a command"}}})
}

#[test]
fn requests_open_once_and_close_when_any_client_answers() {
    let mut session = session();
    session.apply(update(approval(7)));
    // A reset's replay may repeat a request the attach response listed.
    session.apply(update(approval(7)));
    assert_eq!(session.inputs().len(), 1);
    let request = &session.inputs()[0];
    assert_eq!(request.title(), "Run `cargo test`?");
    assert_eq!(request.description(), Some("runs a command"));
    let ids: Vec<_> = request.choices().iter().map(InputChoice::id).collect();
    assert_eq!(ids, ["allow-once", "allow-session", "deny"]);

    session.apply(update(
        json!({"type": "input_resolved", "id": 7, "resolution": "answered"}),
    ));
    assert!(session.inputs().is_empty());
}

/// The steer's answer and its `user_message` reach the UI on separate
/// paths, in either order.
#[test]
fn a_steer_waits_until_it_applies_and_comes_back_if_the_turn_drops_it() {
    let applied = |id: &str| {
        update(json!({"type": "user_message", "message": format!("u-{id}"),
            "content": [{"type": "text", "text": "use tabs"}], "steer": id}))
    };
    let mut session = session();
    session.apply(update(
        json!({"type": "state", "state": "running", "turn": "t1"}),
    ));

    session.steer_sent(SteerId("a".to_owned()), "use tabs".to_owned());
    let pending: Vec<_> = session.pending_steers().map(|(_, text)| text).collect();
    assert_eq!(pending, ["use tabs"]);
    session.apply(applied("a"));
    assert_eq!(session.pending_steers().count(), 0);

    session.apply(applied("b"));
    session.steer_sent(SteerId("b".to_owned()), "use tabs".to_owned());
    assert_eq!(
        session.pending_steers().count(),
        0,
        "it had already applied"
    );

    session.steer_sent(SteerId("c".to_owned()), "never read".to_owned());
    session.apply(update(
        json!({"type": "turn_ended", "turn": "t1", "outcome": {"type": "cancelled"}}),
    ));
    assert_eq!(session.pending_steers().count(), 0);
    assert_eq!(session.take_dropped_steers(), ["never read"]);
    assert_eq!(session.state(), &RunState::Cancelled);
}

/// A steer's answer can arrive after its whole turn. One the model read
/// must not come back as dropped, and one it never read must not wait on.
#[test]
fn a_steer_answered_after_its_turn_ended_is_settled_at_once() {
    let mut session = session();
    session.apply(update(
        json!({"type": "state", "state": "running", "turn": "t1"}),
    ));
    session.apply(update(json!({"type": "user_message", "message": "u-a",
        "content": [{"type": "text", "text": "read"}], "steer": "a"})));
    session.apply(update(
        json!({"type": "turn_ended", "turn": "t1", "outcome": {"type": "cancelled"}}),
    ));
    session.apply(update(
        json!({"type": "state", "state": "cancelled", "turn": "t1"}),
    ));

    session.steer_sent(SteerId("a".to_owned()), "read".to_owned());
    session.steer_sent(SteerId("b".to_owned()), "unread".to_owned());

    assert_eq!(session.pending_steers().count(), 0);
    assert_eq!(session.take_dropped_steers(), ["unread"]);
}

fn turn(id: &str) -> TurnId {
    TurnId::from_wire(&rho_protocol::TurnId::new(id))
}

/// The answer to a prompt and its turn's events reach the UI on separate
/// paths. Whichever comes first, the session runs for exactly the turn.
#[test]
fn a_prompts_answer_and_its_turns_events_may_come_in_either_order() {
    // The answer first: the session runs before any event says so.
    let mut session = session();
    session.turn_began(&turn("t1"));
    assert_eq!(session.state(), &RunState::Running);
    session.apply(update(
        json!({"type": "state", "state": "running", "turn": "t1"}),
    ));
    session.apply(update(json!({"type": "turn_ended", "turn": "t1",
        "outcome": {"type": "answered", "text": "", "stop": "end_turn"}})));
    assert_eq!(session.state(), &RunState::Idle);

    // The whole turn first: the late answer must not start it again.
    session.turn_began(&turn("t1"));
    assert_eq!(session.state(), &RunState::Idle);

    // A turn the daemon's restart cut short ends without a `turn_ended`.
    session.apply(update(
        json!({"type": "state", "state": "running", "turn": "t2"}),
    ));
    session.apply(update(
        json!({"type": "state", "state": "interrupted", "turn": "t2"}),
    ));
    session.turn_began(&turn("t2"));
    assert_eq!(session.state(), &RunState::Idle);
}

/// A turn the daemon begins by itself, to deliver finished background work,
/// opens with a wake row, so it is a turn of its own. A prompt's turn opens
/// with its prompt, even after a notice the daemon sent first, and a turn
/// of the replay opens with nothing.
#[test]
fn a_turn_without_a_prompt_opens_with_a_wake() {
    let turn = |session: &mut Session, id: &str, first: serde_json::Value| {
        session.apply(update(
            json!({"type": "state", "state": "running", "turn": id}),
        ));
        session.apply(update(first));
        session.apply(update(json!({"type": "turn_ended", "turn": id,
            "outcome": {"type": "answered", "text": "", "stop": "end_turn"}})));
        session.apply(update(
            json!({"type": "state", "state": "completed", "turn": id}),
        ));
    };
    let mut session = Session::new(SessionId::from("s1"));
    session.attached(attach_response(JobState::Idle, 1));
    // Replayed: history, not a turn beginning now.
    session.apply(update(
        json!({"type": "state", "state": "running", "turn": "t0"}),
    ));
    turn(
        &mut session,
        "t1",
        json!({"type": "user_message", "message": "u1", "content": [{"type": "text", "text": "hi"}]}),
    );
    turn(
        &mut session,
        "t2",
        json!({"type": "agent_message_chunk", "message": "m2", "text": "it exited"}),
    );
    // A notice before the prompt's echo, as a settings change sends.
    session.apply(update(
        json!({"type": "state", "state": "running", "turn": "t3"}),
    ));
    session.apply(update(json!({"type": "notice", "text": "advisor changed"})));
    turn(
        &mut session,
        "t3",
        json!({"type": "user_message", "message": "u3", "content": [{"type": "text", "text": "go"}]}),
    );
    let kinds: Vec<&str> = texts(&session).into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(kinds, ["user", "wake", "agent", "notice", "user"]);
}

/// A message refused because a turn was under way is due once a turn ended
/// after it was sent and none runs: not while the refusing turn runs, and
/// at once if that turn ended before the refusal came back.
#[test]
fn a_held_message_is_due_once_a_later_turn_ended() {
    let run = |session: &mut Session, id: &str| {
        session.apply(update(
            json!({"type": "state", "state": "running", "turn": id}),
        ));
    };
    let end = |session: &mut Session, id: &str| {
        session.apply(update(json!({"type": "turn_ended", "turn": id,
            "outcome": {"type": "answered", "text": "", "stop": "end_turn"}})));
        session.apply(update(
            json!({"type": "state", "state": "completed", "turn": id}),
        ));
    };
    let mut session = session();
    run(&mut session, "t1");
    end(&mut session, "t1");

    // Sent after t1; the daemon's own t2 refused it.
    let sent_after = session.ended_turn().cloned();
    run(&mut session, "t2");
    session.hold("one".to_owned(), sent_after);
    session.hold("two".to_owned(), None);
    assert_eq!(session.take_due(), None, "t2 still runs");
    end(&mut session, "t2");
    assert_eq!(session.take_due().as_deref(), Some("one\ntwo"));
    assert_eq!(session.held(), None);

    // The refusing turn ended before the refusal came back.
    run(&mut session, "t3");
    let sent_after = session.ended_turn().cloned();
    end(&mut session, "t3");
    session.hold("three".to_owned(), sent_after);
    assert_eq!(session.take_due().as_deref(), Some("three"));

    // Idle with no turn ended since: the refusing turn's events are late.
    let sent_after = session.ended_turn().cloned();
    session.hold("four".to_owned(), sent_after);
    assert_eq!(session.take_due(), None);
    assert_eq!(session.take_held().as_deref(), Some("four"));
}
