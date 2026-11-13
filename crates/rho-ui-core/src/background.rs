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
/// Two are equal when they name the same work: a subagent gains its
/// session partway through its life, and stays the same run.
#[derive(Clone, Debug)]
pub struct BackgroundId(pub(crate) rho_protocol::BackgroundItem);

impl BackgroundId {
    /// What names the work for its whole life. `None` for a kind this
    /// client does not know, which equals nothing.
    fn key(&self) -> Option<(BackgroundKind, &str)> {
        use rho_protocol::BackgroundItem as Item;

        match &self.0 {
            Item::Process { id } => Some((BackgroundKind::Process, id)),
            Item::Workflow { run } => Some((BackgroundKind::Workflow, run)),
            Item::Subagent { run, .. } => Some((BackgroundKind::Subagent, run)),
            _ => None,
        }
    }
}

impl PartialEq for BackgroundId {
    fn eq(&self, other: &Self) -> bool {
        self.key().is_some() && self.key() == other.key()
    }
}

/// One piece of a session's background work.
#[derive(Clone, Debug, PartialEq)]
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
        self.id
            .key()
            .map_or(BackgroundKind::Other, |(kind, _)| kind)
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

#[cfg(test)]
mod tests {
    use rho_protocol::{BackgroundItem, SessionId};

    use super::BackgroundId;

    /// A subagent's session opens partway through its run; a stop asked
    /// for before then still names the same run.
    #[test]
    fn a_subagent_is_the_same_work_once_its_session_opens() {
        let subagent = |session: Option<&str>| {
            BackgroundId(BackgroundItem::Subagent {
                run: "r1".to_owned(),
                session: session.map(SessionId::new),
            })
        };
        assert_eq!(subagent(None), subagent(Some("child")));
        let process = BackgroundId(BackgroundItem::Process {
            id: "r1".to_owned(),
        });
        assert_ne!(
            process,
            subagent(None),
            "a process and a subagent are never one"
        );
        assert_ne!(
            BackgroundId(BackgroundItem::Unknown),
            BackgroundId(BackgroundItem::Unknown)
        );
    }
}
