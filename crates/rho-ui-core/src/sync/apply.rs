//! Running a sync's [`Plan`] on the target's daemon, and retrying a row of
//! one that failed.

use std::collections::BTreeMap;

use rho_protocol::{ConfigRevision, SyncId};

use super::plugins::{self, Unwritten};
use super::{
    hooks, journal, keys, mcp, read, settings, tables, Change, Order, Plan, Planned, Step, Store,
};
use crate::{Daemon, DaemonError};

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
    /// Whether it changed anything on the target: a row the target already
    /// held as it would write lands without a write.
    pub fn wrote(&self) -> bool {
        self.landed.iter().any(|item| !item.change.is_noop())
    }

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

/// The two machines a run writes between: the target, and the source its
/// secrets are sealed from, which a revert doesn't need.
#[derive(Clone, Copy, Debug)]
pub struct Machines<'a> {
    /// The source's daemon; `None` for a revert.
    pub source: Option<&'a Daemon>,
    /// The target's daemon.
    pub target: &'a Daemon,
    /// The target's name, for the reasons rows give.
    pub label: &'a str,
}

/// Writes `plan` to the target, under the sync `sync`: place by place, a
/// store's rows at once against the revision the plan read (the daemon
/// takes it while only this client wrote since), keys and what the journal
/// puts back item by item; then each unguarded row on its own. A store the
/// daemon refuses fails its rows, and the rest go on; a stale one stops
/// the plan.
pub async fn apply(machines: Machines<'_>, plan: Plan, sync: &SyncId) -> Written {
    let mut written = Written {
        landed: plan.settled,
        ..Written::default()
    };
    for step in plan.steps {
        if let Err(Stale) = step.write(machines, sync, &mut written).await {
            written.stale = true;
            return written;
        }
    }
    let target = machines.target;
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

/// A store changed on the target since it was read.
struct Stale;

impl Step {
    /// Writes this step's rows, saying in `written` how each went.
    async fn write(
        self,
        machines: Machines<'_>,
        sync: &SyncId,
        written: &mut Written,
    ) -> Result<(), Stale> {
        let Machines {
            source,
            target,
            label,
        } = machines;
        let (items, wrote) = match self {
            Self::Guarded {
                store,
                revision,
                items,
            } => {
                let wrote = match store {
                    Store::Table(table) => tables::write(target, table, revision, &items).await,
                    Store::Mcp => mcp::write(source, target, label, revision, &items, sync).await,
                    Store::Settings => settings::write(target, revision, &items).await,
                    Store::Hooks => hooks::write(target, revision, &items).await,
                };
                match wrote {
                    Ok(()) => (items, Wrote::All),
                    Err(DaemonError::Conflict(_)) => return Err(Stale),
                    Err(error) => (items, Wrote::None(error.to_string())),
                }
            }
            Self::Keys(items) => {
                let wrote = match source {
                    Some(source) => keys::write(source, target, label, &items, sync).await,
                    None => Err("Keys come from the source, which isn't here".to_owned()),
                };
                (items, Wrote::of(wrote))
            }
            Self::Journal(items) => {
                let wrote = journal::write(target, label, sync, &items).await;
                (items, Wrote::of(wrote))
            }
        };
        written.settle(items, wrote);
        Ok(())
    }
}

/// How a step's write went.
enum Wrote {
    /// Every row landed.
    All,
    /// Each row's failure, if it failed, in the rows' order.
    Each(Vec<Option<String>>),
    /// No row landed, for this reason.
    None(String),
}

impl Wrote {
    /// Each row's failure, if it failed, or why none landed.
    fn of(each: Result<Vec<Option<String>>, String>) -> Self {
        each.map_or_else(Self::None, Self::Each)
    }
}

impl Written {
    /// Says how each of `items` went, by `wrote`. A row `wrote` gives no
    /// outcome for failed: nothing says it landed.
    fn settle(&mut self, items: Vec<Planned>, wrote: Wrote) {
        let mut each = match wrote {
            Wrote::All => {
                self.landed.extend(items);
                return;
            }
            Wrote::Each(each) => each.into_iter(),
            Wrote::None(reason) => vec![Some(reason); items.len()].into_iter(),
        };
        for item in items {
            match each.next() {
                Some(None) => self.landed.push(item),
                Some(Some(reason)) => self.failed.push(Failure { item, reason }),
                None => {
                    let reason = "Rho didn't say whether it wrote it".to_owned();
                    self.failed.push(Failure { item, reason });
                }
            }
        }
    }
}

/// Writes failed row `failure` again to the target, under the sync `sync`.
/// It reads the target first: a guarded row writes against its store's
/// revision now, and overwrites what the target holds now, which Revert
/// then puts back.
pub async fn retry(machines: Machines<'_>, failure: &Failure, sync: &SyncId) -> Written {
    let fail = |reason: String| Written {
        failed: vec![Failure {
            item: failure.item.clone(),
            reason,
        }],
        ..Written::default()
    };
    let now = match read(machines.label.to_owned(), machines.target).await {
        Ok(now) => now,
        Err(error) => return fail(error.to_string()),
    };
    let change = match &failure.item.change {
        Change::Setting { key, after, .. } => Ok(Change::Setting {
            key: key.clone(),
            before: settings::held(&now, key),
            after: after.clone(),
        }),
        Change::Entry {
            table, key, after, ..
        } => tables::value(&now, *table, key).map(|before| Change::Entry {
            table: *table,
            key: key.clone(),
            before,
            after: after.clone(),
        }),
        Change::Hooks { after, .. } => hooks::text(&now).map(|before| Change::Hooks {
            before,
            after: after.clone(),
        }),
        Change::Mcp {
            identity, after, ..
        } => mcp::value(&now, identity).map(|before| Change::Mcp {
            identity: identity.clone(),
            before,
            after: after.clone(),
            // The target may have changed a secret it held the same: seal
            // them all from the source.
            keep: Vec::new(),
        }),
        // What the journal keeps goes back over what the target holds now.
        Change::Restore { item, .. } => Ok(Change::Restore {
            item: item.clone(),
            overwrite: true,
        }),
        unread @ (Change::Plugin { .. } | Change::Key { .. }) => Ok(unread.clone()),
    };
    let item = match change {
        Ok(change) => Planned {
            change,
            ..failure.item.clone()
        },
        Err(why) => return fail(why),
    };
    let revisions: BTreeMap<Store, ConfigRevision> = now.revisions();
    let plan = Plan::new(&revisions, [item], Order::Sync);
    apply(machines, plan, sync).await
}
