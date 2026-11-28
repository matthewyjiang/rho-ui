//! Settings → Machines → Sync: copying rho's settings, tables, hooks file
//! and plugins from this machine to another, and reverting it
//! (`settings_sync.rs`).

use rho_ui_core::{agent_settings::SettingValue, ConfigTable, PluginInstallSource};
use serde_json::json;

use super::configured::{scroll_page, End};
use super::*;

/// How far one step of [`shown`] scrolls the page.
const STEP: f32 = 80.;

/// The most steps [`shown`] takes: twice the 22 the deepest element here,
/// the Sync button under every group, measured at.
const STEPS: usize = 44;

/// Room under the dialog's top for the page title.
const TITLE_ROOM: f32 = 60.;

/// Scrolls the Settings page from its top until element `id` is wholly
/// inside the dialog, as a user scrolls to it; whether it got there. Kit
/// draws only what is on screen, so an element far down isn't there to
/// find until the page scrolls to it.
fn shown(ui: &mut Harness, id: &SharedString) -> bool {
    scroll_page(ui, End::Top);
    for _ in 0..STEPS {
        ui.frame();
        let inside = ui
            .cx
            .update_window(ui.window, |_, window, _| {
                let dialog = window.find("settings-view").bounds();
                window
                    .try_find(id.clone())
                    .filter(gpui_kit::test::ElementSnapshot::visible)
                    .is_some_and(|element| {
                        let at = element.bounds();
                        at.top() > dialog.top() + px(TITLE_ROOM) && at.bottom() < dialog.bottom()
                    })
            })
            .expect("window is open");
        if inside {
            return true;
        }
        let delta = gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-STEP)));
        ui.act(|window, cx| window.scroll("settings-view", delta, cx));
    }
    false
}

/// Waits until element `id` shows, scrolled to.
fn reveal(ui: &mut Harness, id: &str) {
    let id = SharedString::from(id.to_owned());
    ui.poll(
        WAIT,
        &format!("{id} on the page"),
        |ui| shown(ui, &id),
        Harness::labels_seen,
    );
}

/// Reveals element `id`, then clicks it.
fn click_revealed(ui: &mut Harness, id: &'static str) {
    reveal(ui, id);
    ui.click(id);
}

/// Whether checkbox `id` is checked, once revealed.
fn checked(ui: &mut Harness, id: &str) -> Option<bool> {
    reveal(ui, id);
    let id = SharedString::from(id.to_owned());
    ui.cx
        .update_window(ui.window, |_, window, _| {
            window.try_find(id).and_then(|element| element.checked())
        })
        .expect("window is open")
}

/// Waits until a run is done: Revert is disabled while anything writes.
fn wait_written(ui: &mut Harness) {
    ui.wait_until("the run done", |window, _, _| {
        window
            .try_find("sync-revert")
            .is_some_and(|revert| revert.disabled() != Some(true))
    });
}

fn text(value: &str) -> SettingValue {
    SettingValue::Text(value.to_owned())
}

/// Whether `fake` has the user's plugin `name`.
fn has_plugin(fake: &FakeDaemon, name: &str) -> bool {
    fake.plugins(None).iter().any(|plugin| plugin.name == name)
}

/// A sync writes the checked rows to the other machine: a setting that
/// belongs to one machine starts unchecked, a plugin from a folder can't
/// be copied, a target changed since the read stops it part way with the
/// user's checks kept, an install that fails keeps Retry, and a setting
/// that applies on restart offers one. Revert then holds back the row the
/// target changed since, and puts back the rest, both passes' rows.
#[test]
fn a_sync_copies_to_another_machine_and_reverts() {
    let local = FakeDaemon::start(Script::default());
    let remote = FakeDaemon::start(Script::default());
    let DaemonTarget::Socket(remote_socket) = remote.target() else {
        panic!("a fake daemon listens on a socket");
    };
    local.edit_agent_setting("general.permission_mode", text("plan"));
    local.edit_agent_setting("tools.shell", text("zsh"));
    let profile = json!({"base_url": "https://acme.test/v1", "api": "responses"});
    local.edit_config_table(ConfigTable::Providers, "acme", Some(profile));
    local.edit_config_table(ConfigTable::ModelAliases, "fast", Some(json!("acme/big")));
    local.edit_config_table(ConfigTable::FavoriteModels, "acme/big", Some(json!(null)));
    let hooks = "[[hook]]\nevent = \"session_end\"\nprogram = \"notify-send\"\n";
    local.edit_hooks_file(Some(hooks));
    local.install_plugin(PluginInstallSource::Path {
        path: PathBuf::from("/home/me/kit"),
    });
    // `deploy`'s host is out of reach, so it can't install there.
    remote.remove_plugin("deploy");
    remote.remove_plugin("review");
    let mut settings = Settings::default();
    settings.machines = vec![RemoteMachine::new("box")];
    settings.smooth_scroll = SmoothScroll::Off;
    let mut ui = Harness::open_with(
        "sync",
        local.target(),
        Some(local),
        Machines {
            settings,
            remote_sockets: vec![("box".to_owned(), remote_socket)],
            ..Machines::default()
        },
    );
    open_settings(&mut ui);
    open_settings_page(&mut ui, "Machines");
    let rows = [
        "sync-row-settings-general.permission_mode",
        "sync-row-settings-tools.shell",
        "sync-row-plugins-deploy",
        "sync-row-plugins-review",
        "sync-row-plugins-kit",
        "sync-row-models-alias:fast",
        "sync-row-models-favorite:acme/big",
        "sync-row-providers-acme",
        "sync-row-hooks-file",
    ];
    let starts = rows.map(|id| checked(&mut ui, id));
    // The shell belongs to one machine; the folder plugin has no checkbox.
    let one = Some(true);
    assert_eq!(
        starts,
        [one, Some(false), one, one, None, one, one, one, one]
    );
    click_revealed(&mut ui, "sync-row-settings-tools.shell");
    reveal(&mut ui, "sync-submit");
    ui.shot("review");

    // The target's settings change after the read: the tables land, and
    // the sync stops at the settings.
    remote.edit_agent_setting("general.auto_compact", SettingValue::Bool(false));
    click_revealed(&mut ui, "sync-submit");
    wait_written(&mut ui);
    assert_eq!(
        checked(&mut ui, "sync-row-settings-tools.shell"),
        Some(true),
        "the user's checks stay"
    );
    assert_eq!(remote.agent_setting("tools.shell"), Some(text("bash")));
    reveal(&mut ui, "sync-group-settings");
    ui.shot("stopped");

    click_revealed(&mut ui, "sync-submit");
    reveal(&mut ui, "sync-retry-plugins-deploy");
    assert_eq!(remote.agent_setting("tools.shell"), Some(text("zsh")));
    assert!(has_plugin(&remote, "review") && !has_plugin(&remote, "deploy"));
    reveal(&mut ui, "sync-restart");
    ui.shot("synced");

    // Retry runs alone, and fails the same row again.
    click_revealed(&mut ui, "sync-retry-plugins-deploy");
    wait_written(&mut ui);
    reveal(&mut ui, "sync-retry-plugins-deploy");
    assert!(!has_plugin(&remote, "deploy"));

    // The target changes the mode again since the sync.
    remote.edit_agent_setting("general.permission_mode", text("supervised"));
    click_revealed(&mut ui, "sync-revert");
    let changed = "sync-revert-row-settings-general.permission_mode";
    assert_eq!(checked(&mut ui, changed), Some(false));
    reveal(&mut ui, "sync-revert-submit");
    ui.shot("revert");

    click_revealed(&mut ui, "sync-revert-submit");
    ui.wait_until("the rest put back", |_, _, _| {
        remote.agent_setting("tools.shell") == Some(text("bash")) && !has_plugin(&remote, "review")
    });
    assert_eq!(
        remote.config_table_entry(ConfigTable::Providers, "acme"),
        None,
        "the stopped pass's rows revert too"
    );
    assert_eq!(
        remote.agent_setting("general.permission_mode"),
        Some(text("supervised")),
        "the newer change stays"
    );
}
