use std::{collections::BTreeSet, path::PathBuf};

use rho_protocol::{
    Applies, ConfigCategory, ConfigGetResponse, ConfigRevision, ConfigSchema, ConfigTable, Plugin,
    PluginInstallSource, PluginScope, PluginSource, PluginStatus, SettingChange, SettingChoice,
    SettingEntry, SettingKey, SettingKind, SettingSpec, SettingValue, TableEntry, ValueSource,
};
use serde_json::{json, Value};

use super::plugins::Found;
use super::*;
use crate::agent_settings::ListedModels;

/// How a row starts, without its reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Start {
    Checked,
    Unchecked,
    Cant,
}

fn start(row: &Row) -> Start {
    match row.state() {
        RowState::Checked => Start::Checked,
        RowState::Unchecked => Start::Unchecked,
        RowState::CantCopy(_) => Start::Cant,
    }
}

fn text(value: &str) -> SettingValue {
    SettingValue::Text(value.to_owned())
}

fn choice(values: &[&str], default: &str) -> SettingSpec {
    let choices = values
        .iter()
        .map(|value| SettingChoice {
            value: (*value).to_owned(),
            label: value.to_uppercase(),
            help: None,
        })
        .collect();
    spec("c.mode", SettingKind::Choice { choices }, text(default))
}

fn spec(key: &str, kind: SettingKind, default: SettingValue) -> SettingSpec {
    SettingSpec {
        key: SettingKey::new(key),
        kind,
        default: Some(default),
        category: "c".to_owned(),
        section: None,
        label: key.to_owned(),
        help: String::new(),
        applies: Applies::Immediately,
        overridable_per_project: true,
        per_machine: false,
        read_only: false,
    }
}

fn restart(spec: SettingSpec) -> SettingSpec {
    SettingSpec {
        applies: Applies::Restart,
        ..spec
    }
}

fn timeout(max: u64) -> SettingSpec {
    let kind = SettingKind::Integer {
        min: 1,
        max: Some(max),
        unit: None,
    };
    spec("c.timeout", kind, SettingValue::Integer(120))
}

fn shell() -> SettingSpec {
    restart(spec("c.shell", SettingKind::Text, text("bash")))
}

fn read_only(spec: SettingSpec) -> SettingSpec {
    SettingSpec {
        read_only: true,
        ..spec
    }
}

fn per_machine(spec: SettingSpec) -> SettingSpec {
    SettingSpec {
        per_machine: true,
        ..spec
    }
}

/// `side` with `store` holding `entries`, read at revision `rev`.
fn table(side: Side, store: ConfigTable, rev: &str, entries: &[(&str, Value)]) -> Side {
    let entries = entries
        .iter()
        .map(|(key, value)| TableEntry::new((*key).to_owned(), value.clone()))
        .collect();
    side.with_table(
        store,
        TableRead {
            revision: ConfigRevision::new(rev),
            entries,
        },
    )
}

/// `side` with the user's hooks file as `text`.
fn hooks(side: Side, text: Option<&str>) -> Side {
    side.with_hooks(HooksFileRead {
        path: PathBuf::from("/home/me/.rho/hooks.toml"),
        revision: ConfigRevision::new("h0"),
        text: text.map(str::to_owned),
    })
}

/// Machine `label` with the settings `specs`, the file setting `set` (the
/// rest at their defaults), and the user's `plugins`.
fn machine(
    label: &str,
    specs: Vec<SettingSpec>,
    set: &[(&str, SettingValue)],
    plugins: Vec<Plugin>,
) -> Side {
    let values = specs
        .iter()
        .map(|spec| {
            let set = set.iter().find(|(key, _)| *key == spec.key.as_str());
            let entry = match set {
                Some((_, value)) => SettingEntry {
                    value: Some(value.clone()),
                    source: ValueSource::Global,
                },
                None => SettingEntry {
                    value: spec.default.clone(),
                    source: ValueSource::Default,
                },
            };
            (spec.key.clone(), entry)
        })
        .collect();
    let schema = ConfigSchema {
        categories: vec![ConfigCategory {
            id: "c".to_owned(),
            label: "C".to_owned(),
        }],
        settings: specs,
        file_only: Vec::new(),
    };
    let read = ConfigGetResponse {
        revision: ConfigRevision::new(format!("{label}-r0")),
        values,
        errors: Vec::new(),
    };
    let settings = AgentSettings::new(schema, read, ListedModels::Unsupported);
    Side::new(label.to_owned(), settings, Some(plugins))
}

fn plugin(name: &str, source: PluginSource) -> Plugin {
    Plugin::new(
        name.to_owned(),
        None,
        None,
        PluginScope::User,
        PathBuf::from(format!("/plugins/{name}")),
        source,
        true,
        PathBuf::from("/plugins.toml"),
        PluginStatus::Loaded,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

fn disabled(mut plugin: Plugin) -> Plugin {
    plugin.enabled = false;
    plugin
}

fn git(url: &str, commit: &str) -> PluginSource {
    PluginSource::Git {
        url: url.to_owned(),
        git_ref: Some("main".to_owned()),
        path: None,
        commit: commit.to_owned(),
    }
}

fn link(path: &str) -> PluginSource {
    PluginSource::Link {
        target: PathBuf::from(path),
    }
}

/// A setting is listed only when its value differs, starts checked only
/// when the target's rho takes the source's value, and asks for a restart
/// when the target applies it then.
#[test]
fn a_setting_row_follows_the_copy_policy() {
    type Setup = (SettingSpec, Option<SettingValue>);
    type Expected = Option<(Action, Start, bool)>;
    let cases: [(&str, Setup, Option<Setup>, Expected); 9] = [
        (
            "same value",
            (choice(&["auto", "plan"], "auto"), Some(text("plan"))),
            Some((choice(&["auto", "plan"], "auto"), Some(text("plan")))),
            None,
        ),
        (
            "differs",
            (choice(&["auto", "plan"], "auto"), Some(text("plan"))),
            Some((choice(&["auto", "plan"], "auto"), None)),
            Some((Action::Change, Start::Checked, false)),
        ),
        (
            "target lacks it, source sets it",
            (choice(&["auto", "plan"], "auto"), Some(text("plan"))),
            None,
            Some((Action::Add, Start::Cant, false)),
        ),
        (
            "target lacks it, source at its default",
            (choice(&["auto", "plan"], "auto"), None),
            None,
            None,
        ),
        (
            "target doesn't offer the choice",
            (choice(&["auto", "plan"], "auto"), Some(text("plan"))),
            Some((choice(&["auto"], "auto"), None)),
            Some((Action::Change, Start::Cant, false)),
        ),
        (
            "past the target's bound",
            (timeout(7200), Some(SettingValue::Integer(5000))),
            Some((timeout(3600), None)),
            Some((Action::Change, Start::Cant, false)),
        ),
        (
            "applies on restart",
            (shell(), Some(text("zsh"))),
            Some((shell(), None)),
            Some((Action::Change, Start::Checked, true)),
        ),
        (
            "belongs to the target's machine",
            (shell(), Some(text("zsh"))),
            Some((per_machine(shell()), None)),
            Some((Action::Change, Start::Unchecked, true)),
        ),
        (
            "changed only through another method there",
            (choice(&["auto", "plan"], "auto"), Some(text("plan"))),
            Some((read_only(choice(&["auto", "plan"], "auto")), None)),
            Some((Action::Change, Start::Cant, false)),
        ),
    ];
    for (case, (from_spec, from_value), to, expected) in cases {
        let key = from_spec.key.to_string();
        let from_set: Vec<_> = from_value
            .map(|value| (key.as_str(), value))
            .into_iter()
            .collect();
        let from = machine("laptop", vec![from_spec], &from_set, Vec::new());
        let to = match to {
            Some((spec, value)) => {
                let set: Vec<_> = value
                    .map(|value| (key.as_str(), value))
                    .into_iter()
                    .collect();
                machine("box", vec![spec], &set, Vec::new())
            }
            None => machine("box", Vec::new(), &[], Vec::new()),
        };
        let diff = diff(&from, &to);
        let row = diff
            .rows()
            .next()
            .map(|row| (row.action, start(row), row.restart));
        assert_eq!(row, expected, "{case}");
        assert!(diff.rows().nth(1).is_none(), "{case}: one setting, one row");
    }
}

/// A sync writes the source's value only when that is what the target
/// would hold; a source at its default resets the target, unless the
/// target's default differs.
#[test]
fn a_setting_write_resets_to_a_shared_default() {
    let modes = ["auto", "plan", "supervised"];
    let key = SettingKey::new("c.mode");
    let set = |value: &str| SettingChange::Set {
        key: key.clone(),
        value: text(value),
    };
    let cases = [
        (
            "auto",
            Some("plan"),
            "auto",
            Some("supervised"),
            set("plan"),
        ),
        (
            "auto",
            None,
            "auto",
            Some("plan"),
            SettingChange::Reset { key: key.clone() },
        ),
        ("auto", None, "plan", None, set("auto")),
    ];
    for (from_default, from_set, to_default, to_set, expected) in cases {
        let side = |label, default, set: Option<&str>| {
            let set: Vec<_> = set
                .map(|value| ("c.mode", text(value)))
                .into_iter()
                .collect();
            machine(label, vec![choice(&modes, default)], &set, Vec::new())
        };
        let diff = diff(
            &side("laptop", from_default, from_set),
            &side("box", to_default, to_set),
        );
        let plan = diff.plan(&diff.initially_checked());
        let writes: Vec<_> = plan
            .items()
            .filter_map(|item| match &item.change {
                Change::Setting { key, after, .. } => Some(after.write(key)),
                _ => None,
            })
            .collect();
        assert_eq!(writes, [expected]);
    }
}

/// Only a git plugin syncs; one only the target has starts unchecked, and
/// only when Revert could install it again; the same install at another
/// commit, or a folder plugin both have alike, isn't listed.
#[test]
fn a_plugin_row_follows_the_copy_policy() {
    let review = "https://github.com/acme/review";
    let source = vec![
        plugin("added", git(review, "a1")),
        plugin("both-local", PluginSource::Local),
        plugin("disabled-there", git(review, "a1")),
        plugin("folder-there", git(review, "a1")),
        plugin("linked", link("/home/me/linked")),
        plugin("newer", git(review, "b2")),
    ];
    let mut project = plugin("project", PluginSource::Install);
    project.scope = PluginScope::Project;
    let target = vec![
        plugin("both-local", PluginSource::Local),
        disabled(plugin("disabled-there", git(review, "a1"))),
        plugin("folder-there", PluginSource::Install),
        plugin("newer", git(review, "a1")),
        plugin("only-there-copied", PluginSource::Install),
        plugin("only-there-git", git(review, "a1")),
        project,
    ];
    let diff = diff(
        &machine("laptop", Vec::new(), &[], source),
        &machine("box", Vec::new(), &[], target),
    );
    let rows: Vec<_> = diff
        .rows()
        .map(|row| (row.key.id.as_str(), row.action, start(row)))
        .collect();
    assert_eq!(
        rows,
        [
            ("added", Action::Add, Start::Checked),
            ("disabled-there", Action::Change, Start::Checked),
            ("folder-there", Action::Change, Start::Cant),
            ("linked", Action::Add, Start::Cant),
            ("only-there-copied", Action::Remove, Start::Cant),
            ("only-there-git", Action::Remove, Start::Unchecked),
        ]
    );
}

/// An install from a recorded git source names the same ref and folder
/// again, as the GitHub link rho reads them from, segment by segment.
#[test]
fn a_git_source_installs_again_from_the_same_ref_and_folder() {
    let cases = [
        (
            "https://github.com/o/r",
            None,
            None,
            Some("https://github.com/o/r"),
        ),
        (
            "https://example.com/o/r.git",
            None,
            None,
            Some("https://example.com/o/r.git"),
        ),
        (
            "https://github.com/o/r",
            Some("main"),
            Some("plugins/fmt"),
            Some("https://github.com/o/r/tree/main/plugins/fmt"),
        ),
        (
            "https://github.com/o/r",
            Some("feat/x"),
            Some("my plugin"),
            Some("https://github.com/o/r/tree/feat%2Fx/my%20plugin"),
        ),
        ("https://example.com/o/r.git", Some("main"), None, None),
        ("https://github.com/o/r", None, Some("plugins/fmt"), None),
    ];
    for (url, git_ref, path, expected) in cases {
        let source = PluginSource::Git {
            url: url.to_owned(),
            git_ref: git_ref.map(str::to_owned),
            path: path.map(str::to_owned),
            commit: "a1".to_owned(),
        };
        let found = Found::of("p", &[plugin("p", source)]);
        let install = match found {
            Found::Copy(copy) => copy.install_source(),
            Found::Absent | Found::Other => None,
        };
        let expected = expected.map(|url| PluginInstallSource::Git {
            url: url.to_owned(),
        });
        assert_eq!(install, expected, "{url} {git_ref:?} {path:?}");
    }
}

/// Revert holds back what the target changed since the sync, unless the
/// user checks it, never replaces a folder copy, and puts back each other
/// row as the target had it.
#[test]
fn a_revert_holds_back_what_the_target_changed_since() {
    let review = "https://github.com/acme/review";
    let modes = ["auto", "plan", "supervised"];
    let specs = || vec![choice(&modes, "auto"), shell()];
    let laptop = machine(
        "laptop",
        specs(),
        &[("c.mode", text("plan")), ("c.shell", text("zsh"))],
        vec![
            plugin("lint", git(review, "a1")),
            plugin("review", git(review, "a1")),
        ],
    );
    let before = machine("box", specs(), &[], Vec::new());
    let diff = diff(&laptop, &before);
    let snapshot = Snapshot::new(0, &diff.plan(&diff.initially_checked()));
    // After the sync, the target changed the mode again, and swapped lint
    // for a copy of a folder.
    let now = machine(
        "box",
        specs(),
        &[("c.mode", text("supervised")), ("c.shell", text("zsh"))],
        vec![
            plugin("lint", PluginSource::Install),
            plugin("review", git(review, "b2")),
        ],
    );
    let review = review_revert(&snapshot, &now);
    let states: Vec<_> = review
        .rows
        .iter()
        .map(|row| (row.key.id.as_str(), row.state.clone()))
        .collect();
    assert!(
        matches!(
            states.as_slice(),
            [
                ("c.mode", RevertState::Changed),
                ("c.shell", RevertState::Unchanged),
                ("lint", RevertState::Blocked(_)),
                ("review", RevertState::Unchanged),
            ]
        ),
        "{states:?}"
    );
    assert_eq!(
        writes(&review.plan(&BTreeSet::new())),
        [
            ("c.shell".to_owned(), Some(reset("c.shell"))),
            ("review".to_owned(), None),
        ]
    );
    let anyway: BTreeSet<_> = review.changed().map(|row| row.key.clone()).collect();
    assert_eq!(
        writes(&review.plan(&anyway)),
        [
            ("c.mode".to_owned(), Some(reset("c.mode"))),
            ("c.shell".to_owned(), Some(reset("c.shell"))),
            ("review".to_owned(), None),
        ]
    );
}

/// A snapshot keeps each row a sync landed with what the row first
/// overwrote, through a retry of a plugin that installed but didn't
/// disable, so Revert removes it; a sync that landed nothing keeps the last
/// one, and a revert drops what it put back.
#[test]
fn a_snapshot_keeps_what_a_sync_first_overwrote() {
    let review = "https://github.com/acme/review";
    let modes = ["auto", "plan", "supervised"];
    let specs = || vec![choice(&modes, "auto")];
    let lint = || vec![disabled(plugin("lint", git(review, "a1")))];
    let laptop = machine("laptop", specs(), &[("c.mode", text("plan"))], lint());
    let diff = diff(&laptop, &machine("box", specs(), &[], Vec::new()));
    let plan = diff.plan(&diff.initially_checked());
    let [mode, lint_item] = [0, 1].map(|at| plan.items().nth(at).cloned().expect("two rows"));
    let Change::Plugin {
        name,
        after: Some(after),
        ..
    } = &lint_item.change
    else {
        panic!("lint installs");
    };
    let rest = Planned {
        change: plugins::left_to_disable(name, after),
        ..lint_item.clone()
    };
    let landed = |items: Vec<Planned>| Written {
        landed: items,
        ..Written::default()
    };
    let synced = Written {
        failed: vec![Failure {
            item: rest.clone(),
            reason: String::new(),
        }],
        ..landed(vec![mode.clone(), lint_item])
    };
    let snapshot = Snapshot::after(None, Pass::Sync, &synced, 0);
    let snapshot = Snapshot::after(snapshot, Pass::Retry, &landed(vec![rest]), 1);
    let stale = Written {
        stale: true,
        ..Written::default()
    };
    let snapshot = Snapshot::after(snapshot, Pass::Sync, &stale, 2).expect("kept");
    let box_now = machine("box", specs(), &[("c.mode", text("plan"))], lint());
    assert_eq!(
        snapshot.at, 0,
        "the sync's, not the retry's or the stale one's"
    );
    assert_eq!(
        writes(&review_revert(&snapshot, &box_now).plan(&BTreeSet::new())),
        [
            ("c.mode".to_owned(), Some(reset("c.mode"))),
            ("lint".to_owned(), None),
        ]
    );
    let snapshot =
        Snapshot::after(Some(snapshot), Pass::Revert, &landed(vec![mode]), 3).expect("lint left");
    let left: Vec<_> = review_revert(&snapshot, &box_now)
        .rows
        .into_iter()
        .map(|row| row.key.id)
        .collect();
    assert_eq!(left, ["lint"]);
}

/// A table entry or hooks file is listed only when it differs: one only
/// the target has starts unchecked, and none can go to a rho too old to
/// write its store. Two provider profiles with the same endpoint say which
/// fields differ.
#[test]
fn a_table_or_hooks_row_follows_the_copy_policy() {
    let base = |label| machine(label, Vec::new(), &[], Vec::new());
    let ollama = json!({"base_url": "http://localhost:11434/v1"});
    let source = table(
        table(
            table(
                hooks(base("laptop"), Some("[[hook]]\n")),
                ConfigTable::ModelAliases,
                "a",
                &[("fast", json!("openai/gpt-5-mini")), ("same", json!("x/y"))],
            ),
            ConfigTable::FavoriteModels,
            "f",
            &[("openai/gpt-5", Value::Null)],
        ),
        ConfigTable::Providers,
        "p",
        &[("ollama", ollama)],
    );
    let target = table(
        table(
            table(
                hooks(base("box"), None),
                ConfigTable::ModelAliases,
                "a",
                &[("fast", json!("openai/gpt-5")), ("same", json!("x/y"))],
            ),
            ConfigTable::FavoriteModels,
            "f",
            &[("fake/old", Value::Null)],
        ),
        ConfigTable::Providers,
        "p",
        &[(
            "ollama",
            json!({"base_url": "http://localhost:11434/v1", "edit_tool": "patch"}),
        )],
    );
    let rows = |diff: &Diff| -> Vec<(String, Action, Start)> {
        diff.rows()
            .map(|row| (row.key.id.clone(), row.action, start(row)))
            .collect()
    };
    let synced = diff(&source, &target);
    assert_eq!(
        rows(&synced),
        [
            ("alias:fast".to_owned(), Action::Change, Start::Checked),
            (
                "favorite:openai/gpt-5".to_owned(),
                Action::Add,
                Start::Checked
            ),
            (
                "favorite:fake/old".to_owned(),
                Action::Remove,
                Start::Unchecked
            ),
            ("ollama".to_owned(), Action::Change, Start::Checked),
            ("file".to_owned(), Action::Add, Start::Checked),
        ]
    );
    let profile = synced
        .rows()
        .find(|row| row.key.id == "ollama")
        .and_then(|row| row.target.clone());
    assert_eq!(
        profile.as_deref(),
        Some("http://localhost:11434/v1 · edit_tool = patch")
    );
    let old = diff(&source, &base("box"));
    assert_eq!(
        rows(&old),
        [
            ("alias:fast".to_owned(), Action::Add, Start::Cant),
            ("alias:same".to_owned(), Action::Add, Start::Cant),
            ("favorite:openai/gpt-5".to_owned(), Action::Add, Start::Cant),
            ("ollama".to_owned(), Action::Add, Start::Cant),
            ("file".to_owned(), Action::Add, Start::Cant),
        ]
    );
}

/// A sync writes the stores in an order where nothing names a provider
/// the target lacks yet, plugins last; a revert writes them in reverse, and
/// holds back an entry the target changed since.
#[test]
fn stores_sync_in_dependency_order_and_revert_in_reverse() {
    let modes = ["auto", "plan"];
    let side = |label, mode: Option<&str>, alias: &str, hook: Option<&str>| {
        let set: Vec<_> = mode
            .map(|mode| ("c.mode", text(mode)))
            .into_iter()
            .collect();
        let plugins = match mode {
            Some(_) => vec![plugin("lint", git("https://github.com/acme/lint", "a1"))],
            None => Vec::new(),
        };
        let side = machine(label, vec![choice(&modes, "auto")], &set, plugins);
        let side = table(
            side,
            ConfigTable::ModelAliases,
            "a",
            &[("fast", json!(alias))],
        );
        let side = table(side, ConfigTable::FavoriteModels, "f", &[]);
        let providers: &[(&str, Value)] = match mode {
            Some(_) => &[("acme", json!({"base_url": "https://acme.test/v1"}))],
            None => &[],
        };
        hooks(table(side, ConfigTable::Providers, "p", providers), hook)
    };
    let laptop = side("laptop", Some("plan"), "acme/big", Some("[[hook]]\n"));
    let before = side("box", None, "fake/small", None);
    let diff = diff(&laptop, &before);
    let plan = diff.plan(&diff.initially_checked());
    let order = |plan: &Plan| -> Vec<String> { plan.items().map(|i| i.key.id.clone()).collect() };
    assert_eq!(
        order(&plan),
        ["acme", "alias:fast", "c.mode", "file", "lint"]
    );
    let snapshot = Snapshot::new(0, &plan);
    let mut now = side("box", Some("plan"), "acme/big", Some("[[hook]]\n"));
    now = table(
        now,
        ConfigTable::ModelAliases,
        "a2",
        &[("fast", json!("fake/edited"))],
    );
    let review = review_revert(&snapshot, &now);
    let changed: Vec<_> = review.changed().map(|row| row.key.id.as_str()).collect();
    assert_eq!(changed, ["alias:fast"]);
    assert_eq!(
        order(&review.plan(&BTreeSet::new())),
        ["file", "c.mode", "acme", "lint"]
    );
    let anyway: BTreeSet<_> = review.changed().map(|row| row.key.clone()).collect();
    assert_eq!(
        order(&review.plan(&anyway)),
        ["file", "c.mode", "alias:fast", "acme", "lint"]
    );
}

/// What `plan` writes, row by row: a setting's write, or `None` for a
/// plugin, which must be removed.
fn writes(plan: &Plan) -> Vec<(String, Option<SettingChange>)> {
    plan.items()
        .cloned()
        .map(|item| match item.change {
            Change::Setting { key, after, .. } => (key.to_string(), Some(after.write(&key))),
            Change::Plugin { name, after, .. } => {
                assert_eq!(
                    after, None,
                    "{name}: the sync installed it, so Revert removes it"
                );
                (name, None)
            }
            change => panic!("not a setting or plugin: {change:?}"),
        })
        .collect()
}

fn reset(key: &str) -> SettingChange {
    SettingChange::Reset {
        key: SettingKey::new(key),
    }
}
