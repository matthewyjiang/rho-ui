//! What each connected machine's rho configuration sets up for its
//! sessions, for Settings → MCP & hooks: the MCP servers of its
//! `config.toml` and plugins, and its hooks files with the hooks they
//! define, as a session in one workspace would see them.
//!
//! Nothing pushes changes to them. The page reads them when Settings
//! opens, when another machine or project is picked, and on Refresh. The
//! latest read wins: an earlier one still on its way, for another project,
//! answers for nothing.

use std::path::PathBuf;

use gpui_kit::{Context, SharedString};
use rho_ui_core::{Daemon, DaemonError, HooksListResponse, McpListResponse};

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

/// One read of a machine's MCP servers and hooks.
pub(crate) struct Reading {
    /// The project directory they were read for, if any.
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) mcp: Answer<McpListResponse>,
    pub(crate) hooks: Answer<HooksListResponse>,
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
            let (mcp, hooks) = futures::future::join(
                daemon.configured_mcp_servers(place),
                daemon.configured_hooks(place),
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
                    workspace,
                    mcp: mcp.into(),
                    hooks: hooks.into(),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
