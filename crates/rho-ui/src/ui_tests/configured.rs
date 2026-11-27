//! Settings → MCP & hooks: what a machine's rho configuration sets up, as
//! `rho/mcp/list` and `rho/hooks/list` list it for one project
//! (`fake_daemon/configured.rs`).

use rho_ui_core::fake_daemon::{
    ConfiguredList, ConfiguredMethods, ConfiguredRead, INVALID_HOOKS_WORKSPACE,
};

use super::*;

/// The accessibility label of row `id`, once it shows.
fn row(ui: &mut Harness, id: &str) -> Option<String> {
    ui.label(SharedString::from(id.to_owned()))
}

/// Scrolls the page to its `end`: hooks come after the MCP servers.
pub(super) fn scroll_page(ui: &mut Harness, end: End) {
    let dy = match end {
        End::Top => 5000.,
        End::Bottom => -5000.,
    };
    let delta = gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(dy)));
    // One wheel event moves the page only so far; four reach either end.
    for _ in 0..4 {
        ui.act(|window, cx| window.scroll("settings-view", delta, cx));
        // Past the glide.
        ui.cx.advance_clock(Duration::from_secs(2));
        ui.frame();
    }
}

#[derive(Clone, Copy)]
pub(super) enum End {
    Top,
    Bottom,
}

/// The listings the daemon got, both read at once, in a fixed order.
fn reads(ui: &mut Harness) -> Vec<ConfiguredRead> {
    let mut reads = ui.fake().configured_reads();
    // A read asks for both at once, which may arrive in either order.
    for pair in reads.chunks_mut(2) {
        pair.sort_by_key(|read| read.list == ConfiguredList::Hooks);
    }
    reads
}

/// Both listings, for `workspace`.
fn read_for(workspace: &str) -> [ConfiguredRead; 2] {
    let workspace = Some(PathBuf::from(workspace));
    [ConfiguredList::Mcp, ConfiguredList::Hooks].map(|list| ConfiguredRead {
        list,
        workspace: workspace.clone(),
    })
}

/// The page reads for the project of the session on screen, and says of
/// each MCP server and hooks file whether a session gets it; picking
/// another project reads for that one, and Refresh reads again, each once.
#[test]
fn settings_list_the_configured_mcp_servers_and_hooks() {
    let daemon = FakeDaemon::start(Script::default());
    keep(&daemon, &["s1"]);
    let target = daemon.target();
    let mut ui = Harness::open(
        "configured",
        target,
        Some(daemon),
        Projects::of(["/w/a", INVALID_HOOKS_WORKSPACE]),
    );
    ui.wait_until("saved sessions", |window, _, _| {
        is_visible(window, "session-local-s1")
    });
    // s1 works in /w/a.
    ui.click("session-local-s1");
    ui.wait_for_session("s1 open", |session| session.id().as_str() == "s1");
    open_settings(&mut ui);
    open_settings_page(&mut ui, "MCP & hooks");
    ui.wait_until("the MCP servers", |window, _, _| {
        is_visible(window, "mcp-config-docs")
    });
    assert_eq!(
        reads(&mut ui),
        read_for("/w/a"),
        "one read, for s1's project"
    );
    let servers =
        ["docs", "notes", "broken"].map(|name| row(&mut ui, &format!("mcp-config-{name}")));
    assert_eq!(
        servers,
        ["docs: Enabled", "notes: Disabled", "broken: Invalid"].map(|label| Some(label.to_owned())),
    );
    ui.shot("mcp");
    scroll_page(&mut ui, End::Bottom);
    let hooks =
        ["hooks-file-project", "hook-project:fmt", "hook-user:notify"].map(|id| row(&mut ui, id));
    assert_eq!(
        hooks,
        [
            "/w/a/.rho/hooks.toml: Untrusted",
            "project:fmt: Untrusted",
            "user:notify: Active",
        ]
        .map(|label| Some(label.to_owned())),
    );
    ui.shot("untrusted");

    // Another project, whose hooks file doesn't parse, which disables the
    // user's.
    scroll_page(&mut ui, End::Top);
    click_label(&mut ui, "Project: /w/a");
    click_label(&mut ui, INVALID_HOOKS_WORKSPACE);
    ui.wait_until("the read for it", |window, _, _| {
        is_visible(window, "mcp-config-docs")
    });
    let picked = read_for(INVALID_HOOKS_WORKSPACE);
    assert_eq!(
        reads(&mut ui)[2..],
        picked,
        "one read, for the picked project"
    );
    scroll_page(&mut ui, End::Bottom);
    let hooks =
        ["hooks-file-project", "hooks-file-user", "hook-user:notify"].map(|id| row(&mut ui, id));
    assert_eq!(
        hooks[0],
        Some(format!(
            "{INVALID_HOOKS_WORKSPACE}/.rho/hooks.toml: Invalid"
        ))
    );
    // The user's file, as its machine's home names it.
    assert!(
        hooks[1]
            .as_deref()
            .is_some_and(|label| label.ends_with(": Disabled")),
        "the user's file is disabled, not {:?}",
        hooks[1]
    );
    assert_eq!(hooks[2].as_deref(), Some("user:notify: Disabled"));
    ui.shot("invalid");

    ui.click("configured-refresh");
    ui.wait_for_daemon("Refresh's read", |fake| fake.configured_reads().len() == 6);
    ui.wait_until("its answer", |window, _, _| {
        window.find("configured-refresh").disabled() != Some(true)
    });
    assert_eq!(
        reads(&mut ui)[4..],
        picked,
        "Refresh reads the same project"
    );
}

/// A rho from before the listings says so in one quiet line, not as a
/// failure.
#[test]
fn an_older_rho_says_it_is_too_old_to_list_them() {
    let script = Script {
        configured: ConfiguredMethods::Unserved,
        ..Script::default()
    };
    let mut ui = Harness::start("configured-old", script);
    open_settings(&mut ui);
    open_settings_page(&mut ui, "MCP & hooks");
    ui.wait_until("the too-old line", |window, _, _| {
        is_visible(window, "configured-too-old")
    });
    ui.act(|window, _| {
        assert!(
            window.try_find("mcp-config-docs").is_none()
                && window.try_find("configured-failed-MCP servers").is_none(),
            "no servers, and no failure"
        );
    });
    // No project on screen: it read for none.
    assert_eq!(
        ui.fake().configured_reads().len(),
        2,
        "one read of each, refused"
    );
    assert!(ui
        .fake()
        .configured_reads()
        .iter()
        .all(|read| read.workspace.is_none()));
    ui.shot("too-old");
}

/// A lead's draft is in no project yet: the page reads for none, not for
/// the leads folder its session opens in.
#[test]
fn a_leads_draft_reads_for_no_project() {
    let rho_home = tempfile::tempdir().expect("a rho home");
    let daemon = FakeDaemon::start(Script::default());
    let machines = Machines {
        rho_home: Some(rho_home.path().to_path_buf()),
        ..Machines::default()
    };
    let mut ui = Harness::open_with("configured-lead", daemon.target(), Some(daemon), machines);
    ui.wait_until("the sidebar", |window, _, _| is_visible(window, "new-lead"));
    ui.click("new-lead");
    ui.wait_until("the lead's draft", |_, cx, app| {
        app.draft()
            .is_some_and(|draft| draft.read(cx).session_key().is_some())
    });
    open_settings(&mut ui);
    open_settings_page(&mut ui, "MCP & hooks");
    ui.wait_until("the MCP servers", |window, _, _| {
        is_visible(window, "mcp-config-docs")
    });
    let workspaces: Vec<_> = ui
        .fake()
        .configured_reads()
        .into_iter()
        .map(|read| read.workspace)
        .collect();
    assert_eq!(workspaces, [None, None], "one read, for no project");
}
