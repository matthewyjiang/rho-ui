//! rho's own settings on each machine, its `config.toml`, as that machine's
//! daemon describes them: read once its connection is up, then kept current
//! by the daemon's changes, so Settings shows them at once and follows a
//! change made in rho's terminal UI or by hand. [`crate::settings_view`]
//! draws them; changes made there are written back through here, one write
//! at a time per machine.

use gpui_kit::{Context, SharedString, Task};
use rho_ui_core::{
    agent_settings::SettingChange, AgentSettings, AgentSettingsChanged, Daemon, DaemonError,
};

use crate::machines::MachineId;

/// What is known of one machine's rho settings.
pub(crate) enum Loaded {
    /// Being read.
    Reading,
    Ready(AgentSettings),
    /// Couldn't be read, in the daemon's words.
    Failed(SharedString),
}

/// One connected machine's settings, and the writes on their way to it.
struct MachineSettings {
    id: MachineId,
    daemon: Daemon,
    loaded: Loaded,
    /// Changes made while a write was in flight, sent together once it
    /// returns.
    queued: Vec<SettingChange>,
    writing: bool,
    /// Why the last change didn't land, until the next one.
    problem: Option<SharedString>,
    read_task: Task<()>,
    write_task: Task<()>,
}

/// Every connected machine's rho settings. A machine that isn't connected
/// has none: [`crate::machines::Machines`] says why.
#[derive(Default)]
pub(crate) struct RhoSettings {
    machines: Vec<MachineSettings>,
}

impl RhoSettings {
    fn entry(&self, machine: &MachineId) -> Option<&MachineSettings> {
        self.machines.iter().find(|entry| entry.id == *machine)
    }

    fn entry_mut(&mut self, machine: &MachineId) -> Option<&mut MachineSettings> {
        self.machines.iter_mut().find(|entry| entry.id == *machine)
    }

    /// `machine`'s settings, while it is connected.
    pub(crate) fn loaded(&self, machine: &MachineId) -> Option<&Loaded> {
        self.entry(machine).map(|entry| &entry.loaded)
    }

    /// Why the last change on `machine` didn't land.
    pub(crate) fn problem(&self, machine: &MachineId) -> Option<&SharedString> {
        self.entry(machine)?.problem.as_ref()
    }

    /// `machine` connected to `daemon`: reads its settings, which replace
    /// any read on an earlier connection.
    pub(crate) fn connected(
        &mut self,
        machine: &MachineId,
        daemon: Daemon,
        cx: &mut Context<'_, Self>,
    ) {
        self.forget(machine, cx);
        self.machines.push(MachineSettings {
            id: machine.clone(),
            daemon,
            loaded: Loaded::Reading,
            queued: Vec::new(),
            writing: false,
            problem: None,
            read_task: Task::ready(()),
            write_task: Task::ready(()),
        });
        self.read(machine, cx);
    }

    /// `machine`'s connection is gone, or the machine is: drops its
    /// settings and any write on its way.
    pub(crate) fn forget(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let before = self.machines.len();
        self.machines.retain(|entry| entry.id != *machine);
        if self.machines.len() != before {
            cx.notify();
        }
    }

    /// Reads `machine`'s settings afresh. What is shown stays until they
    /// arrive.
    fn read(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        let daemon = entry.daemon.clone();
        let id = machine.clone();
        entry.read_task = cx.spawn(async move |this, cx| {
            let read = daemon.agent_settings().await;
            let _ = this.update(cx, |this, cx| {
                if let Some(entry) = this.entry_mut(&id) {
                    entry.loaded = match read {
                        Ok(settings) => Loaded::Ready(settings),
                        Err(error) => Loaded::Failed(error.to_string().into()),
                    };
                    cx.notify();
                }
            });
        });
    }

    /// Takes in a change `machine`'s daemon sent.
    pub(crate) fn changed(
        &mut self,
        machine: &MachineId,
        changed: AgentSettingsChanged,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(MachineSettings {
            loaded: Loaded::Ready(settings),
            ..
        }) = self.entry_mut(machine)
        {
            settings.apply(changed);
            cx.notify();
        }
    }

    /// Makes `change` on `machine`: shows it at once, and writes it once
    /// the write before it, if any, returns.
    pub(crate) fn change(
        &mut self,
        machine: &MachineId,
        change: SettingChange,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        let Loaded::Ready(settings) = &mut entry.loaded else {
            return;
        };
        settings.change(&change);
        entry.problem = None;
        // A later change to a setting replaces one still waiting.
        entry.queued.retain(|queued| queued.key() != change.key());
        entry.queued.push(change);
        self.write_next(machine, cx);
        cx.notify();
    }

    /// Sends `machine`'s waiting changes, unless a write is in flight.
    fn write_next(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        let Loaded::Ready(settings) = &entry.loaded else {
            return;
        };
        if entry.writing || entry.queued.is_empty() {
            return;
        }
        // The revision read last, not the one this app's last write made:
        // the daemon takes it while only this app wrote since.
        let revision = settings.revision().clone();
        let changes = std::mem::take(&mut entry.queued);
        let daemon = entry.daemon.clone();
        let id = machine.clone();
        entry.writing = true;
        entry.write_task = cx.spawn(async move |this, cx| {
            let written = daemon.set_agent_settings(revision, changes).await;
            let _ = this.update(cx, |this, cx| this.written(&id, written, cx));
        });
    }

    /// A write to `machine` returned. A refused one is undone by reading
    /// the settings again, and the page says why.
    fn written(
        &mut self,
        machine: &MachineId,
        written: Result<(), DaemonError>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        entry.writing = false;
        match written {
            Ok(()) => self.write_next(machine, cx),
            Err(error) => {
                entry.queued.clear();
                entry.problem = Some(match error {
                    DaemonError::Conflict(_) => "rho's settings changed elsewhere, so your \
                        change didn't land. They show as they are now; make it again."
                        .into(),
                    other => format!("Your change didn't land: {other}").into(),
                });
                self.read(machine, cx);
                cx.notify();
            }
        }
    }
}
