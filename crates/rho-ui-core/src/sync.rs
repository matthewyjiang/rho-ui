//! Settings sync: a one-shot copy of what one machine's rho holds to
//! another machine's, with Revert.
//!
//! [`diff`] compares two machines' [`Side`]s, as groups of rows by kind,
//! listing only what differs; each row knows what syncing it would write
//! and what that overwrites. [`Diff::plan`] takes the rows the user
//! checked, and [`apply`] writes them to the target, one at a time,
//! saying which landed. A [`Snapshot`] of the plan keeps what the target
//! held before, so [`review_revert`] can tell which rows the target changed
//! since the sync, and [`RevertReview::plan`] puts back the rest.
//!
//! Each kind is a [`GroupKind`]: rho's settings (`rho/config/*`, in
//! `settings.rs`) and the user's agent plugins (`rho/plugins/*`, in
//! `plugins.rs`). A view draws a group and its rows from their fields
//! alone, so a kind added here needs no change there.

mod plugins;
mod settings;

use std::collections::BTreeSet;

use rho_protocol::{ConfigRevision, Plugin, SettingKey};
use serde::{Deserialize, Serialize};

use crate::{AgentSettings, Daemon, DaemonError};
use plugins::{Found, PluginCopy};
use settings::Held;

/// What a sync copies, a group of rows each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    /// rho's own settings, its `config.toml`.
    RhoSettings,
    /// The user's agent plugins.
    Plugins,
}

impl GroupKind {
    /// The group's heading.
    pub fn title(self) -> &'static str {
        match self {
            Self::RhoSettings => "Rho settings",
            Self::Plugins => "Plugins",
        }
    }

    /// A short name for element IDs.
    pub fn id(self) -> &'static str {
        match self {
            Self::RhoSettings => "settings",
            Self::Plugins => "plugins",
        }
    }
}

/// One row's identity, the same in every read of the same two machines:
/// its kind, and the setting's key or the plugin's name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RowKey {
    /// The group it is in.
    pub kind: GroupKind,
    /// The setting's key, or the plugin's name.
    pub id: String,
}

/// What syncing a row does to the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Gives it something only the source has.
    Add,
    /// Changes something both have to the source's.
    Change,
    /// Removes something only the target has.
    Remove,
}

/// How a row starts out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowState {
    /// It syncs unless the user unchecks it.
    Checked,
    /// It can sync, but only if the user checks it: it belongs to one
    /// machine, or it removes something from the target.
    Unchecked,
    /// It can't sync, and why.
    CantCopy(String),
}

/// One thing that differs between the two machines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Its identity across reads.
    pub key: RowKey,
    /// Its name for people.
    pub label: String,
    /// A line under the label, such as where the thing came from.
    pub note: Option<String>,
    /// What the source has, for people; `None` when it has nothing.
    pub source: Option<String>,
    /// What the target has, for people; `None` when it has nothing.
    pub target: Option<String>,
    /// What syncing it does to the target.
    pub action: Action,
    /// It belongs to one machine (rho-protocol 1.7.0 marks such settings
    /// with `SettingSpec::per_machine`), so a sync leaves it unless the
    /// user checks it.
    pub one_machine: bool,
    /// It applies on the target only once the target's rho restarts.
    pub restart: bool,
    copy: Copy,
}

/// Whether a row can sync, and what that writes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Copy {
    Write(Change),
    Cannot(String),
}

impl Row {
    /// How the row starts: checked, unless it belongs to one machine or
    /// removes something, or it can't be copied at all.
    pub fn state(&self) -> RowState {
        match &self.copy {
            Copy::Cannot(why) => RowState::CantCopy(why.clone()),
            Copy::Write(_) if self.one_machine || self.action == Action::Remove => {
                RowState::Unchecked
            }
            Copy::Write(_) => RowState::Checked,
        }
    }

    /// Whether the user can check it.
    pub fn is_copyable(&self) -> bool {
        matches!(self.copy, Copy::Write(_))
    }
}

/// The rows of one kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// What its rows are.
    pub kind: GroupKind,
    /// What differs, in the order the machine lists it.
    pub rows: Vec<Row>,
}

/// What one machine holds, as a sync reads it.
#[derive(Clone, Debug)]
pub struct Side {
    /// The machine's name, for the rows' notes and reasons.
    label: String,
    settings: AgentSettings,
    /// The user's plugins; `None` from a rho older than plugins.
    plugins: Option<Vec<Plugin>>,
}

impl Side {
    /// What machine `label` holds: its rho settings, and its plugins
    /// (`None` from a rho older than plugins).
    pub fn new(label: String, settings: AgentSettings, plugins: Option<Vec<Plugin>>) -> Self {
        Self {
            label,
            settings,
            plugins,
        }
    }
}

/// What differs between two machines, by kind, in the order a view shows
/// it; only kinds with a difference have a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    /// The kinds with a difference, each with its rows.
    pub groups: Vec<Group>,
    /// The target's settings revision the rows were read at, which every
    /// write sends.
    revision: ConfigRevision,
}

/// Compares what `source` holds with what `target` does.
pub fn diff(source: &Side, target: &Side) -> Diff {
    let settings = settings::rows(source, target);
    let plugins = match (&source.plugins, &target.plugins) {
        (Some(from), Some(to)) => plugins::rows(from, to, &source.label, &target.label),
        // A rho older than plugins has none to compare.
        _ => Vec::new(),
    };
    let groups = [
        (GroupKind::RhoSettings, settings),
        (GroupKind::Plugins, plugins),
    ]
    .into_iter()
    .filter(|(_, rows)| !rows.is_empty())
    .map(|(kind, rows)| Group { kind, rows })
    .collect();
    Diff {
        groups,
        revision: target.settings.revision().clone(),
    }
}

impl Diff {
    /// Every row, group by group.
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.groups.iter().flat_map(|group| &group.rows)
    }

    /// Whether the two machines hold the same.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// The rows that start checked.
    pub fn initially_checked(&self) -> BTreeSet<RowKey> {
        self.rows()
            .filter(|row| row.state() == RowState::Checked)
            .map(|row| row.key.clone())
            .collect()
    }

    /// What syncing the `checked` rows writes, in the diff's order. A row
    /// that can't be copied is never in it.
    pub fn plan(&self, checked: &BTreeSet<RowKey>) -> Plan {
        let items = self
            .rows()
            .filter(|row| checked.contains(&row.key))
            .filter_map(|row| match &row.copy {
                Copy::Write(change) => Some(Planned {
                    key: row.key.clone(),
                    label: row.label.clone(),
                    restart: row.restart,
                    change: change.clone(),
                }),
                Copy::Cannot(_) => None,
            })
            .collect();
        Plan {
            revision: self.revision.clone(),
            items,
        }
    }
}

/// What a row writes, with what it overwrites, so it can be reversed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Change {
    Setting {
        key: SettingKey,
        before: Held,
        after: Held,
    },
    Plugin {
        name: String,
        before: Option<PluginCopy>,
        after: Option<PluginCopy>,
    },
}

/// One row a plan writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Planned {
    key: RowKey,
    label: String,
    restart: bool,
    change: Change,
}

impl Planned {
    /// The row it writes.
    pub fn key(&self) -> &RowKey {
        &self.key
    }

    /// The row's name for people.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// It applies on the target only once the target's rho restarts.
    pub fn needs_restart(&self) -> bool {
        self.restart
    }
}

/// The rows to write to the target, and the settings revision they were
/// read at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    revision: ConfigRevision,
    items: Vec<Planned>,
}

impl Plan {
    /// `item` alone, written against `revision`, as a retry does once it
    /// read the target again.
    pub fn retry(item: Planned, revision: ConfigRevision) -> Self {
        Self {
            revision,
            items: vec![item],
        }
    }

    /// The rows it writes, in order.
    pub fn items(&self) -> &[Planned] {
        &self.items
    }

    /// How many rows it writes.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether it writes nothing.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// What the target held before a sync wrote to it, kept so the sync can be
/// reverted: the sync's rows, each with what it overwrote. Saved as JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// When the sync ran, in Unix seconds.
    pub at: i64,
    items: Vec<Planned>,
}

impl Snapshot {
    /// How long a snapshot is kept after its sync, in seconds: 7 days, as
    /// long as the target daemon keeps its journal of overwritten secrets,
    /// so a revert never outlives what the target can put back.
    pub const KEEP_FOR: i64 = 7 * 24 * 60 * 60;

    /// Whether the snapshot is past [`Self::KEEP_FOR`] at `now`, in Unix
    /// seconds, and should be dropped.
    pub fn is_expired(&self, now: i64) -> bool {
        now.saturating_sub(self.at) >= Self::KEEP_FOR
    }

    /// `plan`'s rows, before it is applied at `at`.
    pub fn new(at: i64, plan: &Plan) -> Self {
        Self {
            at,
            items: plan.items.clone(),
        }
    }

    /// How many rows it can revert.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether it has nothing left to revert.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Keeps only the rows of the sync that landed, once it is applied:
    /// the target holds nothing of the rest to revert.
    pub fn keep_landed(&mut self, applied: &Applied) {
        self.items
            .retain(|item| applied.landed.iter().any(|landed| landed.key == item.key));
    }

    /// Drops the rows a revert put back, once it is applied: what is left
    /// can still be reverted.
    pub fn drop_reverted(&mut self, applied: &Applied) {
        self.items
            .retain(|item| !applied.landed.iter().any(|landed| landed.key == item.key));
    }

    /// Adds `item`, as a retried row that landed.
    pub fn add(&mut self, item: Planned) {
        self.items.retain(|kept| kept.key != item.key);
        self.items.push(item);
    }
}

/// How a row of a revert stands against what the target holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevertState {
    /// The target still holds what the sync wrote.
    Unchanged,
    /// The target changed it since; reverting would undo that.
    Changed,
    /// The target changed it to something Revert can't replace, and why.
    Blocked(String),
}

/// One row of a sync, as Revert finds it on the target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevertRow {
    /// The row of the sync it reverts.
    pub key: RowKey,
    /// The row's name for people.
    pub label: String,
    /// What the sync wrote, for people.
    pub synced: String,
    /// What the target holds now.
    pub now: String,
    /// What it held before the sync, which Revert puts back.
    pub before: String,
    /// How it stands now.
    pub state: RevertState,
    /// What reverting it writes: back from what the target holds now.
    item: Option<Planned>,
}

/// A sync's rows as the target holds them now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevertReview {
    revision: ConfigRevision,
    /// The sync's rows, in its order.
    pub rows: Vec<RevertRow>,
}

/// Compares what `snapshot`'s sync wrote with what `target` holds now.
pub fn review_revert(snapshot: &Snapshot, target: &Side) -> RevertReview {
    let rows = snapshot
        .items
        .iter()
        .map(|item| match &item.change {
            Change::Setting { key, before, after } => {
                settings::revert_row(item, key, before, after, target)
            }
            Change::Plugin {
                name,
                before,
                after,
            } => {
                let found = Found::of(name, target.plugins.as_deref().unwrap_or_default());
                plugins::revert_row(item, name, before.as_ref(), after.as_ref(), &found)
            }
        })
        .collect();
    RevertReview {
        revision: target.settings.revision().clone(),
        rows,
    }
}

impl RevertReview {
    /// The rows the target changed since the sync.
    pub fn changed(&self) -> impl Iterator<Item = &RevertRow> {
        self.rows
            .iter()
            .filter(|row| row.state != RevertState::Unchanged)
    }

    /// What reverting writes: every unchanged row, and the changed ones in
    /// `anyway`.
    pub fn plan(&self, anyway: &BTreeSet<RowKey>) -> Plan {
        let items = self
            .rows
            .iter()
            .filter(|row| match row.state {
                RevertState::Unchanged => true,
                RevertState::Changed => anyway.contains(&row.key),
                RevertState::Blocked(_) => false,
            })
            .filter_map(|row| row.item.clone())
            .collect();
        Plan {
            revision: self.revision.clone(),
            items,
        }
    }
}

/// What applying a plan came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// The rows that landed, in order.
    pub landed: Vec<Planned>,
    /// The rows that didn't, in order.
    pub failed: Vec<Failure>,
    /// The target's settings changed since they were read, so no setting
    /// past that point was written: read them again.
    pub stale: bool,
}

/// A row that didn't land, and why, in the daemon's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The row, to retry.
    pub item: Planned,
    /// Why it didn't land.
    pub reason: String,
}

/// Writes `plan` to `target`'s daemon, one row at a time, so one refusal
/// fails its row alone: settings first, each against the revision the plan
/// read (the daemon takes it while only this client wrote since), then
/// plugins. Settings stop at the first stale write.
pub async fn apply(target: &Daemon, plan: Plan) -> Applied {
    let mut applied = Applied::default();
    for item in plan.items {
        let written = match &item.change {
            Change::Setting { key, after, .. } => {
                if applied.stale {
                    continue;
                }
                let write = after.write(key);
                match target
                    .set_agent_settings(plan.revision.clone(), vec![write])
                    .await
                {
                    Err(DaemonError::Conflict(_)) => {
                        applied.stale = true;
                        continue;
                    }
                    written => written.map_err(|error| error.to_string()),
                }
            }
            Change::Plugin {
                name,
                before,
                after,
            } => plugins::write(target, name, before.as_ref(), after.as_ref()).await,
        };
        match written {
            Ok(()) => applied.landed.push(item),
            Err(reason) => applied.failed.push(Failure { item, reason }),
        }
    }
    applied
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
