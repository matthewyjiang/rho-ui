//! A session's background work: the processes, workflow runs and subagents
//! its turns started that run on their own. The daemon sends the whole list
//! each time it changes; work that ended stays listed, with how it ended,
//! until the session's model has been given its result.

use crate::SessionId;

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
    /// It ended while this client did not follow the session, and the
    /// daemon dropped it once the model had its result, so how it ended is
    /// unknown. Only [`crate::Session::remember_background`] leads here.
    Ended,
}

/// Names one piece of background work to [`crate::Daemon::stop_background`].
/// Two are equal when they name the same work: a subagent gains its
/// session partway through its life, and stays the same run.
#[derive(Clone, Debug)]
pub struct BackgroundId(pub(crate) rho_protocol::BackgroundItem);

impl BackgroundId {
    /// A process, by the ID the session's `process` tool gave it.
    pub fn process(id: String) -> Self {
        Self(rho_protocol::BackgroundItem::Process { id })
    }

    /// A workflow run, by its run ID.
    pub fn workflow(run: String) -> Self {
        Self(rho_protocol::BackgroundItem::Workflow { run })
    }

    /// A subagent, by its run ID, with its own session once it opened one.
    pub fn subagent(run: String, session: Option<&SessionId>) -> Self {
        Self(rho_protocol::BackgroundItem::Subagent {
            run,
            session: session.map(|session| rho_protocol::SessionId::new(session.as_str())),
        })
    }

    /// What names the work for its whole life: its kind, and the ID the
    /// daemon gave it within that kind. `None` for a kind this client does
    /// not know, which equals nothing.
    pub fn key(&self) -> Option<(BackgroundKind, &str)> {
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
    /// Work as a client saw it, such as one it kept between runs to give
    /// [`crate::Session::remember_background`].
    pub fn new(
        id: BackgroundId,
        title: String,
        status: BackgroundStatus,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            id,
            title,
            status,
            exit_code,
        }
    }

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

    /// The same work, ended in a way nobody saw.
    pub(crate) fn ended_unseen(&self) -> Self {
        Self {
            status: BackgroundStatus::Ended,
            exit_code: None,
            ..self.clone()
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

    /// A subagent's own session, once it opened one. Follow it with
    /// [`crate::Daemon::observe`]; only the subagent writes to it.
    pub fn session(&self) -> Option<SessionId> {
        match &self.id.0 {
            rho_protocol::BackgroundItem::Subagent {
                session: Some(session),
                ..
            } => Some(session.clone().into()),
            _ => None,
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
