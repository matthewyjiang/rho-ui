//! A session's background processes, the ones its `process` tool started,
//! as the daemon reads them now (`rho/session/processes`): each one's
//! state, how long it has run and been quiet, and the output the session
//! retains of one, from a cursor on. A read is a snapshot, not a
//! subscription: a client asks again to refresh, and a read changes
//! nothing the session's model sees.

use rho_protocol::{
    methods, ProcessOutputParams, SessionProcessesParams, SessionProcessesResponse,
};

use super::{Daemon, DaemonError, SessionId};

impl Daemon {
    /// The background processes of loaded session `id` as they are now,
    /// oldest first, and with `output` the output the session retains of
    /// one process from a cursor on ([`crate::ProcessLog::request`] says
    /// which). Doesn't wait for the session's turn.
    ///
    /// # Errors
    ///
    /// Fails if the session is not loaded or the connection is gone.
    pub async fn processes(
        &self,
        id: &SessionId,
        output: Option<ProcessOutputParams>,
    ) -> Result<SessionProcessesResponse, DaemonError> {
        let params = SessionProcessesParams {
            session: id.to_wire(),
            output,
        };
        Ok(self
            .client
            .request::<methods::SessionProcesses>(&params)
            .await?)
    }
}
