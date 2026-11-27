//! What each connected machine's rho configuration sets up for its
//! sessions, for Settings → MCP & hooks and Plugins: the MCP servers of
//! its `config.toml` and plugins, its hooks files with the hooks they
//! define, and its agent plugins, as a session in one workspace would see
//! them. All three are read at once, since a plugin brings MCP servers.
//!
//! Nothing pushes changes to them. The pages read them when Settings
//! opens, when another machine or project is picked, on Refresh, and after
//! each change to a plugin, which it also sends. The latest read wins: an
//! earlier one still on its way, for another project, answers for nothing.
//!
//! A plugin's change stays busy until a read sent after the daemon
//! answered it answers in turn, so its row never shows the plugin as it
//! was; then what it said, a failure or an update's outcome, stays until
//! the plugin's next change.

use std::path::PathBuf;

use gpui_kit::{Context, SharedString, Task};
use rho_ui_core::{
    Daemon, DaemonError, Enablement, HooksListResponse, IfExists, McpListResponse, Plugin,
    PluginError, PluginInstallSource, PluginScope, PluginTarget, PluginUpdateOutcome,
    PluginsUpdateResponse,
};

use crate::machines::MachineId;

/// What one listing said.
pub(crate) enum Answer<T> {
    Listed(T),
    /// The daemon is older than the listing.
    TooOld,
    /// The daemon's reason, as a `config.toml` that doesn't load.
    Failed(SharedString),
}

impl<T> From<Result<Option<T>, DaemonError>> for Answer<T> {
    fn from(answer: Result<Option<T>, DaemonError>) -> Self {
        match answer {
            Ok(Some(listed)) => Self::Listed(listed),
            Ok(None) => Self::TooOld,
            Err(error) => Self::Failed(error.to_string().into()),
        }
    }
}

/// One read of a machine's MCP servers, hooks and plugins.
pub(crate) struct Reading {
    /// Its number, among every read's.
    ask: u64,
    /// The project directory they were read for, if any.
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) mcp: Answer<McpListResponse>,
    pub(crate) hooks: Answer<HooksListResponse>,
    pub(crate) plugins: Answer<Vec<Plugin>>,
}

/// One connected machine's.
struct MachineConfig {
    id: MachineId,
    daemon: Daemon,
    /// The latest read that answered.
    read: Option<Reading>,
    /// The read on its way, by its number, and the workspace it asks
    /// about. What was read stays until it answers.
    asking: Option<(u64, Option<PathBuf>)>,
    /// The latest change to each plugin, by the workspace it was asked for.
    changes: Vec<PluginChange>,
    /// The latest Update all, by the workspace it was asked for.
    update_all: Option<UpdateAll>,
}

/// A change the Plugins page asks of a daemon for one plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Enable,
    Disable,
    /// Fetch its git source again.
    Update,
    Remove,
}

/// Where a plugin's latest change stands, for its row.
#[derive(Clone, Debug)]
pub(crate) enum ChangeState {
    /// On its way.
    Busy(Change),
    /// It failed, and why.
    Failed(Change, SharedString),
    /// What an update did.
    Updated(SharedString),
}

/// Where Update all stands, for the page.
#[derive(Clone, Debug)]
pub(crate) enum UpdateAllState {
    Running,
    /// Done; what it says, such as why it failed or which plugins it
    /// skipped.
    Said(SharedString),
}

/// What a change, or Update all, came to.
#[derive(Clone, Debug)]
enum Stage<T> {
    /// The daemon hasn't answered yet.
    Asked,
    /// It answered; read number `read` shows its effect. What it said
    /// shows once that read is in.
    Answered { read: u64, said: Option<T> },
}

impl<T: Clone> Stage<T> {
    /// What it said, once the read after it answered; `Err` while busy.
    fn settled(&self, reading: Option<&Reading>) -> Result<Option<T>, ()> {
        match self {
            Self::Answered { read, said } if reading.is_some_and(|it| it.ask >= *read) => {
                Ok(said.clone())
            }
            _ => Err(()),
        }
    }
}

/// One plugin's latest change.
struct PluginChange {
    /// Tells it from a later change of the same plugin.
    id: u64,
    workspace: Option<PathBuf>,
    target: PluginTarget,
    change: Change,
    stage: Stage<ChangeState>,
}

struct UpdateAll {
    id: u64,
    workspace: Option<PathBuf>,
    stage: Stage<SharedString>,
}

/// Every connected machine's MCP servers and hooks. A machine that isn't
/// connected has none: [`crate::machines::Machines`] says why.
#[derive(Default)]
pub(crate) struct Configured {
    machines: Vec<MachineConfig>,
    /// Numbers each read, so only the latest one answers: a read sent on
    /// an earlier connection, or for another project, answers for nothing.
    asks: u64,
}

impl Configured {
    fn entry(&self, machine: &MachineId) -> Option<&MachineConfig> {
        self.machines.iter().find(|entry| entry.id == *machine)
    }

    fn entry_mut(&mut self, machine: &MachineId) -> Option<&mut MachineConfig> {
        self.machines.iter_mut().find(|entry| entry.id == *machine)
    }

    /// `machine`'s latest read, while it is connected.
    pub(crate) fn reading(&self, machine: &MachineId) -> Option<&Reading> {
        self.entry(machine)?.read.as_ref()
    }

    /// Whether a read of `machine` is on its way.
    pub(crate) fn asking(&self, machine: &MachineId) -> bool {
        self.entry(machine)
            .is_some_and(|entry| entry.asking.is_some())
    }

    /// `machine` connected to `daemon`, replacing what an earlier
    /// connection read. Nothing is read until the page asks.
    pub(crate) fn connected(
        &mut self,
        machine: &MachineId,
        daemon: Daemon,
        cx: &mut Context<'_, Self>,
    ) {
        self.forget(machine, cx);
        self.machines.push(MachineConfig {
            id: machine.clone(),
            daemon,
            read: None,
            asking: None,
            changes: Vec::new(),
            update_all: None,
        });
        cx.notify();
    }

    /// `machine`'s connection is gone, or the machine is.
    pub(crate) fn forget(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let before = self.machines.len();
        self.machines.retain(|entry| entry.id != *machine);
        if self.machines.len() != before {
            cx.notify();
        }
    }

    /// Reads `machine`'s for `workspace`, unless they were read for it,
    /// or a read for it is on its way.
    pub(crate) fn ensure_read(
        &mut self,
        machine: &MachineId,
        workspace: Option<PathBuf>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(entry) = self.entry(machine) else {
            return;
        };
        let asked = match &entry.asking {
            Some((_, asked)) => Some(asked),
            None => entry.read.as_ref().map(|read| &read.workspace),
        };
        if asked != Some(&workspace) {
            self.read(machine, workspace, cx);
        }
    }

    /// Reads `machine`'s for `workspace` afresh, in place of any read on
    /// its way.
    pub(crate) fn read(
        &mut self,
        machine: &MachineId,
        workspace: Option<PathBuf>,
        cx: &mut Context<'_, Self>,
    ) {
        self.asks += 1;
        let ask = self.asks;
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        entry.asking = Some((ask, workspace.clone()));
        let daemon = entry.daemon.clone();
        let id = machine.clone();
        cx.spawn(async move |this, cx| {
            let place = workspace.as_deref();
            let (mcp, hooks, plugins) = futures::future::join3(
                daemon.configured_mcp_servers(place),
                daemon.configured_hooks(place),
                daemon.plugins(place),
            )
            .await;
            let _ = this.update(cx, |this, cx| {
                let Some(entry) = this.entry_mut(&id).filter(|entry| {
                    entry
                        .asking
                        .as_ref()
                        .is_some_and(|(asked, _)| *asked == ask)
                }) else {
                    return;
                };
                entry.asking = None;
                entry.read = Some(Reading {
                    ask,
                    workspace,
                    mcp: mcp.into(),
                    hooks: hooks.into(),
                    plugins: plugins.into(),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Reads `machine`'s again, for the project it shows now, which may not
    /// be `changed`, the one a change was asked for. Returns the read's
    /// number, while the machine is connected.
    fn read_again(
        &mut self,
        machine: &MachineId,
        changed: Option<PathBuf>,
        cx: &mut Context<'_, Self>,
    ) -> Option<u64> {
        let entry = self.entry(machine)?;
        let workspace = match (&entry.asking, &entry.read) {
            (Some((_, asked)), _) => asked.clone(),
            (None, Some(read)) => read.workspace.clone(),
            (None, None) => changed,
        };
        self.read(machine, workspace, cx);
        Some(self.asks)
    }

    /// Where the latest change to `target` in `workspace` stands, if any.
    pub(crate) fn change_state(
        &self,
        machine: &MachineId,
        workspace: Option<&PathBuf>,
        target: &PluginTarget,
    ) -> Option<ChangeState> {
        let entry = self.entry(machine)?;
        let latest = entry
            .changes
            .iter()
            .find(|change| change.workspace.as_ref() == workspace && change.target == *target)?;
        latest
            .stage
            .settled(entry.read.as_ref())
            .unwrap_or(Some(ChangeState::Busy(latest.change)))
    }

    /// Where the latest Update all of `machine` in `workspace` stands, if
    /// any.
    pub(crate) fn update_all_state(
        &self,
        machine: &MachineId,
        workspace: Option<&PathBuf>,
    ) -> Option<UpdateAllState> {
        let entry = self.entry(machine)?;
        let all = entry
            .update_all
            .as_ref()
            .filter(|all| all.workspace.as_ref() == workspace)?;
        match all.stage.settled(entry.read.as_ref()) {
            Ok(said) => said.map(UpdateAllState::Said),
            Err(()) => Some(UpdateAllState::Running),
        }
    }

    /// Asks `machine`'s daemon for `change` to `target`, in place of what
    /// its last change said; then reads it all again.
    pub(crate) fn change_plugin(
        &mut self,
        machine: &MachineId,
        workspace: Option<PathBuf>,
        target: PluginTarget,
        change: Change,
        cx: &mut Context<'_, Self>,
    ) {
        self.asks += 1;
        let id = self.asks;
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        let daemon = entry.daemon.clone();
        entry
            .changes
            .retain(|it| !(it.workspace == workspace && it.target == target));
        entry.changes.push(PluginChange {
            id,
            workspace: workspace.clone(),
            target: target.clone(),
            change,
            stage: Stage::Asked,
        });
        cx.notify();
        let machine = machine.clone();
        cx.spawn(async move |this, cx| {
            let place = workspace.as_deref();
            let asked = target.clone();
            let said: Result<Option<ChangeState>, PluginError> = match change {
                Change::Enable | Change::Disable => {
                    let enablement = if change == Change::Enable {
                        Enablement::Enabled
                    } else {
                        Enablement::Disabled
                    };
                    daemon
                        .set_plugin_enabled(asked, enablement, place)
                        .await
                        .map(|_| None)
                }
                Change::Remove => daemon.remove_plugin(asked, place).await.map(|_| None),
                Change::Update => daemon
                    .update_plugins(Some(asked), place)
                    .await
                    .map(|answer| update_state(&answer, &target)),
            };
            let said = said.unwrap_or_else(|error| {
                Some(ChangeState::Failed(change, error.to_string().into()))
            });
            let _ = this.update(cx, |this, cx| {
                let Some(read) = this.read_again(&machine, workspace, cx) else {
                    return;
                };
                let entry = this.entry_mut(&machine);
                if let Some(latest) =
                    entry.and_then(|entry| entry.changes.iter_mut().find(|change| change.id == id))
                {
                    latest.stage = Stage::Answered { read, said };
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetches every git install of `machine` again, the user's and
    /// `workspace`'s project's; each of `git` is busy meanwhile, but for
    /// one whose own change is still on its way.
    pub(crate) fn update_all_plugins(
        &mut self,
        machine: &MachineId,
        workspace: Option<PathBuf>,
        git: &[PluginTarget],
        cx: &mut Context<'_, Self>,
    ) {
        self.asks += 1;
        let id = self.asks;
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        let daemon = entry.daemon.clone();
        // A change still on its way keeps its row: its answer says more
        // than the update's.
        let running: Vec<PluginTarget> = entry
            .changes
            .iter()
            .filter(|it| it.workspace == workspace && matches!(it.stage, Stage::Asked))
            .map(|it| it.target.clone())
            .collect();
        let git: Vec<&PluginTarget> = git.iter().filter(|it| !running.contains(it)).collect();
        entry
            .changes
            .retain(|it| !(it.workspace == workspace && git.contains(&&it.target)));
        entry
            .changes
            .extend(git.into_iter().map(|target| PluginChange {
                id,
                workspace: workspace.clone(),
                target: target.clone(),
                change: Change::Update,
                stage: Stage::Asked,
            }));
        entry.update_all = Some(UpdateAll {
            id,
            workspace: workspace.clone(),
            stage: Stage::Asked,
        });
        cx.notify();
        let machine = machine.clone();
        cx.spawn(async move |this, cx| {
            let answer = daemon.update_plugins(None, workspace.as_deref()).await;
            let _ = this.update(cx, |this, cx| {
                let Some(read) = this.read_again(&machine, workspace, cx) else {
                    return;
                };
                let Some(entry) = this.entry_mut(&machine) else {
                    return;
                };
                for change in entry.changes.iter_mut().filter(|change| change.id == id) {
                    let said = match &answer {
                        Ok(answer) => update_state(answer, &change.target),
                        Err(_) => None,
                    };
                    change.stage = Stage::Answered { read, said };
                }
                if let Some(all) = entry.update_all.as_mut().filter(|all| all.id == id) {
                    let said = match &answer {
                        Ok(answer) if answer.untrusted.is_empty() => None,
                        Ok(answer) => Some(format!(
                            "Skipped {}: the project isn't trusted.",
                            answer.untrusted.join(", ")
                        )),
                        Err(error) => Some(format!("Update all failed: {error}")),
                    };
                    all.stage = Stage::Answered {
                        read,
                        said: said.map(SharedString::from),
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Installs a plugin from `source` on `machine`, then reads it all
    /// again; `None` while it isn't connected.
    pub(crate) fn install_plugin(
        &mut self,
        machine: &MachineId,
        workspace: Option<PathBuf>,
        (source, scope): (PluginInstallSource, PluginScope),
        if_exists: IfExists,
        cx: &mut Context<'_, Self>,
    ) -> Option<Task<Result<Plugin, PluginError>>> {
        let daemon = self.entry(machine)?.daemon.clone();
        let machine = machine.clone();
        Some(cx.spawn(async move |this, cx| {
            let installed = daemon
                .install_plugin(source, scope, if_exists, workspace.as_deref())
                .await;
            let _ = this.update(cx, |this, cx| this.read_again(&machine, workspace, cx));
            installed
        }))
    }
}

/// What an update said of `target`, if it fetched it.
fn update_state(answer: &PluginsUpdateResponse, target: &PluginTarget) -> Option<ChangeState> {
    let update = answer
        .results
        .iter()
        .find(|update| update.plugin == *target)?;
    match &update.outcome {
        PluginUpdateOutcome::Updated { previous, current } => Some(ChangeState::Updated(
            format!("Updated from {} to {}.", short(previous), short(current)).into(),
        )),
        PluginUpdateOutcome::UpToDate { commit } => Some(ChangeState::Updated(
            format!("Up to date at {}.", short(commit)).into(),
        )),
        PluginUpdateOutcome::Failed { message, .. } => {
            Some(ChangeState::Failed(Change::Update, message.clone().into()))
        }
        _ => None,
    }
}

/// A commit as git abbreviates it.
pub(crate) fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}
