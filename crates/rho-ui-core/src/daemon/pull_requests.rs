//! A session's pull requests, as its machine's daemon reads them: the one
//! of the branch its workspace has checked out, and those linked to it.
//! The daemon reads GitHub with its own `gh`; this client only asks.
//!
//! A daemon reads a pull request only while something wants it, so a
//! client reissues [`Daemon::list_pull_requests`] within 5 minutes for the
//! rows it shows, and [`Daemon::show_pull_request`] within 30 seconds while
//! one is on screen. Between answers, each change to a followed session's
//! set arrives as an update ([`crate::SessionUpdate::pull_requests`]).

use rho_protocol::{
    methods, ClientError, ErrorCode, PullRequestRead, PullRequestTarget,
    SessionPullRequestLinkParams, SessionPullRequestLinkResponse, SessionPullRequestListParams,
    SessionPullRequestShowParams, SessionPullRequestShowResponse, SessionPullRequestUnlinkParams,
    SessionPullRequests,
};

use super::{Daemon, DaemonError, SessionId};

/// How many sessions one `rho/session/pull_request/list` names: the
/// daemon's own limit, past which it refuses the request as
/// `InvalidParams` (`LISTED_SESSIONS` in rho's
/// `crates/rho/src/app/daemon_host/sessions/pull_requests.rs`, and
/// `SESSION_PULL_REQUEST_LIST`'s documentation). Longer lists go in turn,
/// so no list of sessions can reach it.
const LISTED_PER_REQUEST: usize = 1_000;

/// Why a pull request request got no answer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PullRequestError {
    /// The daemon is from before pull requests (`MethodNotFound`): it serves
    /// none until its rho is updated. Ask again only on the next connection.
    #[error("this machine's rho is from before pull requests")]
    Unserved,
    /// The daemon refused what was asked (`InvalidParams`): text that names
    /// no pull request, a host it does not read, or a link the session lacks.
    /// Holds its message, which is meant for people.
    #[error("{0}")]
    Invalid(String),
    /// Any other failure: an unknown session, or the connection is gone.
    #[error(transparent)]
    Daemon(#[from] DaemonError),
}

/// Reads the two refusals that mean something here from the error code,
/// before [`DaemonError`] folds every refusal into one.
fn refusal(error: ClientError) -> PullRequestError {
    match error.error_code() {
        Some(ErrorCode::MethodNotFound) => PullRequestError::Unserved,
        Some(ErrorCode::InvalidParams) => {
            PullRequestError::Invalid(DaemonError::from(error).to_string())
        }
        _ => PullRequestError::Daemon(error.into()),
    }
}

impl Daemon {
    /// The pull requests of each of `sessions`, as the daemon last read
    /// them, in the order asked, and a 5-minute lease on each, so the
    /// daemon keeps reading them. A session the daemon has nothing under,
    /// or could not look up, is left out. Asks nothing for no sessions.
    ///
    /// # Errors
    ///
    /// [`PullRequestError::Unserved`] from a daemon from before pull
    /// requests; [`PullRequestError::Daemon`] if the connection is gone.
    /// Sessions go 1,000 to a request, the daemon's limit, in turn: the
    /// first request that fails fails the whole list.
    pub async fn list_pull_requests(
        &self,
        sessions: &[SessionId],
    ) -> Result<Vec<(SessionId, SessionPullRequests)>, PullRequestError> {
        let mut listed = Vec::with_capacity(sessions.len());
        for chunk in sessions.chunks(LISTED_PER_REQUEST) {
            let params =
                SessionPullRequestListParams::new(chunk.iter().map(SessionId::to_wire).collect());
            let response = self
                .client
                .request::<methods::SessionPullRequestList>(&params)
                .await
                .map_err(refusal)?;
            listed.extend(
                response
                    .sessions
                    .into_iter()
                    .map(|session| (session.session.into(), session.pull_requests)),
            );
        }
        Ok(listed)
    }

    /// `session`'s pull requests and the detail of `target`, with a
    /// 30-second lease on it. With [`PullRequestRead::Fresh`] the daemon
    /// reads GitHub first, as after an action this client took; otherwise
    /// it answers from its last read. The detail is absent while the daemon
    /// has none, as for a branch with no pull request or a read that
    /// failed: the target's trouble says which.
    ///
    /// # Errors
    ///
    /// [`PullRequestError::Unserved`] from a daemon from before pull
    /// requests; [`PullRequestError::Invalid`] for a linked target the
    /// session does not link; [`PullRequestError::Daemon`] for an unknown
    /// session, or if the connection is gone.
    pub async fn show_pull_request(
        &self,
        session: &SessionId,
        target: PullRequestTarget,
        read: PullRequestRead,
    ) -> Result<SessionPullRequestShowResponse, PullRequestError> {
        let mut params = SessionPullRequestShowParams::new(session.to_wire(), target);
        params.read = read;
        self.client
            .request::<methods::SessionPullRequestShow>(&params)
            .await
            .map_err(refusal)
    }

    /// Links the pull request `text` names to `session`, which saves it:
    /// a URL, `owner/name#12`, or `#12` in the repository of the session's
    /// `origin`, as a person typed it; the daemon reads it. Answers with
    /// the pull request it named and the session's pull requests after.
    /// Linking one already linked changes nothing.
    ///
    /// # Errors
    ///
    /// [`PullRequestError::Unserved`] from a daemon from before pull
    /// requests; [`PullRequestError::Invalid`], saying why, for text that
    /// names no pull request, or one on a host the daemon does not read;
    /// [`PullRequestError::Daemon`] for an unknown session, or if the
    /// connection is gone.
    pub async fn link_pull_request(
        &self,
        session: &SessionId,
        text: &str,
    ) -> Result<SessionPullRequestLinkResponse, PullRequestError> {
        let params = SessionPullRequestLinkParams::new(session.to_wire(), text);
        self.client
            .request::<methods::SessionPullRequestLink>(&params)
            .await
            .map_err(refusal)
    }

    /// Unlinks the pull request `text` names from `session`, ending the
    /// agent's watch on it, and answers with the session's pull requests
    /// after. Send a link's own `url`: `owner/name#12` reads as github.com.
    /// Unlinking one that is not linked changes nothing.
    ///
    /// # Errors
    ///
    /// As [`Daemon::link_pull_request`].
    pub async fn unlink_pull_request(
        &self,
        session: &SessionId,
        text: &str,
    ) -> Result<SessionPullRequests, PullRequestError> {
        let params = SessionPullRequestUnlinkParams::new(session.to_wire(), text);
        let response = self
            .client
            .request::<methods::SessionPullRequestUnlink>(&params)
            .await
            .map_err(refusal)?;
        Ok(response.pull_requests)
    }
}
