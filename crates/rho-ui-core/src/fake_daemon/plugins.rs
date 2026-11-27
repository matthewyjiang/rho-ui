//! The fake daemon's agent plugins, `rho/plugins/*`: a list it keeps in
//! memory and changes as each request asks, shaped as rho's are.
//!
//! The user has six plugins, in the order rho lists them, by name:
//! `broken`, whose `plugin.json` doesn't parse, so it is rejected, and
//! can be removed but not enabled or disabled;
//! `deploy`, installed from git on [`UNREACHABLE_HOST`], whose updates fail
//! to fetch; `fmt`; `notes`, copied in by an install and disabled; `review`,
//! installed from git, which brings the MCP server `review/lint`, which
//! `rho/mcp/list` lists while `review` loads; and `scratch`, linked to a folder, with a skill
//! that has a problem. Every git repository has one newer commit, so the
//! first update of a plugin moves it there and the next finds it up to
//! date.
//!
//! Given a workspace, its project has the plugin `fmt` too, which comes
//! first. A project's plugins are untrusted, whatever the client's
//! environment says, so a test doesn't follow the environment it runs in;
//! but those of [`TRUSTED_PLUGINS_WORKSPACE`] are trusted, so there its
//! `fmt` loads and shadows the user's.
//!
//! An install takes an absolute path, or a git URL whose last segment
//! names the plugin. With [`Script::configured`]
//! [`super::ConfiguredMethods::Unserved`] it is a daemon from before
//! plugins, which answers `MethodNotFound`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use rho_protocol::{
    ErrorCode, Plugin, PluginConflict, PluginInstallSource, PluginScope, PluginSource,
    PluginStatus, PluginTarget, PluginUpdate, PluginUpdateOutcome, PluginsInstallParams,
    PluginsInstallResponse, PluginsListParams, PluginsListResponse, PluginsRemoveParams,
    PluginsRemoveResponse, PluginsSetEnabledParams, PluginsSetEnabledResponse, PluginsUpdateParams,
    PluginsUpdateResponse, RpcError, PLUGINS_INSTALL, PLUGINS_LIST, PLUGINS_REMOVE,
    PLUGINS_SET_ENABLED, PLUGINS_UPDATE,
};

use super::{
    configured::{in_workspace, rho_home},
    ConfiguredMethods, FakeDaemon, Script, State,
};
use crate::platform;

/// The workspace whose project the client trusts, as though its
/// environment set `RHO_TRUST_PROJECT_PLUGINS=1`.
pub const TRUSTED_PLUGINS_WORKSPACE: &str = "/w/t";

/// The git host no fetch reaches.
const UNREACHABLE_HOST: &str = "git.example.com";

/// The commit each fake repository's ref points at now.
const LATEST_COMMIT: &str = "9e1d0c4b7a6f5e3d2c1b0a9f8e7d6c5b4a3f2e1d";

/// One package in a plugins root, as its manifest and records say.
#[derive(Clone, Debug)]
struct Package {
    name: String,
    version: Option<String>,
    description: Option<String>,
    source: PluginSource,
    enabled: bool,
    /// Why its `plugin.json` doesn't load, which rejects it.
    error: Option<String>,
    problems: Vec<String>,
    skills: Vec<String>,
    mcp_servers: Vec<String>,
    agent_files: Vec<String>,
}

impl Package {
    /// An enabled package named `name` from `source`, which brings the
    /// skill `name`.
    fn new(name: &str, version: &str, description: &str, source: PluginSource) -> Self {
        Self {
            name: name.to_owned(),
            version: Some(version.to_owned()),
            description: Some(description.to_owned()),
            source,
            enabled: true,
            error: None,
            problems: Vec::new(),
            skills: vec![name.to_owned()],
            mcp_servers: Vec::new(),
            agent_files: Vec::new(),
        }
    }
}

/// A git source of `url` at `commit`.
fn git(url: &str, git_ref: Option<&str>, commit: &str) -> PluginSource {
    PluginSource::Git {
        url: url.to_owned(),
        git_ref: git_ref.map(str::to_owned),
        path: None,
        commit: commit.to_owned(),
    }
}

/// The user's plugins when the daemon starts, by name.
fn user_packages() -> Vec<Package> {
    let home = platform::home_dir().unwrap_or_else(|| PathBuf::from("/home/user"));
    let broken = Package {
        version: None,
        description: None,
        error: Some("plugin.json: expected value at line 1 column 1".to_owned()),
        skills: Vec::new(),
        ..Package::new("broken", "", "", PluginSource::Local)
    };
    let deploy = Package::new(
        "deploy",
        "0.3.1",
        "Ships a branch to staging.",
        git(
            &format!("https://{UNREACHABLE_HOST}/ops/deploy.git"),
            None,
            "5a4b3c2d1e0f9a8b7c6d5e4f3a2b1c0d9e8f7a6b",
        ),
    );
    let fmt = Package::new(
        "fmt",
        "1.0.0",
        "Formats code before a commit.",
        PluginSource::Local,
    );
    let notes = Package {
        enabled: false,
        ..Package::new(
            "notes",
            "2.0.0",
            "Keeps notes beside a session.",
            PluginSource::Install,
        )
    };
    let review = Package {
        mcp_servers: vec!["lint".to_owned()],
        agent_files: vec!["reviewer.md".to_owned()],
        ..Package::new(
            "review",
            "1.2.0",
            "Reviews a diff before you push it.",
            git(
                "https://github.com/acme/review",
                Some("main"),
                "1f3c9a2e8d7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f",
            ),
        )
    };
    let scratch = Package {
        skills: vec!["draft".to_owned()],
        problems: vec!["skills/half: SKILL.md has no description, so it is skipped".to_owned()],
        ..Package::new(
            "scratch",
            "0.1.0",
            "Skills being drafted.",
            PluginSource::Link {
                target: home.join("code/scratch"),
            },
        )
    };
    vec![broken, deploy, fmt, notes, review, scratch]
}

/// A project's plugins, before any request changes them.
fn project_packages() -> Vec<Package> {
    vec![Package::new(
        "fmt",
        "1.1.0",
        "The project's own formatter.",
        PluginSource::Local,
    )]
}

/// The fake's plugins: the user's, and each workspace's project's once a
/// request names it.
#[derive(Debug)]
pub(super) struct FakePlugins {
    methods: ConfiguredMethods,
    user: Vec<Package>,
    projects: BTreeMap<PathBuf, Vec<Package>>,
}

impl FakePlugins {
    pub(super) fn new(script: &Script) -> Self {
        Self {
            methods: script.configured,
            user: user_packages(),
            projects: BTreeMap::new(),
        }
    }

    /// Refuses every request as a daemon from before plugins would.
    fn served(&self, method: &str) -> Result<(), RpcError> {
        match self.methods {
            ConfiguredMethods::Served => Ok(()),
            ConfiguredMethods::Unserved => Err(RpcError::new(
                ErrorCode::MethodNotFound,
                format!("unknown method {method}"),
            )),
        }
    }

    /// The packages of `scope`, for a session in `workspace`.
    fn root_mut(
        &mut self,
        scope: PluginScope,
        workspace: Option<&Path>,
    ) -> Result<&mut Vec<Package>, RpcError> {
        match (scope, workspace) {
            (PluginScope::User, _) => Ok(&mut self.user),
            (PluginScope::Project, Some(workspace)) => Ok(self
                .projects
                .entry(workspace.to_path_buf())
                .or_insert_with(project_packages)),
            (PluginScope::Project, None) => Err(RpcError::new(
                ErrorCode::InvalidParams,
                "project scope needs a workspace",
            )),
            _ => Err(RpcError::new(
                ErrorCode::InvalidParams,
                "unknown plugin scope",
            )),
        }
    }

    /// The package `target` names, for a session in `workspace`.
    fn package_mut(
        &mut self,
        target: &PluginTarget,
        workspace: Option<&Path>,
    ) -> Result<&mut Package, RpcError> {
        self.root_mut(target.scope, workspace)?
            .iter_mut()
            .find(|package| package.name == target.name)
            .ok_or_else(|| not_found(target))
    }

    /// Every plugin a session in `workspace` would find, as rho lists them:
    /// the project's first, then the user's. A loaded or disabled plugin
    /// takes its name from those after it; a rejected or untrusted one
    /// doesn't.
    pub(super) fn list(&self, workspace: Option<&Path>) -> Vec<Plugin> {
        let project = match workspace {
            Some(workspace) => self
                .projects
                .get(workspace)
                .cloned()
                .unwrap_or_else(project_packages),
            None => Vec::new(),
        };
        let trusted = trusted(workspace);
        let packages = project
            .into_iter()
            .map(|package| (PluginScope::Project, package))
            .chain(
                self.user
                    .iter()
                    .cloned()
                    .map(|package| (PluginScope::User, package)),
            );
        let mut taken: Vec<String> = Vec::new();
        packages
            .map(|(scope, package)| {
                let status = if package.error.is_some() {
                    PluginStatus::Rejected
                } else if taken.contains(&package.name) {
                    PluginStatus::Shadowed
                } else if !package.enabled {
                    PluginStatus::Disabled
                } else if scope == PluginScope::Project && !trusted {
                    PluginStatus::Untrusted
                } else {
                    PluginStatus::Loaded
                };
                if matches!(status, PluginStatus::Loaded | PluginStatus::Disabled) {
                    taken.push(package.name.clone());
                }
                listed(package, scope, status, workspace)
            })
            .collect()
    }

    /// `target` as [`Self::list`] lists it for `workspace`.
    fn listed(&self, target: &PluginTarget, workspace: Option<&Path>) -> Plugin {
        self.list(workspace)
            .into_iter()
            .find(|plugin| plugin.name == target.name && plugin.scope == target.scope)
            .expect("a package the request just changed is listed")
    }
}

/// Whether the client trusts the project of `workspace`.
fn trusted(workspace: Option<&Path>) -> bool {
    workspace == Some(Path::new(TRUSTED_PLUGINS_WORKSPACE))
}

/// Fetches `package`'s repository again: its host may be out of
/// reach; else its ref points at [`LATEST_COMMIT`].
fn fetch(package: &mut Package, scope: PluginScope) -> Option<PluginUpdate> {
    let PluginSource::Git { url, commit, .. } = &mut package.source else {
        return None;
    };
    let outcome = if url.contains(UNREACHABLE_HOST) {
        PluginUpdateOutcome::Failed {
            code: ErrorCode::PluginFetchFailed.code(),
            message: unreachable(url),
        }
    } else if commit == LATEST_COMMIT {
        PluginUpdateOutcome::UpToDate {
            commit: commit.clone(),
        }
    } else {
        let previous = std::mem::replace(commit, LATEST_COMMIT.to_owned());
        PluginUpdateOutcome::Updated {
            previous,
            current: LATEST_COMMIT.to_owned(),
        }
    };
    let target = PluginTarget::new(package.name.clone(), scope);
    Some(PluginUpdate::new(target, outcome))
}

/// `package` as rho lists it in `scope` with `status`.
fn listed(
    package: Package,
    scope: PluginScope,
    status: PluginStatus,
    workspace: Option<&Path>,
) -> Plugin {
    let path = package_path(&package.name, scope, workspace);
    let state_path = match (scope, workspace) {
        (PluginScope::Project, Some(workspace)) => in_workspace(workspace, ".rho/plugins.toml"),
        _ => rho_home().join("plugins.toml"),
    };
    // A shadowed or rejected package's components are not read.
    let read = !matches!(status, PluginStatus::Shadowed | PluginStatus::Rejected);
    let components = |names: Vec<String>| if read { names } else { Vec::new() };
    Plugin::new(
        package.name,
        package.version,
        package.description,
        scope,
        path,
        package.source,
        package.enabled,
        state_path,
        status,
        package.error,
        package.problems,
        components(package.skills),
        components(package.mcp_servers),
        components(package.agent_files),
    )
}

/// Where the package named `name` is in `scope`'s plugins root.
fn package_path(name: &str, scope: PluginScope, workspace: Option<&Path>) -> PathBuf {
    match (scope, workspace) {
        (PluginScope::Project, Some(workspace)) => {
            in_workspace(workspace, &format!(".agents/plugins/{name}"))
        }
        _ => platform::home_dir()
            .unwrap_or_else(|| PathBuf::from("/home/user"))
            .join(".agents/plugins")
            .join(name),
    }
}

fn not_found(target: &PluginTarget) -> RpcError {
    RpcError::new(
        ErrorCode::PluginNotFound,
        format!("no plugin named {} is installed", target.name),
    )
}

/// What `git` says of `url` on an unreachable host.
fn unreachable(url: &str) -> String {
    format!(
        "git fetch failed: fatal: unable to access '{url}/': Could not resolve host: \
         {UNREACHABLE_HOST}"
    )
}

/// The package an install of `source` brings: its name is its folder's,
/// or the URL's last segment.
fn installed(source: &PluginInstallSource) -> Result<Package, RpcError> {
    let (name, source) = match source {
        PluginInstallSource::Path { path } | PluginInstallSource::Link { path } => {
            if !path.has_root() {
                return Err(RpcError::new(
                    ErrorCode::InvalidParams,
                    format!("plugin path must be absolute: {}", path.display()),
                ));
            }
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            let source = match source {
                PluginInstallSource::Link { path } => PluginSource::Link {
                    target: path.clone(),
                },
                _ => PluginSource::Install,
            };
            (name, source)
        }
        PluginInstallSource::Git { url } => {
            if url.contains(UNREACHABLE_HOST) {
                return Err(RpcError::new(
                    ErrorCode::PluginFetchFailed,
                    unreachable(url),
                ));
            }
            let name = url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .map(|last| last.trim_end_matches(".git").to_owned());
            (name, git(url, None, LATEST_COMMIT))
        }
        _ => (None, PluginSource::Install),
    };
    let name = name
        .filter(|name| !name.is_empty())
        .ok_or_else(|| RpcError::new(ErrorCode::PluginInvalid, "no plugin.json there"))?;
    Ok(Package::new(&name, "1.0.0", "Installed just now.", source))
}

impl State {
    /// Answers `rho/plugins/list`.
    pub(super) fn plugins_list(
        &mut self,
        params: &PluginsListParams,
    ) -> Result<PluginsListResponse, RpcError> {
        self.plugins.served(PLUGINS_LIST)?;
        Ok(PluginsListResponse::new(
            self.plugins.list(params.workspace.as_deref()),
        ))
    }

    /// Answers `rho/plugins/install`.
    pub(super) fn plugins_install(
        &mut self,
        params: &PluginsInstallParams,
    ) -> Result<PluginsInstallResponse, RpcError> {
        self.plugins.served(PLUGINS_INSTALL)?;
        let workspace = params.workspace.as_deref();
        let package = installed(&params.source)?;
        let target = PluginTarget::new(package.name.clone(), params.scope);
        let root = self.plugins.root_mut(params.scope, workspace)?;
        match root.iter().position(|there| there.name == package.name) {
            Some(_) if !params.force => {
                let conflict = PluginConflict::new(
                    package.name.clone(),
                    package_path(&package.name, params.scope, workspace),
                );
                let data = serde_json::to_value(conflict).expect("a conflict serializes");
                return Err(RpcError::new(
                    ErrorCode::PluginExists,
                    format!("plugin {} is already installed", package.name),
                )
                .with_data(data));
            }
            Some(ix) => root[ix] = package,
            None => {
                root.push(package);
                root.sort_by(|a, b| a.name.cmp(&b.name));
            }
        }
        Ok(PluginsInstallResponse::new(
            self.plugins.listed(&target, workspace),
        ))
    }

    /// Answers `rho/plugins/update`.
    pub(super) fn plugins_update(
        &mut self,
        params: &PluginsUpdateParams,
    ) -> Result<PluginsUpdateResponse, RpcError> {
        self.plugins.served(PLUGINS_UPDATE)?;
        let workspace = params.workspace.as_deref();
        let trusted = trusted(workspace);
        if let Some(target) = &params.plugin {
            if target.scope == PluginScope::Project && !trusted {
                return Err(RpcError::new(
                    ErrorCode::PluginUntrusted,
                    format!("the project's plugin {} is not trusted", target.name),
                ));
            }
            let package = self.plugins.package_mut(target, workspace)?;
            let update = fetch(package, target.scope).ok_or_else(|| {
                RpcError::new(
                    ErrorCode::InvalidParams,
                    format!("plugin {} was not installed from git", target.name),
                )
            })?;
            return Ok(PluginsUpdateResponse::new(vec![update], Vec::new()));
        }
        let mut results = Vec::new();
        let mut untrusted = Vec::new();
        if workspace.is_some() {
            for package in self.plugins.root_mut(PluginScope::Project, workspace)? {
                if !matches!(package.source, PluginSource::Git { .. }) {
                    continue;
                }
                if trusted {
                    results.extend(fetch(package, PluginScope::Project));
                } else {
                    untrusted.push(package.name.clone());
                }
            }
        }
        for package in &mut self.plugins.user {
            results.extend(fetch(package, PluginScope::User));
        }
        Ok(PluginsUpdateResponse::new(results, untrusted))
    }

    /// Answers `rho/plugins/set_enabled`.
    pub(super) fn plugins_set_enabled(
        &mut self,
        params: &PluginsSetEnabledParams,
    ) -> Result<PluginsSetEnabledResponse, RpcError> {
        self.plugins.served(PLUGINS_SET_ENABLED)?;
        let workspace = params.workspace.as_deref();
        let package = self.plugins.package_mut(&params.plugin, workspace)?;
        if let Some(error) = &package.error {
            return Err(RpcError::new(ErrorCode::PluginInvalid, error.clone()));
        }
        package.enabled = params.enabled;
        Ok(PluginsSetEnabledResponse::new(
            self.plugins.listed(&params.plugin, workspace),
        ))
    }

    /// Answers `rho/plugins/remove`.
    pub(super) fn plugins_remove(
        &mut self,
        params: &PluginsRemoveParams,
    ) -> Result<PluginsRemoveResponse, RpcError> {
        self.plugins.served(PLUGINS_REMOVE)?;
        let workspace = params.workspace.as_deref();
        let target = &params.plugin;
        let root = self.plugins.root_mut(target.scope, workspace)?;
        let before = root.len();
        root.retain(|package| package.name != target.name);
        if root.len() == before {
            return Err(not_found(target));
        }
        Ok(PluginsRemoveResponse::new(package_path(
            &target.name,
            target.scope,
            workspace,
        )))
    }
}

impl FakeDaemon {
    /// Every plugin a session in `workspace` would find, as
    /// `rho/plugins/list` lists them now.
    pub fn plugins(&self, workspace: Option<&Path>) -> Vec<Plugin> {
        self.service.lock().plugins.list(workspace)
    }
}
