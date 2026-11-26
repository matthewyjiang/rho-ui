//! What a session's MCP servers are doing: the daemon's session connected
//! them, so it reports each one's status, its tools and what it offers,
//! without waiting for the session's turn.

use rho_protocol::{methods, SessionMcpInventory, SessionMcpParams};

use super::{Daemon, DaemonError, SessionId};

impl Daemon {
    /// The MCP servers session `id` was configured with, each with its
    /// status, as they are now. Nothing pushes changes to it; ask again to
    /// refresh.
    ///
    /// # Errors
    ///
    /// Fails if the daemon has not loaded the session, is older than the
    /// inventory (`MethodNotFound`), or the connection is gone.
    pub async fn mcp_servers(&self, id: &SessionId) -> Result<SessionMcpInventory, DaemonError> {
        let params = SessionMcpParams {
            session: id.to_wire(),
        };
        Ok(self.client.request::<methods::SessionMcp>(&params).await?)
    }
}
