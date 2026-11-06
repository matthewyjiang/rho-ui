//! `Agent` against a scripted ACP v2 agent (`fake_agent.py`): the real
//! process, stdio and JSON-RPC paths, without a model.

use std::{path::Path, time::Duration};

use futures::FutureExt;
use rho_ui_core::{
    Agent, AgentCommand, AgentError, AgentEvent, AgentEvents, ConfigKind, ConfigValue, Entry,
    PermissionKind, PermissionRequest, RunState, Session,
};

fn fake_agent() -> AgentCommand {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_agent.py");
    AgentCommand::new("python3", [script.to_string_lossy().into_owned()])
}

async fn start() -> (Agent, AgentEvents) {
    Agent::start(fake_agent(), |driver| smol::spawn(driver).detach())
        .await
        .expect("fake agent starts")
}

/// The next event, failing the test instead of hanging.
async fn next(events: &AgentEvents) -> AgentEvent {
    let timeout = async {
        smol::Timer::after(Duration::from_secs(5)).await;
        panic!("no event within 5 s");
    };
    futures::select! {
        event = events.recv().fuse() => event.expect("event stream open"),
        never = timeout.fuse() => never,
    }
}

/// Applies updates to `session` until a permission request arrives.
async fn until_permission(events: &AgentEvents, session: &mut Session) -> PermissionRequest {
    loop {
        match next(events).await {
            AgentEvent::Update { update, .. } => {
                session.apply(update);
            }
            AgentEvent::Permission(request) => return request,
            AgentEvent::Exited { error } => panic!("agent exited: {error:?}"),
        }
    }
}

/// Applies updates to `session` until the turn leaves the busy state.
async fn until_idle(events: &AgentEvents, session: &mut Session) {
    while session.state().is_busy() || session.entries().is_empty() {
        match next(events).await {
            AgentEvent::Update { update, .. } => {
                session.apply(update);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

#[test]
fn prompt_turn_with_permission_round_trip() {
    smol::block_on(async {
        let (agent, events) = start().await;
        assert_eq!(
            (agent.info().name(), agent.info().version()),
            ("fake", "9.9.9")
        );

        let setup = agent.new_session(Path::new("/w/a")).await.unwrap();
        let mut session = Session::new(setup.id().clone());
        session.set_setup(setup);
        assert_eq!(session.commands()[0].name(), "compact");

        agent.prompt(session.id(), "list files").await.unwrap();
        let request = until_permission(&events, &mut session).await;
        assert_eq!(session.state(), &RunState::Running);
        assert_eq!(request.title(), "Run `ls`?");
        assert_eq!(request.session(), session.id());
        assert_eq!(request.choices()[0].kind(), PermissionKind::AllowOnce);
        request.choose("yes");

        until_idle(&events, &mut session).await;
        let Some(Entry::Agent(reply)) = session.entries().last() else {
            panic!("{:?}", session.entries())
        };
        assert_eq!(reply.text(), "you chose yes");
    });
}

/// The UI may close a session with an approval still pending; the agent must
/// hear "cancelled" instead of waiting forever.
#[test]
fn dropped_permission_answers_cancelled() {
    smol::block_on(async {
        let (agent, events) = start().await;
        let setup = agent.new_session(Path::new("/w/a")).await.unwrap();
        let mut session = Session::new(setup.id().clone());
        agent.prompt(session.id(), "list files").await.unwrap();
        drop(until_permission(&events, &mut session).await);
        until_idle(&events, &mut session).await;
        let Some(Entry::Agent(reply)) = session.entries().last() else {
            panic!()
        };
        assert_eq!(reply.text(), "you chose cancelled");
    });
}

#[test]
fn list_follows_cursor_and_resume_replays_first() {
    smol::block_on(async {
        let (agent, events) = start().await;
        let sessions = agent.list_sessions().await.unwrap();
        let ids: Vec<_> = sessions.iter().map(|s| s.id().as_str()).collect();
        assert_eq!(ids, ["s1", "old"]);
        assert_eq!(sessions[0].title(), Some("first"));
        assert_eq!(sessions[1].cwd(), Path::new("/w/b"));

        let old = sessions[1].id().clone();
        let mut session = Session::new(old.clone());
        let setup = agent.resume_session(&old, Path::new("/w/b")).await.unwrap();
        session.set_setup(setup);
        // The replay was sent before the response, so it is already queued.
        until_idle(&events, &mut session).await;
        assert!(matches!(&session.entries()[0], Entry::Agent(m) if m.text() == "replayed"));
    });
}

#[test]
fn set_config_returns_the_new_options() {
    smol::block_on(async {
        let (agent, _events) = start().await;
        let setup = agent.new_session(Path::new("/w/a")).await.unwrap();
        let options = agent
            .set_config(setup.id(), "mode", ConfigValue::Select("plan".to_owned()))
            .await
            .unwrap();
        assert!(
            matches!(options[0].kind(), ConfigKind::Select { current, .. } if current == "plan")
        );
    });
}

#[test]
fn refused_prompt_is_an_error_with_the_agents_detail() {
    smol::block_on(async {
        let (agent, _events) = start().await;
        let setup = agent.new_session(Path::new("/w/a")).await.unwrap();
        let error = agent.prompt(setup.id(), "fail").await.unwrap_err();
        assert_eq!(error.to_string(), "Invalid params: busy");
    });
}

#[test]
fn missing_program_fails_startup_with_a_reason() {
    smol::block_on(async {
        let result = Agent::start(AgentCommand::new("/nonexistent/rho", ["acp"]), |driver| {
            smol::spawn(driver).detach();
        })
        .await;
        match result {
            Err(AgentError::Protocol(reason)) => assert!(
                reason.starts_with("couldn't run /nonexistent/rho: "),
                "the reason names the program: {reason}"
            ),
            Err(other) => panic!("expected a reason, got {other:?}"),
            Ok(_) => panic!("started a missing program"),
        }
    });
}

#[test]
fn dropping_the_last_agent_ends_the_process() {
    smol::block_on(async {
        let (agent, events) = start().await;
        drop(agent);
        match next(&events).await {
            AgentEvent::Exited { error: None } => {}
            other => panic!("expected a clean exit, got {other:?}"),
        }
    });
}
