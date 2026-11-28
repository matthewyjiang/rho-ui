//! The tables of rho's `config.toml` no setting edits, read and written
//! entry by entry through `rho/config/table/*`: provider profiles (their
//! own group), and model aliases and favorite models (the Models group).
//! An entry is the TOML as JSON, copied whole.

use serde_json::Value;

use super::{
    Action, Change, Copy, GroupKind, Planned, RevertRow, RevertState, Row, RowKey, Side, Store,
};

/// The entries of `store` that differ, the source's first in its order,
/// then those only the target has. A target whose rho has no such table
/// can take none of the source's.
pub(super) fn rows(store: Store, source: &Side, target: &Side) -> Vec<Row> {
    let Some(from) = source.tables.get(&store) else {
        return Vec::new();
    };
    let Some(to) = target.tables.get(&store) else {
        let why = format!("Rho on {} is too old to take this", target.label);
        return from
            .entries
            .iter()
            .map(|entry| {
                row(
                    store,
                    &entry.key,
                    [Some(&entry.value), None],
                    Copy::Cannot(why.clone()),
                )
            })
            .collect();
    };
    let value = |key: &str| {
        to.entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| &entry.value)
    };
    let ours = from.entries.iter().filter_map(|entry| {
        let there = value(&entry.key);
        (there != Some(&entry.value)).then(|| {
            let change = Change::Entry {
                store,
                key: entry.key.clone(),
                before: there.cloned(),
                after: Some(entry.value.clone()),
            };
            row(
                store,
                &entry.key,
                [Some(&entry.value), there],
                Copy::Write(change),
            )
        })
    });
    let only_theirs = to
        .entries
        .iter()
        .filter(|entry| !from.entries.iter().any(|ours| ours.key == entry.key))
        .map(|entry| {
            let change = Change::Entry {
                store,
                key: entry.key.clone(),
                before: Some(entry.value.clone()),
                after: None,
            };
            row(
                store,
                &entry.key,
                [None, Some(&entry.value)],
                Copy::Write(change),
            )
        });
    ours.chain(only_theirs).collect()
}

/// The row of entry `key` of `store`, which the source holds as `from`
/// and the target as `to`.
fn row(store: Store, key: &str, [from, to]: [Option<&Value>; 2], copy: Copy) -> Row {
    let action = match (from, to) {
        (Some(_), None) => Action::Add,
        (None, Some(_)) => Action::Remove,
        _ => Action::Change,
    };
    let (source, target) = match (store, from, to) {
        // Two profiles whose summaries match still differ: say where.
        (Store::Providers, Some(from), Some(to)) if profile(from) == profile(to) => (
            Some(format!("{} · {}", profile(from), differing(from, to))),
            Some(format!("{} · {}", profile(to), differing(to, from))),
        ),
        _ => (
            from.map(|v| shown(store, key, v)),
            to.map(|v| shown(store, key, v)),
        ),
    };
    let (kind, id) = match store {
        Store::Aliases => (GroupKind::Models, format!("alias:{key}")),
        Store::Favorites => (GroupKind::Models, format!("favorite:{key}")),
        _ => (GroupKind::Providers, key.to_owned()),
    };
    Row {
        key: RowKey { kind, id },
        label: label(store, key),
        source,
        target,
        action,
        one_machine: false,
        restart: false,
        note: None,
        copy,
    }
}

/// Entry `key`'s name for people.
fn label(store: Store, key: &str) -> String {
    match store {
        Store::Aliases => format!("@{key}"),
        Store::Favorites => "Favorite".to_owned(),
        _ => key.to_owned(),
    }
}

/// Entry `key`'s `value` for people: an alias's model, a favorite's
/// reference (its value is always `null`), or a profile's summary.
fn shown(store: Store, key: &str, value: &Value) -> String {
    match store {
        Store::Aliases => value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned),
        Store::Favorites => key.to_owned(),
        _ => profile(value),
    }
}

/// A provider profile's endpoint and wire API, when it names one.
fn profile(value: &Value) -> String {
    let field = |name| value.get(name).and_then(Value::as_str);
    let url = field("base_url").unwrap_or("no URL");
    match field("api") {
        Some(api) => format!("{url} · {api}"),
        None => url.to_owned(),
    }
}

/// The fields of profile `ours` that `theirs` lacks or holds otherwise, as
/// `field = value`.
fn differing(ours: &Value, theirs: &Value) -> String {
    let Some(fields) = ours.as_object() else {
        return ours.to_string();
    };
    let differ: Vec<String> = fields
        .iter()
        .filter(|(name, value)| theirs.get(name.as_str()) != Some(value))
        .map(|(name, value)| match value.as_str() {
            Some(text) => format!("{name} = {text}"),
            None => format!("{name} = {value}"),
        })
        .collect();
    if differ.is_empty() {
        "defaults".to_owned()
    } else {
        differ.join(", ")
    }
}

/// `item`'s row of a revert: unchanged while the target's entry is still
/// what the sync wrote.
pub(super) fn revert_row(
    item: &Planned,
    store: Store,
    key: &str,
    [before, after]: [Option<&Value>; 2],
    target: &Side,
) -> RevertRow {
    let show =
        |value: Option<&Value>| value.map_or_else(|| "None".to_owned(), |v| shown(store, key, v));
    let Some(table) = target.tables.get(&store) else {
        return RevertRow {
            key: item.key.clone(),
            label: item.label.clone(),
            synced: show(after),
            now: "Unknown".to_owned(),
            before: show(before),
            state: RevertState::Blocked(format!(
                "Rho on {} no longer reads its {}",
                target.label,
                store.name()
            )),
            item: None,
        };
    };
    let now = table
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| &entry.value);
    let back = Change::Entry {
        store,
        key: key.to_owned(),
        before: now.cloned(),
        after: before.cloned(),
    };
    RevertRow::of(item, [before, after, now], show, back)
}
