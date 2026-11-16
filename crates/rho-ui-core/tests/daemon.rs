//! `Daemon` against the scripted fake daemon: rho-protocol's real server
//! side over a real Unix socket, without a model. The client side runs on
//! smol's executor, not Tokio, as it runs on GPUI's in the app.

use std::{path::Path, time::Duration};

use futures::FutureExt;
use rho_ui_core::{
    agent_settings::{SettingChange, SettingKey, SettingValue},
    fake_daemon::{
        surface::{FORM_SURFACE, REFUSED_EVENT},
        FakeDaemon, Script, TuiJob, TUI_SESSION,
    },
    surface::{Node, SurfaceStore, SurfaceTree},
    AgentSettings, Answer, Attachment, BackgroundKind, BackgroundStatus, Checkout, ChoiceKind,
    ConfigOption, ConfigValue, Daemon, DaemonError, DaemonEvent, DaemonEvents, DaemonTarget,
    Detached, Entry, Form, Image, ImageFormat, JobActivity, Prompt, RunState, Session,
    SurfaceActionError, ToolStatus, Unsteered, MAX_PROMPT_ATTACHMENT_BYTES,
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

        daemon
            .prompt(session.id(), &"list files".into())
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
            .prompt(session.id(), &"questions timeout".into())
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
        let mode = |config: &[ConfigOption]| {
            config
                .iter()
                .find(|option| option.id() == "mode")
                .map(ConfigOption::value)
        };
        let planned = daemon
            .set_config(draft.id(), "mode", ConfigValue::Select("plan".into()))
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
        let attached = daemon.attach(other.id()).await.expect("the fake attaches");
        assert_eq!(
            attached.workspace(),
            worktree.path(),
            "it works in its worktree"
        );
        assert_ne!(mode(other.config()), mode(&planned));
        let matched = daemon
            .match_config(other.id(), &planned, other.config().to_vec())
            .await
            .expect("the fake takes the settings");
        assert_eq!(matched, planned);
    });
}

#[test]
fn cancel_withdraws_the_approval_and_ends_the_turn() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let mut session = new_session(&daemon).await;
        daemon
            .prompt(session.id(), &"list files".into())
            .await
            .unwrap();
        until(&mut events, &mut session, |s| !s.inputs().is_empty()).await;

        daemon.cancel(session.id()).await.unwrap();

        until(&mut events, &mut session, |s| !s.state().is_busy()).await;
        assert_eq!(session.state(), &RunState::Cancelled);
        assert_eq!(session.inputs(), []);
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
        daemon.prompt(new.id(), &"list files".into()).await.unwrap();
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
        assert_eq!(session.take_dropped_steers(), Vec::<String>::new());
        assert!(daemon.active_jobs().await.unwrap().is_empty());

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
        let refused = daemon.prompt(session.id(), &"fail".into()).await;
        assert_eq!(
            refused,
            Err(DaemonError::Refused(
                "the fake daemon refuses this prompt".to_owned()
            ))
        );
        let busy = daemon.prompt(session.id(), &"busy".into()).await;
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
        let error = daemon.prompt(session.id(), &prompt).await.unwrap_err();
        assert!(matches!(&error, DaemonError::Refused(_)), "{error:?}");
        daemon
            .prompt(session.id(), &"list files".into())
            .await
            .expect("the connection survives the refusal");
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
            .prompt(session.id(), &"background".into())
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
        theirs.attached(other.attach(session.id()).await.unwrap());
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

/// A store shown every surface `session`'s transcript places.
fn store(session: &Session) -> SurfaceStore {
    let mut store = SurfaceStore::default();
    for entry in session.entries() {
        if let Entry::Surface(shown) = entry {
            store.show(shown);
        }
    }
    store
}

/// The button of `tree`'s card whose component is `id`, to press.
fn button<'a>(tree: &'a SurfaceTree, id: &str) -> &'a Node {
    let Node::Card(form) = &tree.root else {
        panic!("the form is a card: {:?}", tree.root);
    };
    let Node::Column { children, .. } = &**form else {
        panic!("the card holds a column: {form:?}");
    };
    children
        .iter()
        .flat_map(|child| match &child.node {
            Node::Row { children, .. } => children.as_slice(),
            _ => &[],
        })
        .map(|child| &child.node)
        .find(|node| matches!(node, Node::Button { target, .. } if target.component() == id))
        .unwrap_or_else(|| panic!("the form has no button {id}"))
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
        daemon.prompt(session.id(), &"ui".into()).await.unwrap();
        until(&mut events, &mut session, |s| !surfaces(s).is_empty()).await;
        assert_eq!(surfaces(&session), [("tool-1", FORM_SURFACE)]);
        let mut shown = store(&session);
        assert_eq!(shown.drawn_under(FORM_SURFACE), Some("tool-1"));

        let mut press = |id: &str| {
            let tree = shown.tree(FORM_SURFACE).unwrap();
            let Node::Button { target, action, .. } = button(&tree, id) else {
                unreachable!("`button` finds buttons");
            };
            let action = action.clone().expect("the button can be pressed");
            let acted = shown.act(FORM_SURFACE, target, &action);
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

/// A client the daemon detached as fallen behind attaches again with a
/// new session, whose replay rebuilds the surface, once, where it was.
#[test]
fn a_replay_rebuilds_a_surface_once() {
    let fake = FakeDaemon::start(Script::default());
    block_on(async {
        let (daemon, mut events) = connect(&fake).await;
        let mut session = new_session(&daemon).await;
        daemon
            .prompt(session.id(), &"gallery".into())
            .await
            .unwrap();
        until(&mut events, &mut session, |s| !surfaces(s).is_empty()).await;
        until(&mut events, &mut session, |s| !s.state().is_busy()).await;
        let live = surfaces(&session)
            .into_iter()
            .map(|(call, surface)| (call.to_owned(), surface.to_owned()))
            .collect::<Vec<_>>();
        let drawn = store(&session).tree("gallery");
        assert!(drawn.is_some(), "the gallery draws");

        fake.fall_behind(session.id());
        // What the turn published last may still be on its way.
        loop {
            match next(&mut events).await {
                DaemonEvent::Detached {
                    session: id,
                    reason,
                } => {
                    assert_eq!((&id, reason), (session.id(), Detached::Behind));
                    break;
                }
                DaemonEvent::Update { update, .. } => {
                    session.apply(update);
                }
                other @ DaemonEvent::AgentSettingsChanged(_) => {
                    panic!("unexpected event: {other:?}")
                }
            }
        }
        let mut again = Session::new(session.id().clone());
        again.attached(daemon.attach(again.id()).await.unwrap());
        until(&mut events, &mut again, |s| !s.is_replaying()).await;
        let replayed = surfaces(&again)
            .into_iter()
            .map(|(call, surface)| (call.to_owned(), surface.to_owned()))
            .collect::<Vec<_>>();
        assert_eq!(replayed, live);
        assert_eq!(store(&again).tree("gallery"), drawn);
    });
}
