//! The surfaces a session's `ui` tool shows, drawn and operated in the
//! transcript. The fake daemon's prompt "ui" shows a form its call waits
//! on, and "gallery" shows every component of the basic catalog.

use rho_ui_core::fake_daemon::surface::REFUSED_EVENT;
use serde_json::json;

use super::*;
use crate::settings::Appearance;

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

/// Whether element `id` is drawn at all. A surface is one transcript row,
/// so a tall one is drawn whole while only part of it is in view.
fn is_drawn(window: &Window, id: &str) -> bool {
    window.try_find(SharedString::from(id.to_owned())).is_some()
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

/// The surfaces the active session's transcript places, by call.
fn placed(ui: &mut Harness) -> Vec<(String, String)> {
    ui.session()
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            Entry::Surface(shown) => Some((shown.call().to_owned(), shown.surface().to_owned())),
            _ => None,
        })
        .collect()
}

/// A waiting `ui` call's form draws under the call, its button disabled
/// until the form's check passes. What the user types and picks reaches
/// the session with the button's event. The form stays in the transcript,
/// usable, after its turn folds, and says so once the session refuses an
/// action from it.
#[test]
fn a_form_sends_what_the_user_entered_and_stays_usable() {
    let mut ui = Harness::start("surface-form", Script::default());
    new_session_with(&mut ui, "ui");
    ui.wait_until("the form", |window, _, _| {
        is_visible(window, "surface-deploy-name") && is_visible(window, "surface-deploy-deploy")
    });
    ui.act(|window, _| {
        assert_eq!(
            window.find("surface-deploy-deploy").disabled(),
            Some(true),
            "the service has no name yet"
        );
    });
    shots(&mut ui, "empty");

    ui.act(|window, cx| {
        window.click("surface-deploy-name", cx);
        window.input("api", cx);
    });
    ui.wait_until("the check to pass", |window, _, _| {
        window
            .try_find("surface-deploy-deploy")
            .is_some_and(|button| button.disabled() != Some(true))
    });
    ui.click("surface-deploy-notify");
    // A radio group names its radios by index: Production is the second.
    ui.act(|window, cx| window.within("surface-deploy-env").click(1usize, cx));
    shots(&mut ui, "filled");

    ui.click("surface-deploy-deploy");
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
    assert_eq!(model["surfaces"]["deploy"]["notify"], json!(false));

    ui.wait_for_session("the turn to finish", |session| {
        session.state() == &RunState::Idle
    });
    assert_eq!(last_reply(&mut ui), "you sent deploy");
    assert_eq!(
        placed(&mut ui),
        [("tool-1".to_owned(), "deploy".to_owned())]
    );
    // The turn's working folds; the form does not fold with it.
    ui.cx.advance_clock(Duration::from_secs(1));
    ui.wait_until("the working folded", |window, _, _| {
        !is_visible(window, "step-1") && is_visible(window, "surface-deploy-name")
    });

    ui.click("surface-deploy-stale");
    ui.wait_until("the form to say it is out of date", |window, _, _| {
        is_visible(window, "surface-deploy-is-stale")
    });
    let names: Vec<_> = ui
        .fake()
        .surface_actions(&session)
        .iter()
        .map(|sent| sent.action["action"]["name"].clone())
        .collect();
    assert_eq!(names, ["deploy", REFUSED_EVENT]);
    ui.act(|window, _| {
        assert_eq!(
            window.find("surface-deploy-deploy").disabled(),
            Some(true),
            "a stale form takes no more actions"
        );
    });
    ui.shot("stale");
}

/// Every component of the basic catalog draws, in tabs that switch. A
/// session the daemon detached as fallen behind opens again, and its
/// replay draws the surface once.
#[test]
fn the_gallery_draws_and_a_replay_draws_it_once() {
    let mut ui = Harness::start("surface-gallery", Script::default());
    new_session_with(&mut ui, "gallery");
    let inputs = [
        "surface-gallery-short",
        "surface-gallery-secret",
        "surface-gallery-agree",
        "surface-gallery-many-docs",
        // A toggle group does not register itself; its chips do.
        "surface-gallery-chips-rust",
        "surface-gallery-volume-value",
        "surface-gallery-primary",
        "surface-gallery-modal",
    ];
    ui.wait_until("the gallery", |window, _, _| {
        is_drawn(window, "surface-gallery-short")
    });
    // Each input is drawn with the surface; `find` names one that is not,
    // with every element that is.
    ui.act(|window, _| {
        for id in inputs {
            let _found = window.find(id);
        }
        // A radio group registers only its radios, by index under it.
        let _medium = window.within("surface-gallery-one").find(1usize);
    });
    ui.wait_for_session("the turn to finish", |session| {
        session.state() == &RunState::Idle
    });
    ui.cx.advance_clock(Duration::from_secs(1));
    shots(&mut ui, "inputs");

    ui.click("surface-gallery-modal");
    ui.wait_until("the modal", |window, _, _| {
        is_visible(window, "surface-modal-modal")
    });
    ui.shot("modal");
    ui.act(|window, cx| window.press("escape", cx));

    scroll_to_top(&mut ui);
    ui.act(|window, cx| window.within("surface-gallery-tabs").click(1usize, cx));
    ui.wait_until("the media tab", |window, _, _| {
        is_drawn(window, "surface-gallery-picture") && !is_drawn(window, "surface-gallery-short")
    });
    shots(&mut ui, "media");

    let session = ui.session().id().clone();
    let live = placed(&mut ui);
    assert_eq!(live.len(), 1);
    ui.fake().fall_behind(&session);
    ui.wait_for_daemon("the session attached again", |fake| {
        fake.attaches(session.as_str()) == 2
    });
    ui.wait_until("the replayed gallery", |window, _, _| {
        is_drawn(window, "surface-gallery-short")
    });
    ui.wait_for_session("the replay", |session| !session.is_replaying());
    assert_eq!(placed(&mut ui), live);
}
