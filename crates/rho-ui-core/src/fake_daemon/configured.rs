//! What the fake daemon's configuration sets up, read without a session:
//! `rho/mcp/list` and `rho/hooks/list`, made up but shaped as rho's are.
//!
//! Its `config.toml` configures [`MCP_DOCS`] (stdio), [`MCP_GITHUB`]
//! (HTTP), [`MCP_NOTES`] (disabled) and `broken`, whose configuration is
//! invalid; the user's plugin `review` adds `review/lint`. The list is the
//! same for every workspace.
//!
//! The user's hooks file defines `user:notify`. Given a workspace, its
//! project's hooks file defines `project:fmt`, and is untrusted whatever
//! the client's environment says, so a test doesn't follow the
//! environment it runs in; but the project's file in
//! [`INVALID_HOOKS_WORKSPACE`] doesn't parse, which disables the user's.
//! With [`Script::configured`] [`ConfiguredMethods::Unserved`] it is a
//! daemon from before both, which answers `MethodNotFound`.

use std::path::{Path, PathBuf};

use rho_protocol::{
    ConfiguredMcpServer, ErrorCode, HookContract, HookEvent, HookStatus, HookTools, HookWarning,
    HooksFile, HooksFileScope, HooksFileStatus, HooksListParams, HooksListResponse, McpListParams,
    McpListResponse, McpServerSource, McpTransport, RpcError, HOOKS_LIST, MCP_LIST,
};

use super::{FakeDaemon, Script, State, MCP_DOCS, MCP_GITHUB, MCP_NOTES};
use crate::platform;

/// The workspace whose project's hooks file is invalid.
pub const INVALID_HOOKS_WORKSPACE: &str = "/w/b";

/// Whether the fake daemon serves `rho/mcp/list` and `rho/hooks/list`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConfiguredMethods {
    /// It serves both.
    #[default]
    Served,
    /// As a daemon from before them: both answer `MethodNotFound`.
    Unserved,
}

/// Which listing a client asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfiguredList {
    /// `rho/mcp/list`.
    Mcp,
    /// `rho/hooks/list`.
    Hooks,
}

/// One `rho/mcp/list` or `rho/hooks/list` request, as the daemon got it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfiguredRead {
    /// Which listing.
    pub list: ConfiguredList,
    /// The workspace it named.
    pub workspace: Option<PathBuf>,
}

/// How the fake answers both listings, and what it was asked.
#[derive(Debug)]
pub(super) struct FakeConfigured {
    methods: ConfiguredMethods,
    reads: Vec<ConfiguredRead>,
}

impl FakeConfigured {
    pub(super) fn new(script: &Script) -> Self {
        Self {
            methods: script.configured,
            reads: Vec::new(),
        }
    }

    /// Notes the request, and refuses it as an older daemon would.
    fn read(
        &mut self,
        list: ConfiguredList,
        workspace: Option<&Path>,
        method: &str,
    ) -> Result<(), RpcError> {
        self.reads.push(ConfiguredRead {
            list,
            workspace: workspace.map(Path::to_path_buf),
        });
        match self.methods {
            ConfiguredMethods::Served => Ok(()),
            ConfiguredMethods::Unserved => Err(RpcError::new(
                ErrorCode::MethodNotFound,
                format!("unknown method {method}"),
            )),
        }
    }
}

/// `rest` in `workspace`, as a daemon on a Unix machine names it: the
/// fake's workspaces are such paths (`/w/a`) on any OS the tests run on,
/// and `Path::join` would add the test machine's separator instead.
fn in_workspace(workspace: &Path, rest: &str) -> PathBuf {
    PathBuf::from(format!("{}/{rest}", workspace.display()))
}

/// The fake's rho home, as a real daemon of this user's would have it.
fn rho_home() -> PathBuf {
    platform::home_dir()
        .unwrap_or_else(|| PathBuf::from("/home/user"))
        .join(".rho")
}

impl State {
    /// Answers `rho/mcp/list`.
    pub(super) fn configured_mcp(
        &mut self,
        params: &McpListParams,
    ) -> Result<McpListResponse, RpcError> {
        self.configured
            .read(ConfiguredList::Mcp, params.workspace.as_deref(), MCP_LIST)?;
        let config = || McpServerSource::Config;
        let stdio = |command: &str, args: &[&str]| {
            Some(McpTransport::Stdio {
                command: command.to_owned(),
                args: args.iter().map(|&arg| arg.to_owned()).collect(),
            })
        };
        let server = ConfiguredMcpServer::new;
        Ok(McpListResponse::new(
            rho_home().join("config.toml"),
            vec![
                server(
                    MCP_DOCS.to_owned(),
                    config(),
                    true,
                    stdio("npx", &["-y", "@acme/docs-mcp"]),
                    None,
                ),
                server(
                    MCP_GITHUB.to_owned(),
                    config(),
                    true,
                    Some(McpTransport::StreamableHttp {
                        url: "https://api.githubcopilot.com/mcp/".to_owned(),
                    }),
                    None,
                ),
                server(
                    MCP_NOTES.to_owned(),
                    config(),
                    false,
                    stdio("notes-mcp", &["--stdio"]),
                    None,
                ),
                server(
                    "broken".to_owned(),
                    config(),
                    false,
                    None,
                    Some("mcp.servers.broken: set `command` or `url`".to_owned()),
                ),
                server(
                    "review/lint".to_owned(),
                    McpServerSource::Plugin {
                        plugin: "review".to_owned(),
                    },
                    true,
                    stdio("uvx", &["review-lint-mcp"]),
                    None,
                ),
            ],
        ))
    }

    /// Answers `rho/hooks/list`.
    pub(super) fn configured_hooks(
        &mut self,
        params: &HooksListParams,
    ) -> Result<HooksListResponse, RpcError> {
        self.configured.read(
            ConfiguredList::Hooks,
            params.workspace.as_deref(),
            HOOKS_LIST,
        )?;
        let home = rho_home();
        let user_path = home.join("hooks.toml");
        let Some(workspace) = &params.workspace else {
            return Ok(HooksListResponse::new(vec![user_file(
                user_path,
                HooksFileStatus::Loaded,
            )]));
        };
        let project_path = in_workspace(workspace, ".rho/hooks.toml");
        if workspace == Path::new(INVALID_HOOKS_WORKSPACE) {
            return Ok(HooksListResponse::new(vec![
                user_file(
                    user_path,
                    HooksFileStatus::Disabled {
                        by: project_path.clone(),
                    },
                ),
                HooksFile::new(
                    project_path,
                    HooksFileScope::Project,
                    HooksFileStatus::Invalid {
                        error: "expected `=`, found newline at line 3 column 9".to_owned(),
                    },
                    Vec::new(),
                ),
            ]));
        }
        let fmt = HookContract {
            id: "project:fmt".to_owned(),
            status: HookStatus::Untrusted,
            event: HookEvent::BeforeToolUse,
            tools: HookTools::Matching {
                patterns: vec!["edit".to_owned(), "write".to_owned()],
            },
            command: vec![
                in_workspace(workspace, "scripts/fmt.sh")
                    .display()
                    .to_string(),
                "--check".to_owned(),
            ],
            working_directory: workspace.clone(),
            timeout_ms: 30_000,
            environment: vec!["RHO_TOOL_NAME".to_owned()],
            warnings: vec![HookWarning::MissingEditTools {
                tools: vec!["apply_patch".to_owned()],
            }],
        };
        Ok(HooksListResponse::new(vec![
            user_file(user_path, HooksFileStatus::Loaded),
            HooksFile::new(
                project_path,
                HooksFileScope::Project,
                HooksFileStatus::Untrusted { error: None },
                vec![fmt],
            ),
        ]))
    }
}

/// The user's hooks file at `path`, with `status`, defining `user:notify`.
fn user_file(path: PathBuf, status: HooksFileStatus) -> HooksFile {
    let hook_status = match status {
        HooksFileStatus::Disabled { .. } => HookStatus::Disabled,
        _ => HookStatus::Active,
    };
    let notify = HookContract {
        id: "user:notify".to_owned(),
        status: hook_status,
        event: HookEvent::RunCompleted,
        tools: HookTools::All,
        command: vec!["notify-send".to_owned(), "rho: turn done".to_owned()],
        working_directory: path.parent().map_or_else(PathBuf::new, Path::to_path_buf),
        timeout_ms: 5_000,
        environment: Vec::new(),
        warnings: Vec::new(),
    };
    HooksFile::new(path, HooksFileScope::User, status, vec![notify])
}

impl FakeDaemon {
    /// Every `rho/mcp/list` and `rho/hooks/list` request the daemon got,
    /// in order, refused ones too.
    pub fn configured_reads(&self) -> Vec<ConfiguredRead> {
        self.service.lock().configured.reads.clone()
    }
}
