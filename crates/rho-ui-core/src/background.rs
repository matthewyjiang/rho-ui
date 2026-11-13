//! A session's background work: the processes, workflow runs and subagents
//! its turns started that run on their own. The daemon sends the whole list
//! each time it changes; work that ended stays listed, with how it ended,
//! until the session's model has been given its result.

/// What a piece of background work is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundKind {
    /// A process the session's `process` tool started.
    Process,
    /// A workflow run.
    Workflow,
    /// A subagent the session delegated to.
    Subagent,
    /// A kind a newer daemon sends. It can't be stopped from here.
    Other,
}

/// Where a piece of background work is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundStatus {
    /// Still running. A status a newer daemon sends counts as this, so
    /// unknown work never looks finished.
    Running,
    /// It finished by itself; a process may have exited with any code.
    Completed,
    /// It failed, timed out, or never started.
    Failed,
    /// Someone stopped it.
    Cancelled,
}

/// Names one piece of background work to [`crate::Daemon::stop_background`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BackgroundId(pub(crate) rho_protocol::BackgroundItem);

/// One piece of a session's background work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundWork {
    id: BackgroundId,
    title: String,
    status: BackgroundStatus,
    exit_code: Option<i32>,
}

impl BackgroundWork {
    pub(crate) fn from_wire(work: rho_protocol::BackgroundWork) -> Self {
        use rho_protocol::BackgroundStatus as Status;

        Self {
            id: BackgroundId(work.item),
            title: work.title,
            status: match work.status {
                Status::Completed => BackgroundStatus::Completed,
                Status::Failed => BackgroundStatus::Failed,
                Status::Cancelled => BackgroundStatus::Cancelled,
                _ => BackgroundStatus::Running,
            },
            exit_code: work.exit_code,
        }
    }

    /// The ID to stop it by.
    pub fn id(&self) -> &BackgroundId {
        &self.id
    }

    /// What it is.
    pub fn kind(&self) -> BackgroundKind {
        use rho_protocol::BackgroundItem as Item;

        match self.id.0 {
            Item::Process { .. } => BackgroundKind::Process,
            Item::Workflow { .. } => BackgroundKind::Workflow,
            Item::Subagent { .. } => BackgroundKind::Subagent,
            _ => BackgroundKind::Other,
        }
    }

    /// What it shows as: a process's command line, a workflow's name, a
    /// subagent's task.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Where it is.
    pub fn status(&self) -> BackgroundStatus {
        self.status
    }

    /// A process's exit code, once it exited with one.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Whether the daemon can be asked to stop it: it runs, and is a kind
    /// this client knows.
    pub fn can_stop(&self) -> bool {
        self.status == BackgroundStatus::Running && self.kind() != BackgroundKind::Other
    }
}
