//! Running a sync's [`Plan`] on the target's daemon, and retrying a row of
//! one that failed.

use std::collections::BTreeMap;

use rho_protocol::ConfigRevision;

use super::plugins::{self, Unwritten};
use super::{hooks, read, settings, tables, Change, Guarded, Order, Plan, Planned, Store};
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

/// Writes `plan` to `target`'s daemon: each store's guarded rows at once,
/// store by store, against the revision the plan read (the daemon takes
/// it while only this client wrote since); then each unguarded row on its
/// own. A store the daemon refuses fails its rows, and the rest go on; a
/// stale one stops the plan.
pub async fn apply(target: &Daemon, plan: Plan) -> Written {
    let mut written = Written {
        landed: plan.settled,
        ..Written::default()
    };
    for Guarded {
        store,
        revision,
        items,
    } in plan.guarded
    {
        let wrote = match store {
            Store::Table(table) => tables::write(target, table, revision, &items).await,
            Store::Settings => settings::write(target, revision, &items).await,
            Store::Hooks => hooks::write(target, revision, &items).await,
        };
        match wrote {
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

/// Writes failed row `failure` again to `target`'s daemon, the machine
/// `label`. It reads the target first: a guarded row writes against its
/// store's revision now, and overwrites what the target holds now, which
/// Revert then puts back.
pub async fn retry(target: &Daemon, label: String, failure: &Failure) -> Written {
    let fail = |reason: String| Written {
        failed: vec![Failure {
            item: failure.item.clone(),
            reason,
        }],
        ..Written::default()
    };
    let now = match read(label, target).await {
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
        plugin @ Change::Plugin { .. } => Ok(plugin.clone()),
    };
    let item = match change {
        Ok(change) => Planned {
            change,
            ..failure.item.clone()
        },
        Err(why) => return fail(why),
    };
    let revisions: BTreeMap<Store, ConfigRevision> = now.revisions();
    apply(target, Plan::new(&revisions, [item], Order::Sync)).await
}
