//! What the daemon's configuration sets up for a session, read without
//! one: its MCP servers (`rho/mcp/list`) and its hooks files
//! (`rho/hooks/list`). Neither starts a server or runs a hook, and each
//! reads the files again.

use std::path::Path;

use rho_protocol::{
    methods, ClientEnvironment, ErrorCode, HooksListParams, HooksListResponse, McpListParams,
    McpListResponse,
};

use super::{Daemon, DaemonError};

impl Daemon {
    /// The MCP servers a session in `workspace` would configure: those of
    /// the daemon's `config.toml`, then those of the plugins that apply,
    /// including disabled and invalid ones. Without a workspace, as for a
    /// session in the daemon user's home, which leaves the user's own
    /// plugins. Whether a project's plugins count follows this client's
    /// environment (`RHO_TRUST_PROJECT_PLUGINS`), as for the sessions it
    /// opens. `None` from a daemon older than the listing, which will never
    /// have one.
    ///
    /// # Errors
    ///
    /// Fails if `config.toml` doesn't load (the daemon's message names each
    /// problem), the daemon refuses `workspace`, or the connection is gone.
    pub async fn configured_mcp_servers(
        &self,
        workspace: Option<&Path>,
    ) -> Result<Option<McpListResponse>, DaemonError> {
        let params = McpListParams::new(
            workspace.map(Path::to_path_buf),
            ClientEnvironment::clone(&self.environment),
        );
        match self.client.request::<methods::McpList>(&params).await {
            Ok(list) => Ok(Some(list)),
            Err(error) if error.error_code() == Some(ErrorCode::MethodNotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// The hooks files a session in `workspace` would load, the user's
    /// then the project's, each with whether it loads and the hooks it
    /// defines. Without a workspace, the user's file alone. Whether the
    /// project's loads follows this client's environment
    /// (`RHO_TRUST_PROJECT_HOOKS`), as for the sessions it opens. `None`
    /// from a daemon older than the listing.
    ///
    /// # Errors
    ///
    /// Fails if the daemon refuses `workspace`, or the connection is gone.
    /// A file that can't be read or parsed is no error: its status says so.
    pub async fn configured_hooks(
        &self,
        workspace: Option<&Path>,
    ) -> Result<Option<HooksListResponse>, DaemonError> {
        let params = HooksListParams::new(
            workspace.map(Path::to_path_buf),
            ClientEnvironment::clone(&self.environment),
        );
        match self.client.request::<methods::HooksList>(&params).await {
            Ok(list) => Ok(Some(list)),
            Err(error) if error.error_code() == Some(ErrorCode::MethodNotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}
