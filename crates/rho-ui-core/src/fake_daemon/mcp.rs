//! The fake daemon's MCP servers. Every session reports the same three,
//! configured in the rho home's `config.toml`: [`MCP_DOCS`], connected with
//! two tools; [`MCP_GITHUB`], connected with one; and [`MCP_NOTES`],
//! disabled. With [`Script::mcp`] [`McpStartup::Connecting`] they are
//! still starting until [`FakeDaemon::finish_mcp_startup`]: the docs server
//! connects, the GitHub server has timed out, and the session's tool
//! sources load.

use std::path::PathBuf;

use rho_protocol::{
    RpcError, SessionMcpInventory, SessionMcpMode, SessionMcpParams, SessionMcpServer,
    SessionMcpStatus,
};

use super::{FakeDaemon, Script, State};

/// The server that exports two tools.
pub const MCP_DOCS: &str = "docs";
/// The server that exports one tool, and times out while starting.
pub const MCP_GITHUB: &str = "github";
/// The server the config disables.
pub const MCP_NOTES: &str = "notes";

/// Whether the fake's MCP servers have finished starting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpStartup {
    /// Every server has connected, or failed to.
    #[default]
    Done,
    /// [`MCP_DOCS`] is connecting, [`MCP_GITHUB`] has timed out, and the
    /// session's tool sources load.
    Connecting,
}

/// What the fake's MCP servers are doing.
#[derive(Debug)]
pub(super) struct FakeMcp {
    startup: McpStartup,
}

impl FakeMcp {
    pub(super) fn new(script: &Script) -> Self {
        Self {
            startup: script.mcp,
        }
    }
}

impl State {
    /// `rho/session/mcp`: the inventory of a session the daemon holds.
    pub(super) fn mcp_inventory(
        &mut self,
        params: &SessionMcpParams,
    ) -> Result<SessionMcpInventory, RpcError> {
        self.session(&params.session)?;
        let connecting = self.mcp.startup == McpStartup::Connecting;
        let server = |identity: &str, status, tool_count, detail: &str| SessionMcpServer {
            identity: identity.to_owned(),
            enabled: true,
            status,
            tool_count,
            detail: detail.to_owned(),
        };
        let (docs, github) = if connecting {
            (
                server(
                    MCP_DOCS,
                    SessionMcpStatus::Connecting,
                    0,
                    "stdio · docs-server --stdio\nconnecting",
                ),
                server(
                    MCP_GITHUB,
                    SessionMcpStatus::TimedOut,
                    0,
                    "http · https://api.github.com/mcp\nerror: no answer to initialize within 30s",
                ),
            )
        } else {
            (
                server(
                    MCP_DOCS,
                    SessionMcpStatus::Connected,
                    2,
                    "stdio · docs-server --stdio\n2 tool(s): mcp__docs__search, mcp__docs__read",
                ),
                server(
                    MCP_GITHUB,
                    SessionMcpStatus::Connected,
                    1,
                    "http · https://api.github.com/mcp\n1 tool(s): mcp__github__issues",
                ),
            )
        };
        Ok(SessionMcpInventory {
            config_path: PathBuf::from("/home/me/.rho/config.toml"),
            mode: SessionMcpMode::Native,
            servers: vec![
                docs,
                github,
                SessionMcpServer {
                    identity: MCP_NOTES.to_owned(),
                    enabled: false,
                    status: SessionMcpStatus::Disabled,
                    tool_count: 0,
                    detail: "stdio · notes-mcp\nenabled = false".to_owned(),
                },
            ],
            tool_sources_loading: connecting,
        })
    }
}

impl FakeDaemon {
    /// Ends the MCP startup [`McpStartup::Connecting`] began: [`MCP_DOCS`]
    /// and [`MCP_GITHUB`] connect, with their tools, and the tool sources
    /// have loaded.
    pub fn finish_mcp_startup(&self) {
        self.service.lock().mcp.startup = McpStartup::Done;
    }
}
