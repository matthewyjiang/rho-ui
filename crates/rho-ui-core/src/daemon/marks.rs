//! Session marks: how the user has organised their saved sessions (done,
//! snoozed, pinned or kept), which the daemon keeps with each session, so
//! every client of the daemon, on any machine, shows the same.
//! [`Daemon::list_sessions`] reads each session's mark,
//! [`Daemon::mark_session`] sets one, and a change made elsewhere reaches
//! the clients attached to the session as [`crate::SessionUpdate::mark_changed`].

use rho_protocol::{methods, ErrorCode, NewSessionMark, SessionMarkParams};

use super::{Daemon, DaemonError, SessionId};

/// A saved session's mark, as its daemon keeps it. Every time is Unix
/// seconds on the daemon's clock, that of
/// [`super::SessionSummary::updated_at`], and the daemon stamps each `at`
/// itself, so the two compare whatever this machine's clock says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMark {
    /// The user is finished with the session; a later save makes it
    /// active again. The daemon also marks a session done when the last
    /// of the pull requests linked to it merges.
    Done {
        /// When it was marked.
        at: u64,
    },
    /// Hidden for a while.
    Snoozed {
        /// When it wakes, or woke.
        until: u64,
    },
    /// Kept on top.
    Pinned {
        /// When it was pinned.
        at: u64,
    },
    /// Put back on the active list, as by restoring a done session or
    /// unpinning one.
    Kept {
        /// When it was put back.
        at: u64,
    },
}

impl SessionMark {
    /// The mark the daemon sent, or `None` for a kind this client does not
    /// know, which it can't show.
    pub(crate) fn from_wire(mark: &rho_protocol::SessionMark) -> Option<Self> {
        match *mark {
            rho_protocol::SessionMark::Done { at } => Some(Self::Done { at }),
            rho_protocol::SessionMark::Snoozed { until } => Some(Self::Snoozed { until }),
            rho_protocol::SessionMark::Pinned { at } => Some(Self::Pinned { at }),
            rho_protocol::SessionMark::Kept { at } => Some(Self::Kept { at }),
            _ => None,
        }
    }

    /// The mark a daemon saves for `new` at `at`, in Unix seconds, or
    /// `None` for a kind this client does not know: what a client shows
    /// from when it asks until its daemon answers with its own stamp.
    #[must_use]
    pub fn stamped(new: &NewSessionMark, at: u64) -> Option<Self> {
        Self::from_wire(&Self::stamp(new, at)?)
    }

    /// [`Self::stamped`], as the daemon sends it.
    pub(crate) fn stamp(new: &NewSessionMark, at: u64) -> Option<rho_protocol::SessionMark> {
        match *new {
            NewSessionMark::Done => Some(rho_protocol::SessionMark::Done { at }),
            NewSessionMark::Snoozed { until } => Some(rho_protocol::SessionMark::Snoozed { until }),
            NewSessionMark::Pinned => Some(rho_protocol::SessionMark::Pinned { at }),
            NewSessionMark::Kept => Some(rho_protocol::SessionMark::Kept { at }),
            _ => None,
        }
    }
}

impl Daemon {
    /// Saves `mark` as session `id`'s mark, in place of any it had; the
    /// daemon stamps its time. Clients attached to the session hear of it
    /// ([`crate::SessionUpdate::mark_changed`]); others read it in their
    /// next [`Self::list_sessions`]. To put a done session back on the
    /// active list, mark it [`NewSessionMark::Kept`]: the daemon may mark
    /// a session with no mark done itself when its pull request merges.
    ///
    /// # Errors
    ///
    /// Fails if the daemon is from before marks, doesn't know the session,
    /// or the connection is gone.
    pub async fn mark_session(
        &self,
        id: &SessionId,
        mark: NewSessionMark,
    ) -> Result<(), DaemonError> {
        let params = SessionMarkParams::new(id.to_wire(), Some(mark));
        match self
            .client
            .request::<methods::SessionMarkMethod>(&params)
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if error.error_code() == Some(ErrorCode::MethodNotFound) => Err(
                DaemonError::Refused("this machine's rho is from before session marks".to_owned()),
            ),
            Err(error) => Err(error.into()),
        }
    }
}
