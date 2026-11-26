//! A session's reports, as its daemon reads them between turns: what fills
//! its context window, its hooks, its compactions, and the provider-native
//! context its model can't be sent (`rho/session/report`).

use rho_protocol::methods::{
    self, SessionCompactionReport, SessionContextReport, SessionHooksReport,
    SessionProviderContextReport, SessionReport, SessionReportData, SessionReportParams,
};
use rho_protocol::{ClientError, ErrorCode};

use super::{Daemon, DaemonError, SessionId};

/// Why a report got no answer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReportError {
    /// A turn, or another job, is writing the session (`SessionBusy`): the
    /// daemon reads reports between turns only. Ask again once it ends.
    #[error("the session is busy")]
    Busy,
    /// The daemon has not loaded the session (`SessionNotLoaded`), as for
    /// one no client follows.
    #[error("the daemon has not loaded the session")]
    NotLoaded,
    /// The daemon is from before typed reports (`MethodNotFound`): it
    /// serves none until its rho is updated.
    #[error("this machine's rho is from before session reports")]
    Unserved,
    /// Any other failure: an unknown session, an answer of another report
    /// than asked, or the connection is gone.
    #[error(transparent)]
    Daemon(#[from] DaemonError),
}

/// One of the reports [`Daemon::session_report`] reads, by its type.
/// Sealed: rho-protocol defines the reports.
pub trait Report: Sized + private::Sealed {
    /// Which report to ask for.
    const KIND: SessionReport;

    /// The report in `data`, if `data` is this kind.
    fn from_data(data: SessionReportData) -> Option<Self>;
}

mod private {
    pub trait Sealed {}
    impl Sealed for super::SessionContextReport {}
    impl Sealed for super::SessionHooksReport {}
    impl Sealed for super::SessionCompactionReport {}
    impl Sealed for super::SessionProviderContextReport {}
}

impl Report for SessionContextReport {
    const KIND: SessionReport = SessionReport::Context;

    fn from_data(data: SessionReportData) -> Option<Self> {
        match data {
            SessionReportData::Context(report) => Some(report),
            _ => None,
        }
    }
}

impl Report for SessionHooksReport {
    const KIND: SessionReport = SessionReport::Hooks;

    fn from_data(data: SessionReportData) -> Option<Self> {
        match data {
            SessionReportData::Hooks(report) => Some(report),
            _ => None,
        }
    }
}

impl Report for SessionCompactionReport {
    const KIND: SessionReport = SessionReport::Compaction;

    fn from_data(data: SessionReportData) -> Option<Self> {
        match data {
            SessionReportData::Compaction(report) => Some(*report),
            _ => None,
        }
    }
}

impl Report for SessionProviderContextReport {
    const KIND: SessionReport = SessionReport::ProviderContext;

    fn from_data(data: SessionReportData) -> Option<Self> {
        match data {
            SessionReportData::ProviderContext(report) => Some(report),
            _ => None,
        }
    }
}

/// Reads the refusals that mean something here from the error code, before
/// [`DaemonError`] folds every refusal into one.
fn refusal(error: ClientError) -> ReportError {
    match error.error_code() {
        Some(ErrorCode::SessionBusy) => ReportError::Busy,
        Some(ErrorCode::SessionNotLoaded) => ReportError::NotLoaded,
        Some(ErrorCode::MethodNotFound) => ReportError::Unserved,
        _ => ReportError::Daemon(error.into()),
    }
}

impl Daemon {
    /// `session`'s report `R`, as the daemon reads it now, between turns.
    ///
    /// # Errors
    ///
    /// [`ReportError::Busy`] while a turn runs, [`ReportError::NotLoaded`]
    /// for a session the daemon has not loaded, [`ReportError::Unserved`]
    /// from a daemon from before typed reports, and [`ReportError::Daemon`]
    /// for an unknown session or if the connection is gone.
    pub async fn session_report<R: Report>(&self, session: &SessionId) -> Result<R, ReportError> {
        let params = SessionReportParams::new(session.to_wire(), R::KIND);
        let response = self
            .client
            .request::<methods::SessionReportRead>(&params)
            .await
            .map_err(refusal)?;
        R::from_data(response.report).ok_or_else(|| {
            DaemonError::Refused(format!(
                "the daemon answered another report than {:?}",
                R::KIND
            ))
            .into()
        })
    }
}
