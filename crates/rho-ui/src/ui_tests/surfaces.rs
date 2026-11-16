//! The surfaces a session's `ui` tool shows, drawn and operated in the
//! transcript. The fake daemon's prompt "ui" shows a form its call waits
//! on, "widen" changes it, and "gallery" shows every component of the
//! basic catalog.

use rho_ui_core::{fake_daemon::surface::REFUSED_EVENT, surface::Target};
use serde_json::json;

use super::*;
use crate::settings::Appearance;
use crate::surface_view::{element_id, Part};

/// Saves the window in each theme as `<step>-dark` and `<step>-light`,
/// and leaves it dark.
fn shots(ui: &mut Harness, step: &str) {
    for (appearance, name) in [(Appearance::Light, "light"), (Appearance::Dark, "dark")] {
        ui.cx.update(|cx| {
            let mut settings = cx.global::<Settings>().clone();
            settings.appearance = appearance;
            settings.apply(cx);
        });
        ui.shot(&format!("{step}-{name}"));
    }
}

/// The ID of what draws the component `component` of `surface`.
fn control(surface: &str, component: &str) -> SharedString {
    element_id(surface, Part::Control(&Target::new(component)))
}

/// Whether element `id` is drawn at all. A surface is one transcript row,
/// so a tall one is drawn whole while only part of it is in view.
fn is_drawn(window: &Window, id: &SharedString) -> bool {
    window.try_find(id.clone()).is_some()
}

/// Whether button `id` is drawn and can be pressed.
fn is_enabled(window: &Window, id: &SharedString) -> bool {
    window
        .try_find(id.clone())
        .is_some_and(|button| button.disabled() != Some(true))
}

/// Scrolls the transcript to its first row, so the top of a surface taller
/// than the window is in view.
fn scroll_to_top(ui: &mut Harness) {
    let app = ui.app.clone();
    let scroller = ui.cx.update(|cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        view.read(cx).transcript().scroller().clone()
    });
    ui.cx
        .update(|cx| scroller.update(cx, |scroller, cx| scroller.scroll_to_item(0, cx)));
    ui.frame();
}

/// A waiting `ui` call's form draws under the call, its button disabled
/// until the form's check passes, and while a number field holds text
/// that is not a number, which stays out of the data model. What the user
/// types and picks reaches the session with the button's event. The form
/// stays in the transcript, usable, after its turn folds; a later call
/// that changes it changes its controls, and it says so once the session
/// refuses an action from it.
#[test]
fn a_form_sends_what_the_user_entered_and_stays_usable() {
    let mut ui = Harness::start("surface-form", Script::default());
    let (name, deploy) = (control("deploy", "name"), control("deploy", "deploy"));
    new_session_with(&mut ui, "ui");
    ui.wait_until("the form", |window, _, _| {
        is_visible(window, name.clone()) && is_visible(window, deploy.clone())
    });
    ui.act(|window, _| {
        assert!(!is_enabled(window, &deploy), "the service has no name yet");
    });
    shots(&mut ui, "empty");

    ui.act(|window, cx| {
        window.click(name.clone(), cx);
        window.input("api", cx);
    });
    ui.wait_until("the check to pass", |window, _, _| {
        is_enabled(window, &deploy)
    });
    ui.act(|window, cx| window.click(control("deploy", "notify"), cx));
    // A radio group names its radios by index: Production is the second.
    ui.act(|window, cx| window.within(control("deploy", "env")).click(1usize, cx));

    let (timeout, problem) = (
        control("deploy", "timeout"),
        element_id("deploy", Part::Problem(&Target::new("timeout"))),
    );
    ui.act(|window, cx| {
        window.click(timeout.clone(), cx);
        window.input("x", cx);
    });
    ui.wait_until("the number field to refuse the text", |window, _, _| {
        is_visible(window, problem.clone()) && !is_enabled(window, &deploy)
    });
    shots(&mut ui, "not-a-number");
    ui.act(|window, cx| window.press("backspace", cx));
    ui.wait_until("the number field to take the text", |window, _, _| {
        !is_drawn(window, &problem) && is_enabled(window, &deploy)
    });
    shots(&mut ui, "filled");

    ui.act(|window, cx| window.click(deploy.clone(), cx));
    let session = ui.session().id().clone();
    ui.wait_for_daemon("the action", |fake| {
        !fake.surface_actions(&session).is_empty()
    });
    let sent = ui.fake().surface_actions(&session).remove(0);
    assert_eq!(sent.surface, "deploy");
    assert_eq!(sent.action["action"]["name"], "deploy");
    assert_eq!(
        sent.action["action"]["context"],
        json!({"name": "api", "env": ["production"]})
    );
    let model = sent.data_model.expect("the form sends its data model");
    let model = &model["surfaces"]["deploy"];
    assert_eq!(
        (&model["notify"], &model["timeout"]),
        (&json!(false), &json!("30")),
        "the number field kept its text out of the model"
    );

    ui.wait_for_session("the turn to finish", |session| {
        session.state() == &RunState::Idle
    });
    assert_eq!(last_reply(&mut ui), "you sent deploy");
    // The turn's working folds; the form does not fold with it.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the working folded", |window, _, _| {
        !is_visible(window, "step-1") && is_visible(window, name.clone())
    });

    widen(&mut ui, &session);

    ui.act(|window, cx| window.click(control("deploy", "stale"), cx));
    let stale = element_id("deploy", Part::Stale);
    ui.wait_until("the form to say it is out of date", |window, _, _| {
        is_visible(window, stale.clone())
    });
    let names: Vec<_> = ui
        .fake()
        .surface_actions(&session)
        .iter()
        .map(|sent| sent.action["action"]["name"].clone())
        .collect();
    assert_eq!(names, ["deploy", "deploy", REFUSED_EVENT]);
    ui.act(|window, _| {
        assert!(
            !is_enabled(window, &deploy),
            "a stale form takes no more actions"
        );
    });
    ui.shot("stale");
}

/// Sends the prompt "widen", whose call raises the form's replicas to 20
/// and names the service's example `web`: the form moves under it, its
/// controls changed. The form's middle replica, sent with Deploy, is about
/// 10 of 1 to 20; of the old 1 to 10, it was about 5.
fn widen(ui: &mut Harness, session: &SessionId) {
    let (name, deploy) = (control("deploy", "name"), control("deploy", "deploy"));
    let replicas = control("deploy", "replicas");
    ui.send("widen");
    ui.wait_until("the widened form", |window, _, _| {
        window
            .try_find(name.clone())
            .is_some_and(|name| name.placeholder() == Some("web"))
            && is_visible(window, replicas.clone())
    });
    ui.act(|window, cx| window.click(replicas.clone(), cx));
    ui.act(|window, cx| window.click(deploy.clone(), cx));
    ui.wait_for_daemon("the second action", |fake| {
        fake.surface_actions(session).len() == 2
    });
    let sent = ui.fake().surface_actions(session).remove(1);
    let model = sent.data_model.expect("the form sends its data model");
    let slid = model["surfaces"]["deploy"]["replicas"].as_f64();
    assert!(
        slid.is_some_and(|replicas| (10.0..=11.0).contains(&replicas)),
        "{slid:?}"
    );
}

/// Every component of the basic catalog draws, in tabs that switch. A
/// session the daemon detached as fallen behind opens again, and draws
/// the surface once, from the replay.
#[test]
fn the_gallery_draws_and_a_replay_draws_it_once() {
    let mut ui = Harness::start("surface-gallery", Script::default());
    new_session_with(&mut ui, "gallery");
    let short = control("gallery", "short");
    let inputs = [
        short.clone(),
        control("gallery", "secret"),
        control("gallery", "agree"),
        element_id("gallery", Part::Option(&Target::new("many"), 1)),
        // A toggle group does not register itself; its chips do.
        element_id("gallery", Part::Option(&Target::new("chips"), 0)),
        element_id("gallery", Part::SliderValue(&Target::new("volume"))),
        control("gallery", "primary"),
        control("gallery", "modal"),
    ];
    ui.wait_until("the gallery", |window, _, _| is_drawn(window, &short));
    // Each input is drawn with the surface; `find` names one that is not,
    // with every element that is.
    ui.act(|window, _| {
        for id in inputs {
            let _found = window.find(id);
        }
        // A radio group registers only its radios, by index under it.
        let _medium = window.within(control("gallery", "one")).find(1usize);
    });
    ui.wait_for_session("the turn to finish", |session| {
        session.state() == &RunState::Idle
    });
    ui.cx.advance_clock(Duration::from_secs(1));
    shots(&mut ui, "inputs");

    ui.act(|window, cx| window.click(control("gallery", "modal"), cx));
    let modal = element_id("gallery", Part::Modal(&Target::new("modal")));
    ui.wait_until("the modal", |window, _, _| {
        is_visible(window, modal.clone())
    });
    ui.shot("modal");
    ui.act(|window, cx| window.press("escape", cx));

    scroll_to_top(&mut ui);
    ui.act(|window, cx| window.within(control("gallery", "tabs")).click(1usize, cx));
    let picture = control("gallery", "picture");
    ui.wait_until("the media tab", |window, _, _| {
        is_drawn(window, &picture) && !is_drawn(window, &short)
    });
    shots(&mut ui, "media");

    let session = ui.session().id().clone();
    ui.fake().fall_behind(&session);
    ui.wait_for_daemon("the session attached again", |fake| {
        fake.attaches(session.as_str()) == 2
    });
    ui.wait_for_session("the replay", |session| !session.is_replaying());
    let panel = element_id("gallery", Part::Panel);
    ui.wait_until("the replayed gallery", |window, _, _| {
        is_drawn(window, &short)
    });
    ui.act(|window, _| assert_eq!(window.find_all(panel.clone()).len(), 1));
}
