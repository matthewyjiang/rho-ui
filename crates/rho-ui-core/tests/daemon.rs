//! `Daemon` against the scripted fake daemon: rho-protocol's real server
//! side over a real Unix socket, without a model. The client side runs on
//! smol's executor, not Tokio, as it runs on GPUI's in the app.

use std::{path::Path, time::Duration};

use futures::FutureExt;
use rho_ui_core::{
    fake_daemon::{FakeDaemon, Script, TuiJob, TUI_SESSION},
    ChoiceKind, ConfigKind, ConfigValue, Daemon, DaemonError, DaemonEvent, DaemonEvents,
    DaemonTarget, Detached, Entry, JobActivity, RunState, Session, SessionId, Unsteered,
};
use smol::block_on;

async fn connect(fake: &FakeDaemon) -> (Daemon, DaemonEvents) {
    Daemon::connect(fake.target())
        .await
        .expect("the fake daemon answers the handshake")
}

/// The next event, failing the test instead of hanging.
async fn next(events: &mut DaemonEvents) -> DaemonEvent {
    let timeout = async {
        smol::Timer::after(Duration::from_secs(5)).await;
        panic!("no event within 5 s");
    };
    futures::select! {
        event = events.recv().fuse() => event.expect("the event stream is open"),
        never = timeout.fuse() => never,
    }
}

/// Applies `session`'s events until `done` holds.
async fn until(events: &mut DaemonEvents, session: &mut Session, done: impl Fn(&Session) -> bool) {
    while !done(session) {
        match next(events).await {
            // Routed by session, as the app does; these tests follow one.
            DaemonEvent::Update {
                session: id,
                update,
            } => {
                if id == *session.id() {
                    session.apply(update);
                }
            }
            other @ DaemonEvent::Detached { .. } => panic!("unexpected event: {other:?}"),
        }
    }
}

/// Opens a new session and follows it.
async fn new_session(daemon: &Daemon) -> Session {
    let setup = daemon
        .new_session(Path::new("/w/a"))
        .await
        .expect("the fake opens a session");
    let mut session = Session::from_setup(setup);
    let attached = daemon.attach(session.id()).await;
    session.attached(attached.expect("the fake attaches to a session it opened"));
    session
}

fn last_reply(session: &Session) -> &str {
    match session.entries().last() {
        Some(Entry::Agent(reply)) => reply.text(),
        other => panic!("the transcript ends in {other:?}"),
    }
}

#[test]
fn a_prompt_runs_a_turn_through_an_approval() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        assert_eq!(daemon.info().notice(), None, "the fake is this build");

        let mut session = new_session(&daemon).await;
        assert_eq!(session.commands()[0].name(), "compact");

        daemon.prompt(session.id(), "list files").await.unwrap();
        until(&mut events, &mut session, |s| !s.inputs().is_empty()).await;
        until(&mut events, &mut session, |s| {
            s.state() == &RunState::NeedsAction
        })
        .await;
        let request = session.inputs()[0].clone();
        assert_eq!(request.title(), "Run `ls`?");
        assert_eq!(request.choices()[0].kind(), ChoiceKind::Accept);
        let allow = request.choice("allow-once").unwrap();
        daemon
            .answer(session.id(), request.id(), allow)
            .await
            .unwrap();
        // A second answer finds the request closed, which is not an error.
        daemon
            .answer(session.id(), request.id(), allow)
            .await
            .unwrap();

        until(&mut events, &mut session, |s| !s.state().is_busy()).await;
        assert!(session.inputs().is_empty(), "the daemon closed the request");
        assert_eq!(last_reply(&session), "you chose allow_once");
        assert_eq!(session.state(), &RunState::Idle);
        let usage = session.usage().unwrap();
        assert_eq!((usage.used(), usage.size()), (2500, Some(10_000)));
        assert_eq!(usage.cost_usd(), Some(0.01));
    });
}

#[test]
fn cancel_withdraws_the_approval_and_ends_the_turn() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let mut session = new_session(&daemon).await;
        daemon.prompt(session.id(), "list files").await.unwrap();
        until(&mut events, &mut session, |s| !s.inputs().is_empty()).await;

        daemon.cancel(session.id()).await.unwrap();

        until(&mut events, &mut session, |s| !s.state().is_busy()).await;
        assert_eq!(session.state(), &RunState::Cancelled);
        assert!(session.inputs().is_empty());
    });
}

#[test]
fn list_follows_the_cursor_and_attach_replays_the_conversation() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let sessions = daemon.list_sessions().await.unwrap();
        let ids: Vec<_> = sessions.iter().map(|s| s.id().as_str()).collect();
        assert_eq!(ids, ["s1", "old"]);
        assert_eq!(sessions[0].title(), Some("first"));
        assert_eq!(sessions[1].cwd(), Path::new("/w/b"));

        // A prompted new session is saved, so with three the list takes
        // more than one page.
        let mut new = new_session(&daemon).await;
        daemon.prompt(new.id(), "list files").await.unwrap();
        until(&mut events, &mut new, |s| !s.inputs().is_empty()).await;
        let sessions = daemon.list_sessions().await.unwrap();
        let ids: Vec<_> = sessions.iter().map(|s| s.id().as_str()).collect();
        assert_eq!(ids, ["new-1", "s1", "old"]);

        let mut old = Session::new(sessions[2].id().clone());
        old.attached(daemon.attach(old.id()).await.unwrap());
        until(&mut events, &mut old, |s| !s.is_replaying()).await;
        assert!(matches!(old.entries(), [Entry::Agent(m)] if m.text() == "replayed"));
    });
}

#[test]
fn a_job_another_client_started_is_listed_attached_and_steered() {
    let fake = FakeDaemon::start(Script {
        tui_job: TuiJob::Running,
        ..Script::default()
    });
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let jobs = daemon.active_jobs().await.unwrap();
        let [job] = jobs.as_slice() else {
            panic!("one job runs: {jobs:?}")
        };
        assert_eq!(job.session().as_str(), TUI_SESSION);
        assert_eq!(job.title(), Some("from the tui"));
        assert_eq!(job.activity(), JobActivity::Working);
        let ran = Duration::from_secs(90);
        assert!(job.turn_elapsed() >= Some(ran), "{:?}", job.turn_elapsed());

        let mut session = Session::new(job.session().clone());
        let attached = daemon.attach(session.id()).await.unwrap();
        assert!(attached.turn_elapsed() >= Some(ran));
        session.attached(attached);
        until(&mut events, &mut session, |s| !s.is_replaying()).await;
        assert_eq!(session.title(), Some("from the tui"));
        assert_eq!(session.state(), &RunState::Running);
        assert_eq!(last_reply(&session), "Reading the parser.");

        // A steer the model has not read can be taken back.
        let waiting = daemon.steer(session.id(), "wait").await.unwrap();
        session.steer_sent(waiting.clone(), "wait".to_owned());
        let withdrawn = daemon.unsteer(session.id(), &waiting).await.unwrap();
        assert_eq!(withdrawn, Unsteered::Withdrawn);
        assert_eq!(session.withdraw_steer(&waiting).as_deref(), Some("wait"));

        let steer = daemon
            .steer(session.id(), "keep the old API")
            .await
            .unwrap();
        session.steer_sent(steer, "keep the old API".to_owned());
        until(&mut events, &mut session, |s| !s.state().is_busy()).await;
        assert_eq!(last_reply(&session), "steered: keep the old API");
        assert!(
            matches!(&session.entries()[session.entries().len() - 2],
                Entry::User(m) if m.text() == "keep the old API"),
            "the steer joined the conversation: {:?}",
            session.entries()
        );
        assert_eq!(session.pending_steers().count(), 0);
        assert!(session.take_dropped_steers().is_empty());
        assert!(daemon.active_jobs().await.unwrap().is_empty());

        // Between turns nothing takes a steer.
        let refused = daemon.steer(session.id(), "too late").await.unwrap_err();
        assert_eq!(
            refused,
            DaemonError::Refused("no model request is taking input".to_owned())
        );
    });
}

#[test]
fn set_config_returns_the_new_options() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, _events) = connect(&fake).await;
        let session = new_session(&daemon).await;
        let options = daemon
            .set_config(session.id(), "mode", ConfigValue::Select("plan".to_owned()))
            .await
            .unwrap();
        assert!(
            matches!(options[0].kind(), ConfigKind::Select { current, .. } if current == "plan")
        );
        let refused = daemon
            .set_config(session.id(), "mode", ConfigValue::Toggle(true))
            .await
            .unwrap_err();
        assert!(matches!(refused, DaemonError::Refused(_)), "{refused:?}");
    });
}

#[test]
fn a_refused_prompt_is_an_error_in_the_daemons_words() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, _events) = connect(&fake).await;
        let session = new_session(&daemon).await;
        let error = daemon.prompt(session.id(), "fail").await.unwrap_err();
        assert_eq!(error.to_string(), "the session is busy");
    });
}

#[test]
fn deleting_a_followed_session_detaches_it() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let id = SessionId::from("s1");
        let mut session = Session::new(id.clone());
        session.attached(daemon.attach(&id).await.unwrap());

        daemon.delete_session(&id).await.unwrap();

        match next(&mut events).await {
            DaemonEvent::Detached { session, reason } => {
                assert_eq!((session, reason), (id, Detached::Deleted));
            }
            other @ DaemonEvent::Update { .. } => panic!("expected the detach, got {other:?}"),
        }
        let gone = daemon.attach(&SessionId::from("s1")).await.unwrap_err();
        assert_eq!(gone, DaemonError::Refused("no session s1".to_owned()));
    });
}

#[test]
fn a_missing_rho_fails_the_connection_with_a_reason() {
    let target = DaemonTarget::Rho("/nonexistent/rho".into());
    match block_on(Daemon::connect(target)) {
        Err(DaemonError::Unreachable(reason)) => assert!(
            reason.starts_with("couldn't run /nonexistent/rho daemon start: "),
            "the reason names the command: {reason}"
        ),
        other => panic!("expected a reason, got {other:?}"),
    }
}

#[test]
fn the_stream_ends_when_the_connection_does() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        drop(daemon);
        let timeout = async {
            smol::Timer::after(Duration::from_secs(5)).await;
            panic!("the stream stayed open for 5 s");
        };
        let ended = futures::select! {
            event = events.recv().fuse() => event,
            never = timeout.fuse() => never,
        };
        assert!(ended.is_none(), "{ended:?}");
    });
}
