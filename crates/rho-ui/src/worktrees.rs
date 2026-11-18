//! Each connected machine's session worktrees, the git worktrees its daemon
//! added for sessions, for Settings → Worktrees: what a prune would do with
//! each, and the prune itself, as `rho worktrees prune` runs it.
//!
//! Nothing pushes changes to the list. The page checks it when Settings
//! opens, when another machine is picked, and on Refresh. A machine runs
//! one pass from here at a time.

use gpui_kit::{Context, SharedString};
use rho_ui_core::{Daemon, DaemonError, ReportedWorktree, WorktreeVerdict};

use crate::machines::MachineId;

/// What is known of one machine's session worktrees.
pub(crate) enum Listing {
    /// Not checked since the machine connected.
    Unchecked,
    /// The pass failed, in the daemon's words, as from a rho too old to
    /// prune worktrees.
    Failed(SharedString),
    /// What a prune would do with each worktree on disk.
    Checked(Vec<ReportedWorktree>),
    /// What a prune did: how many worktrees it removed, and those it kept.
    Pruned {
        removed: usize,
        kept: Vec<ReportedWorktree>,
    },
}

/// A pass over a machine's worktrees.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pass {
    /// Says what a prune would do, removing nothing.
    Check,
    /// Removes each worktree that is clean, merged and unused.
    Prune,
}

/// One connected machine's session worktrees.
struct MachineWorktrees {
    id: MachineId,
    /// Which of the machine's connections this is, so a pass sent on an
    /// earlier one doesn't answer for it.
    connection: u64,
    daemon: Daemon,
    listing: Listing,
    /// The pass on its way to the daemon. What is listed stays until it
    /// answers.
    running: Option<Pass>,
}

/// Every connected machine's session worktrees. A machine that isn't
/// connected has none: [`crate::machines::Machines`] says why.
#[derive(Default)]
pub(crate) struct Worktrees {
    machines: Vec<MachineWorktrees>,
    connections: u64,
}

impl Worktrees {
    fn entry(&self, machine: &MachineId) -> Option<&MachineWorktrees> {
        self.machines.iter().find(|entry| entry.id == *machine)
    }

    fn entry_mut(&mut self, machine: &MachineId) -> Option<&mut MachineWorktrees> {
        self.machines.iter_mut().find(|entry| entry.id == *machine)
    }

    /// `machine`'s worktrees, while it is connected.
    pub(crate) fn listing(&self, machine: &MachineId) -> Option<&Listing> {
        self.entry(machine).map(|entry| &entry.listing)
    }

    /// The pass under way on `machine`, if any.
    pub(crate) fn running(&self, machine: &MachineId) -> Option<Pass> {
        self.entry(machine)?.running
    }

    /// `machine` connected to `daemon`, replacing what an earlier
    /// connection listed. Nothing is checked until the page asks.
    pub(crate) fn connected(
        &mut self,
        machine: &MachineId,
        daemon: Daemon,
        cx: &mut Context<'_, Self>,
    ) {
        self.forget(machine, cx);
        self.connections += 1;
        self.machines.push(MachineWorktrees {
            id: machine.clone(),
            connection: self.connections,
            daemon,
            listing: Listing::Unchecked,
            running: None,
        });
        cx.notify();
    }

    /// `machine`'s connection is gone, or the machine is.
    pub(crate) fn forget(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let before = self.machines.len();
        self.machines.retain(|entry| entry.id != *machine);
        if self.machines.len() != before {
            cx.notify();
        }
    }

    /// Checks `machine`'s worktrees, unless they have been since it
    /// connected or a pass is under way.
    pub(crate) fn ensure_checked(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        if self
            .entry(machine)
            .is_some_and(|entry| matches!(entry.listing, Listing::Unchecked))
        {
            self.run(machine, Pass::Check, cx);
        }
    }

    /// Runs `pass` on `machine`, unless a pass is under way there.
    pub(crate) fn run(&mut self, machine: &MachineId, pass: Pass, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        if entry.running.is_some() {
            return;
        }
        entry.running = Some(pass);
        let (daemon, connection) = (entry.daemon.clone(), entry.connection);
        let id = machine.clone();
        cx.spawn(async move |this, cx| {
            let answer: Result<Vec<ReportedWorktree>, DaemonError> = match pass {
                Pass::Check => daemon.check_worktrees().await,
                Pass::Prune => daemon.prune_worktrees().await,
            };
            let _ = this.update(cx, |this, cx| {
                let Some(entry) = this
                    .entry_mut(&id)
                    .filter(|entry| entry.connection == connection)
                else {
                    return;
                };
                entry.running = None;
                entry.listing = match (answer, pass) {
                    (Ok(reports), Pass::Check) => Listing::Checked(reports),
                    (Ok(reports), Pass::Prune) => pruned(reports),
                    (Err(error), _) => Listing::Failed(error.to_string().into()),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

/// What a prune that answered `reports` did: those reported merged are
/// gone.
fn pruned(reports: Vec<ReportedWorktree>) -> Listing {
    let total = reports.len();
    let kept: Vec<ReportedWorktree> = reports
        .into_iter()
        .filter(|report| !matches!(report.verdict, WorktreeVerdict::Merged { .. }))
        .collect();
    Listing::Pruned {
        removed: total - kept.len(),
        kept,
    }
}
