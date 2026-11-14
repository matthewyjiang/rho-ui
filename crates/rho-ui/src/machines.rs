//! The machines rho-ui reaches: this one, whose daemon follows the rho home,
//! and the remote machines the settings list, each reached over SSH. Every
//! session, project and folder belongs to one machine, so the sidebar lists
//! the sessions of all of them side by side, each labelled with its
//! machine.

use std::{fmt, path::PathBuf};

use gpui_kit::SharedString;
use rho_ui_core::{DaemonTarget, SessionId, SshTarget};

/// Names a machine in the settings and the state database: [`LOCAL`] for
/// this one, else the SSH host a remote machine is reached at.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct MachineId(SharedString);

/// The ID of this machine. No remote machine may use it as its host.
pub(crate) const LOCAL: &str = "local";

impl MachineId {
    pub(crate) fn local() -> Self {
        Self(LOCAL.into())
    }

    /// The remote machine reached at SSH host `host`.
    pub(crate) fn remote(host: &str) -> Self {
        Self(host.to_owned().into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the machine rho-ui runs on, whose files it can read
    /// itself: to find a folder's git repository, or to offer the desktop's
    /// folder chooser.
    pub(crate) fn is_local(&self) -> bool {
        self.0 == LOCAL
    }
}

/// For IDs read back from the state database.
impl From<&str> for MachineId {
    fn from(id: &str) -> Self {
        Self(id.to_owned().into())
    }
}

impl fmt::Display for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A machine rho-ui connects to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Machine {
    pub(crate) id: MachineId,
    /// What the sidebar calls it, as set in the settings.
    pub(crate) label: SharedString,
    pub(crate) target: DaemonTarget,
}

/// A remote machine as the settings file lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteMachine {
    /// Any destination `ssh` accepts, such as a host in `~/.ssh/config`.
    /// Also the machine's ID, so changing it makes another machine.
    pub(crate) host: String,
    /// What the sidebar calls it; the host when empty.
    pub(crate) label: String,
    /// The `rho` program on the remote machine. A non-interactive SSH
    /// session may lack the `PATH` a terminal has, so this can be an
    /// absolute or `~/` path.
    pub(crate) rho: String,
}

/// The program a remote machine runs when none is set.
pub(crate) const DEFAULT_REMOTE_RHO: &str = "rho";

impl RemoteMachine {
    pub(crate) fn new(host: &str) -> Self {
        Self {
            host: host.to_owned(),
            label: String::new(),
            rho: DEFAULT_REMOTE_RHO.to_owned(),
        }
    }

    pub(crate) fn id(&self) -> MachineId {
        MachineId::remote(&self.host)
    }

    /// The label to show: the one set, else the host.
    pub(crate) fn shown_label(&self) -> &str {
        if self.label.is_empty() {
            &self.host
        } else {
            &self.label
        }
    }
}

/// Why a host can't name a remote machine: `ssh` would read it as
/// something else, or the settings file couldn't keep it.
pub(crate) fn host_problem(host: &str) -> Option<&'static str> {
    if host.is_empty() {
        Some("Enter the SSH host")
    } else if host == LOCAL {
        Some("\"local\" names this machine")
    } else if host.starts_with('-') || host.contains(|c: char| c.is_whitespace() || c == '=') {
        Some("An SSH host has no spaces or =, and doesn't start with -")
    } else {
        None
    }
}

impl RemoteMachine {
    /// How rho-ui reaches it: over ssh.
    pub(crate) fn ssh_target(&self) -> DaemonTarget {
        DaemonTarget::Ssh(SshTarget::new(self.host.clone()).with_rho(self.rho.clone()))
    }
}

/// How a machine's connection is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MachineStatus {
    Connecting,
    /// Connected to a daemon of this rho version, such as `rho 3.0.0`.
    Ready {
        version: SharedString,
    },
    /// Couldn't connect, or the connection closed; why, in rho's or ssh's
    /// words.
    Failed(SharedString),
}

/// A machine, and what the views show about its daemon.
#[derive(Debug)]
struct Entry {
    machine: Machine,
    status: MachineStatus,
    /// Why its saved sessions couldn't be listed.
    error: Option<SharedString>,
}

/// Every machine rho-ui connects to, this one first, with how each
/// connection is doing: the one record of them the views read.
#[derive(Debug, Default)]
pub(crate) struct Machines {
    list: Vec<Entry>,
}

impl Machines {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Machine> {
        self.list.iter().map(|entry| &entry.machine)
    }

    /// Follows the settings: `wanted` in their order. A machine kept keeps
    /// its status until its connection says otherwise.
    pub(crate) fn set(&mut self, wanted: Vec<Machine>) {
        let mut old = std::mem::take(&mut self.list);
        self.list = wanted
            .into_iter()
            .map(
                |machine| match old.iter().position(|entry| entry.machine.id == machine.id) {
                    Some(ix) => Entry {
                        machine,
                        ..old.swap_remove(ix)
                    },
                    None => Entry {
                        machine,
                        status: MachineStatus::Connecting,
                        error: None,
                    },
                },
            )
            .collect();
    }

    pub(crate) fn get(&self, id: &MachineId) -> Option<&Machine> {
        self.entry(id).map(|entry| &entry.machine)
    }

    fn entry(&self, id: &MachineId) -> Option<&Entry> {
        self.list.iter().find(|entry| entry.machine.id == *id)
    }

    fn entry_mut(&mut self, id: &MachineId) -> Option<&mut Entry> {
        self.list.iter_mut().find(|entry| entry.machine.id == *id)
    }

    /// What `id` is called; its ID once it is gone from the settings.
    pub(crate) fn label(&self, id: &MachineId) -> SharedString {
        self.get(id)
            .map_or_else(|| id.0.clone(), |machine| machine.label.clone())
    }

    /// Whether there is more than this machine, so labels tell machines
    /// apart.
    pub(crate) fn several(&self) -> bool {
        self.list.len() > 1
    }

    /// The label to show beside something on `id`: none with one machine,
    /// where it would tell nothing.
    pub(crate) fn tag(&self, id: &MachineId) -> Option<SharedString> {
        self.several().then(|| self.label(id))
    }

    /// The label of `id` when it is remote, whose agent can't read this
    /// machine's files; see [`crate::composer::ComposerState`].
    pub(crate) fn remote_label(&self, id: &MachineId) -> Option<SharedString> {
        (!id.is_local()).then(|| self.label(id))
    }

    pub(crate) fn status(&self, id: &MachineId) -> Option<&MachineStatus> {
        self.entry(id).map(|entry| &entry.status)
    }

    /// Records how `id`'s connection is doing. Returns whether that
    /// changed.
    pub(crate) fn set_status(&mut self, id: &MachineId, status: MachineStatus) -> bool {
        match self.entry_mut(id) {
            Some(entry) if entry.status != status => {
                entry.status = status;
                true
            }
            _ => false,
        }
    }

    /// Records why `id`'s saved sessions couldn't be listed, or with
    /// `None` that they were. Returns whether that changed.
    pub(crate) fn set_error(&mut self, id: &MachineId, error: Option<SharedString>) -> bool {
        match self.entry_mut(id) {
            Some(entry) if entry.error != error => {
                entry.error = error;
                true
            }
            _ => false,
        }
    }

    /// Each machine with something wrong: its ID, how its connection is
    /// doing, and why its sessions couldn't be listed.
    pub(crate) fn troubles(
        &self,
    ) -> impl Iterator<Item = (&MachineId, &MachineStatus, Option<&SharedString>)> {
        self.list
            .iter()
            .filter(|entry| {
                entry.error.is_some() || !matches!(entry.status, MachineStatus::Ready { .. })
            })
            .map(|entry| (&entry.machine.id, &entry.status, entry.error.as_ref()))
    }
}

/// A session on a machine. Session IDs are only unique within one daemon,
/// and one daemon can be reached as two machines (`local` and `ssh
/// localhost`), so the machine is part of every session's key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct SessionKey {
    pub(crate) machine: MachineId,
    pub(crate) id: SessionId,
}

impl SessionKey {
    pub(crate) fn new(machine: MachineId, id: SessionId) -> Self {
        Self { machine, id }
    }

    /// For tests: session `id` on this machine.
    #[cfg(test)]
    pub(crate) fn local(id: &str) -> Self {
        Self::new(MachineId::local(), SessionId::from(id))
    }
}

/// `machine-id`, as element IDs use it.
impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.machine, self.id)
    }
}

/// A folder on a machine.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Place {
    pub(crate) machine: MachineId,
    pub(crate) path: PathBuf,
}

impl Place {
    pub(crate) fn new(machine: MachineId, path: impl Into<PathBuf>) -> Self {
        Self {
            machine,
            path: path.into(),
        }
    }

    /// For tests: `path` on this machine.
    #[cfg(test)]
    pub(crate) fn local(path: impl Into<PathBuf>) -> Self {
        Self::new(MachineId::local(), path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_that_ssh_would_misread_are_refused() {
        assert_eq!(host_problem("workstation"), None);
        assert_eq!(host_problem("me@node0.example.edu"), None);
        assert!(host_problem("").is_some());
        assert!(host_problem("local").is_some(), "this machine's ID");
        assert!(host_problem("-oProxyCommand=x").is_some(), "an option");
        assert!(host_problem("two words").is_some());
        assert!(
            host_problem("a=b").is_some(),
            "the settings file splits at ="
        );
    }
}
