use serde_json::json;

use super::*;

fn update(value: serde_json::Value) -> ThreadUpdate {
    ThreadUpdate(serde_json::from_value(value).expect("valid session update"))
}

fn thread() -> Thread {
    Thread::new(SessionId::from(v2::SessionId::new("s1")))
}

fn texts(thread: &Thread) -> Vec<(&'static str, String)> {
    thread
        .entries()
        .iter()
        .map(|entry| match entry {
            Entry::User(m) => ("user", m.text().to_owned()),
            Entry::Agent(m) => ("agent", m.text().to_owned()),
            Entry::Thought(m) => ("thought", m.text().to_owned()),
            Entry::Tool(t) => ("tool", t.title().to_owned()),
            Entry::Notice(n) => ("notice", n.title().to_owned()),
        })
        .collect()
}

/// A real `rho acp` turn (rho 2.32.3): prompt, one bash call, streamed reply.
#[test]
fn recorded_bash_turn_becomes_user_tool_and_agent_rows() {
    let mut thread = thread();
    for line in include_str!("../tests/fixtures/bash_turn.jsonl").lines() {
        thread.apply(update(serde_json::from_str(line).unwrap()));
    }

    let rows = texts(&thread);
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[0].0, "user");
    assert!(rows[0].1.starts_with("Run `echo hello-from-trace`"));
    assert_eq!(rows[1], ("tool", "echo hello-from-trace".to_owned()));
    assert_eq!(rows[2].0, "agent");
    assert!(
        rows[2]
            .1
            .starts_with("The command printed `hello-from-trace`"),
        "{}",
        rows[2].1
    );

    let Entry::Tool(tool) = &thread.entries()[1] else {
        unreachable!()
    };
    assert_eq!(tool.status(), ToolStatus::Done);
    assert!(
        matches!(&tool.content()[0], ToolContent::Text(out) if out.contains("hello-from-trace"))
    );

    assert_eq!(thread.state(), &RunState::Idle);
    let usage = thread.usage().unwrap();
    assert_eq!((usage.used(), usage.size()), (19_837, 1_000_000));
    assert_eq!(usage.cost(), Some((0.002_223, "USD")));
}

/// Resume replay sends each message empty, then its chunks, for messages
/// the host may already hold. Replaying twice must not duplicate text.
#[test]
fn replayed_messages_replace_instead_of_appending() {
    let mut thread = thread();
    for _ in 0..2 {
        thread.apply(update(
            json!({"sessionUpdate": "agent_message", "messageId": "m1", "content": []}),
        ));
        thread.apply(update(
            json!({"sessionUpdate": "agent_message_chunk", "messageId": "m1",
            "content": {"type": "text", "text": "hello "}}),
        ));
        thread.apply(update(
            json!({"sessionUpdate": "agent_message_chunk", "messageId": "m1",
            "content": {"type": "text", "text": "world"}}),
        ));
    }
    assert_eq!(texts(&thread), vec![("agent", "hello world".to_owned())]);

    // An omitted `content` keeps the text; `null` clears it.
    thread.apply(update(
        json!({"sessionUpdate": "agent_message", "messageId": "m1"}),
    ));
    assert_eq!(texts(&thread)[0].1, "hello world");
    thread.apply(update(
        json!({"sessionUpdate": "agent_message", "messageId": "m1", "content": null}),
    ));
    assert_eq!(texts(&thread)[0].1, "");
}

#[test]
fn interleaved_messages_and_tools_keep_first_seen_order() {
    let mut thread = thread();
    let thought = thread.apply(update(
        json!({"sessionUpdate": "agent_thought_chunk", "messageId": "t1",
        "content": {"type": "text", "text": "thinking"}}),
    ));
    let tool = thread.apply(update(
        json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1",
        "title": "read a.rs", "kind": "read", "status": "in_progress"}),
    ));
    let message = thread.apply(update(
        json!({"sessionUpdate": "agent_message_chunk", "messageId": "m1",
        "content": {"type": "text", "text": "done"}}),
    ));
    assert_eq!((thought, tool, message), (Some(0), Some(1), Some(2)));
    // A late update to the first tool edits it in place.
    let late = thread.apply(update(
        json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "failed"}),
    ));
    assert_eq!(late, Some(1), "the update names the row it changed");

    assert_eq!(
        texts(&thread),
        vec![
            ("thought", "thinking".to_owned()),
            ("tool", "read a.rs".to_owned()),
            ("agent", "done".to_owned()),
        ]
    );
    let Entry::Tool(tool) = &thread.entries()[1] else {
        unreachable!()
    };
    assert_eq!(tool.status(), ToolStatus::Failed);
    assert_eq!(tool.title(), "read a.rs", "omitted fields keep their value");
    assert_eq!(tool.kind(), ToolKind::Read);
}

#[test]
fn tool_diffs_keep_paths_and_patch() {
    let mut thread = thread();
    thread.apply(update(
        json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "title": "edit",
        "content": [{"type": "diff",
            "changes": [{"operation": "modify", "path": "/w/a.rs"}],
            "patch": {"format": "git_patch", "text": "--- a/a.rs\n+++ b/a.rs\n"}}]}),
    ));
    let Entry::Tool(tool) = &thread.entries()[0] else {
        unreachable!()
    };
    assert_eq!(
        tool.content(),
        &[ToolContent::Diff {
            paths: vec![PathBuf::from("/w/a.rs")],
            patch: Some("--- a/a.rs\n+++ b/a.rs\n".to_owned()),
        }]
    );
}

/// Real error stop from rho when the provider fails.
#[test]
fn error_stop_reports_the_agents_detail() {
    let mut thread = thread();
    thread.apply(update(
        json!({"sessionUpdate": "state_update", "state": "running"}),
    ));
    assert!(thread.state().is_busy());
    thread.apply(update(
        json!({"sessionUpdate": "state_update", "state": "idle", "stopReason": "error",
        "error": {"code": -32603, "message": "Internal error",
                  "data": "provider failed: provider reported an invalid response"}}),
    ));
    assert_eq!(
        thread.state(),
        &RunState::Failed("provider failed: provider reported an invalid response".to_owned())
    );
    assert!(!thread.state().is_busy());
}

#[test]
fn config_updates_flatten_groups_and_keep_toggles() {
    let mut thread = thread();
    thread.apply(update(json!({"sessionUpdate": "config_option_update", "configOptions": [
        {"configId": "model", "name": "Model", "type": "select", "currentValue": "a/x",
         "options": [{"groupId": "a", "name": "A", "options": [{"value": "a/x", "name": "X"}]}]},
        {"configId": "fast_mode", "name": "Fast", "type": "boolean", "currentValue": true}
    ]})));
    let config = thread.config();
    assert_eq!(config.len(), 2);
    assert_eq!(
        config[0].kind(),
        &ConfigKind::Select {
            current: "a/x".to_owned(),
            choices: vec![ConfigChoice {
                value: "a/x".to_owned(),
                name: "X".to_owned()
            }],
        }
    );
    assert_eq!(config[1].kind(), &ConfigKind::Toggle(true));
}
