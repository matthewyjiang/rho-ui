//! Settings → Machines → Sync: copying rho's settings, provider profiles,
//! model aliases and favorites, the user's hooks file and plugins from one
//! machine to another, once, with Revert. The diff, the plan and what
//! Revert puts back are [`rho_ui_core::sync`]'s; this holds
//! which two machines are compared, which rows the user checked, and the
//! last sync's outcome, and runs the reads and writes on the machines'
//! daemons. It lives with the app ([`SettingsSync::shared`]), not the
//! Settings dialog, so closing the dialog never cuts a sync short.
//!
//! Before a sync writes, the target's old values of what it changes are
//! kept in [`SyncSnapshots`], one per target, for the last sync only.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::DateTime;
use gpui_kit::{App, AppContext as _, Context, Entity, Global, SharedString, Task};
use rho_ui_core::{
    sync::{self, Diff, Failure, GroupKind, Pass, Plan, RevertReview, RowKey, Snapshot, Written},
    AgentSettingsChanged, Daemon, SyncId,
};
use web_time::{SystemTime, UNIX_EPOCH};

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

impl RunKind {
    fn of(pass: Pass) -> Self {
        match pass {
            Pass::Sync | Pass::More | Pass::Retry => Self::Sync,
            Pass::Revert => Self::Revert,
        }
    }
}

/// What the last sync or revert came to, until the next fresh read.
pub(crate) struct Outcome {
    pub(crate) kind: RunKind,
    pub(crate) target: MachineId,
    pub(crate) ran: Ran,
    /// The target's snapshot holds this sync's rows, so a retry of it joins
    /// that snapshot ([`Pass::Retry`]). Not when it wrote nothing: the
    /// snapshot is then an earlier sync's.
    owns: bool,
}

/// Why a run stopped short: the target changed since it was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// Before anything landed.
    Before,
    /// After some rows landed, which stay, and Revert undoes.
    PartWay,
}

/// How a run went.
pub(crate) enum Ran {
    /// The target couldn't be read for a revert, in the daemon's words:
    /// nothing was written.
    Unread(String),
    Written {
        /// How many rows landed whole, of how many it wrote.
        landed: usize,
        total: usize,
        /// The rows that didn't, each with Retry.
        failed: Vec<Failure>,
        /// The landed rows that apply once the target's rho restarts.
        restart: Vec<String>,
        /// The target changed since it was read, so the run stopped there:
        /// what landed before stays, and nothing after it was written.
        stale: bool,
    },
}

impl Ran {
    /// Why the run stopped short, if it did.
    pub(crate) fn stop(&self) -> Option<Stop> {
        match self {
            Self::Written {
                stale: true,
                landed: 0,
                ..
            } => Some(Stop::Before),
            Self::Written { stale: true, .. } => Some(Stop::PartWay),
            Self::Written { stale: false, .. } | Self::Unread(_) => None,
        }
    }
}

impl Outcome {
    /// Folds in the retry of its failed row `key`, which wrote `written`
    /// to the machine `label`.
    fn fold_retry(&mut self, key: &RowKey, written: Written, label: &str) {
        let Ran::Written {
            landed,
            failed,
            restart,
            ..
        } = &mut self.ran
        else {
            return;
        };
        self.owns |= written.wrote();
        *landed += written.whole().count();
        restart.extend(restarts(&written));
        let Some(at) = failed.iter().position(|f| f.item.key() == key) else {
            return;
        };
        if written.stale {
            // Its row is left to retry again.
            failed[at].reason =
                format!("{label} changed again as it was written. Retry reads it first.");
        } else {
            failed.splice(at..=at, written.failed);
        }
    }
}

/// The rows of `written` that landed whole and apply once the target's rho
/// restarts.
fn restarts(written: &Written) -> Vec<String> {
    written
        .whole()
        .filter(|item| item.needs_restart())
        .map(|item| item.label().to_owned())
        .collect()
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
    /// The target's settings moved their revision while the comparison
    /// read them: read again once it is done.
    read_again: bool,
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
    /// After a run stopped as stale: the user's checks of the rows still
    /// listed, `checked` of those it `listed`; a new row starts as it
    /// would.
    Kept {
        listed: BTreeSet<RowKey>,
        checked: BTreeSet<RowKey>,
    },
}

/// What a run writes: a plan, or what is left of one failed row.
enum Job {
    Plan(Plan),
    Retry(Box<Failure>),
}

/// A run under way: what it is to the target's snapshot, to which
/// machine and its daemon, the row it retries, the snapshot from before
/// it, and the sync it writes under.
struct Run {
    pass: Pass,
    target: MachineId,
    daemon: Daemon,
    retried: Option<RowKey>,
    kept: Option<Snapshot>,
    sync: SyncId,
}

/// A new name for a sync on its target: the time here, to the nanosecond,
/// and a count of the syncs this run named, so no two share one even if
/// the clock is off.
fn new_sync() -> SyncId {
    static NAMED: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let named = NAMED.fetch_add(1, Ordering::Relaxed);
    SyncId::new(format!("rho-ui-{nanos:x}-{named}"))
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
            read_again: false,
            read_task: Task::ready(()),
            run_task: Task::ready(()),
        }
    }

    /// Takes in `changed`, a change to `machine`'s rho settings, for the
    /// app's sync, once Settings has opened it. A change to the target's
    /// that moves only their revision, such as an agent model's auth profile, leaves what
    /// differs alone, but a write at the revision read before it would stop
    /// as stale: the comparison reads both machines again, keeping the
    /// user's checks, or does once the read under way ends. A change of
    /// values is left to the write, which the daemon refuses.
    pub(crate) fn settings_changed(
        machine: &MachineId,
        changed: &AgentSettingsChanged,
        cx: &mut App,
    ) {
        if !changed.moves_no_value() {
            return;
        }
        let Some(Shared(sync)) = cx.try_global::<Shared>() else {
            return;
        };
        sync.clone().update(cx, |sync, cx| {
            // Writes go only to the target, at its revisions.
            if sync.to.as_ref() != Some(machine) {
                return;
            }
            match sync.compare {
                Compare::Ready { .. } => sync.read(sync.kept_checks(), cx),
                Compare::Reading => sync.read_again = true,
                Compare::Idle | Compare::Failed(_) => {}
            }
        });
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
        self.read_again = false;
        cx.notify();
        let pair = (self.from.clone(), to);
        self.read_task = cx.spawn(async move |this, cx| {
            let (from, to) = futures::join!(
                sync::read(source_label, &source),
                sync::read(target_label, &target)
            );
            let compared = match (from, to) {
                (Ok(from), Ok(to)) => {
                    let secrets = sync::compare(&source, &from, &target, &to).await;
                    Ok(sync::diff(&from, &to, &secrets))
                }
                (Err(error), _) | (_, Err(error)) => Err(error),
            };
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
                            Checks::Kept { listed, checked } => {
                                let fresh = diff.initially_checked();
                                diff.rows()
                                    .map(|row| &row.key)
                                    .filter(|key| {
                                        if listed.contains(*key) {
                                            checked.contains(*key)
                                        } else {
                                            fresh.contains(*key)
                                        }
                                    })
                                    .cloned()
                                    .collect()
                            }
                        };
                        Compare::Ready { diff, checked }
                    }
                    Err(error) => Compare::Failed(error.to_string().into()),
                };
                if std::mem::take(&mut this.read_again) {
                    this.read(this.kept_checks(), cx);
                }
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
    /// so the sync can be reverted: more of the last sync, if it stopped
    /// part way.
    pub(crate) fn sync(&mut self, cx: &mut Context<'_, Self>) {
        let Compare::Ready { diff, checked } = &self.compare else {
            return;
        };
        let plan = diff.plan(checked);
        let Some(to) = self.to.clone().filter(|_| !plan.is_empty()) else {
            return;
        };
        let pass = match SyncSnapshots::get(&to, cx) {
            Some(kept) if kept.stopped => Pass::More,
            _ => Pass::Sync,
        };
        self.run(pass, to, Job::Plan(plan), cx);
    }

    /// Writes `job` to `target` as `pass`, then reads the two again. Before
    /// a plan writes, every row it may write is saved, in case rho-ui stops
    /// as it writes; once it is done, the snapshot keeps only what landed.
    fn run(&mut self, pass: Pass, target: MachineId, job: Job, cx: &mut Context<'_, Self>) {
        let retried = match &job {
            Job::Retry(failure) => Some(failure.item.key().clone()),
            Job::Plan(_) => None,
        };
        let busy = match (&retried, RunKind::of(pass)) {
            (Some(key), _) => Busy::Retrying(key.clone()),
            (None, RunKind::Sync) => Busy::Syncing,
            (None, RunKind::Revert) => Busy::Reverting,
        };
        // Secrets are sealed from the source; a revert needs none.
        let source = match RunKind::of(pass) {
            RunKind::Sync => {
                let Some(source) = self.daemon(&self.from, cx) else {
                    self.compare = Compare::Idle;
                    cx.notify();
                    return;
                };
                Some(source)
            }
            RunKind::Revert => None,
        };
        let Some(daemon) = self.begin(busy, &target, cx) else {
            return;
        };
        let kept = SyncSnapshots::get(&target, cx);
        let sync = Snapshot::writes_under(kept.as_ref(), pass).unwrap_or_else(new_sync);
        if let Job::Plan(plan) = &job {
            let before = Snapshot::before(kept.clone(), pass, plan, &sync, time::now_unix());
            SyncSnapshots::set(&target, before, cx);
        }
        self.revert = None;
        let total = match &job {
            Job::Plan(plan) => plan.len(),
            Job::Retry(_) => 1,
        };
        let label = self.label(&target, cx);
        let run = Run {
            pass,
            target,
            daemon: daemon.clone(),
            retried,
            kept,
            sync,
        };
        self.run_task = cx.spawn(async move |this, cx| {
            let machines = sync::Machines {
                source: source.as_ref(),
                target: &daemon,
                label: &label,
            };
            let written = match job {
                Job::Plan(plan) => sync::apply(machines, plan, &run.sync).await,
                Job::Retry(failure) => sync::retry(machines, &failure, &run.sync).await,
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                let checks = if written.stale {
                    this.kept_checks()
                } else {
                    Checks::AfterRun
                };
                this.ran(run, total, written, cx);
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
    fn ran(&mut self, run: Run, total: usize, written: Written, cx: &mut Context<'_, Self>) {
        let Run {
            pass,
            target,
            daemon,
            retried,
            kept,
            sync,
        } = run;
        let mut snapshot = Snapshot::after(kept.clone(), pass, &written, &sync, time::now_unix());
        if let Some(old) = Snapshot::replaced(kept, snapshot.as_ref()) {
            // Best effort: what the target can't forget now, it does on its
            // own once the journal expires.
            cx.spawn(async move |_, _| {
                let _ = sync::discard(&daemon, &old).await;
            })
            .detach();
        }
        let label = self.label(&target, cx);
        if let (Some(key), Some(outcome)) = (retried, &mut self.outcome) {
            // A sync that stopped before it wrote anything starts its own
            // snapshot with a retry: the next sync is more of it.
            if let (Some(snapshot), Pass::Sync, true) = (&mut snapshot, pass, written.wrote()) {
                snapshot.stopped |= outcome.ran.stop().is_some();
            }
            outcome.fold_retry(&key, written, &label);
        } else {
            let kind = RunKind::of(pass);
            let owns = kind == RunKind::Sync && (written.wrote() || pass == Pass::More);
            let ran = Ran::Written {
                landed: written.whole().count(),
                total,
                restart: restarts(&written),
                stale: written.stale,
                failed: written.failed,
            };
            self.outcome = Some(Outcome {
                kind,
                target: target.clone(),
                ran,
                owns,
            });
        }
        SyncSnapshots::set(&target, snapshot, cx);
        cx.notify();
    }

    /// Writes what is left of failed row `key` again: as more of its sync
    /// while the snapshot holds that sync's rows, else as a sync of its
    /// own.
    pub(crate) fn retry(&mut self, key: &RowKey, cx: &mut Context<'_, Self>) {
        let Some(Outcome {
            kind,
            target,
            ran: Ran::Written { failed, .. },
            owns,
        }) = &self.outcome
        else {
            return;
        };
        let Some(failure) = failed.iter().find(|failure| failure.item.key() == key) else {
            return;
        };
        let pass = match (kind, owns) {
            (RunKind::Revert, _) => Pass::Revert,
            (RunKind::Sync, true) => Pass::Retry,
            (RunKind::Sync, false) => Pass::Sync,
        };
        let (target, job) = (target.clone(), Job::Retry(Box::new(failure.clone())));
        self.run(pass, target, job, cx);
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
            let target = sync::read(label.clone(), &daemon).await;
            let journal = sync::journal(&daemon, &label, &snapshot).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match target {
                    Ok(target) => {
                        let review = sync::review_revert(&snapshot, &target, &journal);
                        if review.changed().next().is_none() {
                            let plan = review.plan(&BTreeSet::new());
                            this.run(Pass::Revert, to, Job::Plan(plan), cx);
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
                            ran: Ran::Unread(error.to_string()),
                            owns: false,
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
        self.run(Pass::Revert, target, Job::Plan(plan), cx);
    }

    pub(crate) fn cancel_revert(&mut self, cx: &mut Context<'_, Self>) {
        if self.revert.take().is_some() {
            cx.notify();
        }
    }
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
