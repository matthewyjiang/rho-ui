//! What the user asks of a machine's daemon ([`DaemonAction`]): connecting
//! again, and restarting or stopping it once they confirm what that cuts
//! short. A daemon that answers is restarted and stopped through rho's
//! protocol. One that refused rho-ui for its older protocol takes no
//! request, so `rho daemon restart` restarts it, as rho says to.

use gpui_kit::component::{
    button::ButtonVariant, notification::Notification, ActiveTheme as _, Icon, WindowExt as _,
};
use std::{pin::pin, time::Duration};

use futures::future::{self, Either};
use gpui_kit::{assets::IconName as Lucide, Context, Styled as _, Window};
use rho_ui_core::{Daemon, DaemonError, RestartCost, RhoInstall};

use super::RhoApp;
use crate::machines::{DaemonAction, MachineId, MachineStatus};

/// How long Restart and Stop wait for the daemon's status, to say what
/// they would cut short, before they ask without it. `rho daemon status
/// --json` against rho 3.3.0 here took 11 to 30 ms, process start included
/// (n = 10); rho reads its credential store for it, which may wait on
/// D-Bus. A tripwire for a wedged daemon or keyring, which is when Stop
/// matters most: the dialog then says "whatever is in progress there".
const STATUS_DEADLINE: Duration = Duration::from_secs(5);

/// What a confirmed [`DaemonAction`] does to a daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Restart,
    Stop,
}

/// What restarting or stopping a daemon would cut short, as far as rho-ui
/// knows: `None` where nothing said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CutShort {
    /// Turns under way or waiting for a person, logins, and sessions with
    /// background work.
    in_progress: Option<u64>,
    /// Other clients connected, which only a refusal or a build skew
    /// counts: `rho/daemon/status` doesn't.
    clients: Option<u64>,
}

impl CutShort {
    /// What a daemon said a restart would cost, if it said.
    fn of(cost: Option<RestartCost>) -> Self {
        Self {
            in_progress: cost.map(|cost| cost.in_progress),
            clients: cost.map(|cost| cost.clients),
        }
    }

    /// In a sentence, for the dialog that asks first.
    fn sentence(self) -> String {
        let work = match self.in_progress {
            None => Some("whatever is in progress there".to_owned()),
            Some(0) => None,
            Some(1) => Some("1 task in progress".to_owned()),
            Some(n) => Some(format!("{n} tasks in progress")),
        };
        let clients = match self.clients {
            None | Some(0) => None,
            Some(1) => Some("1 other client".to_owned()),
            Some(n) => Some(format!("{n} other clients")),
        };
        match (work, clients) {
            (None, None) => "Nothing is in progress there.".to_owned(),
            (Some(work), None) => format!("It cuts short {work}."),
            (None, Some(clients)) => {
                format!("Nothing is in progress there, but it disconnects {clients}.")
            }
            (Some(work), Some(clients)) => {
                format!("It cuts short {work} and disconnects {clients}.")
            }
        }
    }
}

impl RhoApp {
    /// Does `action`, asking first for a restart or a stop.
    pub(super) fn daemon_action(
        &mut self,
        action: &DaemonAction,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match action {
            DaemonAction::Connect(machine) => self.connect(machine, window, cx),
            DaemonAction::Restart(machine) => self.confirm(machine, Control::Restart, window, cx),
            DaemonAction::Stop(machine) => self.confirm(machine, Control::Stop, window, cx),
        }
    }

    /// Asks the user to confirm `control` of `machine`'s daemon, saying
    /// what it would cut short: for a connected daemon, as its status says
    /// now; for one that refused rho-ui, as its refusal said. Which of the
    /// two it is, and so how OK acts, is decided here once.
    fn confirm(
        &mut self,
        machine: &MachineId,
        control: Control,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(daemon) = self.daemon(machine).cloned() {
            if std::mem::replace(&mut self.asking, true) {
                return;
            }
            let machine = machine.clone();
            cx.spawn_in(window, async move |this, cx| {
                let deadline = cx.background_executor().timer(STATUS_DEADLINE);
                let in_progress = match future::select(pin!(daemon.status()), deadline).await {
                    Either::Left((Ok(status), _)) => Some(status.in_progress),
                    Either::Left((Err(_), _)) | Either::Right(_) => None,
                };
                let cut = CutShort {
                    in_progress,
                    clients: None,
                };
                let _ = this.update_in(cx, |this, window, cx| {
                    this.asking = false;
                    let target = Target::Connected(daemon);
                    this.ask_first(&machine, control, target, cut, window, cx);
                });
            })
            .detach();
            return;
        }
        let machines = self.machines.read(cx);
        let (Control::Restart, Some(MachineStatus::OlderDaemon { cost }), Some(rho)) = (
            control,
            machines.status(machine),
            machines.get(machine).and_then(|known| known.rho.clone()),
        ) else {
            return;
        };
        let cut = CutShort::of(*cost);
        self.ask_first(machine, control, Target::Older(rho), cut, window, cx);
    }

    /// Opens the dialog that asks to confirm `control` of `machine`'s
    /// daemon, `target`, which would cut `cut` short; OK does it.
    fn ask_first(
        &self,
        machine: &MachineId,
        control: Control,
        target: Target,
        cut: CutShort,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let on = self
            .machines
            .read(cx)
            .tag(machine)
            .map_or_default(|label| format!(" on {label}"));
        let (title, ok, icon) = match control {
            Control::Restart => (format!("Restart Rho{on}?"), "Restart", Lucide::RotateCw),
            Control::Stop => (format!("Stop Rho{on}?"), "Stop", Lucide::Square),
        };
        let description = description(control, cut);
        let (view, machine) = (cx.entity().downgrade(), machine.clone());
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (view, machine, target) = (view.clone(), machine.clone(), target.clone());
            alert
                .icon(Icon::new(icon).text_color(cx.theme().danger))
                .title(title.clone())
                .description(description.clone())
                .confirm()
                .ok_text(ok)
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let target = target.clone();
                    let _ = view.update(cx, |this, cx| {
                        this.confirmed(&machine, control, target, window, cx);
                    });
                    true
                })
        });
    }

    /// The user confirmed `control` of `machine`'s daemon, `target`.
    fn confirmed(
        &mut self,
        machine: &MachineId,
        control: Control,
        target: Target,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match (control, target) {
            (Control::Restart, Target::Connected(daemon)) => {
                self.expect_restart(machine);
                self.restart_daemon(machine, daemon, window, cx);
            }
            (Control::Stop, Target::Connected(daemon)) => {
                self.stop_daemon(machine, daemon, window, cx);
            }
            (Control::Restart, Target::Older(rho)) => self.restart_older(machine, rho, window, cx),
            // `confirm` asks to stop only a daemon that answers.
            (Control::Stop, Target::Older(_)) => {}
        }
    }

    /// Restarts `machine`'s connected daemon in place now, into the rho
    /// installed. It detaches every session as restarting, so the
    /// connection comes back on its own (see [`Self::disconnected`]).
    fn restart_daemon(
        &self,
        machine: &MachineId,
        daemon: Daemon,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let label = self.machines.read(cx).label(machine);
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = daemon.restart().await {
                let _ = cx.update(|window, cx| {
                    let note = Notification::error(error.to_string())
                        .title(format!("Couldn't restart Rho on {label}"));
                    window.push_notification(note, cx);
                });
            }
        })
        .detach();
    }

    /// Restarts `machine`'s daemon, which refused rho-ui for its older
    /// protocol, with `rho daemon restart` through `rho`, then connects
    /// again. A restart that fails says why, and the connection then says
    /// what it finds: if the daemon still refuses rho-ui, the Rho
    /// installed there is older too ([`MachineStatus::OlderRho`]). Nothing
    /// else connects meanwhile: a newer connection drops it.
    fn restart_older(
        &mut self,
        machine: &MachineId,
        rho: RhoInstall,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let label = self.machines.read(cx).label(machine);
        self.next_epoch += 1;
        let epoch = self.next_epoch;
        let Some(connection) = self.connection_mut(machine) else {
            return;
        };
        connection.epoch = epoch;
        let id = machine.clone();
        connection.task = cx.spawn_in(window, async move |this, cx| {
            let restarted = rho.restart_daemon().await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.is_current(&id, epoch) {
                    return;
                }
                match restarted {
                    Ok(()) => {
                        if let Some(connection) = this.connection_mut(&id) {
                            connection.restarted_older = true;
                        }
                    }
                    Err(error) => {
                        let note = Notification::error(error.to_string())
                            .title(format!("Couldn't restart Rho on {label}"));
                        window.push_notification(note, cx);
                    }
                }
                this.connect(&id, window, cx);
            });
        });
        self.set_status(machine, MachineStatus::Connecting, cx);
    }

    /// Stops `machine`'s daemon. The connection's end, which follows, shows
    /// the machine stopped, and nothing connects to it again until the
    /// user asks ([`DaemonAction::Connect`]).
    fn stop_daemon(
        &mut self,
        machine: &MachineId,
        daemon: Daemon,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let label = self.machines.read(cx).label(machine);
        let Some(connection) = self.connection_mut(machine) else {
            return;
        };
        connection.stopped = true;
        // Put back if the daemon refuses to stop.
        let restarting = std::mem::take(&mut connection.restarting);
        let id = machine.clone();
        cx.spawn_in(window, async move |this, cx| {
            match daemon.stop().await {
                // The daemon closed the connection as it stopped.
                Ok(()) | Err(DaemonError::Closed) => {}
                Err(error) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        if let Some(connection) = this.connection_mut(&id) {
                            connection.stopped = false;
                            connection.restarting = restarting;
                        }
                        let note = Notification::error(error.to_string())
                            .title(format!("Couldn't stop Rho on {label}"));
                        window.push_notification(note, cx);
                    });
                }
            }
        })
        .detach();
    }
}

/// The daemon a confirmed [`Control`] acts on.
#[derive(Clone)]
enum Target {
    /// Connected: restarted and stopped through rho's protocol.
    Connected(Daemon),
    /// Refused rho-ui for its older protocol: restarted with `rho daemon
    /// restart` through the machine's `rho`.
    Older(RhoInstall),
}

/// What the dialog that asks to confirm `control` says, given that it
/// would cut `cut` short.
fn description(control: Control, cut: CutShort) -> String {
    let cuts = cut.in_progress != Some(0);
    let then = match (control, cuts) {
        (Control::Restart, true) => {
            " Running turns carry on after the restart in a Rho that resumes them; background \
             processes stop."
        }
        (Control::Restart, false) => "",
        (Control::Stop, true) => {
            " Background processes stop too, and rho-ui doesn't start it again until you do."
        }
        (Control::Stop, false) => " rho-ui doesn't start it again until you do.",
    };
    format!("{}{then}", cut.sentence())
}
