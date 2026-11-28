//! Reverting what a sync wrote of the secrets: the target journals the
//! value each key and MCP server had before, under the sync's [`SyncId`],
//! and says whether each changed since (`rho/sync/status`); Revert asks it
//! to put them back (`rho/sync/revert`), and a sync replaced by a newer
//! one is forgotten (`rho/sync/discard`). rho-ui never holds the values.

use rho_protocol::{RevertOutcome, SyncId, SyncItem, SyncItemState};

use super::{mcp, Change, McpServer, Planned, RevertRow, RevertState, Side, Snapshot};
use crate::{Daemon, DaemonError};

/// What the target's journal of a sync says of the keys and MCP servers
/// the sync wrote: whether each changed since.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Journal {
    states: Result<Vec<(SyncItem, SyncItemState)>, String>,
}

impl Journal {
    /// A journal of nothing, for a sync that wrote no secret.
    pub fn none() -> Self {
        Self {
            states: Ok(Vec::new()),
        }
    }

    /// How `item` stands; `None` when the journal no longer has it.
    fn state(&self, item: &SyncItem) -> Result<Option<SyncItemState>, &str> {
        match &self.states {
            Ok(states) => Ok(states
                .iter()
                .find(|(journaled, _)| journaled == item)
                .map(|(_, state)| *state)),
            Err(why) => Err(why),
        }
    }
}

/// Reads the journal `target`, the machine `label`, keeps of `snapshot`'s
/// sync, if the sync wrote a secret.
pub async fn journal(target: &Daemon, label: &str, snapshot: &Snapshot) -> Journal {
    if !snapshot.journaled() {
        return Journal::none();
    }
    let states = match target.sync_status(snapshot.sync().clone()).await {
        Ok(Some(status)) => Ok(status
            .items
            .into_iter()
            .map(|status| (status.item, status.state))
            .collect()),
        Ok(None) => Err(format!("Rho on {label} no longer keeps what this replaced")),
        Err(error) => Err(format!("Couldn't read it on {label}: {error}")),
    };
    Journal { states }
}

/// Forgets what `target` journaled of `snapshot`'s sync, once Revert can no
/// longer reach it, as when a newer sync replaced it.
///
/// # Errors
///
/// Fails if the daemon refuses, or the connection is gone.
pub async fn discard(target: &Daemon, snapshot: &Snapshot) -> Result<(), DaemonError> {
    if !snapshot.journaled() {
        return Ok(());
    }
    target.sync_discard(snapshot.sync().clone()).await
}

/// `item`'s row of a revert, for `journaled`, as the journal has it; the
/// target's values are `[before, after, now]`, for people, and `state`
/// says whether the target changed it since.
fn row(
    item: &Planned,
    journaled: SyncItem,
    [before, after, now]: [String; 3],
    state: RevertState,
) -> RevertRow {
    let overwrite = state == RevertState::Changed;
    RevertRow {
        key: item.key.clone(),
        label: item.label.clone(),
        synced: after,
        now,
        before,
        state,
        item: Some(Planned {
            change: Change::Restore {
                item: journaled,
                overwrite,
            },
            ..item.clone()
        }),
    }
}

/// Whether the journal says `journaled` changed since, or why Revert can't
/// put it back.
fn changed(journal: &Journal, journaled: &SyncItem) -> Result<RevertState, String> {
    match journal.state(journaled) {
        Ok(Some(SyncItemState::Unchanged)) => Ok(RevertState::Unchanged),
        Ok(Some(SyncItemState::Changed)) => Ok(RevertState::Changed),
        Ok(Some(_)) => Err("Its journal says something this rho-ui doesn't know".to_owned()),
        Ok(None) => Err("Its journal no longer has it".to_owned()),
        Err(why) => Err(why.to_owned()),
    }
}

/// API key `auth`'s row of a revert; the target held `before` of it before
/// the sync, for people.
pub(super) fn key_row(item: &Planned, auth: &str, before: &str, journal: &Journal) -> RevertRow {
    let (before, synced) = (before.to_owned(), "The source's key".to_owned());
    let journaled = SyncItem::api_key(auth);
    match changed(journal, &journaled) {
        Ok(state) => {
            let now = match state {
                RevertState::Changed => "Another key",
                _ => "The source's key",
            };
            row(item, journaled, [before, synced, now.to_owned()], state)
        }
        Err(why) => RevertRow::blocked(item, [before, synced], "Unknown".to_owned(), why),
    }
}

/// MCP server `identity`'s row of a revert: changed since if the journal
/// says so, or its entry is no longer what the sync wrote.
pub(super) fn mcp_row(
    item: &Planned,
    identity: &str,
    [before, after]: [Option<&McpServer>; 2],
    target: &Side,
    journal: &Journal,
) -> RevertRow {
    let show = |server: Option<&McpServer>| server.map_or_else(|| "None".to_owned(), mcp::shown);
    let journaled = SyncItem::mcp_server(identity);
    let now = mcp::value(target, identity);
    match (now, changed(journal, &journaled)) {
        (Ok(now), Ok(state)) => {
            let state = if now.as_ref() == after {
                state
            } else {
                RevertState::Changed
            };
            let values = [show(before), show(after), show(now.as_ref())];
            row(item, journaled, values, state)
        }
        (Err(why), _) | (_, Err(why)) => {
            RevertRow::blocked(item, [show(before), show(after)], "Unknown".to_owned(), why)
        }
    }
}

/// Puts back what the target, the machine `label`, journaled of `sync` for
/// `items`: each item's failure, if it failed, in order; or why none was
/// put back.
pub(super) async fn write(
    target: &Daemon,
    label: &str,
    sync: &SyncId,
    items: &[Planned],
) -> Result<Vec<Option<String>>, String> {
    // One per item, so each outcome is its item's.
    let restores: Vec<Option<(&SyncItem, bool)>> = items
        .iter()
        .map(|item| match &item.change {
            Change::Restore { item, overwrite } => Some((item, *overwrite)),
            _ => None,
        })
        .collect();
    let all = restores
        .iter()
        .flatten()
        .map(|(item, _)| (*item).clone())
        .collect();
    let overwrite = restores
        .iter()
        .flatten()
        .filter(|(_, overwrite)| *overwrite)
        .map(|(item, _)| (*item).clone())
        .collect();
    let reverted = target
        .sync_revert(sync.clone(), all, overwrite)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("Rho on {label} no longer keeps what this replaced"))?;
    let outcome = |&(item, _): &(&SyncItem, bool)| {
        let outcome = reverted
            .iter()
            .find(|r| r.item == *item)
            .map(|r| &r.outcome);
        match outcome {
            Some(RevertOutcome::Restored) => None,
            Some(RevertOutcome::Changed) => Some(format!("It changed on {label} since")),
            Some(RevertOutcome::Failed { message, .. }) => Some(message.clone()),
            _ => Some(format!("Rho on {label} didn't say whether it put it back")),
        }
    };
    let not_kept = || Some("The journal doesn't keep it".to_owned());
    Ok(restores
        .iter()
        .map(|restore| restore.as_ref().map_or_else(not_kept, outcome))
        .collect())
}
