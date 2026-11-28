//! Settings sync against two scripted fake daemons, a source and a target:
//! what a plan writes over rho-protocol, and in what order.

use rho_ui_core::{
    agent_settings::SettingValue,
    fake_daemon::{FakeDaemon, Script, KEY_AUTH, LOGIN_AUTH, SAVED_KEY},
    sync::{self, Diff, Journal, Machines, Offer, Pass, RowKey, RowState, Side, Snapshot},
    ConfigTable, Daemon, DaemonEvents, McpSecretKind, PluginInstallSource, SyncId,
};
use serde_json::json;
use smol::block_on;
use std::collections::BTreeSet;

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

/// What syncing `source` to `target` would change, their secrets compared.
async fn diffed(source: &Daemon, target: &Daemon) -> Diff {
    let (from, to) = (read("laptop", source).await, read("box", target).await);
    let secrets = sync::compare(source, &from, target, &to).await;
    sync::diff(&from, &to, &secrets)
}

/// A sync from `source` to `target`, the machine "box".
fn machines<'a>(source: &'a Daemon, target: &'a Daemon) -> Machines<'a> {
    Machines {
        source: Some(source),
        target,
        label: "box",
    }
}

/// A revert on `target`, the machine "box", which needs no source.
fn reverting(target: &Daemon) -> Machines<'_> {
    Machines {
        source: None,
        target,
        label: "box",
    }
}

fn sync_id() -> SyncId {
    SyncId::new("sync-1")
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

        // A hand edit of the target's settings after the read.
        let diff = diffed(&from, &to).await;
        target.edit_agent_setting("general.auto_compact", SettingValue::Bool(false));
        let plan = diff.plan(&diff.initially_checked());
        let written = sync::apply(machines(&from, &to), plan, &sync_id()).await;
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

        let diff = diffed(&from, &to).await;
        let plan = diff.plan(&diff.initially_checked());
        let written = sync::apply(machines(&from, &to), plan, &sync_id()).await;
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

/// A retried row overwrites what the target holds when it is retried, not
/// what it held at the sync, so Revert puts back the target's own edit made
/// in between.
#[test]
fn revert_after_a_retry_restores_what_the_retry_overwrote() {
    let laptop = FakeDaemon::start(Script::default());
    let target = FakeDaemon::start(Script::default());
    let synced = "[[hook]]\nprogram = \"/nonexistent/lint.sh\"\n";
    laptop.edit_hooks_file(Some(synced));
    block_on(async {
        let (from, _from_events) = connect(&laptop).await;
        let (to, _to_events) = connect(&target).await;
        let diff = diffed(&from, &to).await;
        let plan = diff.plan(&diff.initially_checked());
        let written = sync::apply(machines(&from, &to), plan, &sync_id()).await;
        let [failure] = written.failed.as_slice() else {
            panic!("the hooks file fails: its program is missing on box");
        };
        // It landed nothing, so the retry is a sync of its own.
        assert!(!written.wrote());

        target.edit_hooks_file(Some("# mine\n"));
        target.add_hook_programs();
        let retried = sync::retry(machines(&from, &to), failure, &sync_id()).await;
        assert!(retried.failed.is_empty());
        assert_eq!(target.hooks_file().as_deref(), Some(synced));

        let snapshot =
            Snapshot::after(None, Pass::Sync, &retried, &sync_id(), 0).expect("the retry landed");
        let review = sync::review_revert(&snapshot, &read("box", &to).await, &Journal::none());
        let plan = review.plan(&BTreeSet::new());
        let reverted = sync::apply(reverting(&to), plan, &sync_id()).await;
        assert!(reverted.failed.is_empty() && !reverted.stale);
        assert_eq!(target.hooks_file().as_deref(), Some("# mine\n"));
    });
}

/// An API key and an MCP server's secrets cross sealed: a secret the target
/// already holds the same is kept, the rest come from the source. Revert
/// puts back, through the target's journal, what the sync replaced, and
/// holds back a key the target changed since.
#[test]
fn secrets_sync_sealed_and_revert_through_the_journal() {
    let laptop = FakeDaemon::start(Script::default());
    let target = FakeDaemon::start(Script::default());
    laptop.set_api_key(KEY_AUTH, Some("laptop-key"));
    let docs = json!({"command": "docs-mcp"});
    let env = |key| {
        [
            (McpSecretKind::Env, "TOKEN", "same"),
            (McpSecretKind::Env, "KEY", key),
        ]
    };
    laptop.edit_mcp_server("docs", Some((docs.clone(), &env("laptop"))));
    target.edit_mcp_server("docs", Some((docs, &env("box"))));
    let secrets = |fake: &FakeDaemon| {
        let (_, secrets) = fake.mcp_server("docs").expect("docs is configured");
        secrets
            .into_iter()
            .map(|(_, name, value)| (name, value))
            .collect::<Vec<_>>()
    };
    let pair = |name: &str, value: &str| (name.to_owned(), value.to_owned());
    block_on(async {
        let (from, _from_events) = connect(&laptop).await;
        let (to, _to_events) = connect(&target).await;
        let diff = diffed(&from, &to).await;
        let plan = diff.plan(&diff.initially_checked());
        let written = sync::apply(machines(&from, &to), plan, &sync_id()).await;
        assert!(written.failed.is_empty() && !written.stale, "{written:?}");
        assert_eq!(target.api_key(KEY_AUTH).as_deref(), Some("laptop-key"));
        assert_eq!(
            secrets(&target),
            [pair("TOKEN", "same"), pair("KEY", "laptop")]
        );

        // The target saves its own key again since.
        target.set_api_key(KEY_AUTH, Some("box-again"));
        let snapshot =
            Snapshot::after(None, Pass::Sync, &written, &sync_id(), 0).expect("it wrote");
        let journal = sync::journal(&to, "box", &snapshot).await;
        let review = sync::review_revert(&snapshot, &read("box", &to).await, &journal);
        let changed: Vec<&str> = review.changed().map(|row| row.key.id.as_str()).collect();
        assert_eq!(changed, [KEY_AUTH]);
        let plan = review.plan(&BTreeSet::new());
        let reverted = sync::apply(reverting(&to), plan, &sync_id()).await;
        assert!(
            reverted.failed.is_empty() && !reverted.stale,
            "{reverted:?}"
        );
        assert_eq!(
            secrets(&target),
            [pair("TOKEN", "same"), pair("KEY", "box")]
        );
        assert_eq!(target.api_key(KEY_AUTH).as_deref(), Some("box-again"));
    });
}

/// A sign-in the sync can't copy says so: a key an environment variable
/// holds has no saved value to send, and a login offers to sign the target
/// in instead.
#[test]
fn sign_ins_the_sync_cant_copy_say_why() {
    let laptop = FakeDaemon::start(Script::default());
    let target = FakeDaemon::start(Script::default());
    laptop.key_from_env(KEY_AUTH);
    laptop.sign_in_login();
    block_on(async {
        let (from, _from_events) = connect(&laptop).await;
        let (to, _to_events) = connect(&target).await;
        let diff = diffed(&from, &to).await;
        let rows: Vec<(&str, bool, Option<&Offer>)> = diff
            .rows()
            .map(|row| {
                let cant = matches!(row.state(), RowState::CantCopy(_));
                (row.key.id.as_str(), cant, row.offer.as_ref())
            })
            .collect();
        let sign_in = Offer::SignIn {
            auth: LOGIN_AUTH.to_owned(),
        };
        assert_eq!(
            rows,
            [(KEY_AUTH, true, None), (LOGIN_AUTH, true, Some(&sign_in))]
        );
        assert_eq!(target.api_key(KEY_AUTH).as_deref(), Some(SAVED_KEY));
    });
}
