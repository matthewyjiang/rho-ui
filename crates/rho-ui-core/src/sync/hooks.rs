//! The user's hooks file, `~/.rho/hooks.toml`, read and replaced whole
//! through `rho/hooks/file/*`: one row when the two machines' files
//! differ. The programs its hooks run stay behind.

use super::{Action, Change, Copy, GroupKind, Planned, RevertRow, RevertState, Row, RowKey, Side};

/// The row of the hooks file, if the two machines' differ. A target whose
/// rho can't write it can't take the source's.
pub(super) fn rows(source: &Side, target: &Side) -> Vec<Row> {
    let Some(from) = &source.hooks else {
        return Vec::new();
    };
    let to = target.hooks.as_ref();
    let there = to.and_then(|hooks| hooks.text.as_ref());
    if from.text.as_ref() == there {
        return Vec::new();
    }
    let action = match (&from.text, there) {
        (Some(_), None) => Action::Add,
        (None, Some(_)) => Action::Remove,
        _ => Action::Change,
    };
    let copy = match to {
        Some(_) => Copy::Write(Change::Hooks {
            before: there.cloned(),
            after: from.text.clone(),
        }),
        None => Copy::Cannot(format!("Rho on {} is too old to take this", target.label)),
    };
    // Only a file that lands leaves its programs behind.
    let note = (to.is_some() && from.text.is_some()).then(|| {
        format!(
            "The scripts its hooks run aren't copied: put them on {} first",
            target.label
        )
    });
    vec![Row {
        key: RowKey {
            kind: GroupKind::Hooks,
            id: "file".to_owned(),
        },
        label: "Hooks file".to_owned(),
        source: from.text.as_deref().map(shown),
        target: there.map(|text| shown(text)),
        action,
        one_machine: false,
        restart: false,
        note,
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

/// `item`'s row of a revert: unchanged while the target's file is still
/// what the sync wrote.
pub(super) fn revert_row(
    item: &Planned,
    [before, after]: [Option<&String>; 2],
    target: &Side,
) -> RevertRow {
    let show = |text: Option<&String>| text.map_or_else(|| "No file".to_owned(), |t| shown(t));
    let Some(hooks) = &target.hooks else {
        return RevertRow {
            key: item.key.clone(),
            label: item.label.clone(),
            synced: show(after),
            now: "Unknown".to_owned(),
            before: show(before),
            state: RevertState::Blocked(format!(
                "Rho on {} no longer reads its hooks file",
                target.label
            )),
            item: None,
        };
    };
    let now = hooks.text.as_ref();
    let back = Change::Hooks {
        before: now.cloned(),
        after: before.cloned(),
    };
    RevertRow::of(item, [before, after, now], show, back)
}
