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
//! since the sync, and [`RevertReview::plan`] puts back the rest, the
//! stores in the reverse order.
//!
//! Each kind is a [`GroupKind`]: rho's settings (`rho/config/*`, in
//! `settings.rs`), the user's agent plugins (`rho/plugins/*`, in
//! `plugins.rs`), model aliases and favorites and provider profiles
//! (`rho/config/table/*`, in `tables.rs`), API keys and logins
//! (`keys.rs`), MCP servers (`rho/mcp/config/*`, in `mcp.rs`), and the
//! user's hooks file (`rho/hooks/file/*`, in `hooks.rs`). Each kind's
//! module reads, diffs and writes it; `apply.rs` runs a plan. A view draws
//! a group and its rows from their fields alone, so a kind added here
//! needs no change there.
//!
//! Secrets, API keys and MCP servers' header and environment values, never
//! pass through rho-ui: the source daemon seals them to a one-time key the
//! target issues, and only the target opens them (`secrets.rs`). The target
//! journals what a sync replaced of them under the sync's [`SyncId`], and
//! puts it back on Revert (`journal.rs`); a [`Snapshot`] keeps only which
//! of them the sync wrote.

mod apply;
mod hooks;
mod journal;
mod keys;
mod mcp;
mod plugins;
mod secrets;
mod settings;
mod tables;

use std::collections::{BTreeMap, BTreeSet};

use rho_protocol::{
    AuthSummary, ConfigRevision, ConfigTable, McpConfigGetResponse, McpSecretSlot, Plugin,
    SettingKey, SyncId, SyncItem,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AgentSettings, Daemon, DaemonError, HooksFileRead, TableRead};
use plugins::{Found, PluginCopy};
use settings::Held;

pub use apply::{apply, retry, Failure, Machines, Written};
pub use journal::{discard, journal, Journal};
pub use secrets::{compare, Secrets};

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
    /// API keys, and logins to make on the target.
    SignIns,
    /// MCP servers, their secrets with them.
    Mcp,
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
            Self::SignIns => "Sign-ins",
            Self::Mcp => "MCP servers",
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
            Self::SignIns => "sign-ins",
            Self::Mcp => "mcp",
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

impl Action {
    /// What syncing does, given what the source and the target have.
    fn of<A, B>(source: Option<A>, target: Option<B>) -> Self {
        match (source, target) {
            (Some(_), None) => Self::Add,
            (None, Some(_)) => Self::Remove,
            _ => Self::Change,
        }
    }
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
    /// What the user can do on the target instead, for a row that can't be
    /// copied.
    pub offer: Option<Offer>,
    copy: Copy,
}

/// Something the user can do on the target for a row a sync can't copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    /// Run auth profile `auth`'s login on the target: logins are never
    /// copied.
    SignIn {
        /// The auth profile.
        auth: String,
    },
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

    /// The row `id` of `kind` standing for a whole part the source couldn't
    /// be read for, saying `why`.
    fn unread(kind: GroupKind, id: &str, label: &str, source: &Side, why: &str) -> Self {
        Self {
            key: RowKey {
                kind,
                id: id.to_owned(),
            },
            label: label.to_owned(),
            source: None,
            target: None,
            action: Action::Change,
            one_machine: false,
            restart: false,
            note: None,
            offer: None,
            copy: Copy::Cannot(format!("Couldn't read it on {}: {why}", source.label)),
        }
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

/// A table of `config.toml` a sync copies entry by entry, in the order a
/// sync writes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Table {
    Providers,
    Aliases,
    Favorites,
}

impl Table {
    const ALL: [Self; 3] = [Self::Providers, Self::Aliases, Self::Favorites];

    /// The table as `rho/config/table/*` names it.
    fn config(self) -> ConfigTable {
        match self {
            Self::Providers => ConfigTable::Providers,
            Self::Aliases => ConfigTable::ModelAliases,
            Self::Favorites => ConfigTable::FavoriteModels,
        }
    }

    /// Its name for people.
    fn name(self) -> &'static str {
        match self {
            Self::Providers => "provider profiles",
            Self::Aliases => "model aliases",
            Self::Favorites => "favorite models",
        }
    }
}

/// A part of rho's configuration the daemon writes only against the
/// revision it was read at, all of a write or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Store {
    Table(Table),
    Mcp,
    Settings,
    Hooks,
}

/// Where a plan writes a change, in the order a sync writes them, so a key
/// or model reference never names a provider the target lacks yet. A
/// revert writes them in the reverse order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    Providers,
    /// API keys, sealed, each saved on its own.
    Keys,
    Aliases,
    Favorites,
    /// The target's sync journal, which puts back the keys and MCP servers
    /// a sync replaced: only a revert writes it.
    Journal,
    Mcp,
    Settings,
    Hooks,
}

impl Place {
    /// The store the daemon guards this place's writes with; `None` for
    /// the places it writes item by item.
    fn store(self) -> Option<Store> {
        match self {
            Self::Providers => Some(Store::Table(Table::Providers)),
            Self::Aliases => Some(Store::Table(Table::Aliases)),
            Self::Favorites => Some(Store::Table(Table::Favorites)),
            Self::Mcp => Some(Store::Mcp),
            Self::Settings => Some(Store::Settings),
            Self::Hooks => Some(Store::Hooks),
            Self::Keys | Self::Journal => None,
        }
    }
}

/// One part of what a machine holds, as a sync read it.
#[derive(Clone, Debug)]
enum Part<T> {
    Read(T),
    /// Its rho is older than the method that reads it.
    Lacks,
    /// The read failed, in the daemon's words.
    Failed(String),
}

impl<T> Part<T> {
    /// A read's part: one an older rho lacks, or one that failed, is
    /// still a part, so the rest can be compared. Only a lost connection
    /// fails the whole read.
    fn of(read: Result<Option<T>, DaemonError>) -> Result<Self, DaemonError> {
        match read {
            Ok(Some(read)) => Ok(Self::Read(read)),
            Ok(None) => Ok(Self::Lacks),
            Err(DaemonError::Closed) => Err(DaemonError::Closed),
            Err(error) => Ok(Self::Failed(error.to_string())),
        }
    }

    fn read(&self) -> Option<&T> {
        match self {
            Self::Read(read) => Some(read),
            Self::Lacks | Self::Failed(_) => None,
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
    /// Each table read; one left out is as one an older rho lacks.
    tables: BTreeMap<Table, Part<TableRead>>,
    hooks: Part<HooksFileRead>,
    /// Its auth profiles, signed in or not.
    auths: Part<Vec<AuthSummary>>,
    /// Its MCP servers, their secrets named without values.
    mcp: Part<McpConfigGetResponse>,
}

impl Side {
    /// `table` as this machine holds it.
    fn table(&self, table: Table) -> &Part<TableRead> {
        self.tables.get(&table).unwrap_or(&Part::Lacks)
    }

    /// What this machine holds of `part`, or why it can't say, and so
    /// can't take rows of it.
    fn take<'a, T>(&self, part: &'a Part<T>) -> Result<&'a T, String> {
        match part {
            Part::Read(read) => Ok(read),
            Part::Lacks => Err(format!("Rho on {} is too old to take this", self.label)),
            Part::Failed(why) => Err(format!("Couldn't read it on {}: {why}", self.label)),
        }
    }

    /// The revision a write to `store` sends, if this machine has it.
    fn revision(&self, store: Store) -> Option<ConfigRevision> {
        match store {
            Store::Table(table) => self.table(table).read().map(|read| read.revision.clone()),
            Store::Mcp => self.mcp.read().map(|read| read.revision.clone()),
            Store::Settings => Some(self.settings.revision().clone()),
            Store::Hooks => self.hooks.read().map(|hooks| hooks.revision.clone()),
        }
    }

    /// Every revision this machine has, by store.
    fn revisions(&self) -> BTreeMap<Store, ConfigRevision> {
        Table::ALL
            .into_iter()
            .map(Store::Table)
            .chain([Store::Mcp, Store::Settings, Store::Hooks])
            .filter_map(|store| Some((store, self.revision(store)?)))
            .collect()
    }
}

/// Reads what `daemon`'s machine, `label`, holds, all at once.
///
/// # Errors
///
/// Fails if its rho settings or plugins can't be read, or the connection
/// is gone. A table or the hooks file that an older rho lacks, or that
/// fails to read, is kept as such, and its rows say so.
pub async fn read(label: String, daemon: &Daemon) -> Result<Side, DaemonError> {
    let tables =
        futures::future::join_all(Table::ALL.map(|table| daemon.config_table(table.config())));
    let (settings, plugins, tables, hooks, auths, mcp) = futures::join!(
        daemon.agent_settings(),
        daemon.plugins(None),
        tables,
        daemon.hooks_file(),
        daemon.auths(),
        daemon.mcp_config(),
    );
    let tables = Table::ALL
        .into_iter()
        .zip(tables)
        .map(|(table, read)| Ok((table, Part::of(read)?)))
        .collect::<Result<_, DaemonError>>()?;
    Ok(Side {
        label,
        settings: settings?,
        plugins: plugins?,
        tables,
        hooks: Part::of(hooks)?,
        auths: Part::of(auths.map(Some))?,
        mcp: Part::of(mcp)?,
    })
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

/// Compares what `source` holds with what `target` does, their secrets as
/// `secrets` compared them.
pub fn diff(source: &Side, target: &Side, secrets: &Secrets) -> Diff {
    let plugins = match (&source.plugins, &target.plugins) {
        (Some(from), Some(to)) => plugins::rows(from, to, &source.label, &target.label),
        // A rho older than plugins has none to compare.
        _ => Vec::new(),
    };
    let models = [Table::Aliases, Table::Favorites]
        .into_iter()
        .flat_map(|table| tables::rows(table, source, target))
        .collect();
    let groups = [
        (GroupKind::RhoSettings, settings::rows(source, target)),
        (GroupKind::Plugins, plugins),
        (GroupKind::Models, models),
        (
            GroupKind::Providers,
            tables::rows(Table::Providers, source, target),
        ),
        (GroupKind::SignIns, keys::rows(source, target, secrets)),
        (GroupKind::Mcp, mcp::rows(source, target, secrets)),
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
        table: Table,
        key: String,
        before: Option<Value>,
        after: Option<Value>,
    },
    /// The user's hooks file's text; `None` when there is no file.
    Hooks {
        before: Option<String>,
        after: Option<String>,
    },
    /// Auth profile `auth`'s API key, sealed from the source; it `replaced`
    /// a key the target had. Its values never leave the daemons.
    Key { auth: String, replaced: bool },
    /// MCP server `identity`, without its secrets' values; `None` when the
    /// target lacks it. A put fills the secrets in `keep` from the target's
    /// own values, and the rest sealed from the source.
    Mcp {
        identity: String,
        before: Option<McpServer>,
        after: Option<McpServer>,
        keep: Vec<McpSecretSlot>,
    },
    /// What the target's journal of the sync keeps of `item`, put back;
    /// `overwrite` even though it changed since.
    Restore { item: SyncItem, overwrite: bool },
}

/// An MCP server as the daemon reads it: its entry, without `headers` and
/// `env`, and the names of the secrets those hold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct McpServer {
    /// Boxed: a JSON object is large, and a [`Change`] is kept by value.
    value: Box<Value>,
    secrets: Vec<McpSecretSlot>,
}

impl Change {
    /// Where a plan writes it; `None` for one it writes on its own, after
    /// the rest.
    fn place(&self) -> Option<Place> {
        match self {
            Self::Setting { .. } => Some(Place::Settings),
            Self::Entry { table, .. } => Some(match table {
                Table::Providers => Place::Providers,
                Table::Aliases => Place::Aliases,
                Table::Favorites => Place::Favorites,
            }),
            Self::Hooks { .. } => Some(Place::Hooks),
            Self::Key { .. } => Some(Place::Keys),
            Self::Mcp { .. } => Some(Place::Mcp),
            Self::Restore { .. } => Some(Place::Journal),
            Self::Plugin { .. } => None,
        }
    }

    /// Whether the target already holds what it writes.
    fn is_noop(&self) -> bool {
        match self {
            Self::Setting { before, after, .. } => before == after,
            Self::Plugin { before, after, .. } => before == after,
            Self::Entry { before, after, .. } => before == after,
            Self::Hooks { before, after } => before == after,
            Self::Mcp {
                before,
                after,
                keep,
                ..
            } => {
                before == after
                    && after
                        .as_ref()
                        .is_none_or(|server| server.secrets.iter().all(|s| keep.contains(s)))
            }
            Self::Key { .. } | Self::Restore { .. } => false,
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
            (Self::Mcp { before, .. }, Self::Mcp { before: first, .. }) => {
                first.clone_from(before);
            }
            (
                Self::Key { replaced, .. },
                Self::Key {
                    replaced: first, ..
                },
            ) => *first = *replaced,
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

/// The rows to write to the target: first each place's rows, place by
/// place, a store's all at once against the revision they were read at;
/// then, unless a store was stale, the unguarded ones, each on its own. A
/// row the target already holds as it would write needs no write at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    settled: Vec<Planned>,
    steps: Vec<Step>,
    unguarded: Vec<Planned>,
}

/// One place's rows of a plan, never none.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    /// A store's rows, and the revision they were read at.
    Guarded {
        store: Store,
        revision: ConfigRevision,
        items: Vec<Planned>,
    },
    /// API keys, sealed from the source and saved on the target.
    Keys(Vec<Planned>),
    /// What the target's journal of the sync puts back.
    Journal(Vec<Planned>),
}

impl Step {
    fn items(&self) -> &[Planned] {
        match self {
            Self::Guarded { items, .. } | Self::Keys(items) | Self::Journal(items) => items,
        }
    }
}

impl Plan {
    /// `items`, read at `revisions`, split by place and written in
    /// `order`. A row of a store the target has no revision for can't be
    /// written, and neither a diff nor a revert review offers one.
    fn new(
        revisions: &BTreeMap<Store, ConfigRevision>,
        items: impl IntoIterator<Item = Planned>,
        order: Order,
    ) -> Self {
        let mut settled = Vec::new();
        let mut by_place: BTreeMap<Place, Vec<Planned>> = BTreeMap::new();
        let mut unguarded = Vec::new();
        for item in items {
            match item.change.place() {
                _ if item.change.is_noop() => settled.push(item),
                Some(place) => by_place.entry(place).or_default().push(item),
                None => unguarded.push(item),
            }
        }
        let mut steps: Vec<Step> = by_place
            .into_iter()
            .filter_map(|(place, items)| match place {
                Place::Keys => Some(Step::Keys(items)),
                Place::Journal => Some(Step::Journal(items)),
                _ => {
                    let store = place.store()?;
                    let revision = revisions.get(&store)?.clone();
                    Some(Step::Guarded {
                        store,
                        revision,
                        items,
                    })
                }
            })
            .collect();
        if order == Order::Revert {
            steps.reverse();
        }
        Self {
            settled,
            steps,
            unguarded,
        }
    }

    /// The rows it writes, in order.
    pub fn items(&self) -> impl Iterator<Item = &Planned> {
        let steps = self.steps.iter().flat_map(Step::items);
        self.settled.iter().chain(steps).chain(&self.unguarded)
    }

    /// How many rows it writes.
    pub fn len(&self) -> usize {
        self.items().count()
    }

    /// Whether it writes nothing.
    pub fn is_empty(&self) -> bool {
        self.items().next().is_none()
    }
}

/// What the target held before a sync wrote to it, kept so the sync can be
/// reverted: the sync's rows, each with what it overwrote. Saved as JSON.
/// It holds no credential rho keeps as one, but holds the hooks file word
/// for word, so a token written into a hook's command is in it too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// When the sync ran, in Unix seconds.
    pub at: i64,
    /// The sync stopped part way, as the target changed under it: the next
    /// sync to the target is more of it ([`Pass::More`]).
    pub stopped: bool,
    /// The sync's name on the target, whose journal keeps what it replaced
    /// of the secrets.
    sync: SyncId,
    items: Vec<Planned>,
}

/// What a run of [`apply`] or [`retry`] is, for what it makes of the
/// target's [`Snapshot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// A sync of the rows the user checked.
    Sync,
    /// A sync after the last one stopped part way ([`Snapshot::stopped`]):
    /// more of it.
    More,
    /// A failed row of the last sync written again, while the snapshot
    /// holds that sync's rows.
    Retry,
    /// A revert, or a failed row of one written again.
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

    /// What to keep while `pass` writes `plan` at `now`, given `kept`, the
    /// snapshot from before: every row it may write, so Revert finds them
    /// if rho-ui stops as it writes.
    pub fn before(
        kept: Option<Self>,
        pass: Pass,
        plan: &Plan,
        sync: &SyncId,
        now: i64,
    ) -> Option<Self> {
        match pass {
            Pass::Sync => Some(Self::empty(sync, now).with(plan.items())),
            Pass::More | Pass::Retry => Some(
                kept.unwrap_or_else(|| Self::empty(sync, now))
                    .with(plan.items()),
            ),
            Pass::Revert => kept,
        }
    }

    /// What Revert can put back once `pass` wrote `written` at `now`,
    /// given `kept`, the snapshot from before it. A sync that wrote
    /// something replaces the last one; one that wrote nothing leaves it.
    /// More of a sync, or a retry, joins it, each row keeping what it first
    /// overwrote, and a revert drops what it put back. A sync, or more of
    /// one, is [`Self::stopped`] if it stopped part way; a retry leaves
    /// that as it was. `None` when nothing is left.
    pub fn after(
        kept: Option<Self>,
        pass: Pass,
        written: &Written,
        sync: &SyncId,
        now: i64,
    ) -> Option<Self> {
        let joined = |base: Option<Self>| {
            let mut next = base
                .unwrap_or_else(|| Self::empty(sync, now))
                .with(&written.landed);
            next.stopped = written.stale;
            next
        };
        let next = match pass {
            Pass::Sync if !written.wrote() => kept,
            Pass::Sync => Some(joined(None)),
            Pass::More => Some(joined(kept)),
            Pass::Retry => kept.map(|snapshot| snapshot.with(&written.landed)),
            Pass::Revert => kept.map(|mut snapshot| {
                snapshot
                    .items
                    .retain(|item| !written.landed.iter().any(|landed| landed.key == item.key));
                snapshot.stopped = false;
                snapshot
            }),
        };
        next.filter(|snapshot| !snapshot.items.is_empty())
    }

    /// The sync `pass` writes under, given `kept`, the target's snapshot:
    /// the snapshot's, unless the pass starts a sync of its own, which
    /// needs a new one.
    pub fn writes_under(kept: Option<&Self>, pass: Pass) -> Option<SyncId> {
        match pass {
            Pass::Sync => None,
            Pass::More | Pass::Retry | Pass::Revert => kept.map(|kept| kept.sync.clone()),
        }
    }

    /// Of `kept`, the target's snapshot before a run, the one `next`, its
    /// snapshot after, no longer reverts: a newer sync replaced it, or a
    /// revert ended it. The target can forget what it journaled of it.
    pub fn replaced(kept: Option<Self>, next: Option<&Self>) -> Option<Self> {
        kept.filter(|kept| next.map(Self::sync) != Some(kept.sync()))
    }

    /// The sync's name on the target.
    pub fn sync(&self) -> &SyncId {
        &self.sync
    }

    /// Whether the target's journal keeps anything of this sync: it wrote
    /// a key or an MCP server.
    fn journaled(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item.change, Change::Key { .. } | Change::Mcp { .. }))
    }

    fn empty(sync: &SyncId, at: i64) -> Self {
        Self {
            at,
            stopped: false,
            sync: sync.clone(),
            items: Vec::new(),
        }
    }

    /// `self` with `items`: a row already here keeps what it first
    /// overwrote, and one that changed nothing joins none.
    fn with<'a>(mut self, items: impl IntoIterator<Item = &'a Planned>) -> Self {
        for item in items {
            match self.items.iter_mut().find(|kept| kept.key == item.key) {
                Some(kept) => kept.change = kept.change.then(&item.change),
                None if item.change.is_noop() => {}
                None => self.items.push(item.clone()),
            }
        }
        self
    }
}

/// How a row of a revert stands against what the target holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevertState {
    /// Nothing since the sync stands in the way: the target holds what the
    /// sync wrote, or already what it had before.
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
    /// What reverting it writes: back from what the target holds now;
    /// `None` exactly when it is blocked.
    item: Option<Planned>,
}

impl RevertRow {
    /// `item`'s row, whose values `show` gives: unchanged while the target
    /// holds `now` equal to `after`, what the sync wrote, or to `before`.
    /// Reverting writes `back`, from `now` to `before`.
    fn of<T: PartialEq>(
        item: &Planned,
        [before, after, now]: [Option<&T>; 3],
        show: impl Fn(Option<&T>) -> String,
        back: Change,
    ) -> Self {
        let state = if now == after || now == before {
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

    /// `item`'s row when the target holds what Revert can't replace, `now`,
    /// and why.
    fn blocked(item: &Planned, [before, after]: [String; 2], now: String, why: String) -> Self {
        Self {
            key: item.key.clone(),
            label: item.label.clone(),
            synced: after,
            now,
            before,
            state: RevertState::Blocked(why),
            item: None,
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

/// Compares what `snapshot`'s sync wrote with what `target` holds now, and
/// with what the target's `journal` of it says of its secrets.
pub fn review_revert(snapshot: &Snapshot, target: &Side, journal: &Journal) -> RevertReview {
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
                table,
                key,
                before,
                after,
            } => tables::revert_row(item, *table, key, [before.as_ref(), after.as_ref()], target),
            Change::Hooks { before, after } => {
                hooks::revert_row(item, [before.as_ref(), after.as_ref()], target)
            }
            Change::Key { auth, replaced } => {
                let before = if *replaced { "Its own key" } else { "No key" };
                journal::key_row(item, auth, before, journal)
            }
            Change::Mcp {
                identity,
                before,
                after,
                ..
            } => {
                let values = [before.as_ref(), after.as_ref()];
                journal::mcp_row(item, identity, values, target, journal)
            }
            // A snapshot keeps what a sync wrote, never a revert.
            Change::Restore { .. } => {
                let none = || "Nothing".to_owned();
                let why = "A revert wrote it, not a sync".to_owned();
                RevertRow::blocked(item, [none(), none()], none(), why)
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

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
