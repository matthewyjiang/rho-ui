//! The machines rho-ui reaches: this one, whose daemon follows the rho home,
//! and the remote machines the settings list, each reached over SSH (from a
//! browser, by the server over SSH). Every
//! session, project and folder belongs to one machine, so the sidebar lists
//! the sessions of all of them side by side, each labelled with its
//! machine.

use std::{fmt, path::PathBuf};

use gpui_kit::SharedString;
#[cfg(target_family = "wasm")]
use rho_ui_core::ServerTarget;
#[cfg(not(target_family = "wasm"))]
use rho_ui_core::SshTarget;
use rho_ui_core::{DaemonTarget, RhoInstall, SessionId};

use crate::forge::CliPaths;

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
    /// Its `rho` program, to check for updates, install or update; `None`
    /// for a daemon reached at a socket alone, whose rho isn't rho-ui's.
    pub(crate) rho: Option<RhoInstall>,
    /// Where projects are cloned on it, as set in the settings: absolute,
    /// or from its home directory. Empty means `$RHO_HOME/projects`, as
    /// that machine sees it; see [`crate::cloning`].
    pub(crate) clone_folder: SharedString,
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
    /// See [`Machine::clone_folder`].
    pub(crate) clone_folder: String,
    /// Its forge CLIs' programs, as Settings → Source control sets them.
    pub(crate) cli: CliPaths,
}

/// The program a remote machine runs when none is set.
pub(crate) const DEFAULT_REMOTE_RHO: &str = "rho";

impl RemoteMachine {
    pub(crate) fn new(host: &str) -> Self {
        Self {
            host: host.to_owned(),
            label: String::new(),
            rho: DEFAULT_REMOTE_RHO.to_owned(),
            clone_folder: String::new(),
            cli: CliPaths::default(),
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

/// How rho-ui reaches a machine: its daemon, and its `rho` program apart
/// from the daemon.
#[derive(Clone, Debug)]
pub(crate) struct Reach {
    pub(crate) daemon: DaemonTarget,
    /// See [`Machine::rho`].
    pub(crate) rho: Option<RhoInstall>,
}

#[cfg(not(target_family = "wasm"))]
impl RemoteMachine {
    /// How rho-ui reaches it: over ssh.
    pub(crate) fn ssh_reach(&self) -> Reach {
        let target = SshTarget::new(self.host.clone()).with_rho(self.rho.clone());
        Reach {
            daemon: DaemonTarget::Ssh(target.clone()),
            rho: Some(RhoInstall::Ssh(target)),
        }
    }
}

#[cfg(target_family = "wasm")]
impl RemoteMachine {
    /// How a browser reaches it: through `server`, which reaches it over
    /// ssh as the desktop app on the server's machine would.
    pub(crate) fn server_reach(&self, server: &ServerTarget) -> Reach {
        let target = server
            .clone()
            .with_machine(self.host.clone())
            .with_rho(self.rho.clone());
        Reach {
            daemon: DaemonTarget::Server(target.clone()),
            rho: Some(RhoInstall::Server(target)),
        }
    }
}

/// How a machine's connection is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MachineStatus {
    Connecting,
    /// Connected to a daemon of this rho version, such as `Rho 3.0.0`.
    Ready {
        version: SharedString,
    },
    /// Couldn't connect, or the connection closed; why, in rho's or ssh's
    /// words.
    Failed(SharedString),
}

/// What rho-ui asks each daemon to list. Each can be refused on its own,
/// and each refusal shows until that listing succeeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Listing {
    /// The saved sessions.
    Sessions,
    /// The sessions with work under way, polled every second.
    Jobs,
}

impl Listing {
    /// What the sidebar says couldn't be done.
    pub(crate) fn failed(self) -> &'static str {
        match self {
            Listing::Sessions => "Couldn't list sessions",
            Listing::Jobs => "Couldn't read which sessions are working",
        }
    }

    /// The name in the element ID of its error.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Listing::Sessions => "sessions",
            Listing::Jobs => "jobs",
        }
    }
}

/// A machine, and what the views show about its daemon.
#[derive(Debug)]
struct Entry {
    machine: Machine,
    status: MachineStatus,
    /// Why its daemon refused to list its saved sessions.
    sessions_error: Option<SharedString>,
    /// Why its daemon refused to list its jobs.
    jobs_error: Option<SharedString>,
}

impl Entry {
    fn error_mut(&mut self, listing: Listing) -> &mut Option<SharedString> {
        match listing {
            Listing::Sessions => &mut self.sessions_error,
            Listing::Jobs => &mut self.jobs_error,
        }
    }
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
                        sessions_error: None,
                        jobs_error: None,
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

    /// Where projects are cloned on `id`; see [`Machine::clone_folder`].
    pub(crate) fn clone_folder(&self, id: &MachineId) -> SharedString {
        self.get(id)
            .map_or_default(|machine| machine.clone_folder.clone())
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

    /// Records how `id`'s connection is doing. A new connection, or none,
    /// starts its listings over, so their refusals go. Returns whether
    /// anything changed.
    pub(crate) fn set_status(&mut self, id: &MachineId, status: MachineStatus) -> bool {
        match self.entry_mut(id) {
            Some(entry) if entry.status != status => {
                entry.status = status;
                entry.sessions_error = None;
                entry.jobs_error = None;
                true
            }
            _ => false,
        }
    }

    /// Records why `id`'s daemon refused `listing`, or with `None` that it
    /// listed it. Returns whether that changed.
    pub(crate) fn set_error(
        &mut self,
        id: &MachineId,
        listing: Listing,
        error: Option<SharedString>,
    ) -> bool {
        let Some(slot) = self.entry_mut(id).map(|entry| entry.error_mut(listing)) else {
            return false;
        };
        if *slot == error {
            return false;
        }
        *slot = error;
        true
    }

    /// Each machine with something wrong: its ID, how its connection is
    /// doing, and each listing its daemon refused, with why.
    pub(crate) fn troubles(
        &self,
    ) -> impl Iterator<Item = (&MachineId, &MachineStatus, Vec<(Listing, &SharedString)>)> {
        self.list.iter().filter_map(|entry| {
            let refused: Vec<(Listing, &SharedString)> = [
                (Listing::Sessions, entry.sessions_error.as_ref()),
                (Listing::Jobs, entry.jobs_error.as_ref()),
            ]
            .into_iter()
            .filter_map(|(listing, error)| Some((listing, error?)))
            .collect();
            let ready = matches!(entry.status, MachineStatus::Ready { .. });
            (!ready || !refused.is_empty()).then_some((&entry.machine.id, &entry.status, refused))
        })
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

    /// The saved-session listing and the job poll each say why they were
    /// refused; one succeeding never clears or hides the other's reason,
    /// and a new connection starts both over.
    #[test]
    fn each_listing_keeps_its_own_refusal() {
        let remote = RemoteMachine::new("workstation");
        let id = remote.id();
        let mut machines = Machines::default();
        let reach = remote.ssh_reach();
        machines.set(vec![Machine {
            id: id.clone(),
            label: "workstation".into(),
            target: reach.daemon,
            rho: reach.rho,
            clone_folder: SharedString::default(),
        }]);
        let ready = || MachineStatus::Ready {
            version: "Rho 3".into(),
        };
        assert!(machines.set_status(&id, ready()));
        let refused = |machines: &Machines| -> Vec<(Listing, String)> {
            machines
                .troubles()
                .flat_map(|(_, _, refused)| refused)
                .map(|(listing, error)| (listing, error.to_string()))
                .collect()
        };
        assert!(refused(&machines).is_empty());

        assert!(machines.set_error(&id, Listing::Sessions, Some("no list".into())));
        assert!(machines.set_error(&id, Listing::Jobs, Some("no jobs".into())));
        assert!(!machines.set_error(&id, Listing::Jobs, Some("no jobs".into())));
        assert_eq!(
            refused(&machines),
            [
                (Listing::Sessions, "no list".to_owned()),
                (Listing::Jobs, "no jobs".to_owned())
            ]
        );
        // The job poll recovers; the saved list still fails.
        assert!(machines.set_error(&id, Listing::Jobs, None));
        assert_eq!(
            refused(&machines),
            [(Listing::Sessions, "no list".to_owned())]
        );

        assert!(machines.set_status(&id, MachineStatus::Connecting));
        assert!(
            refused(&machines).is_empty(),
            "a new connection starts over"
        );
    }

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
