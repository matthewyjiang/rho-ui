//! The Sync group at the end of the Machines page: a From and a To machine,
//! what differs between them by kind, checked for the next sync, and what
//! the last sync or revert came to. [`crate::settings_sync`] holds it all;
//! this draws it. A group and its rows draw from their fields alone, so a
//! kind `rho_ui_core::sync` adds shows up with no change here.

use std::collections::BTreeSet;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    setting::{SettingField, SettingGroup, SettingItem},
    Disableable as _, Sizable as _,
};
use gpui_kit::{
    rems, App, Context, Entity, ParentElement as _, SharedString, Styled as _, Subscription,
};
use rho_ui_core::sync::{Action, Group, RevertRow, RevertState, Row, RowKey, RowState, Snapshot};

use super::{described, status_mark, value, Mark, SettingsView, Tone, CONTROL_REMS};
use crate::{
    machines::{self, DaemonAction, MachineId, Machines},
    rho_settings::RhoSettings,
    settings_sync::{clock, Busy, Compare, Outcome, Ran, Revert, RunKind, SettingsSync, Stop},
};

/// The group's own state: the app's sync, which outlives the dialog.
pub(super) struct SyncFields {
    sync: Entity<SettingsSync>,
    _subscriptions: [Subscription; 2],
}

impl SyncFields {
    /// Opens the sync on the page, and reads again once a machine it
    /// waits on connects.
    pub(super) fn new(
        machines: &Entity<Machines>,
        rho_settings: &Entity<RhoSettings>,
        cx: &mut Context<'_, SettingsView>,
    ) -> Self {
        let sync = SettingsSync::shared(machines, rho_settings, cx);
        sync.update(cx, SettingsSync::open);
        let shown = cx.observe(&sync, |_, _, cx| cx.notify());
        let connected = cx.observe(rho_settings, |this, _, cx| {
            this.sync.sync.update(cx, SettingsSync::open);
        });
        Self {
            sync,
            _subscriptions: [shown, connected],
        }
    }
}

/// A row's element ID: `part`, its kind, and its key.
fn row_id(part: &str, key: &RowKey) -> SharedString {
    format!("{part}-{}-{}", key.kind.id(), key.id).into()
}

impl SettingsView {
    /// The Sync group, and below it the groups of what differs, or of what
    /// a revert would undo.
    pub(super) fn sync_groups(&self, cx: &mut Context<'_, Self>) -> Vec<SettingGroup> {
        let entity = self.sync.sync.clone();
        let sync = entity.read(cx);
        let machines = self.machine_list.read(cx);
        let mut head = SettingGroup::new()
            .title("Sync")
            .description("Copy Rho's settings, models, provider profiles, hooks and plugins from one machine to another.");
        let Some(to) = sync.to().cloned() else {
            return vec![head.item(SettingItem::new(
                "Machines",
                value("Add a machine above to sync with it.", Tone::Muted),
            ))];
        };
        let target = machines.label(&to);
        let busy = sync.busy();
        head = head
            .item(machine_item(
                "From",
                machines,
                sync.from(),
                &entity,
                SettingsSync::pick_from,
                busy,
            ))
            .item(machine_item(
                "To",
                machines,
                &to,
                &entity,
                SettingsSync::pick_to,
                busy,
            ))
            .item(compare_item(sync, machines, &to, &entity));
        head = head.items(banners(sync, &target, &entity, &self.machine_list, cx));
        let mut groups = vec![head];
        if let Some(revert) = sync.revert() {
            groups.push(revert_group(revert, sync, &target, &entity, cx));
        } else if let Compare::Ready { diff, checked } = sync.compare() {
            for group in &diff.groups {
                groups.push(diff_group(group, checked, &target, &entity));
            }
            let selected = diff.plan(checked).len();
            groups.push(submit_group(selected, sync.busy(), &target, &entity));
        }
        groups
    }
}

/// The picker of the From or To machine.
fn machine_item(
    title: &'static str,
    machines: &Machines,
    shown: &MachineId,
    sync: &Entity<SettingsSync>,
    pick: fn(&mut SettingsSync, MachineId, &mut Context<'_, SettingsSync>),
    busy: Option<&Busy>,
) -> SettingItem {
    let options: Vec<(SharedString, SharedString)> = machines
        .iter()
        .map(|machine| (machine.id.as_str().to_owned().into(), machine.label.clone()))
        .collect();
    let shown = SharedString::from(shown.as_str().to_owned());
    let sync = sync.clone();
    SettingItem::new(
        title,
        SettingField::dropdown(
            options,
            move |_: &App| shown.clone(),
            move |id: SharedString, cx: &mut App| {
                sync.update(cx, |sync, cx| pick(sync, MachineId::from(&*id), cx));
            },
        )
        .w(rems(CONTROL_REMS))
        .justify_between(),
    )
    .disabled(busy.is_some())
}

/// What the comparison stands at, with Swap and Re-read.
fn compare_item(
    sync: &SettingsSync,
    machines: &Machines,
    to: &MachineId,
    entity: &Entity<SettingsSync>,
) -> SettingItem {
    let summary = match sync.compare() {
        Compare::Idle => [sync.from(), to]
            .into_iter()
            .find_map(|machine| super::unreachable_note(machines, machine))
            .unwrap_or_else(|| "Connecting…".to_owned()),
        Compare::Reading => "Reading both machines…".to_owned(),
        Compare::Failed(why) => format!("Couldn't read them: {why}"),
        Compare::Ready { diff, checked } => {
            let differ = diff.rows().count();
            let cant = diff
                .rows()
                .filter(|row| matches!(row.state(), RowState::CantCopy(_)))
                .count();
            let mut parts = vec![
                format!("{differ} differ"),
                format!("{} selected", checked.len()),
            ];
            if cant > 0 {
                parts.push(format!("{cant} can't be copied"));
            }
            if diff.is_empty() {
                "Both machines hold the same.".to_owned()
            } else {
                parts.join(" · ")
            }
        }
    };
    let idle = sync.busy().is_none();
    let (swap, reread) = (entity.clone(), entity.clone());
    let field = SettingField::render(move |options, _, _| {
        let (swap, reread) = (swap.clone(), reread.clone());
        h_flex()
            .gap_2()
            .child(
                Button::new("sync-swap")
                    .outline()
                    .with_size(options.size())
                    .icon(Lucide::ArrowLeftRight)
                    .label("Swap")
                    .disabled(!idle)
                    .on_click(move |_, _, cx| swap.update(cx, SettingsSync::swap)),
            )
            .child(
                Button::new("sync-reread")
                    .outline()
                    .with_size(options.size())
                    .icon(Lucide::RefreshCw)
                    .label("Re-read")
                    .disabled(!idle)
                    .on_click(move |_, _, cx| reread.update(cx, SettingsSync::reread)),
            )
    });
    SettingItem::new("Differences", field).description(summary)
}

/// The last run's outcome and Revert, a line with Retry per row that
/// failed, one prompt to restart the target, or why the run wrote
/// nothing.
fn banners(
    sync: &SettingsSync,
    target: &str,
    entity: &Entity<SettingsSync>,
    machine_list: &Entity<Machines>,
    cx: &App,
) -> Vec<SettingItem> {
    let busy = sync.busy();
    let revertible = sync.revertible(cx);
    let outcome = sync
        .outcome()
        .filter(|outcome| Some(&outcome.target) == sync.to());
    let mut items = Vec::new();
    let title = outcome
        .and_then(|outcome| outcome_title(outcome, target))
        .or_else(|| revertible.map(|at| (format!("Last sync to {target} · {}", clock(at)), None)));
    if let Some((title, stopped)) = title {
        let revert = revertible.map(|_| {
            let (entity, busy) = (entity.clone(), busy.cloned());
            SettingField::render(move |options, _, _| {
                let entity = entity.clone();
                Button::new("sync-revert")
                    .outline()
                    .with_size(options.size())
                    .icon(Lucide::Undo2)
                    .label("Revert")
                    .loading(busy == Some(Busy::Reverting))
                    .disabled(busy.is_some())
                    .on_click(move |_, _, cx| entity.update(cx, SettingsSync::start_revert))
            })
        });
        let note = stopped.or_else(|| {
            revertible.map(|_| {
                let days = Snapshot::KEEP_FOR / (24 * 60 * 60);
                format!(
                    "Revert puts back what {target} had before, for {days} days after the sync."
                )
            })
        });
        let field = revert.unwrap_or_else(|| value("", Tone::Muted));
        items.push(described(SettingItem::new(title, field), note));
    }
    let Some(outcome) = outcome else {
        return items;
    };
    let (failed, restart) = match &outcome.ran {
        Ran::Unread(why) => {
            items.push(unwritten_item(outcome.kind, why.clone(), target));
            return items;
        }
        Ran::Written {
            failed, restart, ..
        } => {
            if outcome.ran.stop() == Some(Stop::Before) {
                let again = match outcome.kind {
                    RunKind::Sync => "sync again",
                    RunKind::Revert => "revert again",
                };
                let why =
                    format!("It changed since it was read. Check what differs now, then {again}.");
                items.push(unwritten_item(outcome.kind, why, target));
            }
            (failed, restart)
        }
    };
    let verb = match outcome.kind {
        RunKind::Sync => "sync",
        RunKind::Revert => "revert",
    };
    for failure in failed {
        let key = failure.item.key().clone();
        let (entity, busy) = (entity.clone(), busy.cloned());
        let field = SettingField::render(move |options, _, _| {
            let (entity, key) = (entity.clone(), key.clone());
            let retrying = busy.as_ref() == Some(&Busy::Retrying(key.clone()));
            Button::new(row_id("sync-retry", &key))
                .outline()
                .with_size(options.size())
                .label("Retry")
                .loading(retrying)
                .disabled(busy.is_some())
                .on_click(move |_, _, cx| entity.update(cx, |sync, cx| sync.retry(&key, cx)))
        });
        let title = format!("{} didn't {verb}", failure.item.label());
        items.push(SettingItem::new(title, field).description(failure.reason.clone()));
    }
    if !restart.is_empty() {
        items.push(restart_item(restart, &outcome.target, target, machine_list));
    }
    items
}

/// One prompt to restart `machine`, `target`, for the settings that apply
/// only then.
fn restart_item(
    restart: &[String],
    machine: &MachineId,
    target: &str,
    machine_list: &Entity<Machines>,
) -> SettingItem {
    let (machines, machine) = (machine_list.clone(), machine.clone());
    let label = SharedString::from(format!("Restart {target}"));
    let field = SettingField::render(move |options, _, _| {
        let (machines, machine) = (machines.clone(), machine.clone());
        Button::new("sync-restart")
            .outline()
            .with_size(options.size())
            .label(label.clone())
            .on_click(move |_, _, cx| {
                machines::ask(&machines, DaemonAction::Restart(machine.clone()), cx);
            })
    });
    let title = format!(
        "{} {} once {target}'s Rho restarts",
        restart.join(", "),
        if restart.len() == 1 {
            "applies"
        } else {
            "apply"
        },
    );
    SettingItem::new(title, field)
        .description("Or leave it: they apply whenever its daemon next starts.")
}

/// Why a run wrote nothing, and what to do about it.
fn unwritten_item(kind: RunKind, why: String, target: &str) -> SettingItem {
    let title = match kind {
        RunKind::Sync => format!("Nothing was synced to {target}"),
        RunKind::Revert => format!("Nothing was reverted on {target}"),
    };
    SettingItem::new(title, value("", Tone::Muted)).description(why)
}

/// What the last run wrote, as its banner's title, with what a run that
/// stopped part way left; `None` when it wrote nothing.
fn outcome_title(outcome: &Outcome, target: &str) -> Option<(String, Option<String>)> {
    let Ran::Written { landed, total, .. } = &outcome.ran else {
        return None;
    };
    let whole = landed == total;
    Some(match (outcome.ran.stop(), outcome.kind) {
        (Some(Stop::Before), _) => return None,
        (Some(Stop::PartWay), RunKind::Sync) => {
            let note = format!(
                "{landed} of {total} landed, and Revert undoes them. Check what differs now, then sync again."
            );
            (
                format!("Stopped: {target} changed while syncing"),
                Some(note),
            )
        }
        (Some(Stop::PartWay), RunKind::Revert) => {
            let note =
                format!("{landed} of {total} reverted. Check what differs now, then revert again.");
            (
                format!("Stopped: {target} changed while reverting"),
                Some(note),
            )
        }
        (None, RunKind::Sync) if whole => (format!("Synced {total} to {target}"), None),
        (None, RunKind::Sync) => (format!("Synced {landed} of {total} to {target}"), None),
        (None, RunKind::Revert) if whole => (format!("Reverted {total} on {target}"), None),
        (None, RunKind::Revert) => (format!("Reverted {landed} of {total} on {target}"), None),
    })
}

/// One kind's rows, under a header that checks or unchecks them all.
fn diff_group(
    group: &Group,
    checked: &BTreeSet<RowKey>,
    target: &str,
    entity: &Entity<SettingsSync>,
) -> SettingGroup {
    let copyable: Vec<&RowKey> = group
        .rows
        .iter()
        .filter(|row| row.is_copyable())
        .map(|row| &row.key)
        .collect();
    let selected = copyable
        .iter()
        .filter(|key| checked.contains(**key))
        .count();
    let all = !copyable.is_empty() && selected == copyable.len();
    let kind = group.kind;
    let header_field = if copyable.is_empty() {
        value("", Tone::Muted)
    } else {
        let entity = entity.clone();
        let title = kind.title();
        SettingField::render(move |_, _, _| {
            let entity = entity.clone();
            Checkbox::new(SharedString::from(format!("sync-group-{}", kind.id())))
                .accessibility_label(format!("Sync all {title}"))
                .checked(all)
                .on_click(move |_, _, cx| entity.update(cx, |sync, cx| sync.toggle_group(kind, cx)))
        })
    };
    let differ = group.rows.len();
    let header = SettingItem::new(kind.title(), header_field)
        .description(format!("{differ} differ · {selected} selected"));
    let rows = group
        .rows
        .iter()
        .map(|row| row_item(row, checked, target, entity));
    SettingGroup::new().item(header).items(rows)
}

/// One row: its check, or why it can't be copied, under what it changes.
fn row_item(
    row: &Row,
    checked: &BTreeSet<RowKey>,
    target: &str,
    entity: &Entity<SettingsSync>,
) -> SettingItem {
    let checked = checked.contains(&row.key);
    let id = row_id("sync-row", &row.key);
    let label = row.label.clone();
    let field = match row.state() {
        RowState::CantCopy(_) => SettingField::render(move |_, _, cx| {
            status_mark(
                id.clone(),
                format!("{label}: can't copy"),
                Mark::Off,
                "Can't copy",
                cx,
            )
        }),
        RowState::Checked | RowState::Unchecked => {
            let (entity, key) = (entity.clone(), row.key.clone());
            SettingField::render(move |_, _, _| {
                let (entity, key) = (entity.clone(), key.clone());
                Checkbox::new(id.clone())
                    .accessibility_label(format!("Sync {label}"))
                    .checked(checked)
                    .on_click(move |_, _, cx| entity.update(cx, |sync, cx| sync.toggle(&key, cx)))
            })
        }
    };
    let lines = row_lines(row, target);
    SettingItem::new(row.label.clone(), field).description(lines.join("\n"))
}

/// What a row says under its label: what syncing it changes, when it
/// applies, and why it can't be copied.
fn row_lines(row: &Row, target: &str) -> Vec<String> {
    let missing = String::new;
    let source = row.source.clone().unwrap_or_else(missing);
    let there = row.target.clone().unwrap_or_else(missing);
    let change = match row.action {
        Action::Add => format!("Adds {source}"),
        Action::Change => format!("{there} → {source}"),
        Action::Remove => format!("Only on {target}: removes {there}"),
    };
    let reason = match row.state() {
        RowState::CantCopy(why) => Some(why),
        RowState::Checked | RowState::Unchecked => None,
    };
    std::iter::once(change)
        .chain(
            row.restart
                .then(|| format!("Applies once {target}'s Rho restarts.")),
        )
        .chain(
            row.one_machine
                .then(|| "Belongs to one machine.".to_owned()),
        )
        .chain(row.note.clone())
        .chain(reason)
        .collect()
}

/// The Sync button, under what it writes.
fn submit_group(
    selected: usize,
    busy: Option<&Busy>,
    target: &str,
    entity: &Entity<SettingsSync>,
) -> SettingGroup {
    let label = SharedString::from(format!("Sync {selected} to {target}"));
    let (entity, busy) = (entity.clone(), busy.cloned());
    let field = SettingField::render(move |options, _, _| {
        let entity = entity.clone();
        Button::new("sync-submit")
            .primary()
            .with_size(options.size())
            .label(label.clone())
            .loading(busy == Some(Busy::Syncing))
            .disabled(selected == 0 || busy.is_some())
            .on_click(move |_, _, cx| entity.update(cx, SettingsSync::sync))
    });
    SettingGroup::new().item(SettingItem::new("Sync", field).description(format!(
        "Writes the checked rows to {target}. Revert stays here."
    )))
}

/// What the target changed since the sync, each kept unless checked, and
/// Revert for the rest.
fn revert_group(
    revert: &Revert,
    sync: &SettingsSync,
    target: &str,
    entity: &Entity<SettingsSync>,
    cx: &App,
) -> SettingGroup {
    let changed: Vec<&RevertRow> = revert.review.changed().collect();
    let at = sync.revertible(cx).map(clock).unwrap_or_default();
    let others = revert.review.rows.len() - changed.len();
    let header = SettingItem::new(
        format!(
            "{target} changed {} of these since the sync at {at}",
            changed.len()
        ),
        value("Kept as they are now", Tone::Muted),
    )
    .description(format!(
        "Reverting those would undo the newer change. The other {others} revert as usual. \
         Check a row to revert it anyway."
    ));
    let rows = changed.into_iter().map(|row| {
        let id = row_id("sync-revert-row", &row.key);
        let label = row.label.clone();
        let (field, reason) = match &row.state {
            RevertState::Blocked(why) => (
                SettingField::render(move |_, _, cx| {
                    let text = format!("{label}: can't revert");
                    status_mark(id.clone(), text, Mark::Off, "Can't revert", cx)
                }),
                Some(why.clone()),
            ),
            RevertState::Changed | RevertState::Unchanged => {
                let (entity, key) = (entity.clone(), row.key.clone());
                let checked = revert.anyway.contains(&row.key);
                let field = SettingField::render(move |_, _, _| {
                    let (entity, key) = (entity.clone(), key.clone());
                    Checkbox::new(id.clone())
                        .accessibility_label(format!("Revert {label} anyway"))
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |sync, cx| sync.toggle_anyway(&key, cx));
                        })
                });
                (field, None)
            }
        };
        let values = format!(
            "Synced {} · now {} · before the sync {}",
            row.synced, row.now, row.before
        );
        let lines: Vec<String> = std::iter::once(values).chain(reason).collect();
        SettingItem::new(row.label.clone(), field).description(lines.join("\n"))
    });
    let count = revert.review.plan(&revert.anyway).len();
    let label = SharedString::from(if revert.anyway.is_empty() {
        format!("Revert the other {count}")
    } else {
        format!("Revert {count}")
    });
    let busy = sync.busy().is_some();
    let (cancel, submit) = (entity.clone(), entity.clone());
    let footer = SettingField::render(move |options, _, _| {
        let (cancel, submit) = (cancel.clone(), submit.clone());
        h_flex()
            .gap_2()
            .child(
                Button::new("sync-revert-cancel")
                    .outline()
                    .with_size(options.size())
                    .label("Cancel")
                    .on_click(move |_, _, cx| cancel.update(cx, SettingsSync::cancel_revert)),
            )
            .child(
                Button::new("sync-revert-submit")
                    .danger()
                    .with_size(options.size())
                    .label(label.clone())
                    .disabled(count == 0 || busy)
                    .on_click(move |_, _, cx| submit.update(cx, SettingsSync::finish_revert)),
            )
    });
    SettingGroup::new().item(header).items(rows).item(
        SettingItem::new("Revert", footer)
            .description(format!("Puts back what the sync overwrote on {target}.")),
    )
}
