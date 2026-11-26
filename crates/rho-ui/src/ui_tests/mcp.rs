//! A session's MCP servers: the mark in the composer's footer while one is
//! in trouble, and the list it opens. The fake daemon gives every session
//! three servers (`fake_daemon/mcp.rs`).

use rho_ui_core::fake_daemon::McpStartup;

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
