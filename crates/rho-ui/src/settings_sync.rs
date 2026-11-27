//! Settings → Machines → Sync: copying rho's settings and the user's
//! plugins from one machine to another, once, with Revert. The diff, the
//! plan and what Revert puts back are [`rho_ui_core::sync`]'s; this holds
//! which two machines are compared, which rows the user checked, and the
//! last sync's outcome, and runs the reads and writes on the machines'
//! daemons. It lives with the app ([`SettingsSync::shared`]), not the
//! Settings dialog, so closing the dialog never cuts a sync short.
//!
//! Before a sync writes, the target's old values of what it changes are
//! kept in [`SyncSnapshots`], one per target, for the last sync only.

use std::collections::{BTreeMap, BTreeSet};

use chrono::DateTime;
use gpui_kit::{App, AppContext as _, Context, Entity, Global, SharedString, Task};
use rho_ui_core::{
    sync::{
        self, Applied, Diff, Failure, GroupKind, Plan, Refusal, RevertReview, RowKey, Side,
        Snapshot,
    },
    Daemon, DaemonError,
};

use crate::{
    machines::{MachineId, Machines},
    rho_settings::RhoSettings,
    store::StateDb,
    time,
};

/// What the two machines' comparison stands at.
pub(crate) enum Compare {
    /// No target picked, or a machine is unreachable.
    Idle,
    Reading,
    /// A read failed, in the daemon's words.
    Failed(SharedString),
    Ready {
        diff: Diff,
        checked: BTreeSet<RowKey>,
    },
}

/// Which way a run wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunKind {
    Sync,
    Revert,
}

/// What the last sync or revert came to, until the next fresh read.
pub(crate) struct Outcome {
    pub(crate) kind: RunKind,
    pub(crate) target: MachineId,
    pub(crate) ran: Ran,
}

/// How a run went.
pub(crate) enum Ran {
    /// The target refused its guarded rows, or couldn't be read for a
    /// revert: nothing was written.
    Refused(Refusal),
    Written {
        /// How many rows landed whole, of how many it wrote.
        landed: usize,
        total: usize,
        /// The rows that didn't, each with Retry.
        failed: Vec<Failure>,
        /// The landed rows that apply once the target's rho restarts.
        restart: Vec<String>,
    },
}

/// A revert waiting on the user: the target changed some rows since the
/// sync.
pub(crate) struct Revert {
    /// The machine the review read, which the revert writes to.
    target: MachineId,
    pub(crate) review: RevertReview,
    /// The changed rows the user checked to revert anyway.
    pub(crate) anyway: BTreeSet<RowKey>,
}

/// What is being written, so its button shows it and nothing else starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Busy {
    Syncing,
    Reverting,
    Retrying(RowKey),
}

/// Settings sync between two machines.
pub(crate) struct SettingsSync {
    machines: Entity<Machines>,
    rho_settings: Entity<RhoSettings>,
    from: MachineId,
    to: Option<MachineId>,
    compare: Compare,
    outcome: Option<Outcome>,
    revert: Option<Revert>,
    busy: Option<Busy>,
    read_task: Task<()>,
    run_task: Task<()>,
}

/// How a read leaves the rows' checks.
#[derive(Clone, PartialEq, Eq)]
enum Checks {
    /// A fresh comparison: the rows that start checked, and no outcome.
    Fresh,
    /// After a run wrote: what still differs, left unchecked, under its
    /// outcome.
    AfterRun,
    /// After a run the target refused: the user's checks of the rows still
    /// listed, `checked` of those it `listed`; a new row starts as it
    /// would.
    Kept {
        listed: BTreeSet<RowKey>,
        checked: BTreeSet<RowKey>,
    },
}

/// A run under way: which way it writes, to which machine, the row it
/// retries, and the target's snapshot from before it, which a refusal
/// leaves as it was.
struct Run {
    kind: RunKind,
    target: MachineId,
    retried: Option<RowKey>,
    kept: Option<Snapshot>,
}

/// The app's one sync, made when Settings first opens, and kept with the
/// app from then on.
struct Shared(Entity<SettingsSync>);

impl Global for Shared {}

impl SettingsSync {
    /// The app's sync, between the machines `machines` lists, through the
    /// daemons `rho_settings` holds.
    pub(crate) fn shared(
        machines: &Entity<Machines>,
        rho_settings: &Entity<RhoSettings>,
        cx: &mut App,
    ) -> Entity<Self> {
        if let Some(Shared(sync)) = cx.try_global::<Shared>() {
            return sync.clone();
        }
        let sync = cx.new(|_| Self::new(machines.clone(), rho_settings.clone()));
        cx.set_global(Shared(sync.clone()));
        sync
    }

    /// From this machine; to none until the page opens and picks one.
    fn new(machines: Entity<Machines>, rho_settings: Entity<RhoSettings>) -> Self {
        Self {
            machines,
            rho_settings,
            from: MachineId::local(),
            to: None,
            compare: Compare::Idle,
            outcome: None,
            revert: None,
            busy: None,
            read_task: Task::ready(()),
            run_task: Task::ready(()),
        }
    }

    pub(crate) fn from(&self) -> &MachineId {
        &self.from
    }

    pub(crate) fn to(&self) -> Option<&MachineId> {
        self.to.as_ref()
    }

    pub(crate) fn compare(&self) -> &Compare {
        &self.compare
    }

    pub(crate) fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    pub(crate) fn busy(&self) -> Option<&Busy> {
        self.busy.as_ref()
    }

    /// When the target's last sync ran, while Revert can still put it back.
    pub(crate) fn revertible(&self, cx: &App) -> Option<i64> {
        let to = self.to.as_ref()?;
        SyncSnapshots::get(to, cx).map(|snapshot| snapshot.at)
    }

    /// The revert waiting on the user, while the target it read is picked.
    pub(crate) fn revert(&self) -> Option<&Revert> {
        self.revert
            .as_ref()
            .filter(|revert| self.to.as_ref() == Some(&revert.target))
    }

    /// Marks the machines busy with `busy`, writing to `target`, and gives
    /// its daemon; `None` while something else writes, or when `target` is
    /// unreachable, which the page then says. Every write starts here.
    fn begin(
        &mut self,
        busy: Busy,
        target: &MachineId,
        cx: &mut Context<'_, Self>,
    ) -> Option<Daemon> {
        if self.busy.is_some() {
            return None;
        }
        let Some(daemon) = self.daemon(target, cx) else {
            self.compare = Compare::Idle;
            cx.notify();
            return None;
        };
        self.busy = Some(busy);
        cx.notify();
        Some(daemon)
    }

    /// Opens on the first other machine when none is picked, and reads the
    /// two. A sync the page shows stays as it is.
    pub(crate) fn open(&mut self, cx: &mut Context<'_, Self>) {
        let machines = self.machines.read(cx);
        let gone = |id: &MachineId| machines.get(id).is_none();
        if gone(&self.from) {
            self.from = MachineId::local();
        }
        if self.to.as_ref().is_some_and(gone) || self.to.as_ref() == Some(&self.from) {
            self.to = None;
        }
        if self.to.is_none() {
            self.to = machines
                .iter()
                .map(|machine| machine.id.clone())
                .find(|id| *id != self.from);
            self.read(Checks::Fresh, cx);
        } else if matches!(self.compare, Compare::Idle | Compare::Failed(_)) {
            self.read(Checks::Fresh, cx);
        }
    }

    /// Copies from `machine`; picking the target swaps the two. Nothing
    /// changes while a run writes.
    pub(crate) fn pick_from(&mut self, machine: MachineId, cx: &mut Context<'_, Self>) {
        if self.busy.is_some() {
            return;
        }
        if self.to.as_ref() == Some(&machine) {
            self.swap(cx);
        } else if machine != self.from {
            self.from = machine;
            self.read(Checks::Fresh, cx);
        }
    }

    /// Copies to `machine`; picking the source swaps the two. Nothing
    /// changes while a run writes.
    pub(crate) fn pick_to(&mut self, machine: MachineId, cx: &mut Context<'_, Self>) {
        if self.busy.is_some() {
            return;
        }
        if machine == self.from {
            self.swap(cx);
        } else if self.to.as_ref() != Some(&machine) {
            self.to = Some(machine);
            self.read(Checks::Fresh, cx);
        }
    }

    /// Copies the other way.
    pub(crate) fn swap(&mut self, cx: &mut Context<'_, Self>) {
        if self.busy.is_some() {
            return;
        }
        if let Some(to) = self.to.replace(self.from.clone()) {
            self.from = to;
        }
        self.read(Checks::Fresh, cx);
    }

    /// Reads both machines again, with the rows that start checked.
    pub(crate) fn reread(&mut self, cx: &mut Context<'_, Self>) {
        self.read(Checks::Fresh, cx);
    }

    fn daemon(&self, machine: &MachineId, cx: &App) -> Option<Daemon> {
        self.rho_settings.read(cx).daemon(machine)
    }

    fn label(&self, machine: &MachineId, cx: &App) -> String {
        self.machines.read(cx).label(machine).to_string()
    }

    /// Reads what the two machines hold and compares them. Nothing starts
    /// while a run writes: the read after it shows what it left.
    fn read(&mut self, checks: Checks, cx: &mut Context<'_, Self>) {
        if self.busy.is_some() {
            return;
        }
        if checks == Checks::Fresh {
            self.outcome = None;
            self.revert = None;
        }
        let Some(to) = self.to.clone() else {
            self.compare = Compare::Idle;
            cx.notify();
            return;
        };
        let (Some(source), Some(target)) = (self.daemon(&self.from, cx), self.daemon(&to, cx))
        else {
            self.compare = Compare::Idle;
            cx.notify();
            return;
        };
        let (source_label, target_label) = (self.label(&self.from, cx), self.label(&to, cx));
        self.compare = Compare::Reading;
        cx.notify();
        let pair = (self.from.clone(), to);
        self.read_task = cx.spawn(async move |this, cx| {
            let (from, to) = futures::join!(
                read_side(source_label, &source),
                read_side(target_label, &target)
            );
            let compared = from.and_then(|from| Ok(sync::diff(&from, &to?)));
            let _ = this.update(cx, |this, cx| {
                // The user picked another pair since.
                if (this.from.clone(), this.to.clone()) != (pair.0, Some(pair.1)) {
                    return;
                }
                this.compare = match compared {
                    Ok(diff) => {
                        let checked = match checks {
                            Checks::Fresh => diff.initially_checked(),
                            Checks::AfterRun => BTreeSet::new(),
                            Checks::Kept { listed, checked } => diff
                                .initially_checked()
                                .into_iter()
                                .filter(|key| !listed.contains(key))
                                .chain(checked)
                                .collect(),
                        };
                        Compare::Ready { diff, checked }
                    }
                    Err(error) => Compare::Failed(error.to_string().into()),
                };
                cx.notify();
            });
        });
    }

    /// Checks row `key`, or unchecks it.
    pub(crate) fn toggle(&mut self, key: &RowKey, cx: &mut Context<'_, Self>) {
        if let Compare::Ready { checked, .. } = &mut self.compare {
            if !checked.remove(key) {
                checked.insert(key.clone());
            }
            cx.notify();
        }
    }

    /// Checks every copyable row of `kind`, or unchecks them all when all
    /// are checked.
    pub(crate) fn toggle_group(&mut self, kind: GroupKind, cx: &mut Context<'_, Self>) {
        let Compare::Ready { diff, checked } = &mut self.compare else {
            return;
        };
        let keys: Vec<RowKey> = diff
            .rows()
            .filter(|row| row.key.kind == kind && row.is_copyable())
            .map(|row| row.key.clone())
            .collect();
        if keys.iter().all(|key| checked.contains(key)) {
            for key in &keys {
                checked.remove(key);
            }
        } else {
            checked.extend(keys);
        }
        cx.notify();
    }

    /// Writes the checked rows to the target, keeping what they overwrite
    /// so the sync can be reverted.
    pub(crate) fn sync(&mut self, cx: &mut Context<'_, Self>) {
        let Compare::Ready { diff, checked } = &self.compare else {
            return;
        };
        let plan = diff.plan(checked);
        if let (false, Some(to)) = (plan.is_empty(), self.to.clone()) {
            self.run(RunKind::Sync, to, plan, None, cx);
        }
    }

    /// Writes `plan` to `target`, then reads the two again. Before a sync
    /// writes, what it overwrites is saved, in case rho-ui stops as it
    /// writes; once it is done, the snapshot keeps only what landed.
    fn run(
        &mut self,
        kind: RunKind,
        target: MachineId,
        plan: Plan,
        retried: Option<RowKey>,
        cx: &mut Context<'_, Self>,
    ) {
        let busy = match (&retried, kind) {
            (Some(key), _) => Busy::Retrying(key.clone()),
            (None, RunKind::Sync) => Busy::Syncing,
            (None, RunKind::Revert) => Busy::Reverting,
        };
        let Some(daemon) = self.begin(busy, &target, cx) else {
            return;
        };
        let kept = SyncSnapshots::get(&target, cx);
        if kind == RunKind::Sync && retried.is_none() {
            SyncSnapshots::set(&target, Some(Snapshot::new(time::now_unix(), &plan)), cx);
        }
        self.revert = None;
        let total = plan.len();
        let run = Run {
            kind,
            target,
            retried,
            kept,
        };
        self.run_task = cx.spawn(async move |this, cx| {
            let applied = sync::apply(&daemon, plan).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                let checks = match &applied {
                    Applied::Written(_) => Checks::AfterRun,
                    Applied::Refused(_) => this.kept_checks(),
                };
                this.ran(run, total, applied, cx);
                this.read(checks, cx);
            });
        });
    }

    /// The user's checks, to keep through a read.
    fn kept_checks(&self) -> Checks {
        match &self.compare {
            Compare::Ready { diff, checked } => Checks::Kept {
                listed: diff.rows().map(|row| row.key.clone()).collect(),
                checked: checked.clone(),
            },
            Compare::Idle | Compare::Reading | Compare::Failed(_) => Checks::Fresh,
        }
    }

    /// Keeps in the snapshot what Revert can still put back, and says what
    /// the run came to; a retry folds into the outcome its row failed in.
    fn ran(&mut self, run: Run, total: usize, applied: Applied, cx: &mut Context<'_, Self>) {
        let Run {
            kind,
            target,
            retried,
            kept,
        } = run;
        let now = time::now_unix();
        let snapshot = match (&applied, kind) {
            (Applied::Written(written), RunKind::Sync) => {
                // A fresh sync replaces the last one; a retry adds to it.
                let mut snapshot = match retried {
                    None => Snapshot::empty(now),
                    Some(_) => kept.unwrap_or_else(|| Snapshot::empty(now)),
                };
                snapshot.after_sync(written);
                Some(snapshot)
            }
            (Applied::Written(written), RunKind::Revert) => kept.map(|mut snapshot| {
                snapshot.after_revert(written);
                snapshot
            }),
            (Applied::Refused(_), _) => kept,
        };
        SyncSnapshots::set(
            &target,
            snapshot.filter(|snapshot| !snapshot.is_empty()),
            cx,
        );
        let ran = match applied {
            Applied::Refused(refusal) => Ran::Refused(refusal),
            Applied::Written(written) => Ran::Written {
                landed: written.whole().count(),
                total,
                restart: written
                    .whole()
                    .filter(|item| item.needs_restart())
                    .map(|item| item.label().to_owned())
                    .collect(),
                failed: written.failed,
            },
        };
        match (retried, &mut self.outcome, ran) {
            (
                Some(key),
                Some(Outcome {
                    ran:
                        Ran::Written {
                            landed,
                            failed,
                            restart,
                            ..
                        },
                    ..
                }),
                Ran::Written {
                    landed: more,
                    failed: still,
                    restart: also,
                    ..
                },
            ) => {
                if let Some(at) = failed.iter().position(|f| *f.item.key() == key) {
                    failed.splice(at..=at, still);
                }
                *landed += more;
                restart.extend(also);
            }
            (_, _, ran) => {
                self.outcome = Some(Outcome { kind, target, ran });
            }
        }
        cx.notify();
    }

    /// Writes what is left of failed row `key` again.
    pub(crate) fn retry(&mut self, key: &RowKey, cx: &mut Context<'_, Self>) {
        let Some(Outcome {
            kind,
            target,
            ran: Ran::Written { failed, .. },
        }) = &self.outcome
        else {
            return;
        };
        let Some(failure) = failed.iter().find(|failure| failure.item.key() == key) else {
            return;
        };
        let (kind, target, plan) = (*kind, target.clone(), Plan::retry(failure));
        self.run(kind, target, plan, Some(key.clone()), cx);
    }

    /// Reverts the target's last sync: at once if the target still holds
    /// what it wrote, else after the user reviews what changed since.
    pub(crate) fn start_revert(&mut self, cx: &mut Context<'_, Self>) {
        let Some(to) = self.to.clone() else {
            return;
        };
        let Some(snapshot) = SyncSnapshots::get(&to, cx) else {
            return;
        };
        let Some(daemon) = self.begin(Busy::Reverting, &to, cx) else {
            return;
        };
        let label = self.label(&to, cx);
        self.run_task = cx.spawn(async move |this, cx| {
            let target = read_side(label, &daemon).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match target {
                    Ok(target) => {
                        let review = sync::review_revert(&snapshot, &target);
                        if review.changed().next().is_none() {
                            let plan = review.plan(&BTreeSet::new());
                            this.run(RunKind::Revert, to, plan, None, cx);
                        } else {
                            this.revert = Some(Revert {
                                target: to,
                                review,
                                anyway: BTreeSet::new(),
                            });
                        }
                    }
                    Err(error) => {
                        this.outcome = Some(Outcome {
                            kind: RunKind::Revert,
                            target: to,
                            ran: Ran::Refused(Refusal::Failed(error.to_string())),
                        });
                    }
                }
                cx.notify();
            });
        });
    }

    /// Checks changed row `key` to revert it anyway, or unchecks it.
    pub(crate) fn toggle_anyway(&mut self, key: &RowKey, cx: &mut Context<'_, Self>) {
        if let Some(revert) = &mut self.revert {
            if !revert.anyway.remove(key) {
                revert.anyway.insert(key.clone());
            }
            cx.notify();
        }
    }

    /// Reverts what the review leaves checked, on the machine it read.
    pub(crate) fn finish_revert(&mut self, cx: &mut Context<'_, Self>) {
        let Some(revert) = self.revert() else {
            return;
        };
        let (target, plan) = (revert.target.clone(), revert.review.plan(&revert.anyway));
        self.run(RunKind::Revert, target, plan, None, cx);
    }

    pub(crate) fn cancel_revert(&mut self, cx: &mut Context<'_, Self>) {
        if self.revert.take().is_some() {
            cx.notify();
        }
    }
}

/// What `daemon`'s machine holds, as a sync reads it.
async fn read_side(label: String, daemon: &Daemon) -> Result<Side, DaemonError> {
    let settings = daemon.agent_settings().await?;
    let plugins = daemon.plugins(None).await?;
    Ok(Side::new(label, settings, plugins))
}

/// `at`, in Unix seconds, as a wall-clock time here.
pub(crate) fn clock(at: i64) -> String {
    DateTime::from_timestamp(at, 0).map_or_else(String::new, time::clock)
}

/// The last sync to each target, until it is reverted or is past
/// [`Snapshot::KEEP_FOR`]: kept here, and as [`Snapshot`] JSON in the
/// `sync_snapshots` table. A global, as [`crate::main`]'s `init` loads it
/// with the other things rho-ui remembers.
pub(crate) struct SyncSnapshots {
    db: StateDb,
    kept: BTreeMap<String, Snapshot>,
}

impl Global for SyncSnapshots {}

impl SyncSnapshots {
    /// The table, with the snapshots past their time dropped.
    pub(crate) fn load(db: &StateDb) -> Self {
        let now = time::now_unix();
        let rows = db.query("SELECT machine, snapshot FROM sync_snapshots", [], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        });
        let mut kept = BTreeMap::new();
        for (machine, json) in rows {
            match serde_json::from_str::<Snapshot>(&json) {
                Ok(snapshot) if !snapshot.is_expired(now) => {
                    kept.insert(machine, snapshot);
                }
                _ => db.execute("DELETE FROM sync_snapshots WHERE machine = ?1", [machine]),
            }
        }
        Self {
            db: db.clone(),
            kept,
        }
    }

    /// The last sync to `machine`, unless it is past its time.
    fn get(machine: &MachineId, cx: &App) -> Option<Snapshot> {
        let snapshot = cx.try_global::<Self>()?.kept.get(machine.as_str())?;
        (!snapshot.is_expired(time::now_unix())).then(|| snapshot.clone())
    }

    /// Keeps `snapshot` as the last sync to `machine`, or none.
    fn set(machine: &MachineId, snapshot: Option<Snapshot>, cx: &mut App) {
        if !cx.has_global::<Self>() {
            return;
        }
        let this = cx.global_mut::<Self>();
        let machine = machine.as_str().to_owned();
        if let Some(snapshot) = snapshot {
            if let Ok(json) = serde_json::to_string(&snapshot) {
                this.db.execute(
                    "INSERT OR REPLACE INTO sync_snapshots (machine, snapshot) VALUES (?1, ?2)",
                    (&machine, json),
                );
            }
            this.kept.insert(machine, snapshot);
        } else {
            this.db
                .execute("DELETE FROM sync_snapshots WHERE machine = ?1", [&machine]);
            this.kept.remove(&machine);
        }
    }
}
