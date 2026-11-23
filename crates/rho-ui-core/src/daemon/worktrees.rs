//! Cleaning up the git worktrees the daemon added for sessions, as
//! `rho worktrees prune` does: each one that is clean, merged and unused
//! goes, keeping its branch, and the daemon says why each other one stays.

use std::collections::HashMap;

use rho_protocol::{methods, JobListParams, SessionWorktree, WorktreePruneParams, WorktreeVerdict};

use super::{Daemon, DaemonError, SessionId};

/// A session worktree on disk, and what a prune would do, or did, with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportedWorktree {
    /// The session it was added for, while the daemon still lists its
    /// job; none too when the job list couldn't be read.
    pub session: Option<SessionId>,
    /// Its branch, where it is, and the checkout it was added from.
    pub worktree: SessionWorktree,
    /// What a prune would do with it, or did: only a
    /// [`WorktreeVerdict::Merged`] one is removed.
    pub verdict: WorktreeVerdict,
    /// Untouched for 14 days, so it likely holds work to merge or remove;
    /// see [`rho_protocol::JobSummary::worktree_stale`].
    pub stale: bool,
}

impl Daemon {
    /// Every session worktree on disk, with what a prune would do with it,
    /// removing nothing.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects the request, as a rho too old to prune
    /// worktrees, or the connection is gone.
    pub async fn check_worktrees(&self) -> Result<Vec<ReportedWorktree>, DaemonError> {
        self.prune(WorktreePruneParams {
            dry_run: true,
            measure: false,
        })
        .await
    }

    /// Removes each session worktree that is clean, merged and unused,
    /// keeping its branch. Reports every one that was on disk: those
    /// reported [`WorktreeVerdict::Merged`] are gone.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects the request, as a rho too old to prune
    /// worktrees, or the connection is gone. Git failing on one worktree
    /// is no error: its report says so.
    pub async fn prune_worktrees(&self) -> Result<Vec<ReportedWorktree>, DaemonError> {
        self.prune(WorktreePruneParams {
            dry_run: false,
            measure: false,
        })
        .await
    }

    async fn prune(
        &self,
        params: WorktreePruneParams,
    ) -> Result<Vec<ReportedWorktree>, DaemonError> {
        // A report names its worktree's job, if a session owns it, not the
        // session (rho-protocol 1.3's `WorktreeReport` has no `session`):
        // the job list says which session each job is. Without it,
        // worktrees go unnamed.
        let (response, jobs) = futures::future::join(
            self.client.request::<methods::WorktreePrune>(&params),
            self.client.request::<methods::JobList>(&JobListParams {}),
        )
        .await;
        let response = response?;
        let sessions: HashMap<_, SessionId> = jobs
            .map(|list| {
                list.jobs
                    .into_iter()
                    .filter_map(|job| Some((job.id, job.session?.into())))
                    .collect()
            })
            .unwrap_or_default();
        Ok(response
            .worktrees
            .into_iter()
            .map(|report| ReportedWorktree {
                session: report.job.and_then(|job| sessions.get(&job).cloned()),
                worktree: report.worktree,
                verdict: report.verdict,
                stale: report.stale,
            })
            .collect())
    }
}
