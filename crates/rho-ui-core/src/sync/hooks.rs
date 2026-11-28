//! The user's hooks file, `~/.rho/hooks.toml`, read and replaced whole
//! through `rho/hooks/file/*`: one row when the two machines' files
//! differ. The programs its hooks run stay behind.

use rho_protocol::ConfigRevision;

use super::{Action, Change, Copy, GroupKind, Part, Planned, RevertRow, Row, RowKey, Side};
use crate::{Daemon, DaemonError};

/// The row's id in its group.
const ID: &str = "file";

/// The row's label.
const LABEL: &str = "Hooks file";

/// The row of the hooks file, if the two machines' differ. A target that
/// can't take the file can't take the source's.
pub(super) fn rows(source: &Side, target: &Side) -> Vec<Row> {
    let from = match &source.hooks {
        Part::Read(from) => from.text.as_ref(),
        Part::Failed(why) => return vec![Row::unread(GroupKind::Hooks, ID, LABEL, source, why)],
        Part::Lacks => return Vec::new(),
    };
    let to = target.take(&target.hooks);
    let there = to.as_ref().ok().and_then(|hooks| hooks.text.as_ref());
    if from == there {
        return Vec::new();
    }
    let copy = match &to {
        Ok(_) => Copy::Write(Change::Hooks {
            before: there.cloned(),
            after: from.cloned(),
        }),
        Err(why) => Copy::Cannot(why.clone()),
    };
    // Replacing a file drops the target's own hooks, and a file that lands
    // leaves its programs behind.
    let note = match (to.is_ok(), from, there) {
        (true, Some(_), Some(_)) => Some(format!(
            "Replaces {0}'s whole file, its own hooks too. The scripts its hooks run aren't copied: put them on {0} first",
            target.label
        )),
        (true, Some(_), None) => Some(format!(
            "The scripts its hooks run aren't copied: put them on {} first",
            target.label
        )),
        _ => None,
    };
    vec![Row {
        key: RowKey {
            kind: GroupKind::Hooks,
            id: ID.to_owned(),
        },
        label: LABEL.to_owned(),
        source: from.map(|text| shown(text)),
        target: there.map(|text| shown(text)),
        action: Action::of(from, there),
        one_machine: false,
        restart: false,
        note,
        offer: None,
        copy,
    }]
}

/// A hooks file for people: how many hooks it defines.
fn shown(text: &str) -> String {
    let hooks = text
        .lines()
        .filter(|line| line.trim() == "[[hook]]")
        .count();
    match hooks {
        1 => "1 hook".to_owned(),
        n => format!("{n} hooks"),
    }
}

/// The hooks file's text as `side` holds it now, or why it can't say.
pub(super) fn text(side: &Side) -> Result<Option<String>, String> {
    side.take(&side.hooks).map(|hooks| hooks.text.clone())
}

/// Writes the file `items`, its one row, leaves, against `revision`.
pub(super) async fn write(
    target: &Daemon,
    revision: ConfigRevision,
    items: &[Planned],
) -> Result<(), DaemonError> {
    let text = items.iter().find_map(|item| match &item.change {
        Change::Hooks { after, .. } => Some(after.clone()),
        Change::Setting { .. }
        | Change::Plugin { .. }
        | Change::Entry { .. }
        | Change::Key { .. }
        | Change::Mcp { .. }
        | Change::Restore { .. } => None,
    });
    match text {
        Some(text) => target.set_hooks_file(revision, text).await,
        // Never `None` for no row: that deletes the file.
        None => Ok(()),
    }
}

/// `item`'s row of a revert: unchanged while the target's file is still
/// what the sync wrote.
pub(super) fn revert_row(
    item: &Planned,
    [before, after]: [Option<&String>; 2],
    target: &Side,
) -> RevertRow {
    let show = |text: Option<&String>| text.map_or_else(|| "No file".to_owned(), |t| shown(t));
    let now = match text(target) {
        Ok(now) => now,
        Err(why) => {
            let values = [show(before), show(after)];
            return RevertRow::blocked(item, values, "Unknown".to_owned(), why);
        }
    };
    let back = Change::Hooks {
        before: now.clone(),
        after: before.cloned(),
    };
    RevertRow::of(item, [before, after, now.as_ref()], show, back)
}
