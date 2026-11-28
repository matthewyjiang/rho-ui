//! Settings sync against two scripted fake daemons, a source and a target:
//! what a plan writes over rho-protocol, and in what order.

use rho_ui_core::{
    agent_settings::SettingValue,
    fake_daemon::{FakeDaemon, Script},
    sync::{self, RowKey, Side},
    ConfigTable, Daemon, DaemonEvents, PluginInstallSource,
};
use serde_json::json;
use smol::block_on;

async fn connect(fake: &FakeDaemon) -> (Daemon, DaemonEvents) {
    Daemon::connect(fake.target())
        .await
        .expect("the fake daemon answers the handshake")
}

async fn read(label: &str, daemon: &Daemon) -> Side {
    sync::read(label.to_owned(), daemon)
        .await
        .expect("the fake reads")
}

fn text(value: &str) -> SettingValue {
    SettingValue::Text(value.to_owned())
}

/// A store changed since the read stops the sync there: the stores before
/// it stay written, and nothing after it is, plugins included. Once read
/// again, a store the target refuses for what it holds fails its own rows,
/// and the later stores and the plugins still land.
#[test]
fn a_stale_store_stops_the_sync_and_a_refused_one_fails_alone() {
    let laptop = FakeDaemon::start(Script::default());
    let target = FakeDaemon::start(Script::default());
    let profile = json!({"base_url": "https://acme.test/v1"});
    laptop.edit_config_table(ConfigTable::Providers, "acme", Some(profile.clone()));
    laptop.edit_agent_setting("general.permission_mode", text("plan"));
    laptop.edit_hooks_file(Some("[[hook]]\nprogram = \"/nonexistent/lint.sh\"\n"));
    laptop.install_plugin(PluginInstallSource::Git {
        url: "https://github.com/acme/kit/tree/release%2F2/plugins/lint".to_owned(),
    });
    let has_lint = || {
        target
            .plugins(None)
            .iter()
            .any(|plugin| plugin.name == "lint")
    };
    let ids = |items: Vec<&sync::Planned>| -> Vec<String> {
        items
            .into_iter()
            .map(|item| item.key().id.clone())
            .collect()
    };
    block_on(async {
        let (from, _from_events) = connect(&laptop).await;
        let (to, _to_events) = connect(&target).await;
        let source = read("laptop", &from).await;

        // A hand edit of the target's settings after the read.
        let diff = sync::diff(&source, &read("box", &to).await);
        target.edit_agent_setting("general.auto_compact", SettingValue::Bool(false));
        let written = sync::apply(&to, diff.plan(&diff.initially_checked())).await;
        assert!(written.stale);
        assert_eq!(ids(written.landed.iter().collect()), ["acme"]);
        assert_eq!(
            target.config_table_entry(ConfigTable::Providers, "acme"),
            Some(profile)
        );
        assert_eq!(
            target.agent_setting("general.permission_mode"),
            Some(text("auto"))
        );
        assert!(!has_lint(), "nothing past a stale store is written");
        target.edit_agent_setting("general.auto_compact", SettingValue::Bool(true));

        let diff = sync::diff(&source, &read("box", &to).await);
        let written = sync::apply(&to, diff.plan(&diff.initially_checked())).await;
        assert!(!written.stale);
        assert_eq!(
            ids(written.whole().collect()),
            ["general.permission_mode", "lint"]
        );
        let failed: Vec<&RowKey> = written.failed.iter().map(|f| f.item.key()).collect();
        assert_eq!(
            failed.iter().map(|key| key.id.as_str()).collect::<Vec<_>>(),
            ["file"]
        );
        assert_eq!(target.hooks_file(), None);
        assert_eq!(
            target.agent_setting("general.permission_mode"),
            Some(text("plan"))
        );
        assert!(has_lint());
    });
}
