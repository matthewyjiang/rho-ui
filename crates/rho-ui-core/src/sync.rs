//! Settings sync: a one-shot copy of what one machine's rho holds to
//! another machine's, with Revert.
//!
//! [`read`] reads a machine as a [`Side`], and [`diff`] compares two of
//! them, as groups of rows by kind, listing only what differs; each row
//! knows what syncing it would write and what that overwrites.
//! [`Diff::plan`] takes the rows the user checked, and [`apply`] writes
//! them to the target: first the parts of rho's configuration the daemon
//! guards with a revision (`Store`s), each all at once, in an order that
//! never names something the target lacks yet; then, unless one was stale,
//! the rest one at a time. A [`Snapshot`] keeps what the target held
//! before, so [`review_revert`] can tell which rows the target changed
//! since the sync, and [`RevertReview::plan`] puts back the rest, in the
//! reverse order.
//!
//! Each kind is a [`GroupKind`]: rho's settings (`rho/config/*`, in
//! `settings.rs`), the user's agent plugins (`rho/plugins/*`, in
//! `plugins.rs`), model aliases and favorites and provider profiles
//! (`rho/config/table/*`, in `tables.rs`), and the user's hooks file
//! (`rho/hooks/file/*`, in `hooks.rs`). A view draws a group and its rows
//! from their fields alone, so a kind added here needs no change there.

mod hooks;
mod plugins;
mod settings;
mod tables;

use std::collections::{BTreeMap, BTreeSet};

use rho_protocol::{ConfigRevision, ConfigTable, Plugin, SettingKey, TableChange};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AgentSettings, Daemon, DaemonError, HooksFileRead, TableRead};
use plugins::{Found, PluginCopy};
use settings::Held;

/// What a sync copies, a group of rows each, in the order a view shows
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    /// rho's own settings, its `config.toml`.
    RhoSettings,
    /// The user's agent plugins.
    Plugins,
    /// Model aliases and favorite models.
    Models,
    /// Provider profiles: `ollama` and custom providers.
    Providers,
    /// The user's hooks file.
    Hooks,
}

impl GroupKind {
    /// The group's heading.
    pub fn title(self) -> &'static str {
        match self {
            Self::RhoSettings => "Rho settings",
            Self::Plugins => "Plugins",
            Self::Models => "Models",
            Self::Providers => "Provider profiles",
            Self::Hooks => "Hooks",
        }
    }

    /// A short name for element IDs.
    pub fn id(self) -> &'static str {
        match self {
            Self::RhoSettings => "settings",
            Self::Plugins => "plugins",
            Self::Models => "models",
            Self::Providers => "providers",
            Self::Hooks => "hooks",
        }
    }
}

/// One row's identity, the same in every read of the same two machines:
/// its kind, and the setting's key, the plugin's name, or the entry's.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RowKey {
    /// The group it is in.
    pub kind: GroupKind,
    /// The setting's key, the plugin's name, or the entry's.
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
    /// It belongs to one machine (`SettingSpec::per_machine`), so a sync
    /// leaves it unless the user checks it.
    pub one_machine: bool,
    /// It applies on the target only once the target's rho restarts.
    pub restart: bool,
    /// What else a person should know before syncing it, such as what it
    /// leaves behind.
    pub note: Option<String>,
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

/// A part of rho's configuration the daemon writes only against the
/// revision it was read at, all of a write or none: in the order a sync
/// writes them, so a key or model reference never names a provider the
/// target lacks yet. A revert writes them in the reverse order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Store {
    Providers,
    Aliases,
    Favorites,
    Settings,
    Hooks,
}

impl Store {
    /// The tables a sync reads.
    const TABLES: [Self; 3] = [Self::Providers, Self::Aliases, Self::Favorites];

    /// The `rho/config/table/*` table it is, if it is one.
    fn table(self) -> Option<ConfigTable> {
        match self {
            Self::Providers => Some(ConfigTable::Providers),
            Self::Aliases => Some(ConfigTable::ModelAliases),
            Self::Favorites => Some(ConfigTable::FavoriteModels),
            Self::Settings | Self::Hooks => None,
        }
    }

    /// The store `table` is, if this crate knows it.
    fn of_table(table: ConfigTable) -> Option<Self> {
        match table {
            ConfigTable::Providers => Some(Self::Providers),
            ConfigTable::ModelAliases => Some(Self::Aliases),
            ConfigTable::FavoriteModels => Some(Self::Favorites),
            _ => None,
        }
    }

    /// Its name for people.
    fn name(self) -> &'static str {
        match self {
            Self::Providers => "provider profiles",
            Self::Aliases => "model aliases",
            Self::Favorites => "favorite models",
            Self::Settings => "settings",
            Self::Hooks => "hooks file",
        }
    }
}

/// What one machine holds, as a sync reads it.
#[derive(Clone, Debug)]
pub struct Side {
    /// The machine's name, for the rows' notes and reasons.
    label: String,
    settings: AgentSettings,
    /// The user's plugins; `None` from a rho older than plugins.
    plugins: Option<Vec<Plugin>>,
    /// The tables its rho lets a client write; none from an older rho.
    tables: BTreeMap<Store, TableRead>,
    /// The user's hooks file; `None` from a rho older than reading it.
    hooks: Option<HooksFileRead>,
}

impl Side {
    /// What machine `label` holds: its rho settings, and its plugins
    /// (`None` from a rho older than plugins); no tables or hooks file
    /// until [`Self::with_table`] and [`Self::with_hooks`] add them.
    pub fn new(label: String, settings: AgentSettings, plugins: Option<Vec<Plugin>>) -> Self {
        Self {
            label,
            settings,
            plugins,
            tables: BTreeMap::new(),
            hooks: None,
        }
    }

    /// With `table` as `read`. A table this crate doesn't know is left
    /// out.
    #[must_use]
    pub fn with_table(mut self, table: ConfigTable, read: TableRead) -> Self {
        if let Some(store) = Store::of_table(table) {
            self.tables.insert(store, read);
        }
        self
    }

    /// With the user's hooks file as `hooks`.
    #[must_use]
    pub fn with_hooks(mut self, hooks: HooksFileRead) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// The revision a write to `store` sends, if this machine has it.
    fn revision(&self, store: Store) -> Option<ConfigRevision> {
        match store {
            Store::Settings => Some(self.settings.revision().clone()),
            Store::Hooks => self.hooks.as_ref().map(|hooks| hooks.revision.clone()),
            table => self.tables.get(&table).map(|read| read.revision.clone()),
        }
    }

    /// Every revision this machine has, by store.
    fn revisions(&self) -> BTreeMap<Store, ConfigRevision> {
        [Store::Settings, Store::Hooks]
            .into_iter()
            .chain(Store::TABLES)
            .filter_map(|store| Some((store, self.revision(store)?)))
            .collect()
    }
}

/// Reads what `daemon`'s machine, `label`, holds, all at once.
///
/// # Errors
///
/// Fails if any read does; a part an older rho lacks is left out instead.
pub async fn read(label: String, daemon: &Daemon) -> Result<Side, DaemonError> {
    let (settings, plugins, providers, aliases, favorites, hooks) = futures::join!(
        daemon.agent_settings(),
        daemon.plugins(None),
        daemon.config_table(ConfigTable::Providers),
        daemon.config_table(ConfigTable::ModelAliases),
        daemon.config_table(ConfigTable::FavoriteModels),
        daemon.hooks_file(),
    );
    let mut side = Side::new(label, settings?, plugins?);
    let tables = [
        (ConfigTable::Providers, providers?),
        (ConfigTable::ModelAliases, aliases?),
        (ConfigTable::FavoriteModels, favorites?),
    ];
    for (table, read) in tables {
        if let Some(read) = read {
            side = side.with_table(table, read);
        }
    }
    if let Some(hooks) = hooks? {
        side = side.with_hooks(hooks);
    }
    Ok(side)
}

/// What differs between two machines, by kind, in the order a view shows
/// it; only kinds with a difference have a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diff {
    /// The kinds with a difference, each with its rows.
    pub groups: Vec<Group>,
    /// The target's revisions the rows were read at, which the writes
    /// send.
    revisions: BTreeMap<Store, ConfigRevision>,
}

/// Compares what `source` holds with what `target` does.
pub fn diff(source: &Side, target: &Side) -> Diff {
    let plugins = match (&source.plugins, &target.plugins) {
        (Some(from), Some(to)) => plugins::rows(from, to, &source.label, &target.label),
        // A rho older than plugins has none to compare.
        _ => Vec::new(),
    };
    let models = [Store::Aliases, Store::Favorites]
        .into_iter()
        .flat_map(|store| tables::rows(store, source, target))
        .collect();
    let groups = [
        (GroupKind::RhoSettings, settings::rows(source, target)),
        (GroupKind::Plugins, plugins),
        (GroupKind::Models, models),
        (
            GroupKind::Providers,
            tables::rows(Store::Providers, source, target),
        ),
        (GroupKind::Hooks, hooks::rows(source, target)),
    ]
    .into_iter()
    .filter(|(_, rows)| !rows.is_empty())
    .map(|(kind, rows)| Group { kind, rows })
    .collect();
    Diff {
        groups,
        revisions: target.revisions(),
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

    /// What syncing the `checked` rows writes. A row that can't be copied
    /// is never in it.
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
        Plan::new(&self.revisions, items, Order::Sync)
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
    /// An entry of a table, as JSON; `None` when the table lacks it.
    Entry {
        store: Store,
        key: String,
        before: Option<Value>,
        after: Option<Value>,
    },
    /// The user's hooks file's text; `None` when there is no file.
    Hooks {
        before: Option<String>,
        after: Option<String>,
    },
}

impl Change {
    /// The store the daemon guards it in, by revision; `None` for one it
    /// writes on its own, unguarded.
    fn store(&self) -> Option<Store> {
        match self {
            Self::Setting { .. } => Some(Store::Settings),
            Self::Entry { store, .. } => Some(*store),
            Self::Hooks { .. } => Some(Store::Hooks),
            Self::Plugin { .. } => None,
        }
    }

    /// This change, then `later` to the same thing: what this one
    /// overwrote, and what `later` left.
    fn then(&self, later: &Self) -> Self {
        let mut combined = later.clone();
        match (self, &mut combined) {
            (Self::Setting { before, .. }, Self::Setting { before: first, .. }) => {
                first.clone_from(before);
            }
            (Self::Plugin { before, .. }, Self::Plugin { before: first, .. }) => {
                first.clone_from(before);
            }
            (Self::Entry { before, .. }, Self::Entry { before: first, .. }) => {
                first.clone_from(before);
            }
            (Self::Hooks { before, .. }, Self::Hooks { before: first, .. }) => {
                first.clone_from(before);
            }
            // Rows of the same key are of the same kind.
            _ => {}
        }
        combined
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

/// Which way a plan writes the stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Order {
    Sync,
    Revert,
}

/// The rows to write to the target: first each store's guarded rows, all
/// at once against the revision they were read at, store by store; then,
/// unless a store was stale, the unguarded ones, each on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    guarded: Vec<Guarded>,
    unguarded: Vec<Planned>,
}

/// One store's rows of a plan, never none, and the revision they were
/// read at.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Guarded {
    store: Store,
    revision: ConfigRevision,
    items: Vec<Planned>,
}

impl Plan {
    /// `items`, read at `revisions`, split by store and written in
    /// `order`. A row of a store the target has no revision for can't be
    /// written, and a diff never lists one.
    fn new(
        revisions: &BTreeMap<Store, ConfigRevision>,
        items: impl IntoIterator<Item = Planned>,
        order: Order,
    ) -> Self {
        let mut by_store: BTreeMap<Store, Vec<Planned>> = BTreeMap::new();
        let mut unguarded = Vec::new();
        for item in items {
            match item.change.store() {
                Some(store) => by_store.entry(store).or_default().push(item),
                None => unguarded.push(item),
            }
        }
        let mut guarded: Vec<Guarded> = by_store
            .into_iter()
            .filter_map(|(store, items)| {
                let revision = revisions.get(&store)?.clone();
                Some(Guarded {
                    store,
                    revision,
                    items,
                })
            })
            .collect();
        if order == Order::Revert {
            guarded.reverse();
        }
        Self { guarded, unguarded }
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
        self.guarded.is_empty() && self.unguarded.is_empty()
    }
}

/// What the target held before a sync wrote to it, kept so the sync can be
/// reverted: the sync's rows, each with what it overwrote. Saved as JSON.
/// It never holds a secret: no kind here has one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// When the sync ran, in Unix seconds.
    pub at: i64,
    items: Vec<Planned>,
}

/// What a run of [`apply`] was, for what it leaves of the target's
/// [`Snapshot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// A sync of the rows the user checked.
    Sync,
    /// One failed row of a sync, written again.
    Retry,
    /// A revert, or one failed row of one written again.
    Revert,
}

impl Snapshot {
    /// How long a snapshot is kept after its sync, in seconds: 7 days, as
    /// long as the target daemon keeps its sync journal of overwritten
    /// secrets (`rho/sync/*`), so both expire together.
    pub const KEEP_FOR: i64 = 7 * 24 * 60 * 60;

    /// Whether the snapshot is past [`Self::KEEP_FOR`] at `now`, in Unix
    /// seconds, and should be dropped.
    pub fn is_expired(&self, now: i64) -> bool {
        now.saturating_sub(self.at) >= Self::KEEP_FOR
    }

    /// All of `plan`'s rows, before it is applied at `at`: what Revert
    /// finds if rho-ui stops while it writes.
    pub fn new(at: i64, plan: &Plan) -> Self {
        Self {
            at,
            items: plan.items().cloned().collect(),
        }
    }

    /// What Revert can put back once `pass` wrote `written` at `now`,
    /// given `kept`, the snapshot from before it. A sync that landed
    /// something replaces the last one; one that landed nothing leaves it.
    /// A retried row joins its sync, keeping what it first overwrote, and
    /// a revert drops what it put back. `None` when nothing is left.
    pub fn after(kept: Option<Self>, pass: Pass, written: &Written, now: i64) -> Option<Self> {
        let next = match pass {
            Pass::Sync if written.landed.is_empty() => kept,
            Pass::Sync => Some(Self::empty(now).landing(written)),
            Pass::Retry => Some(kept.unwrap_or_else(|| Self::empty(now)).landing(written)),
            Pass::Revert => kept.map(|mut snapshot| {
                snapshot
                    .items
                    .retain(|item| !written.landed.iter().any(|landed| landed.key == item.key));
                snapshot
            }),
        };
        next.filter(|snapshot| !snapshot.items.is_empty())
    }

    fn empty(at: i64) -> Self {
        Self {
            at,
            items: Vec::new(),
        }
    }

    /// `self` with `written`'s landed rows: a row already here keeps what
    /// it first overwrote.
    fn landing(mut self, written: &Written) -> Self {
        for item in &written.landed {
            match self.items.iter_mut().find(|kept| kept.key == item.key) {
                Some(kept) => kept.change = kept.change.then(&item.change),
                None => self.items.push(item.clone()),
            }
        }
        self
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

impl RevertRow {
    /// `item`'s row, whose values `show` gives: unchanged while the target
    /// holds `now` equal to `after`, what the sync wrote. Reverting writes
    /// `back`, from `now` to `before`.
    fn of<T: PartialEq>(
        item: &Planned,
        [before, after, now]: [Option<&T>; 3],
        show: impl Fn(Option<&T>) -> String,
        back: Change,
    ) -> Self {
        let state = if now == after {
            RevertState::Unchanged
        } else {
            RevertState::Changed
        };
        Self {
            key: item.key.clone(),
            label: item.label.clone(),
            synced: show(after),
            now: show(now),
            before: show(before),
            state,
            item: Some(Planned {
                change: back,
                ..item.clone()
            }),
        }
    }
}

/// A sync's rows as the target holds them now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevertReview {
    revisions: BTreeMap<Store, ConfigRevision>,
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
            Change::Entry {
                store,
                key,
                before,
                after,
            } => tables::revert_row(item, *store, key, [before.as_ref(), after.as_ref()], target),
            Change::Hooks { before, after } => {
                hooks::revert_row(item, [before.as_ref(), after.as_ref()], target)
            }
        })
        .collect();
    RevertReview {
        revisions: target.revisions(),
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
        Plan::new(&self.revisions, items, Order::Revert)
    }
}

/// What applying a plan came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Written {
    /// The rows that landed, in order, with those that landed only in part.
    pub landed: Vec<Planned>,
    /// The rows that didn't, or only in part, in order, each with why.
    pub failed: Vec<Failure>,
    /// A store changed on the target since it was read, so the plan
    /// stopped there: nothing after it was written, plugins included.
    pub stale: bool,
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

/// Writes `plan` to `target`'s daemon: each store's guarded rows at once,
/// store by store, against the revision the plan read (the daemon takes
/// it while only this client wrote since); then each unguarded row on its
/// own. A store the daemon refuses fails its rows, and the rest go on; a
/// stale one stops the plan.
pub async fn apply(target: &Daemon, plan: Plan) -> Written {
    let mut written = Written::default();
    for Guarded {
        store,
        revision,
        items,
    } in plan.guarded
    {
        match write_store(target, store, revision, &items).await {
            Ok(()) => written.landed.extend(items),
            Err(DaemonError::Conflict(_)) => {
                written.stale = true;
                return written;
            }
            Err(error) => {
                let reason = error.to_string();
                written.failed.extend(items.into_iter().map(|item| Failure {
                    item,
                    reason: reason.clone(),
                }));
            }
        }
    }
    for item in plan.unguarded {
        // A plan keeps every other kind with its store.
        let Change::Plugin {
            name,
            before,
            after,
        } = &item.change
        else {
            continue;
        };
        match plugins::write(target, name, before.as_ref(), after.as_ref()).await {
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
    written
}

/// Writes `items`, all of `store`, in one request against `revision`.
async fn write_store(
    target: &Daemon,
    store: Store,
    revision: ConfigRevision,
    items: &[Planned],
) -> Result<(), DaemonError> {
    if let Some(table) = store.table() {
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
                _ => None,
            })
            .collect();
        return target
            .set_config_table(table, revision, changes)
            .await
            .map(drop);
    }
    if store == Store::Settings {
        let changes = items
            .iter()
            .filter_map(|item| match &item.change {
                Change::Setting { key, after, .. } => Some(after.write(key)),
                _ => None,
            })
            .collect();
        return target.set_agent_settings(revision, changes).await;
    }
    // The hooks file is one row.
    let text = items.iter().find_map(|item| match &item.change {
        Change::Hooks { after, .. } => Some(after.clone()),
        _ => None,
    });
    target
        .set_hooks_file(revision, text.flatten())
        .await
        .map(drop)
}

/// Writes failed row `failure` again: a guarded one against its store's
/// revision as the target holds it now.
pub async fn retry(target: &Daemon, failure: &Failure) -> Written {
    let item = failure.item.clone();
    let mut revisions = BTreeMap::new();
    if let Some(store) = item.change.store() {
        match revision_now(target, store).await {
            Ok(revision) => {
                revisions.insert(store, revision);
            }
            Err(error) => {
                return Written {
                    failed: vec![Failure {
                        item,
                        reason: error.to_string(),
                    }],
                    ..Written::default()
                };
            }
        }
    }
    apply(target, Plan::new(&revisions, [item], Order::Sync)).await
}

/// `store`'s revision as `target` holds it now.
async fn revision_now(target: &Daemon, store: Store) -> Result<ConfigRevision, DaemonError> {
    let gone = || DaemonError::Refused(format!("Rho there no longer has its {}", store.name()));
    if let Some(table) = store.table() {
        return target
            .config_table(table)
            .await?
            .map(|read| read.revision)
            .ok_or_else(gone);
    }
    if store == Store::Settings {
        return Ok(target.agent_settings().await?.revision().clone());
    }
    target
        .hooks_file()
        .await?
        .map(|hooks| hooks.revision)
        .ok_or_else(gone)
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
