//! The rho settings group: one row per setting whose value differs, read
//! through `rho/config/schema` and `rho/config/get`, written through
//! `rho/config/set`.

use serde::{Deserialize, Serialize};

use super::{Action, Change, Copy, GroupKind, Planned, RevertRow, RevertState, Row, RowKey, Side};
use crate::agent_settings::{
    selection_name, unit_label, Applies, ModelRuntime, ModelSelection, SettingChange, SettingKey,
    SettingKind, SettingSpec, SettingValue,
};
use crate::AgentSettings;

/// A setting's value on one machine, and whether its file sets it, so a
/// write can put it back as it was: set to the value, or left to its
/// default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Held {
    value: Option<SettingValue>,
    set: bool,
}

impl Held {
    /// The write that makes `key` hold this.
    pub(super) fn write(&self, key: &SettingKey) -> SettingChange {
        match (&self.value, self.set) {
            (Some(value), true) => SettingChange::Set {
                key: key.clone(),
                value: value.clone(),
            },
            _ => SettingChange::Reset { key: key.clone() },
        }
    }
}

/// The settings whose value differs, in the source's order of pages and
/// settings.
pub(super) fn rows(source: &Side, target: &Side) -> Vec<Row> {
    let from = &source.settings;
    from.categories()
        .iter()
        .flat_map(|category| from.settings(&category.id))
        .filter_map(|spec| row(spec, source, target))
        .collect()
}

/// `spec`'s row, if its value differs. A setting only the source's rho
/// has is listed only when the source sets it.
fn row(spec: &SettingSpec, source: &Side, target: &Side) -> Option<Row> {
    let key = &spec.key;
    let from = source.settings.value(key);
    let show = |value| shown(value, spec);
    let base = |action, target_value, copy| Row {
        key: RowKey {
            kind: GroupKind::RhoSettings,
            id: key.to_string(),
        },
        label: spec.label.clone(),
        source: Some(show(from)),
        target: target_value,
        action,
        one_machine: false,
        restart: false,
        note: None,
        copy,
    };
    let Some(tspec) = target.settings.spec(key) else {
        let why = format!("Rho on {} doesn't have this setting", target.label);
        return source
            .settings
            .is_set(key)
            .then(|| base(Action::Add, None, Copy::Cannot(why)));
    };
    let to = target.settings.value(key);
    if from == to {
        return None;
    }
    let after = Held {
        value: from.cloned(),
        set: source.settings.is_set(key) || tspec.default.as_ref() != from,
    };
    let before = Held {
        value: to.cloned(),
        set: target.settings.is_set(key),
    };
    // A setting another method changes, such as the credential store
    // through Providers, can't be written here.
    let fits = if tspec.read_only {
        Err(format!("Change it in Providers on {}", target.label))
    } else {
        fits(from, tspec, &target.label)
    };
    let copy = match fits {
        Ok(()) => Copy::Write(Change::Setting {
            key: key.clone(),
            before,
            after,
        }),
        Err(why) => Copy::Cannot(why),
    };
    Some(Row {
        one_machine: tspec.per_machine,
        restart: tspec.applies == Applies::Restart,
        ..base(Action::Change, Some(show(to)), copy)
    })
}

/// Whether the target's rho takes `value` for `spec`, or why not.
fn fits(value: Option<&SettingValue>, spec: &SettingSpec, target: &str) -> Result<(), String> {
    let Some(value) = value else {
        // Only a reset leaves a setting unset, and a reset gives the
        // target's default.
        return match spec.default {
            None => Ok(()),
            Some(_) => Err(format!("Rho on {target} can't leave this unset")),
        };
    };
    let takes = match (&spec.kind, value) {
        (SettingKind::Bool, SettingValue::Bool(_)) | (SettingKind::Text, SettingValue::Text(_)) => {
            true
        }
        (SettingKind::Choice { choices }, SettingValue::Text(text)) => {
            choices.iter().any(|choice| choice.value == *text)
        }
        (SettingKind::Integer { min, max, .. }, SettingValue::Integer(number)) => {
            number >= min && max.is_none_or(|max| *number <= max)
        }
        (SettingKind::Model { runtimes }, SettingValue::Model(model)) => {
            runtimes.contains(&match model {
                ModelSelection::Rho { .. } => ModelRuntime::Rho,
                ModelSelection::ClaudeCli { .. } => ModelRuntime::ClaudeCli,
            })
        }
        _ => false,
    };
    takes
        .then_some(())
        .ok_or_else(|| format!("Rho on {target} doesn't take this value"))
}

/// `value` for people, as `spec`'s control shows it.
fn shown(value: Option<&SettingValue>, spec: &SettingSpec) -> String {
    let Some(value) = value else {
        return "Not set".to_owned();
    };
    match (value, &spec.kind) {
        (SettingValue::Bool(on), _) => if *on { "On" } else { "Off" }.to_owned(),
        (SettingValue::Text(text), SettingKind::Choice { choices }) => choices
            .iter()
            .find(|choice| choice.value == *text)
            .map_or_else(|| text.clone(), |choice| choice.label.clone()),
        (SettingValue::Integer(number), SettingKind::Integer { unit, .. }) => {
            match unit.and_then(unit_label) {
                Some(unit) => format!("{number} {unit}"),
                None => number.to_string(),
            }
        }
        (SettingValue::Integer(number), _) => number.to_string(),
        (SettingValue::Text(text), _) => text.clone(),
        (SettingValue::Model(model), _) => selection_name(model),
    }
}

/// `item`'s row of a revert: unchanged while the target still holds what
/// the sync wrote.
pub(super) fn revert_row(
    item: &Planned,
    key: &SettingKey,
    before: &Held,
    after: &Held,
    target: &Side,
) -> RevertRow {
    let settings: &AgentSettings = &target.settings;
    let now = settings.value(key);
    let Some(spec) = settings.spec(key) else {
        let not_set = "Not set".to_owned();
        return RevertRow {
            key: item.key.clone(),
            label: item.label.clone(),
            synced: not_set.clone(),
            now: not_set.clone(),
            before: not_set,
            state: RevertState::Blocked(format!(
                "Rho on {} no longer has this setting",
                target.label
            )),
            item: None,
        };
    };
    let back = Change::Setting {
        key: key.clone(),
        before: Held {
            value: now.cloned(),
            set: settings.is_set(key),
        },
        after: before.clone(),
    };
    RevertRow::of(
        item,
        [before.value.as_ref(), after.value.as_ref(), now],
        |value| shown(value, spec),
        back,
    )
}
