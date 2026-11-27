//! Settings sync against two scripted fake daemons, a source and a target:
//! what a plan writes over rho-protocol, and what Revert puts back.

use std::collections::BTreeSet;

use rho_ui_core::{
    agent_settings::SettingValue,
    fake_daemon::{FakeDaemon, Script},
    sync::{self, Side},
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

/// A sync makes the target hold what the source does, but for a write
/// refused as stale and an install that fails; after the target changes a
/// row again, Revert puts back the rest and leaves that row as it is.
#[test]
fn a_sync_lands_and_reverts_over_the_protocol() {
    let laptop = FakeDaemon::start(Script::default());
    let target = FakeDaemon::start(Script::default());
    laptop.edit_agent_setting("general.permission_mode", text("plan"));
    laptop.edit_agent_setting("tools.shell", text("zsh"));
    // The laptop has a plugin from a GitHub branch's folder the target
    // lacks, and one the target lacks whose host no fetch reaches.
    laptop.install_plugin(PluginInstallSource::Git {
        url: "https://github.com/acme/kit/tree/release%2F2/plugins/lint".to_owned(),
    });
    target.remove_plugin("deploy");
    block_on(async {
        let (from, _from_events) = connect(&laptop).await;
        let (to, _to_events) = connect(&target).await;
        let source = read("laptop", &from).await;

        // A hand edit of the target after the read: the settings writes are
        // refused, the plugins still install.
        let diff = sync::diff(&source, &read("box", &to).await);
        target.edit_agent_setting("general.auto_compact", SettingValue::Bool(false));
        let applied = sync::apply(&to, diff.plan(&diff.initially_checked())).await;
        assert!(applied.stale);
        let landed: Vec<_> = applied
            .landed
            .iter()
            .map(|item| item.key().id.as_str())
            .collect();
        assert_eq!(landed, ["lint"]);
        let failed: Vec<_> = applied
            .failed
            .iter()
            .map(|f| f.item.key().id.as_str())
            .collect();
        assert_eq!(failed, ["deploy"]);
        target.edit_agent_setting("general.auto_compact", SettingValue::Bool(true));

        // Read again: the settings land, and only the failed plugin still
        // differs.
        let diff = sync::diff(&source, &read("box", &to).await);
        let plan = diff.plan(&diff.initially_checked());
        let mut snapshot = sync::Snapshot::new(0, &plan);
        let applied = sync::apply(&to, plan).await;
        assert!(!applied.stale);
        snapshot.keep_landed(&applied);
        assert_eq!(target.agent_setting("tools.shell"), Some(text("zsh")));
        let left: Vec<_> = sync::diff(&source, &read("box", &to).await)
            .rows()
            .map(|row| row.key.id.clone())
            .collect();
        assert_eq!(left, ["deploy"]);

        // The target changes the mode again; Revert leaves it and puts
        // back the shell.
        target.edit_agent_setting("general.permission_mode", text("supervised"));
        let review = sync::review_revert(&snapshot, &read("box", &to).await);
        let changed: Vec<_> = review.changed().map(|row| row.key.id.as_str()).collect();
        assert_eq!(changed, ["general.permission_mode"]);
        let applied = sync::apply(&to, review.plan(&BTreeSet::new())).await;
        assert!(applied.failed.is_empty() && !applied.stale);
        assert_eq!(
            target.agent_setting("general.permission_mode"),
            Some(text("supervised"))
        );
        assert_eq!(target.agent_setting("tools.shell"), Some(text("bash")));
    });
}
