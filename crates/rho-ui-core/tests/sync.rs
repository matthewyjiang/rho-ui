//! Settings sync against two scripted fake daemons, a source and a target:
//! what a plan writes over rho-protocol, and in what order.

use rho_ui_core::{
    agent_settings::SettingValue,
    fake_daemon::{FakeDaemon, Script},
    sync::{self, Applied, Refusal, Side},
    Daemon, DaemonEvents, PluginInstallSource,
};
use smol::block_on;

async fn connect(fake: &FakeDaemon) -> (Daemon, DaemonEvents) {
    Daemon::connect(fake.target())
        .await
        .expect("the fake daemon answers the handshake")
}

/// What `daemon` holds now, as a sync reads it.
async fn read(label: &str, daemon: &Daemon) -> Side {
    let settings = daemon.agent_settings().await.expect("the fake reads");
    let plugins = daemon.plugins(None).await.expect("the fake lists");
    Side::new(label.to_owned(), settings, plugins)
}

fn text(value: &str) -> SettingValue {
    SettingValue::Text(value.to_owned())
}

/// A sync refused as stale writes nothing, plugins included; once read
/// again, its settings land first, then its plugins, each on its own, so
/// an install that fails fails its row alone.
#[test]
fn a_sync_writes_plugins_only_after_its_settings_land() {
    let laptop = FakeDaemon::start(Script::default());
    let target = FakeDaemon::start(Script::default());
    laptop.edit_agent_setting("tools.shell", text("zsh"));
    // The laptop has a plugin from a GitHub branch's folder the target
    // lacks, and one the target lacks whose host no fetch reaches.
    laptop.install_plugin(PluginInstallSource::Git {
        url: "https://github.com/acme/kit/tree/release%2F2/plugins/lint".to_owned(),
    });
    target.remove_plugin("deploy");
    let has = |name: &str| {
        target
            .plugins(None)
            .iter()
            .any(|plugin| plugin.name == name)
    };
    block_on(async {
        let (from, _from_events) = connect(&laptop).await;
        let (to, _to_events) = connect(&target).await;
        let source = read("laptop", &from).await;

        // A hand edit of the target after the read.
        let diff = sync::diff(&source, &read("box", &to).await);
        target.edit_agent_setting("general.auto_compact", SettingValue::Bool(false));
        let applied = sync::apply(&to, diff.plan(&diff.initially_checked())).await;
        assert_eq!(applied, Applied::Refused(Refusal::Stale));
        assert_eq!(target.agent_setting("tools.shell"), Some(text("bash")));
        assert!(!has("lint"), "nothing past a stale refusal is written");
        target.edit_agent_setting("general.auto_compact", SettingValue::Bool(true));

        let diff = sync::diff(&source, &read("box", &to).await);
        let Applied::Written(written) =
            sync::apply(&to, diff.plan(&diff.initially_checked())).await
        else {
            panic!("a fresh read's settings land");
        };
        let ids = |items: Vec<&sync::Planned>| -> Vec<String> {
            items
                .into_iter()
                .map(|item| item.key().id.clone())
                .collect()
        };
        assert_eq!(ids(written.whole().collect()), ["tools.shell", "lint"]);
        assert_eq!(
            ids(written.failed.iter().map(|f| &f.item).collect()),
            ["deploy"]
        );
        assert_eq!(target.agent_setting("tools.shell"), Some(text("zsh")));
        assert!(has("lint") && !has("deploy"));
    });
}
