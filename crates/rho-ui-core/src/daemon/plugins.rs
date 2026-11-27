//! A machine's agent plugins, managed without a session, as `rho plugins`
//! manages them: listed, installed from a folder or a git repository,
//! linked, updated, enabled or disabled, and removed (`rho/plugins/*`).
//!
//! Every call acts on the daemon's machine: a path names a folder there,
//! and an install from a URL runs `git` there, answered once it finishes.
//! A change applies to sessions opened afterwards; a loaded session keeps
//! the plugins it loaded. A project's plugins count for the workspace each
//! call names, and whether they load follows this client's environment
//! (`RHO_TRUST_PROJECT_PLUGINS`), as for the sessions it opens.

use std::path::{Path, PathBuf};

use rho_protocol::{
    methods, ClientEnvironment, ClientError, ErrorCode, Plugin, PluginConflict,
    PluginInstallSource, PluginScope, PluginTarget, PluginsInstallParams, PluginsListParams,
    PluginsRemoveParams, PluginsSetEnabledParams, PluginsUpdateParams, PluginsUpdateResponse,
};

use super::{Daemon, DaemonError};

/// Why the daemon changed no plugin. Each plugin refusal is its own
/// variant, read from its error code; each holds the daemon's message,
/// which is meant for people.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PluginError {
    /// No plugin of that name is in that scope (`PluginNotFound`): the
    /// list it was picked from is out of date.
    #[error("{0}")]
    NotFound(String),
    /// A plugin of that name is already where the install would put it
    /// (`PluginExists`), so nothing was installed. The same install with
    /// [`IfExists::Replace`] replaces `conflict`.
    #[error("{message}")]
    Exists {
        /// The plugin already there.
        conflict: PluginConflict,
        /// The daemon's message.
        message: String,
    },
    /// The update names a project plugin, and this client doesn't trust the
    /// project (`PluginUntrusted`), so nothing was fetched.
    #[error("{0}")]
    Untrusted(String),
    /// What the install or update names is not a plugin package
    /// (`PluginInvalid`): no `plugin.json`, an invalid manifest, or an
    /// update that would rename it. Nothing was changed.
    #[error("{0}")]
    Invalid(String),
    /// `git` is missing on the daemon's machine, or couldn't fetch or check
    /// out the repository (`PluginFetchFailed`); the message carries what
    /// `git` said. Nothing was changed.
    #[error("{0}")]
    FetchFailed(String),
    /// Any other failure: a refusal such as a relative path or project
    /// scope without a project, a daemon from before plugins, or the
    /// connection is gone.
    #[error(transparent)]
    Daemon(#[from] DaemonError),
}

impl From<ClientError> for PluginError {
    fn from(error: ClientError) -> Self {
        let ClientError::Rpc { error: refusal, .. } = &error else {
            return Self::Daemon(error.into());
        };
        let message = refusal.message.clone();
        match refusal.error_code() {
            Some(ErrorCode::PluginNotFound) => Self::NotFound(message),
            Some(ErrorCode::PluginExists) => {
                // Its data names the plugin there; without it, there is
                // nothing to offer to replace.
                let conflict = refusal
                    .data
                    .clone()
                    .and_then(|data| serde_json::from_value(data).ok());
                match conflict {
                    Some(conflict) => Self::Exists { conflict, message },
                    None => Self::Daemon(error.into()),
                }
            }
            Some(ErrorCode::PluginUntrusted) => Self::Untrusted(message),
            Some(ErrorCode::PluginInvalid) => Self::Invalid(message),
            Some(ErrorCode::PluginFetchFailed) => Self::FetchFailed(message),
            _ => Self::Daemon(error.into()),
        }
    }
}

/// What an install does when a plugin of the same name is already in its
/// scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IfExists {
    /// Install nothing, and fail with [`PluginError::Exists`].
    Refuse,
    /// Replace it.
    Replace,
}

/// Whether a plugin loads in the sessions opened afterwards, as its
/// scope's `plugins.toml` saves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enablement {
    /// Sessions load it, if nothing else stops them.
    Enabled,
    /// Sessions skip it.
    Disabled,
}

impl Daemon {
    /// Every plugin a session in `workspace` would find, in the order it
    /// loads them: the project's, nearest root first, then the user's;
    /// including those disabled, untrusted, shadowed by a nearer one of the
    /// same name, or rejected for a broken `plugin.json`. Without a
    /// workspace, as for a session in the daemon user's home, which leaves
    /// the user's own. `None` from a daemon older than plugins, which will
    /// never have one.
    ///
    /// # Errors
    ///
    /// Fails if the daemon refuses `workspace`, or the connection is gone.
    pub async fn plugins(
        &self,
        workspace: Option<&Path>,
    ) -> Result<Option<Vec<Plugin>>, DaemonError> {
        let params = PluginsListParams::new(
            workspace.map(Path::to_path_buf),
            ClientEnvironment::clone(&self.environment),
        );
        match self.client.request::<methods::PluginsList>(&params).await {
            Ok(list) => Ok(Some(list.plugins)),
            Err(error) if error.error_code() == Some(ErrorCode::MethodNotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Installs the package `source` names into `scope`'s plugins root,
    /// enabled: the user's, or the project of `workspace`. Answered once a
    /// git fetch finishes. Returns it as [`Self::plugins`] now lists it.
    ///
    /// # Errors
    ///
    /// [`PluginError::Exists`] with [`IfExists::Refuse`] when a plugin of
    /// its name is there; [`PluginError::Invalid`] or
    /// [`PluginError::FetchFailed`] when `source` holds no plugin or can't
    /// be fetched; else as [`PluginError::Daemon`] says.
    pub async fn install_plugin(
        &self,
        source: PluginInstallSource,
        scope: PluginScope,
        if_exists: IfExists,
        workspace: Option<&Path>,
    ) -> Result<Plugin, PluginError> {
        let params = PluginsInstallParams::new(
            source,
            scope,
            if_exists == IfExists::Replace,
            workspace.map(Path::to_path_buf),
            ClientEnvironment::clone(&self.environment),
        );
        let installed = self
            .client
            .request::<methods::PluginsInstall>(&params)
            .await?;
        Ok(installed.plugin)
    }

    /// Fetches the git source of `plugin` again, or of every git-installed
    /// plugin of the user and of `workspace`'s project when `None`, and
    /// replaces each whose commit moved. One plugin's failure is its
    /// outcome, not an error. Answered once every fetch finishes.
    ///
    /// # Errors
    ///
    /// For a named `plugin`: [`PluginError::NotFound`], or
    /// [`PluginError::Untrusted`] for a project plugin of a project this
    /// client doesn't trust. Else as [`PluginError::Daemon`] says, such as
    /// a plugin not installed from git.
    pub async fn update_plugins(
        &self,
        plugin: Option<PluginTarget>,
        workspace: Option<&Path>,
    ) -> Result<PluginsUpdateResponse, PluginError> {
        let params = PluginsUpdateParams::new(
            plugin,
            workspace.map(Path::to_path_buf),
            ClientEnvironment::clone(&self.environment),
        );
        Ok(self
            .client
            .request::<methods::PluginsUpdate>(&params)
            .await?)
    }

    /// Enables or disables `plugin`. Returns it as [`Self::plugins`] now
    /// lists it.
    ///
    /// # Errors
    ///
    /// [`PluginError::NotFound`] when it is gone; else as
    /// [`PluginError::Daemon`] says.
    pub async fn set_plugin_enabled(
        &self,
        plugin: PluginTarget,
        enablement: Enablement,
        workspace: Option<&Path>,
    ) -> Result<Plugin, PluginError> {
        let params = PluginsSetEnabledParams::new(
            plugin,
            enablement == Enablement::Enabled,
            workspace.map(Path::to_path_buf),
            ClientEnvironment::clone(&self.environment),
        );
        let set = self
            .client
            .request::<methods::PluginsSetEnabled>(&params)
            .await?;
        Ok(set.plugin)
    }

    /// Removes `plugin`'s package, or a linked one's link, and its record.
    /// The request is the confirmation: ask the user first. Its data folder
    /// stays. Returns where the package was.
    ///
    /// # Errors
    ///
    /// [`PluginError::NotFound`] when it is gone; else as
    /// [`PluginError::Daemon`] says.
    pub async fn remove_plugin(
        &self,
        plugin: PluginTarget,
        workspace: Option<&Path>,
    ) -> Result<PathBuf, PluginError> {
        let params = PluginsRemoveParams::new(plugin, workspace.map(Path::to_path_buf));
        let removed = self
            .client
            .request::<methods::PluginsRemove>(&params)
            .await?;
        Ok(removed.path)
    }
}
