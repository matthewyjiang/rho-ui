//! Settings sync: a one-shot copy of what one machine's rho holds to
//! another machine's, with Revert.
//!
//! [`diff`] compares two machines' [`Side`]s, as groups of rows by kind,
//! listing only what differs; each row knows what syncing it would write
//! and what that overwrites. [`Diff::plan`] takes the rows the user
//! checked, and [`apply`] writes them to the target: the rows the daemon
//! guards with a revision first, all at once, then the rest one at a time,
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
            });
        Plan::new(&self.revision, items)
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

impl Change {
    /// Whether the daemon writes it only against the revision it was read
    /// at, refusing it as stale once another client wrote since.
    fn is_guarded(&self) -> bool {
        match self {
            Self::Setting { .. } => true,
            Self::Plugin { .. } => false,
        }
    }

    /// This change, then `later` to the same thing: what this one
    /// overwrote, and what `later` left.
    fn then(&self, later: &Self) -> Self {
        match (self, later) {
            (Self::Setting { before, .. }, Self::Setting { key, after, .. }) => Self::Setting {
                key: key.clone(),
                before: before.clone(),
                after: after.clone(),
            },
            (Self::Plugin { before, .. }, Self::Plugin { name, after, .. }) => Self::Plugin {
                name: name.clone(),
                before: before.clone(),
                after: after.clone(),
            },
            // Rows of the same key are of the same kind.
            _ => later.clone(),
        }
    }
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

/// The rows to write to the target: first the guarded ones, all at once
/// against the revision they were read at, then, once those landed, the
/// unguarded ones, each on its own. So a sync refused as stale writes
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    guarded: Option<Guarded>,
    unguarded: Vec<Planned>,
}

/// A plan's guarded rows, never none, and the revision they were read at.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Guarded {
    revision: ConfigRevision,
    items: Vec<Planned>,
}

impl Plan {
    /// `items`, read at `revision`, split by whether they are guarded.
    fn new(revision: &ConfigRevision, items: impl IntoIterator<Item = Planned>) -> Self {
        let (guarded, unguarded): (Vec<_>, Vec<_>) =
            items.into_iter().partition(|item| item.change.is_guarded());
        let guarded = (!guarded.is_empty()).then(|| Guarded {
            revision: revision.clone(),
            items: guarded,
        });
        Self { guarded, unguarded }
    }

    /// `failure`'s row alone, to write again. Only an unguarded row fails
    /// alone, so it needs no revision.
    pub fn retry(failure: &Failure) -> Self {
        Self {
            guarded: None,
            unguarded: vec![failure.item.clone()],
        }
    }

    /// The rows it writes, in order.
    pub fn items(&self) -> impl Iterator<Item = &Planned> {
        let guarded = self.guarded.iter().flat_map(|guarded| &guarded.items);
        guarded.chain(&self.unguarded)
    }

    /// How many rows it writes.
    pub fn len(&self) -> usize {
        self.items().count()
    }

    /// Whether it writes nothing.
    pub fn is_empty(&self) -> bool {
        self.guarded.is_none() && self.unguarded.is_empty()
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
    /// How long a snapshot is kept after its sync, in seconds: 7 days, the
    /// span rho's sync design (for the methods still to come) gives the
    /// target daemon's journal of overwritten secrets, so both expire
    /// together. rho-protocol 1.6.0 has no such journal yet.
    pub const KEEP_FOR: i64 = 7 * 24 * 60 * 60;

    /// Whether the snapshot is past [`Self::KEEP_FOR`] at `now`, in Unix
    /// seconds, and should be dropped.
    pub fn is_expired(&self, now: i64) -> bool {
        now.saturating_sub(self.at) >= Self::KEEP_FOR
    }

    /// Nothing yet, for a sync at `at`.
    pub fn empty(at: i64) -> Self {
        Self {
            at,
            items: Vec::new(),
        }
    }

    /// All of `plan`'s rows, before it is applied at `at`: what Revert
    /// finds if rho-ui stops while it writes.
    pub fn new(at: i64, plan: &Plan) -> Self {
        Self {
            at,
            items: plan.items().cloned().collect(),
        }
    }

    /// Whether it has nothing left to revert.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Adds what a sync, or a retry of one of its rows, `written` to the
    /// target. A row already here keeps what it first overwrote.
    pub fn after_sync(&mut self, written: &Written) {
        for item in &written.landed {
            match self.items.iter_mut().find(|kept| kept.key == item.key) {
                Some(kept) => kept.change = kept.change.then(&item.change),
                None => self.items.push(item.clone()),
            }
        }
    }

    /// Drops what a revert `written` put back: the rest can still be
    /// reverted.
    pub fn after_revert(&mut self, written: &Written) {
        self.items
            .retain(|item| !written.landed.iter().any(|landed| landed.key == item.key));
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
            .filter_map(|row| row.item.clone());
        Plan::new(&self.revision, items)
    }
}

/// What applying a plan came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The guarded rows were refused, so nothing was written.
    Refused(Refusal),
    /// The guarded rows landed, and the unguarded ones were written.
    Written(Written),
}

/// Why the target refused a plan's guarded rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Its settings changed since they were read: read them again.
    Stale,
    /// It refused them, in the daemon's words.
    Failed(String),
}

/// The rows a plan wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Written {
    /// The rows that landed, in order, with those that landed only in part.
    pub landed: Vec<Planned>,
    /// The rows that didn't, or only in part, in order: an unguarded row
    /// each.
    pub failed: Vec<Failure>,
}

impl Written {
    /// The rows that landed whole.
    pub fn whole(&self) -> impl Iterator<Item = &Planned> {
        self.landed
            .iter()
            .filter(|item| !self.failed.iter().any(|f| f.item.key == item.key))
    }
}

/// A row that didn't land, and why, in the daemon's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What of the row is left to write, to retry.
    pub item: Planned,
    /// Why it didn't land.
    pub reason: String,
}

/// Why a row's write didn't land, and what of it is left when part did.
struct Unwritten {
    reason: String,
    rest: Option<Box<Change>>,
}

/// Writes `plan` to `target`'s daemon: its guarded rows at once, against
/// the revision the plan read (the daemon takes it while only this client
/// wrote since); then, only if they landed, each unguarded row on its own,
/// so one refusal fails its row alone.
pub async fn apply(target: &Daemon, plan: Plan) -> Applied {
    let mut written = Written::default();
    if let Some(Guarded { revision, items }) = plan.guarded {
        let changes = items
            .iter()
            .filter_map(|item| match &item.change {
                Change::Setting { key, after, .. } => Some(after.write(key)),
                Change::Plugin { .. } => None,
            })
            .collect();
        match target.set_agent_settings(revision, changes).await {
            Ok(()) => written.landed = items,
            Err(DaemonError::Conflict(_)) => return Applied::Refused(Refusal::Stale),
            Err(error) => return Applied::Refused(Refusal::Failed(error.to_string())),
        }
    }
    for item in plan.unguarded {
        // A plan keeps settings with the guarded rows.
        let Change::Plugin {
            name,
            before,
            after,
        } = &item.change
        else {
            continue;
        };
        let result = plugins::write(target, name, before.as_ref(), after.as_ref()).await;
        match result {
            Ok(()) => written.landed.push(item),
            Err(Unwritten { reason, rest }) => {
                let left = match rest {
                    Some(change) => {
                        written.landed.push(item.clone());
                        Planned {
                            change: *change,
                            ..item
                        }
                    }
                    None => item,
                };
                written.failed.push(Failure { item: left, reason });
            }
        }
    }
    Applied::Written(written)
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
