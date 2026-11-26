//! `Daemon` against the scripted fake daemon: rho-protocol's real server
//! side over a real Unix socket, without a model. The client side runs on
//! smol's executor, not Tokio, as it runs on GPUI's in the app.

use std::{path::Path, time::Duration};

use futures::FutureExt;
use rho_ui_core::{
    agent_settings::{SettingChange, SettingKey, SettingValue},
    fake_daemon::{
        surface::{FORM_SURFACE, REFUSED_EVENT},
        FakeDaemon, PullRequestMethods, ReportMethod, Script, SessionLists, Started, TuiJob,
        KEY_AUTH, LOGIN_AUTH, LOGIN_URL, TUI_SESSION,
    },
    surface::{Node, SurfaceStore},
    AgentSettings, Answer, Attachment, AuthStatus, BackgroundKind, BackgroundStatus,
    BackgroundWork, Checkout, ChoiceKind, Choices, Continuity, Daemon, DaemonError, DaemonEvent,
    DaemonEvents, DaemonTarget, Detached, Entry, Form, Image, ImageFormat, JobActivity,
    LoginOutcome, ModelRef, NewSessionMark, OptionChange, PermissionMode, Prompt, PromptKey,
    PullRequestError, PullRequestId, ReportError, RestartCost, RunState, Session,
    SessionCompactionReport, SessionContextReport, SessionHooksReport, SessionId, SessionMark,
    SessionProviderContextReport, SessionSummary, StartedMode, StartedSessionChange,
    StartedSessionDefaults, SurfaceActionError, ToolStatus, Unsteered, VersionFix, WorktreeVerdict,
    MAX_PROMPT_ATTACHMENT_BYTES,
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
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

/// Applies changes to the agent settings until `done` holds.
async fn settings_until(
    events: &mut DaemonEvents,
    settings: &mut AgentSettings,
    done: impl Fn(&AgentSettings) -> bool,
) {
    while !done(settings) {
        match next(events).await {
            DaemonEvent::AgentSettingsChanged(changed) => settings.apply(changed),
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

/// rho's settings follow every write, this app's and a hand edit's, and a
/// write is refused as a conflict only when someone else wrote since its
/// read: this app's own second write, sent before it heard of its first,
/// lands.
#[test]
fn agent_settings_follow_writes_and_refuse_stale_ones() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let mut settings = daemon
            .agent_settings()
            .await
            .expect("the fake describes its settings");
        let compact = SettingKey::new("general.auto_compact");
        assert_eq!(settings.value(&compact), Some(&SettingValue::Bool(true)));
        assert!(!settings.is_set(&compact), "at its default");

        let read = settings.revision().clone();
        let off = SettingChange::Set {
            key: compact.clone(),
            value: SettingValue::Bool(false),
        };
        daemon
            .set_agent_settings(read.clone(), vec![off])
            .await
            .expect("a write against the current revision lands");
        assert_eq!(
            fake.agent_setting(compact.as_str()),
            Some(SettingValue::Bool(false))
        );
        let shell = SettingKey::new("tools.shell");
        let zsh = SettingChange::Set {
            key: shell.clone(),
            value: SettingValue::Text("zsh".to_owned()),
        };
        daemon
            .set_agent_settings(read, vec![zsh])
            .await
            .expect("only this app wrote since its read");
        settings_until(&mut events, &mut settings, |settings| {
            settings.value(&shell) == Some(&SettingValue::Text("zsh".to_owned()))
        })
        .await;
        assert_eq!(settings.value(&compact), Some(&SettingValue::Bool(false)));
        assert!(settings.is_set(&compact), "the file sets it now");

        // A hand edit reaches this client, and makes a write against the
        // revision before it a conflict.
        let before_edit = settings.revision().clone();
        fake.edit_agent_setting("tools.shell", SettingValue::Text("fish".to_owned()));
        settings_until(&mut events, &mut settings, |settings| {
            settings.value(&shell) == Some(&SettingValue::Text("fish".to_owned()))
        })
        .await;
        let reset = SettingChange::Reset {
            key: compact.clone(),
        };
        let stale = daemon
            .set_agent_settings(before_edit, vec![reset.clone()])
            .await;
        assert!(matches!(stale, Err(DaemonError::Conflict(_))), "{stale:?}");
        assert_eq!(
            fake.agent_setting(compact.as_str()),
            Some(SettingValue::Bool(false)),
            "a refused write changes nothing"
        );

        daemon
            .set_agent_settings(settings.revision().clone(), vec![reset])
            .await
            .expect("a write against the edited revision lands");
        settings_until(&mut events, &mut settings, |settings| {
            !settings.is_set(&compact)
        })
        .await;
        assert_eq!(
            settings.value(&compact),
            Some(&SettingValue::Bool(true)),
            "its default"
        );
    });
}

/// Opens a new session and follows it.
async fn new_session(daemon: &Daemon) -> Session {
    let setup = daemon
        .new_session(Path::new("/w/a"), Checkout::Local)
        .await
        .expect("the fake opens a session");
    let mut session = Session::from_setup(setup);
    let attached = daemon.attach(session.id(), None).await;
    session.attached(attached.expect("the fake attaches to a session it opened"));
    session
}

fn last_reply(session: &Session) -> &str {
    match session.entries().last() {
        Some(Entry::Agent(reply)) => reply.text(),
        other => panic!("the transcript ends in {other:?}"),
    }
}

/// The next event that isn't a session's.
async fn next_login(events: &mut DaemonEvents) -> DaemonEvent {
    loop {
        match next(events).await {
            DaemonEvent::Update { .. } => {}
            other => return other,
        }
    }
}

/// A session whose model has no credentials still opens, signed out, and
/// a login started through the daemon reaches this client: its prompt,
/// then how it ended, whether approved or cancelled.
#[test]
fn a_signed_out_session_opens_and_a_login_is_relayed() {
    let fake = FakeDaemon::start(Script {
        signed_out: true,
        ..Script::default()
    });
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let session = new_session(&daemon).await;
        assert_eq!(session.auth(), Some(KEY_AUTH));

        // The fake is this machine's, so the login opens the browser: no
        // code to enter.
        let cancelled = daemon.login(LOGIN_AUTH).await.expect("a login starts");
        match next_login(&mut events).await {
            DaemonEvent::LoginPrompt { login, prompt } => {
                assert_eq!(
                    (login, prompt.url, prompt.user_code),
                    (cancelled.clone(), LOGIN_URL.to_owned(), None)
                );
            }
            other => panic!("expected the login's prompt, got {other:?}"),
        }
        daemon
            .cancel_login(&cancelled)
            .await
            .expect("the login stops");
        match next_login(&mut events).await {
            DaemonEvent::LoginFinished { login, outcome } => {
                assert_eq!((login, outcome), (cancelled, LoginOutcome::Cancelled));
            }
            other => panic!("expected the login to end, got {other:?}"),
        }

        let approved = daemon
            .login(LOGIN_AUTH)
            .await
            .expect("a second login starts");
        assert!(matches!(
            next_login(&mut events).await,
            DaemonEvent::LoginPrompt { .. }
        ));
        fake.approve_login();
        match next_login(&mut events).await {
            DaemonEvent::LoginFinished { login, outcome } => {
                assert_eq!((login, outcome), (approved, LoginOutcome::Succeeded));
            }
            other => panic!("expected the login to end, got {other:?}"),
        }
        let signed_in: Vec<_> = daemon
            .auths()
            .await
            .expect("the fake lists its profiles")
            .into_iter()
            .map(|auth| (auth.auth, auth.status))
            .collect();
        assert_eq!(
            signed_in,
            [
                (KEY_AUTH.to_owned(), AuthStatus::SignedOut),
                (LOGIN_AUTH.to_owned(), AuthStatus::SignedIn)
            ]
        );
    });
}

#[test]
fn a_prompt_runs_a_turn_through_an_approval() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        assert_eq!(daemon.info().skew(), None, "the fake is this build");

        let mut session = new_session(&daemon).await;
        assert_eq!(session.commands()[0].name(), "compact");

        daemon
            .prompt(session.id(), &"list files".into(), &PromptKey::generate())
            .await
            .unwrap();
        until(&mut events, &mut session, |s| !s.inputs().is_empty()).await;
        until(&mut events, &mut session, |s| {
            s.state() == &RunState::NeedsAction
        })
        .await;
        let request = session.inputs()[0].clone();
        assert_eq!(request.title(), "Run `ls`?");
        let Form::Choices { choices, .. } = request.form() else {
            panic!("an approval is answered by choosing");
        };
        assert_eq!(choices[0].id(), "allow-once");
        assert_eq!(choices[0].kind(), ChoiceKind::Accept);
        let allow = choices[0].answer("");
        daemon
            .answer(session.id(), request.id(), &allow)
            .await
            .unwrap();
        // A second answer finds the request closed, which is not an error.
        daemon
            .answer(session.id(), request.id(), &allow)
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

/// A question with a timeout shows its fallback until a client holds it;
/// held, it waits for the questionnaire's answer.
#[test]
fn a_held_question_waits_for_its_answers() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let mut session = new_session(&daemon).await;
        daemon
            .prompt(
                session.id(),
                &"questions timeout".into(),
                &PromptKey::generate(),
            )
            .await
            .unwrap();
        until(&mut events, &mut session, |s| !s.inputs().is_empty()).await;
        let request = session.inputs()[0].clone();
        let Form::Questions(questions) = request.form() else {
            panic!("a question is a questionnaire");
        };
        assert_eq!(questions.questions.len(), 2);
        let timeout = request.timeout().expect("the question times out");
        assert_eq!(timeout.fallback(), "Language: Rust");

        daemon.hold(session.id(), request.id()).await.unwrap();
        until(&mut events, &mut session, |s| {
            s.inputs()
                .first()
                .is_some_and(|open| open.timeout().is_none())
        })
        .await;
        assert_eq!(fake.holds(), 1);

        let answers = Answer::questions(
            [
                ("lang".to_owned(), vec!["Zig".to_owned()]),
                (
                    "checks".to_owned(),
                    vec!["fmt".to_owned(), "test".to_owned()],
                ),
            ]
            .into(),
        );
        daemon
            .answer(session.id(), request.id(), &answers)
            .await
            .unwrap();
        until(&mut events, &mut session, |s| !s.state().is_busy()).await;
        assert_eq!(
            last_reply(&session),
            "you answered checks=fmt,test; lang=Zig"
        );
        // Holding a request that closed is no error.
        daemon.hold(session.id(), request.id()).await.unwrap();
    });
}

/// A session asked for a new worktree works in it, and takes another
/// session's settings, as a draft's worktree session takes the draft's.
#[test]
fn a_session_takes_another_sessions_settings() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, _events) = connect(&fake).await;
        let draft = new_session(&daemon).await;
        let planned = daemon
            .set_option(
                draft.id(),
                OptionChange::PermissionMode(PermissionMode::Plan),
            )
            .await
            .expect("the fake takes plan mode");
        let setup = daemon
            .new_session(Path::new("/w/a"), Checkout::NewWorktree)
            .await
            .expect("the fake opens a session");
        let worktree = setup
            .worktree()
            .cloned()
            .expect("the setup names its worktree");
        let other = Session::from_setup(setup);
        assert!(worktree.branch().starts_with("rho/a-"));
        let attached = daemon
            .attach(other.id(), None)
            .await
            .expect("the fake attaches");
        assert_eq!(
            attached.workspace(),
            worktree.path(),
            "it works in its worktree"
        );
        let initial = other.options().expect("a new session has options").clone();
        assert_ne!(
            initial.permission_mode().map(Choices::current),
            planned.permission_mode().map(Choices::current)
        );
        let matched = daemon
            .match_options(other.id(), &planned, initial)
            .await
            .expect("the fake takes the settings");
        assert_eq!(matched, planned);
    });
}

/// What a person chose for the sessions a session starts reaches the
/// session as the daemon reports it, on each change and in a later
/// client's replay; `None` clears a part, and a mode the daemon reports as
/// unknown keeps the model it sent.
#[test]
fn a_session_follows_the_choice_for_the_sessions_it_starts() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let mut session = new_session(&daemon).await;
        let unchosen = StartedSessionDefaults::default();
        until(&mut events, &mut session, |s| {
            s.started_session_defaults() == Some(&unchosen)
        })
        .await;

        let large = ModelRef::new("fake", "large");
        for change in [
            StartedSessionChange::PermissionMode(Some(PermissionMode::Plan)),
            StartedSessionChange::Model(Some(large.clone())),
        ] {
            daemon
                .set_started_session_default(session.id(), change)
                .await
                .expect("the fake takes the choice");
        }
        let chosen = StartedSessionDefaults::new(
            StartedMode::Chosen(PermissionMode::Plan),
            Some(large.clone()),
        );
        until(&mut events, &mut session, |s| {
            s.started_session_defaults() == Some(&chosen)
        })
        .await;

        let (other, mut other_events) = connect(&fake).await;
        let mut replayed = Session::new(session.id().clone());
        replayed.attached(other.attach(replayed.id(), None).await.unwrap());
        until(&mut other_events, &mut replayed, |s| !s.is_replaying()).await;
        assert_eq!(replayed.started_session_defaults(), Some(&chosen));

        for change in [
            StartedSessionChange::PermissionMode(None),
            StartedSessionChange::Model(None),
        ] {
            daemon
                .set_started_session_default(session.id(), change)
                .await
                .expect("the fake clears the choice");
        }
        until(&mut events, &mut session, |s| {
            s.started_session_defaults() == Some(&unchosen)
        })
        .await;

        let saved = SessionId::from("s1");
        fake.save_unknown_started_mode(&saved);
        let mut unknown = Session::new(saved);
        unknown.attached(daemon.attach(unknown.id(), None).await.unwrap());
        until(&mut events, &mut unknown, |s| !s.is_replaying()).await;
        assert_eq!(
            unknown.started_session_defaults(),
            Some(&StartedSessionDefaults::new(
                StartedMode::Unknown,
                Some(large)
            ))
        );
    });
}

#[test]
fn cancel_withdraws_the_approval_and_ends_the_turn() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let mut session = new_session(&daemon).await;
        daemon
            .prompt(session.id(), &"list files".into(), &PromptKey::generate())
            .await
            .unwrap();
        until(&mut events, &mut session, |s| !s.inputs().is_empty()).await;

        daemon.cancel(session.id()).await.unwrap();

        until(&mut events, &mut session, |s| !s.state().is_busy()).await;
        assert_eq!(session.state(), &RunState::Cancelled);
        assert_eq!(session.inputs(), []);
    });
}

/// A worktree's report names the session it was added for, which the
/// prune's own answer leaves out, and none for a job the daemon no longer
/// lists.
#[test]
fn checked_worktrees_name_their_sessions() {
    let fake = FakeDaemon::start(Script::default());
    fake.add_session_worktree(Some("s1"), "rho/one", WorktreeVerdict::Dirty);
    fake.add_session_worktree(None, "rho/gone", WorktreeVerdict::Dirty);
    block_on(async {
        let (daemon, _events) = connect(&fake).await;
        let sessions: Vec<_> = daemon
            .check_worktrees()
            .await
            .unwrap()
            .into_iter()
            .map(|worktree| (worktree.worktree.branch, worktree.session))
            .collect();
        assert_eq!(
            sessions,
            [
                ("rho/one".to_owned(), Some(SessionId::from("s1"))),
                ("rho/gone".to_owned(), None),
            ]
        );
    });
}

/// A daemon from before pull requests reads as one that serves none, not
/// as a failure to retry, and text that names no pull request as the
/// daemon's reason, for a person; a link the daemon takes names its pull
/// request and comes back in the session's links.
#[test]
fn pull_request_refusals_read_as_what_they_are() {
    let older = FakeDaemon::start(Script {
        pull_requests: PullRequestMethods::Unserved,
        ..Script::default()
    });
    let served = FakeDaemon::start(Script::default());
    let s1 = SessionId::from("s1");
    let widgets = PullRequestId::new("github.com", "acme", "widgets", 12);
    block_on(async {
        let (daemon, _events) = connect(&older).await;
        let unserved = daemon.list_pull_requests(&[SessionId::from("s1")]).await;
        assert_eq!(unserved, Err(PullRequestError::Unserved));

        let (daemon, _events) = connect(&served).await;
        let refused = daemon.link_pull_request(&s1, "widgets").await;
        assert!(
            matches!(refused, Err(PullRequestError::Invalid(_))),
            "{refused:?}"
        );
        let linked = daemon
            .link_pull_request(&s1, "acme/widgets#12")
            .await
            .expect("the daemon links acme/widgets#12");
        let links: Vec<_> = linked
            .pull_requests
            .linked
            .iter()
            .map(|link| &link.id)
            .collect();
        assert_eq!((&linked.pull_request, links), (&widgets, vec![&widgets]));
    });
}

/// A session's reports come between turns, typed: a turn under way reads
/// as busy, a session the daemon hasn't loaded as such, and a daemon from
/// before typed reports as one that serves none, so the app can say each
/// plainly rather than fail.
#[test]
fn reports_come_between_turns_and_refusals_read_as_what_they_are() {
    let older = FakeDaemon::start(Script {
        reports: ReportMethod::Unserved,
        ..Script::default()
    });
    let served = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, _events) = connect(&older).await;
        let session = new_session(&daemon).await;
        let unserved = daemon
            .session_report::<SessionContextReport>(session.id())
            .await;
        assert_eq!(unserved, Err(ReportError::Unserved));

        let (daemon, mut events) = connect(&served).await;
        let unloaded = daemon
            .session_report::<SessionContextReport>(&SessionId::from("s1"))
            .await;
        assert_eq!(unloaded, Err(ReportError::NotLoaded));

        let mut session = new_session(&daemon).await;
        daemon
            .prompt(session.id(), &"list files".into(), &PromptKey::generate())
            .await
            .unwrap();
        until(&mut events, &mut session, |s| !s.inputs().is_empty()).await;
        let busy = daemon
            .session_report::<SessionHooksReport>(session.id())
            .await;
        assert_eq!(busy, Err(ReportError::Busy));

        let request = session.inputs()[0].clone();
        let Form::Choices { choices, .. } = request.form() else {
            panic!("an approval is answered by choosing");
        };
        let allow = choices[0].answer("");
        daemon
            .answer(session.id(), request.id(), &allow)
            .await
            .unwrap();
        until(&mut events, &mut session, |s| !s.state().is_busy()).await;
        let id = session.id();
        // Each type reads its own report: another would be refused.
        let context = daemon.session_report::<SessionContextReport>(id).await;
        assert!(context.is_ok(), "{context:?}");
        let hooks = daemon.session_report::<SessionHooksReport>(id).await;
        assert!(hooks.is_ok(), "{hooks:?}");
        let compaction = daemon.session_report::<SessionCompactionReport>(id).await;
        assert!(compaction.is_ok(), "{compaction:?}");
        let native = daemon
            .session_report::<SessionProviderContextReport>(id)
            .await;
        assert!(native.is_ok(), "{native:?}");
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
        daemon
            .prompt(new.id(), &"list files".into(), &PromptKey::generate())
            .await
            .unwrap();
        until(&mut events, &mut new, |s| !s.inputs().is_empty()).await;
        let sessions = daemon.list_sessions().await.unwrap();
        let ids: Vec<_> = sessions.iter().map(|s| s.id().as_str()).collect();
        assert_eq!(ids, ["new-1", "s1", "old"]);

        let mut old = Session::new(sessions[2].id().clone());
        old.attached(daemon.attach(old.id(), None).await.unwrap());
        until(&mut events, &mut old, |s| !s.is_replaying()).await;
        assert!(matches!(old.entries(), [Entry::Agent(m)] if m.text() == "replayed"));
    });
}

/// The list and the job list name the session that started a session, so
/// the sidebar can fold it under that one; a rho from before
/// `list_detailed` still lists it, naming none, and is asked for
/// `list_detailed` once a connection.
#[test]
fn the_session_list_names_the_session_that_started_each() {
    let fake = FakeDaemon::start(Script::default());
    fake.start_session("s1", "w1", "Write the parser", Started::Idle);
    fake.start_session("s1", "w2", "Test the parser", Started::Working);
    block_on(async {
        let (daemon, _events) = connect(&fake).await;
        let sessions = daemon.list_sessions().await.unwrap();
        let parents: Vec<_> = sessions
            .iter()
            .map(|s| (s.id().as_str(), s.parent().map(SessionId::as_str)))
            .collect();
        assert_eq!(
            parents,
            [
                ("w2", Some("s1")),
                ("w1", Some("s1")),
                ("s1", None),
                ("old", None)
            ]
        );
        assert_eq!(sessions[0].agent(), Some("default"));

        // The job list names it too, for a session that saved nothing yet.
        let jobs = daemon.jobs().await.unwrap().active;
        let [job] = jobs.as_slice() else {
            panic!("one job runs: {jobs:?}")
        };
        assert_eq!(job.session().as_str(), "w2");
        assert_eq!(job.parent().map(SessionId::as_str), Some("s1"));
    });

    let older = FakeDaemon::start(Script {
        session_lists: SessionLists::Plain,
        ..Script::default()
    });
    older.start_session("s1", "w1", "Write the parser", Started::Idle);
    block_on(async {
        let (daemon, _events) = connect(&older).await;
        for _ in 0..2 {
            let sessions = daemon.list_sessions().await.unwrap();
            let parents: Vec<_> = sessions
                .iter()
                .map(|s| (s.id().as_str(), s.parent()))
                .collect();
            assert_eq!(parents, [("w1", None), ("s1", None), ("old", None)]);
        }
    });
    assert_eq!(older.detailed_lists(), 1);
}

/// A mark one client saves is listed with the session, and a client
/// attached to the session hears it as it changes.
#[test]
fn a_mark_is_listed_and_reaches_the_sessions_followers() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, _events) = connect(&fake).await;
        let (follower, mut events) = connect(&fake).await;
        let s1 = SessionId::from("s1");
        let mut session = Session::new(s1.clone());
        session.attached(follower.attach(&s1, None).await.unwrap());
        until(&mut events, &mut session, |s| !s.is_replaying()).await;

        daemon
            .mark_session(&s1, NewSessionMark::Done)
            .await
            .unwrap();
        let listed = daemon.list_sessions().await.unwrap();
        let mark = listed
            .iter()
            .find(|listed| *listed.id() == s1)
            .and_then(SessionSummary::mark);
        assert!(matches!(mark, Some(SessionMark::Done { .. })), "{mark:?}");
        let heard = loop {
            match next(&mut events).await {
                DaemonEvent::Update { update, .. } => {
                    if let Some(mark) = update.mark_changed() {
                        break mark;
                    }
                }
                other => panic!("unexpected event: {other:?}"),
            }
        };
        assert_eq!(heard, mark, "the follower hears the mark as listed");
    });
}

/// A daemon from before marks refuses one, with an error the user can
/// read rather than the protocol's.
#[test]
fn a_daemon_from_before_marks_refuses_one() {
    let fake = FakeDaemon::start(Script {
        session_lists: SessionLists::Plain,
        ..Script::default()
    });
    block_on(async {
        let (daemon, _events) = connect(&fake).await;
        let refused = daemon
            .mark_session(&SessionId::from("s1"), NewSessionMark::Done)
            .await;
        assert!(
            matches!(refused, Err(DaemonError::Refused(_))),
            "{refused:?}"
        );
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
        let jobs = daemon.jobs().await.unwrap().active;
        let [job] = jobs.as_slice() else {
            panic!("one job runs: {jobs:?}")
        };
        assert_eq!(job.session().as_str(), TUI_SESSION);
        assert_eq!(job.title(), Some("from the tui"));
        assert_eq!(job.activity(), JobActivity::Working);
        let ran = Duration::from_secs(90);
        assert!(job.turn_elapsed() >= Some(ran), "{:?}", job.turn_elapsed());

        let mut session = Session::new(job.session().clone());
        let attached = daemon.attach(session.id(), None).await.unwrap();
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
        assert_eq!(session.take_dropped_steers(), Vec::<String>::new());
        assert!(daemon.jobs().await.unwrap().active.is_empty());

        // Between turns nothing takes a steer.
        let refused = daemon.steer(session.id(), "too late").await.unwrap_err();
        assert_eq!(
            refused,
            DaemonError::Busy("no model request is taking input".to_owned())
        );
    });
}

/// A refused prompt is an error in the daemon's words, and one that finds a
/// turn under way, as one the daemon started itself, is told apart as busy,
/// so the UI can hold it for later.
#[test]
fn refused_prompts_say_why_and_busy_ones_are_told_apart() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, _events) = connect(&fake).await;
        let session = new_session(&daemon).await;
        let refused = daemon
            .prompt(session.id(), &"fail".into(), &PromptKey::generate())
            .await;
        assert_eq!(
            refused,
            Err(DaemonError::Refused(
                "the fake daemon refuses this prompt".to_owned()
            ))
        );
        let busy = daemon
            .prompt(session.id(), &"busy".into(), &PromptKey::generate())
            .await;
        assert!(matches!(busy, Err(DaemonError::Busy(_))), "{busy:?}");
    });
}

/// A prompt too large for one message is refused before it is sent, and
/// the connection stays up.
#[test]
fn a_prompt_past_the_message_limit_is_refused_and_the_connection_lives() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, _events) = connect(&fake).await;
        let session = new_session(&daemon).await;
        let huge = Image::new(
            ImageFormat::Png,
            vec![0; MAX_PROMPT_ATTACHMENT_BYTES / 4 * 3 + 1],
        );
        let prompt = Prompt::new(String::new(), vec![Attachment::Image(huge)]);
        let error = daemon
            .prompt(session.id(), &prompt, &PromptKey::generate())
            .await
            .unwrap_err();
        assert!(matches!(&error, DaemonError::Refused(_)), "{error:?}");
        daemon
            .prompt(session.id(), &"list files".into(), &PromptKey::generate())
            .await
            .expect("the connection survives the refusal");
    });
}

/// What a restart would cost reaches the app both ways rho says it: in the
/// refusal of a daemon older than rho-ui, which a restart fixes, and in the
/// answer of one of another build, which a restart replaces with the
/// client's.
#[test]
fn a_daemon_that_needs_a_restart_says_what_it_costs() {
    let fake = FakeDaemon::start(Script::default());
    let cost = RestartCost {
        in_progress: 2,
        clients: 1,
    };
    let dir = tempfile::tempdir().expect("a folder for the marker");
    let replaced = dir.path().join("replaced");
    fake.speak_older_protocol(cost, &replaced);
    let refused = block_on(Daemon::connect(fake.target()));
    let Err(DaemonError::Incompatible(mismatch)) = &refused else {
        panic!("{refused:?}");
    };
    assert_eq!(
        (mismatch.fix, mismatch.restart),
        (VersionFix::RestartDaemon, Some(cost))
    );

    std::fs::write(&replaced, "").expect("the marker");
    fake.run_another_build(cost);
    block_on(async {
        let (daemon, _events) = connect(&fake).await;
        assert_eq!(daemon.info().skew(), Some(cost));
        assert_eq!(daemon.info().pid(), std::process::id(), "the fake's pid");
        daemon.restart().await.expect("the restart");
        let (daemon, _events) = connect(&fake).await;
        assert_eq!(daemon.info().skew(), None, "the restart ran this build");
    });
}

/// A rho that isn't there fails the connection with a reason that names
/// it, so the user knows which path to fix.
#[test]
fn a_missing_rho_fails_the_connection_with_a_reason() {
    // A home with no daemon, so the connection must start one.
    let target = DaemonTarget::Home {
        home: "/nonexistent/rho-home".into(),
        rho: "/nonexistent/missing-rho".into(),
    };
    let result = block_on(Daemon::connect(target));
    let Err(DaemonError::Unreachable(reason)) = &result else {
        panic!("{result:?}");
    };
    assert!(reason.contains("/nonexistent/missing-rho"), "{reason}");
}

#[test]
fn the_stream_ends_when_the_daemon_goes() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async move {
        let (_daemon, mut events) = connect(&fake).await;
        drop(fake);
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

/// A turn ends while the process it started runs on, which the session
/// lists, as does the answer to a later attach. A stopped process is
/// listed as stopped while the daemon's own turn tells the model, and
/// leaves the list as that turn ends.
#[test]
fn background_work_is_listed_until_the_model_has_its_result() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let mut session = new_session(&daemon).await;
        daemon
            .prompt(session.id(), &"background".into(), &PromptKey::generate())
            .await
            .unwrap();
        until(&mut events, &mut session, |session| {
            session.state() == &RunState::Idle && !session.background().is_empty()
        })
        .await;
        let work = session.background()[0].clone();
        assert_eq!(
            (work.kind(), work.title(), work.status()),
            (
                BackgroundKind::Process,
                "sleep 30",
                BackgroundStatus::Running
            )
        );

        let (other, _other_events) = connect(&fake).await;
        let mut theirs = Session::new(session.id().clone());
        theirs.attached(other.attach(session.id(), None).await.unwrap());
        assert_eq!(theirs.background(), session.background());

        daemon
            .stop_background(session.id(), work.id())
            .await
            .unwrap();
        until(&mut events, &mut session, |session| {
            session.state() == &RunState::Running
                && session.background()[0].status() == BackgroundStatus::Cancelled
        })
        .await;
        let again = daemon.stop_background(session.id(), work.id()).await;
        assert!(matches!(again, Err(DaemonError::Refused(_))), "{again:?}");

        daemon.steer(session.id(), "thanks").await.unwrap();
        until(&mut events, &mut session, |session| {
            session.state() == &RunState::Idle && session.background().is_empty()
        })
        .await;
    });
}

/// The surfaces `session`'s transcript places, as (call, surface).
fn surfaces(session: &Session) -> Vec<(&str, &str)> {
    session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            Entry::Surface(shown) => Some((shown.call(), shown.surface())),
            _ => None,
        })
        .collect()
}

/// A store shown every surface `session` placed.
fn store(session: &mut Session) -> SurfaceStore {
    let mut store = SurfaceStore::default();
    for placed in session.take_placed_surfaces() {
        store.show(placed);
    }
    store
}

/// A `ui` call's surface takes one place in the transcript however often
/// the call repeats it, a pressed button's event reaches the session,
/// which the waiting call returns, and an event the session refuses says
/// the surface is stale instead of failing like a lost connection.
#[test]
fn a_surface_is_placed_once_and_its_actions_reach_the_session() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let mut session = new_session(&daemon).await;
        daemon
            .prompt(session.id(), &"ui".into(), &PromptKey::generate())
            .await
            .unwrap();
        until(&mut events, &mut session, |s| !surfaces(s).is_empty()).await;
        assert_eq!(surfaces(&session), [("tool-1", FORM_SURFACE)]);
        let mut shown = store(&mut session);

        let mut press = |id: &str| {
            let tree = shown.tree(FORM_SURFACE).unwrap();
            let button = tree
                .root
                .descendants()
                .find_map(|node| match node {
                    Node::Button(button) if button.target.component() == id => Some(button),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("the form has no button {id}"));
            let action = button.action.clone().expect("the button can be pressed");
            let acted = shown.act(FORM_SURFACE, &button.target, &action);
            acted.send.expect("the button's action is an event")
        };
        let refused = press("stale");
        let error = daemon.surface_action(session.id(), &refused).await;
        assert!(
            matches!(error, Err(SurfaceActionError::Stale(_))),
            "{error:?}"
        );

        let cancel = press("cancel");
        daemon.surface_action(session.id(), &cancel).await.unwrap();
        until(&mut events, &mut session, |s| !s.state().is_busy()).await;
        let names: Vec<_> = fake
            .surface_actions(session.id())
            .iter()
            .map(|sent| sent.action["action"]["name"].clone())
            .collect();
        assert_eq!(names, [REFUSED_EVENT, "cancel"]);
        assert_eq!(last_reply(&session), "you sent cancel");
        // The finished call repeated its surface: still one place.
        let call = session.entries().iter().find_map(|entry| match entry {
            Entry::Tool(tool) => Some(tool.status()),
            _ => None,
        });
        assert_eq!(call, Some(ToolStatus::Done));
        assert_eq!(surfaces(&session), [("tool-1", FORM_SURFACE)]);
    });
}

/// Applies `session`'s events until the daemon stops sending them, and
/// says why: what it published last may still be on its way.
async fn until_detached(events: &mut DaemonEvents, session: &mut Session) -> Detached {
    loop {
        match next(events).await {
            DaemonEvent::Detached {
                session: id,
                reason,
            } if id == *session.id() => return reason,
            DaemonEvent::Update { update, .. } => {
                session.apply(update);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

/// A session the daemon detached as fallen behind attaches again with its
/// cursor and carries on: the daemon sends only what it missed, which
/// lands once, after what it showed, a turn of the daemon's own opening
/// as it would have live. Work it saw running, and that ended meanwhile,
/// is listed as it ended.
#[test]
fn a_session_that_fell_behind_carries_on_from_its_cursor() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let mut session = new_session(&daemon).await;
        daemon
            .prompt(session.id(), &"background".into(), &PromptKey::generate())
            .await
            .unwrap();
        until(&mut events, &mut session, |s| {
            s.state() == &RunState::Idle && !s.background().is_empty()
        })
        .await;
        fake.fall_behind(session.id());
        assert_eq!(
            until_detached(&mut events, &mut session).await,
            Detached::Behind
        );
        let shown = session.entries().to_vec();
        // The daemon's own turn, while nothing follows the session.
        fake.finish_background(session.id(), 0);

        let cursor = session.reattach();
        assert!(cursor.is_some(), "events arrived, so there is a place");
        let attached = daemon.attach(session.id(), cursor.as_ref()).await.unwrap();
        assert_eq!(session.attached(attached), Continuity::Kept);
        until(&mut events, &mut session, |s| !s.is_replaying()).await;
        assert_eq!(&session.entries()[..shown.len()], shown.as_slice());
        assert_eq!(
            session.entries().len(),
            shown.len() + 3,
            "the daemon's turn's wake, thought and reply, once"
        );
        assert_eq!(
            session.entries()[shown.len()],
            Entry::Wake,
            "the missed turn opens as it would have live"
        );
        assert_eq!(last_reply(&session), "`sleep 30` exited with 0.");
        assert_eq!(session.state(), &RunState::Running);
        let ended: Vec<_> = session
            .ended_background()
            .iter()
            .map(BackgroundWork::status)
            .collect();
        assert_eq!(ended, [BackgroundStatus::Completed]);
    });
}

/// A prompt sent again with its key, as after a connection that closed
/// before the daemon answered, answers with the turn it started and starts
/// no other.
#[test]
fn a_prompt_sent_again_with_its_key_runs_once() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, _events) = connect(&fake).await;
        let session = new_session(&daemon).await;
        let (prompt, key) = (Prompt::from("list files"), PromptKey::generate());
        let turn = daemon.prompt(session.id(), &prompt, &key).await.unwrap();
        // Its turn waits for an approval, so a prompt without the key
        // would be refused as busy.
        let (later, _later_events) = connect(&fake).await;
        let again = later.prompt(session.id(), &prompt, &key).await;
        assert_eq!(again, Ok(turn));
        assert_eq!(fake.prompts(session.id().as_str()), 1);
    });
}

/// A client the daemon detached as fallen behind attaches again with a
/// new session, whose replay rebuilds the surface, once, where it was.
#[test]
fn a_replay_rebuilds_a_surface_once() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let mut session = new_session(&daemon).await;
        daemon
            .prompt(session.id(), &"gallery".into(), &PromptKey::generate())
            .await
            .unwrap();
        until(&mut events, &mut session, |s| !surfaces(s).is_empty()).await;
        until(&mut events, &mut session, |s| !s.state().is_busy()).await;
        let live = surfaces(&session)
            .into_iter()
            .map(|(call, surface)| (call.to_owned(), surface.to_owned()))
            .collect::<Vec<_>>();
        let drawn = store(&mut session).tree("gallery");
        assert!(drawn.is_some(), "the gallery draws");

        fake.fall_behind(session.id());
        assert_eq!(
            until_detached(&mut events, &mut session).await,
            Detached::Behind
        );
        let mut again = Session::new(session.id().clone());
        again.attached(daemon.attach(again.id(), None).await.unwrap());
        until(&mut events, &mut again, |s| !s.is_replaying()).await;
        let replayed = surfaces(&again)
            .into_iter()
            .map(|(call, surface)| (call.to_owned(), surface.to_owned()))
            .collect::<Vec<_>>();
        assert_eq!(replayed, live);
        assert_eq!(store(&mut again).tree("gallery"), drawn);
    });
}
