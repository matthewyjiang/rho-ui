//! Settings → Plugins: a machine's agent plugins, as `rho/plugins/list`
//! lists them for one project, and what the page changes of them
//! (`fake_daemon/plugins.rs`).

use rho_ui_core::{
    fake_daemon::{Answers, TRUSTED_PLUGINS_WORKSPACE},
    Plugin, PluginScope, PluginSource,
};

use super::configured::{scroll_page, End};
use super::*;

/// How far one step of [`reveal`] scrolls the page.
const STEP: f32 = 40.;

/// How far below the page's Refresh button [`reveal`] stops an element's
/// top: past the header's 16 pixels of padding by more than a [`STEP`],
/// so the element is never under the header.
const BELOW_HEADER: f32 = 70.;

/// Scrolls the Settings page from its top until element `id` is near the
/// top of the page, or the page's end, as a user scrolls to a row: where
/// a row lies depends on how the machine's fonts wrap the text above it.
/// Near the top, a row that grows, as one that says why it failed does,
/// stays on screen.
fn reveal(ui: &mut Harness, id: &str) {
    let id = SharedString::from(id.to_owned());
    scroll_page(ui, End::Top);
    let place = |ui: &mut Harness| {
        ui.frame();
        ui.cx
            .update_window(ui.window, |_, window, _| {
                let header = window.find("configured-refresh").bounds().bottom();
                let shown = window
                    .try_find(id.clone())
                    .filter(gpui_kit::test::ElementSnapshot::visible)
                    .map(|element| element.bounds());
                (header, shown)
            })
            .expect("window is open")
    };
    let mut last = None;
    // No page here is taller than 100 steps.
    for _ in 0..100 {
        let (header, shown) = place(ui);
        let near_top = shown.is_some_and(|it| it.top() < header + px(BELOW_HEADER));
        // At the page's end, the scroll moves nothing.
        if near_top || (shown.is_some() && shown == last) {
            return;
        }
        last = shown;
        let delta = gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-STEP)));
        ui.act(|window, cx| window.scroll("settings-view", delta, cx));
    }
    panic!("{id} never showed on the page");
}

/// The accessibility label of row status `id`, once revealed.
fn row(ui: &mut Harness, id: &str) -> Option<String> {
    reveal(ui, id);
    ui.label(SharedString::from(id.to_owned()))
}

/// Settings with no glide, so a scroll moves the page at once, by as much
/// as it asks: a glide's fling follows the wall clock, not the test's.
fn without_glide(ui: &mut Harness) {
    ui.cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.smooth_scroll = SmoothScroll::Off;
        settings.apply(cx);
    });
}

/// Plugin `name` of `scope`, as the daemon now lists it for `workspace`.
fn listed(ui: &Harness, workspace: Option<&str>, scope: PluginScope, name: &str) -> Option<Plugin> {
    ui.fake()
        .plugins(workspace.map(Path::new))
        .into_iter()
        .find(|plugin| plugin.scope == scope && plugin.name == name)
}

/// The commit a git install was copied from.
fn commit(plugin: &Plugin) -> Option<&str> {
    match &plugin.source {
        PluginSource::Git { commit, .. } => Some(commit),
        _ => None,
    }
}

/// Opens Settings → Plugins, for `project` if one is given.
fn open_plugins(ui: &mut Harness, project: Option<&str>) {
    without_glide(ui);
    open_settings(ui);
    open_settings_page(ui, "Plugins");
    ui.wait_until("the plugins", |window, _, _| {
        is_visible(window, "plugins-add")
    });
    if let Some(project) = project {
        pick_project(ui, "None", project);
    }
}

/// Picks `project` in place of `shown` in the page's project picker.
fn pick_project(ui: &mut Harness, shown: &str, project: &str) {
    click_label(ui, &format!("Project: {shown}"));
    click_label(ui, project);
    ui.wait_until("the project's plugins", |window, _, _| {
        is_visible(window, "plugin-project-fmt")
    });
}

/// The page lists the project's plugins, then the user's, each saying
/// whether a session loads it: a trusted project's plugin shadows the
/// user's of the same name, and an untrusted one leaves it loading.
#[test]
fn plugins_list_by_scope_with_their_status() {
    let daemon = FakeDaemon::start(Script::default());
    let target = daemon.target();
    let mut ui = Harness::open(
        "plugins",
        target,
        Some(daemon),
        Projects::of(["/w/a", TRUSTED_PLUGINS_WORKSPACE]),
    );
    open_plugins(&mut ui, Some(TRUSTED_PLUGINS_WORKSPACE));
    ui.shot("plugins");
    let ids = [
        "plugin-project-fmt",
        "plugin-user-broken",
        "plugin-user-fmt",
        "plugin-user-notes",
    ];
    let labels = ids.map(|id| row(&mut ui, id));
    assert_eq!(
        labels,
        [
            "fmt: Loaded",
            "broken: Rejected",
            "fmt: Shadowed",
            "notes: Disabled",
        ]
        .map(|label| Some(label.to_owned())),
    );
    reveal(&mut ui, "plugin-user-fmt");
    ui.shot("shadowed");
    scroll_page(&mut ui, End::Top);

    pick_project(&mut ui, TRUSTED_PLUGINS_WORKSPACE, "/w/a");
    ui.wait_for_label("plugin-project-fmt", "fmt: Untrusted");
    ui.shot("untrusted");
}

/// A row's switch enables its plugin, its Update fetches it again and
/// shows why that failed, and its Remove… removes it, even a rejected
/// one, only once the user confirms.
#[test]
fn a_plugin_row_enables_updates_and_removes_it() {
    let mut ui = Harness::start("plugins-rows", Script::default());
    open_plugins(&mut ui, None);
    let user = PluginScope::User;

    reveal(&mut ui, "plugin-user-notes");
    ui.wait_for_label("plugin-user-notes", "notes: Disabled");
    ui.click("plugin-enabled-user-notes");
    ui.wait_for_label("plugin-user-notes", "notes: Loaded");
    assert!(
        listed(&ui, None, user, "notes").is_some_and(|notes| notes.enabled),
        "the daemon saved it enabled"
    );

    let before = listed(&ui, None, user, "deploy").expect("deploy is installed");
    reveal(&mut ui, "plugin-menu-user-deploy");
    ui.click("plugin-menu-user-deploy");
    click_menu_item(&mut ui, "Update");
    ui.wait_for_label("plugin-user-deploy", "deploy: Update failed");
    let after = listed(&ui, None, user, "deploy").expect("deploy stays");
    assert_eq!(
        commit(&after),
        commit(&before),
        "the failed update moved nothing"
    );
    ui.shot("update-failed");

    // A rejected plugin can't be switched, only removed.
    reveal(&mut ui, "plugin-menu-user-broken");
    assert!(
        ui.label("plugin-enabled-user-broken").is_none(),
        "no switch"
    );
    ui.click("plugin-menu-user-broken");
    click_menu_item(&mut ui, "Remove…");
    shot_dialog(&mut ui, "Remove", "remove");
    assert!(
        listed(&ui, None, user, "broken").is_some(),
        "nothing is removed before the user confirms"
    );
    click_label(&mut ui, "Remove");
    ui.wait_until("the row gone", |window, _, _| {
        window.try_find("plugin-user-broken").is_none()
    });
    assert!(
        listed(&ui, None, user, "broken").is_none(),
        "the daemon removed it"
    );
}

/// A plugin's change reads its MCP servers again too: disabling a plugin
/// takes its server off MCP & hooks without a Refresh.
#[test]
fn a_plugin_change_reaches_mcp_and_hooks() {
    let mut ui = Harness::start("plugins-mcp", Script::default());
    let lint = "mcp-config-review/lint";
    open_settings(&mut ui);
    open_settings_page(&mut ui, "MCP & hooks");
    ui.wait_until("review's server", |window, _, _| {
        window.try_find(lint).is_some()
    });

    open_settings_page(&mut ui, "Plugins");
    reveal(&mut ui, "plugin-user-review");
    ui.wait_for_label("plugin-user-review", "review: Loaded");
    ui.click("plugin-enabled-user-review");
    ui.wait_for_label("plugin-user-review", "review: Disabled");

    open_settings_page(&mut ui, "MCP & hooks");
    ui.wait_until("the servers without review's", |window, _, _| {
        window.try_find("mcp-config-docs").is_some() && window.try_find(lint).is_none()
    });
}

/// Add plugin… installs a folder, and one whose name is taken only once
/// the user chooses to replace it.
#[test]
fn add_plugin_installs_a_folder_and_asks_before_replacing() {
    let mut ui = Harness::start("plugins-add", Script::default());
    open_plugins(&mut ui, None);
    let user = PluginScope::User;
    let add = |ui: &mut Harness, path: &str| {
        ui.click("plugins-add");
        ui.wait_until("the Add dialog", |window, _, _| {
            is_visible(window, "plugin-source")
        });
        ui.act(|window, cx| window.input(path, cx));
    };

    add(&mut ui, "/srv/plugins/tidy");
    shot_dialog(&mut ui, "Add", "add");
    ui.click("plugin-add-submit");
    ui.wait_until("the dialog closed", |window, _, _| {
        window.try_find("plugin-add").is_none()
    });
    let tidy = listed(&ui, None, user, "tidy").expect("the daemon installed it");
    assert_eq!(tidy.source, PluginSource::Install, "a copy of the folder");
    ui.wait_until("tidy's row", |window, _, _| {
        window.try_find("plugin-user-tidy").is_some()
    });
    assert_eq!(
        row(&mut ui, "plugin-user-tidy").as_deref(),
        Some("tidy: Loaded")
    );

    scroll_page(&mut ui, End::Top);
    // Enter sends it, as the button does, and the dialog stays to ask.
    add(&mut ui, "/srv/plugins/review");
    ui.act(|window, cx| window.press("enter", cx));
    ui.wait_until("the Replace prompt", |window, _, _| {
        is_visible(window, "plugin-exists")
    });
    shot_dialog(&mut ui, "Replace", "replace");
    assert!(
        listed(&ui, None, user, "review")
            .is_some_and(|review| matches!(review.source, PluginSource::Git { .. })),
        "the git install stays until the user replaces it"
    );
    ui.click("plugin-add-submit");
    ui.wait_until("the dialog closed", |window, _, _| {
        window.try_find("plugin-add").is_none()
    });
    assert_eq!(
        listed(&ui, None, user, "review").map(|review| review.source),
        Some(PluginSource::Install),
        "the folder replaced it"
    );
}

/// An install goes on once its dialog is closed, and says so if it fails:
/// the window waits on no git fetch.
#[test]
fn a_closed_add_dialog_still_reports_a_failed_install() {
    let mut ui = Harness::start("plugins-closed", Script::default());
    open_plugins(&mut ui, None);
    ui.fake().hold(Answers::PluginInstalls);
    ui.click("plugins-add");
    ui.wait_until("the Add dialog", |window, _, _| {
        is_visible(window, "plugin-source")
    });
    ui.act(|window, cx| window.input("https://git.example.com/ops/ship.git", cx));
    ui.click("plugin-add-submit");
    ui.poll(
        WAIT,
        "the install sent",
        |ui| ui.fake().waiting(Answers::PluginInstalls) == 1,
        Harness::labels_seen,
    );
    ui.click("plugin-add-cancel");
    ui.wait_until("the dialog closed", |window, _, _| {
        window.try_find("plugin-add").is_none()
    });

    let notifications = |ui: &mut Harness| {
        ui.frame();
        ui.cx
            .update_window(ui.window, |_, window, cx| window.notifications(cx).len())
            .expect("window is open")
    };
    let before = notifications(&mut ui);
    ui.fake().release(Answers::PluginInstalls);
    ui.poll(
        WAIT,
        "the failure",
        |ui| notifications(ui) > before,
        Harness::labels_seen,
    );
    assert!(
        listed(&ui, None, PluginScope::User, "ship").is_none(),
        "the fetch failed, so nothing was installed"
    );
}
