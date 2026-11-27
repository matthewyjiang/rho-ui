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

use std::collections::BTreeSet;

use chrono::DateTime;
use gpui_kit::{App, AppContext as _, Context, Entity, Global, SharedString, Task};
use rho_ui_core::{
    sync::{self, Applied, Diff, Failure, GroupKind, Plan, RevertReview, RowKey, Side, Snapshot},
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
    /// How many rows landed, of how many it wrote.
    pub(crate) landed: usize,
    pub(crate) total: usize,
    /// The rows that didn't, each with Retry.
    pub(crate) failed: Vec<Failure>,
    /// The landed rows that apply once the target's rho restarts.
    pub(crate) restart: Vec<String>,
    /// The target's settings changed since they were read, so some
    /// weren't written: the fresh diff shows them.
    pub(crate) stale: bool,
}

/// A revert waiting on the user: the target changed some rows since the
/// sync.
pub(crate) struct Revert {
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum Checks {
    /// A fresh comparison: the rows that start checked, and no outcome.
    Fresh,
    /// After a run: what still differs, left unchecked, under its outcome.
    AfterRun,
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

    pub(crate) fn revert(&self) -> Option<&Revert> {
        self.revert.as_ref()
    }

    pub(crate) fn busy(&self) -> Option<&Busy> {
        self.busy.as_ref()
    }

    /// When the target's last sync ran, while Revert can still put it back.
    pub(crate) fn revertible(&self, cx: &App) -> Option<i64> {
        let to = self.to.as_ref()?;
        SyncSnapshots::get(to, cx).map(|snapshot| snapshot.at)
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

    /// Copies from `machine`; picking the target swaps the two.
    pub(crate) fn pick_from(&mut self, machine: MachineId, cx: &mut Context<'_, Self>) {
        if self.to.as_ref() == Some(&machine) {
            self.swap(cx);
        } else if machine != self.from {
            self.from = machine;
            self.read(Checks::Fresh, cx);
        }
    }

    /// Copies to `machine`; picking the source swaps the two.
    pub(crate) fn pick_to(&mut self, machine: MachineId, cx: &mut Context<'_, Self>) {
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

    /// Writes the checked rows to the target, keeping what it held before
    /// so the sync can be reverted.
    pub(crate) fn sync(&mut self, cx: &mut Context<'_, Self>) {
        let Compare::Ready { diff, checked } = &self.compare else {
            return;
        };
        let plan = diff.plan(checked);
        if plan.is_empty() {
            return;
        }
        let Some(to) = self.to.clone() else {
            return;
        };
        SyncSnapshots::put(&to, &Snapshot::new(time::now_unix(), &plan), cx);
        self.run(RunKind::Sync, to, plan, Busy::Syncing, cx);
    }

    /// Writes `plan` to `target`, then reads the two again. A retry of one
    /// failed row folds into the outcome it failed in.
    fn run(
        &mut self,
        kind: RunKind,
        target: MachineId,
        plan: Plan,
        busy: Busy,
        cx: &mut Context<'_, Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        let Some(daemon) = self.daemon(&target, cx) else {
            return;
        };
        let ran_as = busy.clone();
        self.busy = Some(busy);
        self.revert = None;
        cx.notify();
        let total = plan.len();
        self.run_task = cx.spawn(async move |this, cx| {
            let applied = sync::apply(&daemon, plan).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                this.ran(kind, target, total, &ran_as, applied, cx);
                this.read(Checks::AfterRun, cx);
            });
        });
    }

    /// Keeps in the snapshot what Revert can still put back, and says what
    /// the run came to; a retry, `ran_as` [`Busy::Retrying`], folds into
    /// the outcome its row failed in.
    fn ran(
        &mut self,
        kind: RunKind,
        target: MachineId,
        total: usize,
        ran_as: &Busy,
        applied: Applied,
        cx: &mut Context<'_, Self>,
    ) {
        let retry = matches!(ran_as, Busy::Retrying(_));
        let snapshot = SyncSnapshots::get(&target, cx);
        let kept = match (kind, retry, snapshot) {
            (RunKind::Sync, false, Some(mut snapshot)) => {
                snapshot.keep_landed(&applied);
                Some(snapshot)
            }
            (RunKind::Sync, true, Some(mut snapshot)) => {
                for item in &applied.landed {
                    snapshot.add(item.clone());
                }
                Some(snapshot)
            }
            (RunKind::Sync, true, None) => Some(Snapshot::landed(time::now_unix(), &applied)),
            (RunKind::Revert, _, Some(mut snapshot)) => {
                snapshot.drop_reverted(&applied);
                Some(snapshot)
            }
            (RunKind::Sync, false, None) | (RunKind::Revert, _, None) => None,
        };
        match kept {
            Some(snapshot) if !snapshot.is_empty() => SyncSnapshots::put(&target, &snapshot, cx),
            _ => SyncSnapshots::remove(&target, cx),
        }
        let restart = applied
            .landed
            .iter()
            .filter(|item| item.needs_restart())
            .map(|item| item.label().to_owned());
        match (&mut self.outcome, retry) {
            (Some(outcome), true) => {
                for item in &applied.landed {
                    outcome
                        .failed
                        .retain(|failure| failure.item.key() != item.key());
                }
                for failure in applied.failed {
                    if let Some(kept) = outcome
                        .failed
                        .iter_mut()
                        .find(|kept| kept.item.key() == failure.item.key())
                    {
                        *kept = failure;
                    }
                }
                outcome.landed += applied.landed.len();
                outcome.restart.extend(restart);
                outcome.stale |= applied.stale;
            }
            _ => {
                self.outcome = Some(Outcome {
                    kind,
                    target,
                    landed: applied.landed.len(),
                    total,
                    restart: restart.collect(),
                    failed: applied.failed,
                    stale: applied.stale,
                });
            }
        }
        cx.notify();
    }

    /// Writes failed row `key` again, against the target's settings as they
    /// are now.
    pub(crate) fn retry(&mut self, key: &RowKey, cx: &mut Context<'_, Self>) {
        let Some(outcome) = &self.outcome else {
            return;
        };
        let Some(failure) = outcome
            .failed
            .iter()
            .find(|failure| failure.item.key() == key)
        else {
            return;
        };
        let (kind, target, item) = (outcome.kind, outcome.target.clone(), failure.item.clone());
        let Some(daemon) = self.daemon(&target, cx) else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let key = key.clone();
        self.busy = Some(Busy::Retrying(key.clone()));
        cx.notify();
        self.run_task = cx.spawn(async move |this, cx| {
            let revision = daemon
                .agent_settings()
                .await
                .map(|settings| settings.revision().clone());
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match revision {
                    Ok(revision) => {
                        let plan = Plan::retry(item, revision);
                        this.run(kind, target, plan, Busy::Retrying(key), cx);
                    }
                    Err(error) => this.retry_failed(&key, &error, cx),
                }
            });
        });
    }

    /// The target couldn't be read for a retry: the row says why.
    fn retry_failed(&mut self, key: &RowKey, error: &DaemonError, cx: &mut Context<'_, Self>) {
        if let Some(failure) = self
            .outcome
            .as_mut()
            .and_then(|outcome| outcome.failed.iter_mut().find(|f| f.item.key() == key))
        {
            failure.reason = error.to_string();
            cx.notify();
        }
    }

    /// Reverts the target's last sync: at once if the target still holds
    /// what it wrote, else after the user reviews what changed since.
    pub(crate) fn start_revert(&mut self, cx: &mut Context<'_, Self>) {
        let Some(to) = self.to.clone() else {
            return;
        };
        let (Some(snapshot), Some(daemon)) = (SyncSnapshots::get(&to, cx), self.daemon(&to, cx))
        else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let label = self.label(&to, cx);
        self.busy = Some(Busy::Reverting);
        cx.notify();
        self.run_task = cx.spawn(async move |this, cx| {
            let target = read_side(label, &daemon).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match target {
                    Ok(target) => {
                        let review = sync::review_revert(&snapshot, &target);
                        if review.changed().next().is_none() {
                            let plan = review.plan(&BTreeSet::new());
                            this.run(RunKind::Revert, to, plan, Busy::Reverting, cx);
                        } else {
                            this.revert = Some(Revert {
                                review,
                                anyway: BTreeSet::new(),
                            });
                            cx.notify();
                        }
                    }
                    Err(error) => {
                        this.compare = Compare::Failed(error.to_string().into());
                        cx.notify();
                    }
                }
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

    /// Reverts what the review leaves checked.
    pub(crate) fn finish_revert(&mut self, cx: &mut Context<'_, Self>) {
        let (Some(revert), Some(to)) = (&self.revert, self.to.clone()) else {
            return;
        };
        let plan = revert.review.plan(&revert.anyway);
        self.run(RunKind::Revert, to, plan, Busy::Reverting, cx);
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

/// The last sync to each target, as [`Snapshot`] JSON in the
/// `sync_snapshots` table, until it is reverted or is past
/// [`Snapshot::KEEP_FOR`]. A global, as [`crate::main`]'s `init` loads it
/// with the other things rho-ui remembers.
pub(crate) struct SyncSnapshots {
    db: StateDb,
}

impl Global for SyncSnapshots {}

impl SyncSnapshots {
    /// The table, with the snapshots past their time dropped.
    pub(crate) fn load(db: &StateDb) -> Self {
        let this = Self { db: db.clone() };
        let now = time::now_unix();
        let rows = db.query("SELECT machine, snapshot FROM sync_snapshots", [], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        });
        for (machine, json) in rows {
            let keep = serde_json::from_str::<Snapshot>(&json)
                .is_ok_and(|snapshot| !snapshot.is_expired(now));
            if !keep {
                this.delete(&machine);
            }
        }
        this
    }

    fn delete(&self, machine: &str) {
        self.db
            .execute("DELETE FROM sync_snapshots WHERE machine = ?1", [machine]);
    }

    /// The last sync to `machine`, unless it is past its time.
    fn get(machine: &MachineId, cx: &App) -> Option<Snapshot> {
        let this = cx.try_global::<Self>()?;
        let json = this
            .db
            .query(
                "SELECT snapshot FROM sync_snapshots WHERE machine = ?1",
                [machine.as_str()],
                |row| row.get::<_, String>(0),
            )
            .pop()?;
        let snapshot: Snapshot = serde_json::from_str(&json).ok()?;
        if snapshot.is_expired(time::now_unix()) {
            this.delete(machine.as_str());
            return None;
        }
        Some(snapshot)
    }

    /// Keeps `snapshot` as the last sync to `machine`.
    fn put(machine: &MachineId, snapshot: &Snapshot, cx: &App) {
        let (Some(this), Ok(json)) = (cx.try_global::<Self>(), serde_json::to_string(snapshot))
        else {
            return;
        };
        this.db.execute(
            "INSERT OR REPLACE INTO sync_snapshots (machine, snapshot) VALUES (?1, ?2)",
            (machine.as_str(), json),
        );
    }

    fn remove(machine: &MachineId, cx: &App) {
        if let Some(this) = cx.try_global::<Self>() {
            this.delete(machine.as_str());
        }
    }
}
