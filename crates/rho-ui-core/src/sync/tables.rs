//! The tables of rho's `config.toml` no setting edits, read and written
//! entry by entry through `rho/config/table/*`: provider profiles (their
//! own group), and model aliases and favorite models (the Models group).
//! An entry is the TOML as JSON, copied whole.

use rho_protocol::{ConfigRevision, TableChange};
use serde_json::Value;

use super::{Action, Change, Copy, GroupKind, Part, Planned, RevertRow, Row, RowKey, Side, Table};
use crate::{Daemon, DaemonError};

/// The entries of `table` that differ, the source's first in its order,
/// then those only the target has. A target that can't take the table
/// takes none of the source's.
pub(super) fn rows(table: Table, source: &Side, target: &Side) -> Vec<Row> {
    let from = match source.table(table) {
        Part::Read(from) => from,
        Part::Failed(why) => {
            let name = table.name();
            return vec![Row::unread(group(table), name, name, source, why)];
        }
        Part::Lacks => return Vec::new(),
    };
    let to = match target.take(target.table(table)) {
        Ok(to) => to,
        Err(why) => {
            return from
                .entries
                .iter()
                .map(|entry| {
                    let copy = Copy::Cannot(why.clone());
                    row(table, &entry.key, [Some(&entry.value), None], copy)
                })
                .collect()
        }
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
                table,
                key: entry.key.clone(),
                before: there.cloned(),
                after: Some(entry.value.clone()),
            };
            let values = [Some(&entry.value), there];
            row(table, &entry.key, values, Copy::Write(change))
        })
    });
    let only_theirs = to
        .entries
        .iter()
        .filter(|entry| !from.entries.iter().any(|ours| ours.key == entry.key))
        .map(|entry| {
            let change = Change::Entry {
                table,
                key: entry.key.clone(),
                before: Some(entry.value.clone()),
                after: None,
            };
            let values = [None, Some(&entry.value)];
            row(table, &entry.key, values, Copy::Write(change))
        });
    ours.chain(only_theirs).collect()
}

/// The group `table`'s rows are in.
fn group(table: Table) -> GroupKind {
    match table {
        Table::Providers => GroupKind::Providers,
        Table::Aliases | Table::Favorites => GroupKind::Models,
    }
}

/// The row id of entry `key` of `table`, unique in its group.
fn id(table: Table, key: &str) -> String {
    match table {
        Table::Providers => key.to_owned(),
        Table::Aliases => format!("alias:{key}"),
        Table::Favorites => format!("favorite:{key}"),
    }
}

/// The row of entry `key` of `table`, which the source holds as `from`
/// and the target as `to`.
fn row(table: Table, key: &str, [from, to]: [Option<&Value>; 2], copy: Copy) -> Row {
    let (source, target) = match (table, from, to) {
        // Two profiles whose summaries match still differ: say where.
        (Table::Providers, Some(from), Some(to)) if profile(from) == profile(to) => (
            Some(format!("{} · {}", profile(from), differing(from, to))),
            Some(format!("{} · {}", profile(to), differing(to, from))),
        ),
        _ => (
            from.map(|v| shown(table, key, v)),
            to.map(|v| shown(table, key, v)),
        ),
    };
    Row {
        key: RowKey {
            kind: group(table),
            id: id(table, key),
        },
        label: label(table, key),
        source,
        target,
        action: Action::of(from, to),
        one_machine: false,
        restart: false,
        note: None,
        copy,
    }
}

/// Entry `key`'s name for people.
fn label(table: Table, key: &str) -> String {
    match table {
        Table::Providers => key.to_owned(),
        Table::Aliases => format!("@{key}"),
        Table::Favorites => "Favorite".to_owned(),
    }
}

/// Entry `key`'s `value` for people: a profile's summary, an alias's
/// model, or a favorite's reference (its value is always `null`).
fn shown(table: Table, key: &str, value: &Value) -> String {
    match table {
        Table::Providers => profile(value),
        Table::Aliases => value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned),
        Table::Favorites => key.to_owned(),
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

/// Entry `key` of `table` as `side` holds it now, or why it can't say.
pub(super) fn value(side: &Side, table: Table, key: &str) -> Result<Option<Value>, String> {
    Ok(side
        .take(side.table(table))?
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.value.clone()))
}

/// Writes the entries `items` change, all of `table`, in one request
/// against `revision`.
pub(super) async fn write(
    target: &Daemon,
    table: Table,
    revision: ConfigRevision,
    items: &[Planned],
) -> Result<(), DaemonError> {
    let changes = items
        .iter()
        .filter_map(|item| match &item.change {
            Change::Entry { key, after, .. } => Some(match after {
                Some(value) => TableChange::Put {
                    key: key.clone(),
                    value: value.clone(),
                },
                None => TableChange::Remove { key: key.clone() },
            }),
            Change::Setting { .. } | Change::Plugin { .. } | Change::Hooks { .. } => None,
        })
        .collect();
    target
        .set_config_table(table.config(), revision, changes)
        .await
}

/// `item`'s row of a revert: unchanged while the target's entry is still
/// what the sync wrote.
pub(super) fn revert_row(
    item: &Planned,
    table: Table,
    key: &str,
    [before, after]: [Option<&Value>; 2],
    target: &Side,
) -> RevertRow {
    let show =
        |value: Option<&Value>| value.map_or_else(|| "None".to_owned(), |v| shown(table, key, v));
    let now = match value(target, table, key) {
        Ok(now) => now,
        Err(why) => {
            let values = [show(before), show(after)];
            return RevertRow::blocked(item, values, "Unknown".to_owned(), why);
        }
    };
    let back = Change::Entry {
        table,
        key: key.to_owned(),
        before: now.clone(),
        after: before.cloned(),
    };
    RevertRow::of(item, [before, after, now.as_ref()], show, back)
}
