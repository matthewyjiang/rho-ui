//! A session's MCP servers: the mark in the composer's footer while one is
//! in trouble, and the list it opens, and the composer's resources and
//! prompt arguments. The fake daemon gives every session three servers, and
//! its docs server offers prompts and resources (`fake_daemon/mcp.rs`).

use rho_ui_core::fake_daemon::{McpStartup, MCP_INTRO, MCP_INTRO_TEXT};
use rho_ui_core::Prompt;

use super::*;

/// While a server is connecting and another has timed out, a mark shows
/// in the composer's footer, and it lists the session's servers, the one
/// that failed first. Once they are up, the mark goes by itself: the view
/// reads them again while they start, with nothing clicked.
#[test]
fn a_mark_shows_while_mcp_servers_are_in_trouble() {
    let mut ui = Harness::start(
        "mcp-status",
        Script {
            mcp: McpStartup::Connecting,
            ..Script::default()
        },
    );
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("session-local-s1");
    ui.wait_until("the mark", |window, _, _| {
        is_visible(window, "mcp-status-button")
    });
    // A turn's usage brings the context ring, which the mark sits before.
    ui.send("hi");
    ui.wait_until("the approval card", |window, _, _| {
        is_visible(window, "approval-allow-once")
    });
    ui.click("approval-allow-once");
    ui.wait_until("the context ring", |window, _, _| {
        is_visible(window, "composer-usage") && is_visible(window, "mcp-status-button")
    });
    ui.act(|window, _| {
        let (mark, ring) = (
            window.find("mcp-status-button").bounds(),
            window.find("composer-usage").bounds(),
        );
        let gap = ring.left() - mark.right();
        assert!(
            gap >= px(0.) && gap <= px(8.),
            "the mark right before the ring: {mark:?} {ring:?}"
        );
    });
    ui.shot("mark");
    ui.click("mcp-status-button");
    ui.wait_until("the servers", |window, _, _| {
        is_visible(window, "mcp-status-servers")
    });
    let rows: Vec<_> = (0..3)
        .map(|ix| ui.label(SharedString::from(format!("mcp-server-{ix}"))))
        .collect();
    assert_eq!(
        rows,
        [
            Some("github: Timed out".to_owned()),
            Some("docs: Connecting…".to_owned()),
            Some("notes: Disabled".to_owned()),
        ],
        "the one that failed first"
    );
    ui.shot("trouble");

    ui.act(|window, cx| window.press("escape", cx));
    ui.fake().finish_mcp_startup();
    // The view's own timer reads again: no click asks it to.
    ui.poll(
        WAIT,
        "the mark to go",
        |ui| {
            ui.cx.advance_clock(Duration::from_secs(1));
            ui.frame();
            ui.label("mcp-status-button").is_none()
        },
        Harness::labels_seen,
    );
}

/// The message in the active session's composer.
fn composer_prompt(cx: &App, app: &RhoApp) -> Prompt {
    let view = app.active_session().expect("a session is open");
    view.read(cx).composer_prompt(cx)
}

/// Opens saved session `s1`, whose commands include the docs server's
/// prompt.
fn open_session(ui: &mut Harness) {
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    ui.click("session-local-s1");
    ui.wait_for_session("its commands", |session| !session.commands().is_empty());
}

/// An MCP prompt's command completes its argument from the values its
/// server suggests for what is typed, and once one is taken, those
/// suggested before go.
#[test]
fn the_composer_completes_mcp_prompt_arguments() {
    let mut ui = Harness::start("mcp-arguments", Script::default());
    open_session(&mut ui);

    ui.type_in("/mcp:docs:se");
    ui.act(|window, cx| window.press("tab", cx));
    assert_eq!(ui.composer_text(), "/mcp:docs:search ");
    ui.wait_until("the server's values", |window, _, _| {
        is_visible(window, "completion-value-rust ownership")
            && is_visible(window, "completion-value-async runtimes")
    });
    ui.shot("values");
    // The server narrows them as the value is typed.
    ui.act(|window, cx| window.input("rust", cx));
    ui.wait_until("the rust values", |window, _, _| {
        is_visible(window, "completion-value-rust lifetimes")
            && !is_visible(window, "completion-value-async runtimes")
    });
    ui.act(|window, cx| {
        window.press("down", cx);
        window.press("tab", cx);
    });
    assert_eq!(ui.composer_text(), "/mcp:docs:search rust lifetimes");
    ui.act(|window, _| {
        assert!(
            !is_visible(window, "completion-value-rust ownership"),
            "what was suggested for \"rust\" doesn't stay offered"
        );
    });
}

/// `@` offers the servers' resources. A template is written out to fill
/// in; a resource is read, and the message can't go until it is; one
/// whose read fails says why. The resource goes with the message.
#[test]
fn the_composer_attaches_mcp_resources() {
    let mut ui = Harness::start("mcp-resources", Script::default());
    open_session(&mut ui);
    let entries = ui.session().entries().len();

    // A read cancelled with Backspace attaches nothing, even once it ends.
    ui.fake().hold_mcp_reads();
    ui.type_in("@intr");
    let intro = SharedString::from(format!("completion-@docs:{MCP_INTRO}"));
    ui.wait_until("the matching resource", |window, _, _| {
        is_visible(window, intro.clone())
    });
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_until("the read under way", |_, cx, app| {
        app.active_session()
            .is_some_and(|view| view.read(cx).composer_is_reading(cx))
    });
    ui.act(|window, cx| window.press("backspace", cx));
    ui.wait_until("the read cancelled", |_, cx, app| {
        app.active_session()
            .is_some_and(|view| !view.read(cx).composer_is_reading(cx))
    });
    ui.fake().release_mcp_reads();

    ui.type_in("read @us");
    ui.wait_until("the template", |window, _, _| {
        is_visible(window, "completion-@docs:docs://users/{id}")
    });
    ui.act(|window, cx| window.press("tab", cx));
    assert_eq!(ui.composer_text(), "read @docs://users/{id} ");

    ui.fake().hold_mcp_reads();
    ui.act(|window, cx| window.input("@intr", cx));
    ui.wait_until("the matching resource", |window, _, _| {
        is_visible(window, intro.clone())
            && !is_visible(window, "completion-@docs:docs://changelog")
    });
    ui.shot("resources");
    ui.act(|window, cx| window.press("enter", cx));
    assert_eq!(
        ui.composer_text(),
        "read @docs://users/{id} ",
        "the mention goes"
    );
    ui.wait_until("the read under way", |window, _, _| {
        window.find("composer-send").disabled() == Some(true)
    });
    // Enter waits for the read rather than sending without it.
    ui.act(|window, cx| window.press("enter", cx));
    ui.frame();
    assert_eq!(ui.session().entries().len(), entries, "nothing sent");
    ui.shot("reading");

    ui.fake().release_mcp_reads();
    ui.wait_until("the page attached", |_, cx, app| {
        composer_prompt(cx, app).attachments().len() == 1
    });
    ui.act(|window, cx| window.input("@changelog", cx));
    ui.wait_until("the resource", |window, _, _| {
        is_visible(window, "completion-@docs:docs://changelog")
    });
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_until("why it didn't come", |window, _, _| {
        is_visible(window, "composer-refusal")
    });
    ui.shot("attached");

    ui.act(|window, cx| window.press("enter", cx));
    let sent = |entry: &Entry| match entry {
        Entry::User(sent) if sent.text() == "read @docs://users/{id}" => Some(sent.clone()),
        _ => None,
    };
    ui.wait_for_session("the message", |session| {
        session.entries().iter().any(|entry| sent(entry).is_some())
    });
    let sent = ui
        .session()
        .entries()
        .iter()
        .find_map(sent)
        .expect("the message");
    let [Attachment::File(page)] = sent.attachments() else {
        panic!(
            "the page alone, not the read cancelled: {:?}",
            sent.attachments()
        );
    };
    assert_eq!((page.uri(), page.text()), (MCP_INTRO, Some(MCP_INTRO_TEXT)));
}

/// A catalog read that fails asks again for the keys typed while it was
/// asked, and after that only at the next key: a session still loading, or
/// a server that times out, gets one request a key, not a stream of them,
/// and no key is lost to one in flight.
#[test]
fn a_failed_catalog_read_asks_again_at_the_next_key() {
    let mut ui = Harness::start("mcp-catalog-fails", Script::default());
    open_session(&mut ui);
    ui.fake().fail_mcp_catalog();
    ui.fake().hold_mcp_catalog();

    // `b` comes while `@a`'s read is held: the same mention, so it asks
    // nothing until that one fails.
    ui.type_in("@a");
    ui.type_in("b");
    ui.fake().release_mcp_catalog();
    settle_catalog(&mut ui, 2, "for the key typed while it was asked");

    ui.type_in("c");
    settle_catalog(&mut ui, 3, "at the next key");
}

/// Waits until the composer has had `reads` catalog answers and asks no
/// more, then checks a frame later that it has asked no more.
fn settle_catalog(ui: &mut Harness, reads: usize, why: &str) {
    ui.wait_for_daemon("the catalog asked", |fake| {
        fake.mcp_catalog_reads() >= reads
    });
    ui.wait_until("the answer landed", |_, cx, app| {
        app.active_session()
            .is_some_and(|view| !view.read(cx).composer_is_asking_catalog(cx))
    });
    ui.frame();
    assert_eq!(ui.fake().mcp_catalog_reads(), reads, "asked {why}");
}

/// Find and the `@` menu share the keyboard: Ctrl+F opens find while the
/// menu is open, and Escape closes what is nearest the keyboard first,
/// the menu, then the find bar.
#[test]
fn escape_closes_the_mention_menu_before_find() {
    let mut ui = Harness::start("mcp-find", Script::default());
    open_session(&mut ui);
    let intro = SharedString::from(format!("completion-@docs:{MCP_INTRO}"));
    ui.type_in("@intr");
    ui.wait_until("the mention menu", |window, _, _| {
        is_visible(window, intro.clone())
    });
    ui.act(|window, cx| window.press("secondary-f", cx));
    ui.wait_until("the find bar", |window, _, _| {
        is_visible(window, "find-bar")
    });

    let app = ui.app.clone();
    ui.act(|window, cx| {
        let view = app.read(cx).active_session().expect("a session is open");
        let composer = view.read(cx).composer_focus_handle(cx);
        window.focus(&composer, cx);
    });
    ui.act(|window, cx| window.press("escape", cx));
    ui.wait_until("the menu closed", |window, _, _| {
        !is_visible(window, intro.clone())
    });
    ui.act(|window, _| assert!(is_visible(window, "find-bar"), "find stays open"));
    ui.act(|window, cx| window.press("escape", cx));
    ui.wait_until("the find bar closed", |window, _, _| {
        !is_visible(window, "find-bar")
    });
    assert_eq!(ui.composer_text(), "@intr", "nothing typed went");
}
