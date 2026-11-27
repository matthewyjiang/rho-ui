//! The fake daemon's MCP servers. Every session reports the same three,
//! configured in the rho home's `config.toml`: [`MCP_DOCS`], connected with
//! two tools; [`MCP_GITHUB`], connected with one; and [`MCP_NOTES`],
//! disabled. With [`Script::mcp`] [`McpStartup::Connecting`] they are
//! still starting until [`FakeDaemon::finish_mcp_startup`]: the docs server
//! connects, the GitHub server has timed out, and the session's tool
//! sources load.
//!
//! The docs server offers two prompts: [`MCP_SEARCH`], one argument whose
//! values it suggests, and `outline`, which suggests none. It offers three
//! resources: [`MCP_INTRO`], a page it reads; [`MCP_CHANGELOG`], whose read
//! fails; and [`MCP_USER`], a template. Listings and reads wait while a
//! test holds them ([`super::Answers`]).

use std::path::PathBuf;

use rho_protocol::{
    ErrorCode, RpcError, SessionMcpCatalog, SessionMcpCatalogParams, SessionMcpCompleteParams,
    SessionMcpCompleteResponse, SessionMcpInventory, SessionMcpMode, SessionMcpParams,
    SessionMcpPrompt, SessionMcpPromptArgument, SessionMcpResource, SessionMcpResourceContent,
    SessionMcpResourceParams, SessionMcpResourceResponse, SessionMcpServer, SessionMcpStatus,
};

use super::{FakeDaemon, Script, State};

/// The server that exports two tools.
pub const MCP_DOCS: &str = "docs";
/// The server that exports one tool, and times out while starting.
pub const MCP_GITHUB: &str = "github";
/// The server the config disables.
pub const MCP_NOTES: &str = "notes";

/// The docs server's prompt that suggests values for its one argument,
/// `query`: the [`MCP_QUERIES`] that start with what is typed.
pub const MCP_SEARCH: &str = "search";
/// The values [`MCP_SEARCH`] suggests from.
pub const MCP_QUERIES: [&str; 3] = ["rust ownership", "rust lifetimes", "async runtimes"];
/// The docs server's page, which reads as [`MCP_INTRO_TEXT`].
pub const MCP_INTRO: &str = "docs://guide/intro";
/// What [`MCP_INTRO`] reads as.
pub const MCP_INTRO_TEXT: &str = "# Intro\n\nWelcome to the docs.";
/// The docs server's resource whose read fails.
pub const MCP_CHANGELOG: &str = "docs://changelog";
/// The docs server's resource template.
pub const MCP_USER: &str = "docs://users/{id}";

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
    /// Whether the catalog fails; see [`FakeDaemon::fail_mcp_catalog`].
    catalog_fails: bool,
    /// How many times the catalog was asked for.
    catalog_reads: usize,
}

impl FakeMcp {
    pub(super) fn new(script: &Script) -> Self {
        Self {
            startup: script.mcp,
            catalog_fails: false,
            catalog_reads: 0,
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

impl State {
    /// `rho/session/mcp/catalog`: what the docs server offers.
    pub(super) fn mcp_catalog(
        &mut self,
        params: &SessionMcpCatalogParams,
    ) -> Result<SessionMcpCatalog, RpcError> {
        self.session(&params.session)?;
        self.mcp.catalog_reads += 1;
        if self.mcp.catalog_fails {
            let message = format!("{MCP_DOCS}: listing failed: timed out");
            return Err(RpcError::new(ErrorCode::InternalError, message));
        }
        let prompt = |name: &str, argument: &str, completes_arguments| SessionMcpPrompt {
            server: MCP_DOCS.to_owned(),
            name: name.to_owned(),
            title: None,
            description: None,
            arguments: vec![SessionMcpPromptArgument {
                name: argument.to_owned(),
                description: None,
                required: true,
            }],
            completes_arguments,
        };
        let resource = |uri: &str, name: &str, templated| SessionMcpResource {
            server: MCP_DOCS.to_owned(),
            uri: uri.to_owned(),
            name: name.to_owned(),
            title: None,
            description: None,
            mime_type: Some("text/markdown".to_owned()),
            templated,
        };
        Ok(SessionMcpCatalog {
            prompts: vec![
                prompt(MCP_SEARCH, "query", true),
                prompt("outline", "topic", false),
            ],
            resources: vec![
                resource(MCP_INTRO, "Intro", false),
                resource(MCP_CHANGELOG, "Changelog", false),
                resource(MCP_USER, "User", true),
            ],
        })
    }

    /// `rho/session/mcp/resource`: a read of one of the docs server's
    /// resources.
    pub(super) fn mcp_resource(
        &mut self,
        params: &SessionMcpResourceParams,
    ) -> Result<SessionMcpResourceResponse, RpcError> {
        self.session(&params.session)?;
        if params.server != MCP_DOCS {
            let message = format!("MCP server {} is not connected", params.server);
            return Err(RpcError::new(ErrorCode::InvalidParams, message));
        }
        let uri = params.uri.as_str();
        if uri != MCP_INTRO {
            let message = format!("{MCP_DOCS}: reading {uri} failed: timed out");
            return Err(RpcError::new(ErrorCode::InternalError, message));
        }
        Ok(SessionMcpResourceResponse {
            contents: vec![SessionMcpResourceContent::Text {
                uri: params.uri.clone(),
                mime_type: Some("text/markdown".to_owned()),
                text: MCP_INTRO_TEXT.to_owned(),
            }],
        })
    }

    /// `rho/session/mcp/complete`: [`MCP_SEARCH`]'s suggestions.
    pub(super) fn mcp_complete(
        &mut self,
        params: &SessionMcpCompleteParams,
    ) -> Result<SessionMcpCompleteResponse, RpcError> {
        self.session(&params.session)?;
        let values = if params.server == MCP_DOCS && params.prompt == MCP_SEARCH {
            MCP_QUERIES
                .iter()
                .filter(|query| query.starts_with(&params.value))
                .map(|&query| query.to_owned())
                .collect()
        } else {
            Vec::new()
        };
        Ok(SessionMcpCompleteResponse { values })
    }
}

impl FakeDaemon {
    /// Ends the MCP startup [`McpStartup::Connecting`] began: [`MCP_DOCS`]
    /// and [`MCP_GITHUB`] connect, with their tools, and the tool sources
    /// have loaded.
    pub fn finish_mcp_startup(&self) {
        self.service.lock().mcp.startup = McpStartup::Done;
    }

    /// Makes `rho/session/mcp/catalog` fail, as a server's listing that
    /// times out does.
    pub fn fail_mcp_catalog(&self) {
        self.service.lock().mcp.catalog_fails = true;
    }

    /// How many times `rho/session/mcp/catalog` was answered.
    pub fn mcp_catalog_reads(&self) -> usize {
        self.service.lock().mcp.catalog_reads
    }
}
