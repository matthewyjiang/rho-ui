//! Installing and updating rho on each machine: whether rho is there and a
//! newer release is out ([`rho_ui_core::RhoInstall::check`]), installing or
//! updating it, and the upgrade its daemon waits to run. [`Installs`] keeps
//! that for every machine; [`Installs::offer`] says what the views offer:
//! the setup screen, the sidebar's footer, and Settings › About.

use std::{collections::BTreeMap, time::Duration};

use web_time::Instant;

use gpui_kit::component::{
    alert::Alert,
    button::{Button, ButtonVariants as _},
    clipboard::Clipboard,
    h_flex,
    spinner::Spinner,
    v_flex, ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::{
    div, AnyElement, App, Context, Div, Entity, EventEmitter, IntoElement, ParentElement as _,
    SharedString, Styled as _, Task,
};
use rho_ui_core::{platform, DaemonError, PendingUpgrade, RhoCheck, RhoInstall, Update};

use crate::{
    machines::{MachineId, MachineStatus, Machines},
    workspaces::tilde,
};

/// How long a check that found rho stays fresh. Each check asks GitHub's
/// API once, which allows 60 unauthenticated requests an hour from one
/// address, shared with rho's terminal UI, which asks at every start; one
/// an hour per machine leaves it the rest.
const CHECK_EVERY: Duration = Duration::from_hours(1);

/// What rho-ui changes about a machine's rho.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Install,
    Update,
}

/// What is known of one machine's rho.
#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    /// A check is under way.
    Checking,
    /// What the last check found, and when.
    Checked { check: RhoCheck, at: Instant },
    /// The check failed, as when ssh can't reach the machine, which its
    /// connection says too; why, in the script's or ssh's words.
    CheckFailed(SharedString),
    /// `Change` is under way.
    Changing(Change),
    /// `change` failed; why, in rho's, the script's or ssh's words.
    Failed { change: Change, why: SharedString },
}

/// What the views offer to do about a machine's rho.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Offer {
    /// rho isn't there: install it.
    Install,
    /// `Change` is under way.
    Changing(Change),
    /// Installing or updating failed: try again.
    Failed { change: Change, why: SharedString },
    /// Rho `latest` is out: update.
    Update { latest: SharedString },
    /// A newer rho is out, and `command` installs it in a terminal.
    InTerminal {
        latest: SharedString,
        command: SharedString,
    },
    /// The daemon runs once `waiting_for` things end, or now on request.
    Restart {
        version: SharedString,
        waiting_for: u64,
    },
}

impl Offer {
    /// Whether it means the machine has no rho to connect to, so its
    /// connection's failure says nothing more.
    pub(crate) fn replaces_connection_error(&self) -> bool {
        matches!(
            self,
            Self::Install
                | Self::Changing(Change::Install)
                | Self::Failed {
                    change: Change::Install,
                    ..
                }
        )
    }
}

/// Asked of [`crate::app::RhoApp`], which holds each machine's connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InstallsEvent {
    /// rho was installed on the machine: connect to it again.
    Installed(MachineId),
    /// rho was updated under the machine's daemon, which may now restart
    /// in place, or stop for the next client to start the new rho.
    Updated(MachineId),
    /// The user asked the machine's daemon to restart now.
    Restart(MachineId),
}

/// Every machine's rho, apart from its daemon.
pub(crate) struct Installs {
    machines: Entity<Machines>,
    states: BTreeMap<MachineId, State>,
    /// The upgrade each connected daemon waits to run.
    upgrades: BTreeMap<MachineId, PendingUpgrade>,
    /// Each machine's check or change under way; replacing one drops it.
    tasks: BTreeMap<MachineId, Task<()>>,
}

impl EventEmitter<InstallsEvent> for Installs {}

impl Installs {
    pub(crate) fn new(machines: Entity<Machines>) -> Self {
        Self {
            machines,
            states: BTreeMap::new(),
            upgrades: BTreeMap::new(),
            tasks: BTreeMap::new(),
        }
    }

    /// What to offer for `machine`'s rho, if anything. A change under way
    /// or failed comes first, then an upgrade the daemon waits to run,
    /// which an update offer would only repeat. A machine whose daemon
    /// answers has rho, wherever the check looked, so it is never offered
    /// an install.
    pub(crate) fn offer(&self, machine: &MachineId, cx: &App) -> Option<Offer> {
        offer(
            self.states.get(machine),
            self.upgrades.get(machine),
            self.machines.read(cx).status(machine),
        )
    }

    /// Checks `machine`'s rho unless a check or change is under way, or a
    /// check that found rho is fresh (see [`CHECK_EVERY`]).
    pub(crate) fn check(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let fresh = match self.states.get(machine) {
            Some(State::Checking | State::Changing(_)) => true,
            Some(State::Checked {
                check: RhoCheck::Installed { .. },
                at,
            }) => at.elapsed() < CHECK_EVERY,
            _ => false,
        };
        if !fresh {
            self.run_check(machine, cx);
        }
    }

    /// Installs rho on `machine`, then connects to it again.
    pub(crate) fn install(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        self.change(machine, Change::Install, cx);
    }

    /// Updates `machine`'s rho; its daemon upgrades in place once nothing
    /// would be cut short.
    pub(crate) fn update(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        self.change(machine, Change::Update, cx);
    }

    /// Asks [`crate::app::RhoApp`] to restart `machine`'s daemon now.
    #[expect(
        clippy::unused_self,
        reason = "a button takes it as it takes `install` and `update`"
    )]
    pub(crate) fn restart(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        cx.emit(InstallsEvent::Restart(machine.clone()));
    }

    /// Records the upgrade `machine`'s daemon waits to run, from its
    /// status; `None` for none, or once it isn't connected.
    pub(crate) fn set_upgrade(
        &mut self,
        machine: &MachineId,
        upgrade: Option<PendingUpgrade>,
        cx: &mut Context<'_, Self>,
    ) {
        let changed = match upgrade {
            Some(upgrade) => {
                self.upgrades.insert(machine.clone(), upgrade.clone()) != Some(upgrade)
            }
            None => self.upgrades.remove(machine).is_some(),
        };
        if changed {
            cx.notify();
        }
    }

    /// Where installing rho on this machine puts it, to show: the folder
    /// of its program when `--rho` gave a path, else the installer's.
    fn local_install_dir(&self, cx: &App) -> String {
        match self.rho(&MachineId::local(), cx) {
            #[cfg(not(target_family = "wasm"))]
            Some(RhoInstall::Here { rho }) if rho.components().count() > 1 => {
                rho.parent().map_or_default(|dir| dir.display().to_string())
            }
            _ => platform::rho_install_dir().map_or_else(
                || "rho's install folder".to_owned(),
                |dir| tilde(&dir, platform::home_dir().as_deref()),
            ),
        }
    }

    /// `machine`'s `rho`, as the settings name it.
    fn rho(&self, machine: &MachineId, cx: &App) -> Option<RhoInstall> {
        let machines = self.machines.read(cx);
        machines.get(machine).and_then(|known| known.rho.clone())
    }

    fn run_check(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let Some(rho) = self.rho(machine, cx) else {
            return;
        };
        self.states.insert(machine.clone(), State::Checking);
        cx.notify();
        let id = machine.clone();
        let task = cx.spawn(async move |this, cx| {
            let checked = rho.check().await;
            let _ = this.update(cx, |this, cx| {
                let state = match checked {
                    Ok(check) => State::Checked {
                        check,
                        at: Instant::now(),
                    },
                    Err(error) => State::CheckFailed(error.to_string().into()),
                };
                this.tasks.remove(&id);
                this.states.insert(id, state);
                cx.notify();
            });
        });
        self.tasks.insert(machine.clone(), task);
    }

    fn change(&mut self, machine: &MachineId, change: Change, cx: &mut Context<'_, Self>) {
        let Some(rho) = self.rho(machine, cx) else {
            return;
        };
        self.states.insert(machine.clone(), State::Changing(change));
        cx.notify();
        let id = machine.clone();
        let task = cx.spawn(async move |this, cx| {
            let changed = match change {
                Change::Install => rho.install().await,
                Change::Update => rho.update().await,
            };
            let _ = this.update(cx, |this, cx| this.changed(&id, change, changed, cx));
        });
        self.tasks.insert(machine.clone(), task);
    }

    /// `change` on `machine` ended. What is installed changed, so a check
    /// follows a success.
    fn changed(
        &mut self,
        machine: &MachineId,
        change: Change,
        changed: Result<(), DaemonError>,
        cx: &mut Context<'_, Self>,
    ) {
        self.tasks.remove(machine);
        match changed {
            Ok(()) => {
                cx.emit(match change {
                    Change::Install => InstallsEvent::Installed(machine.clone()),
                    Change::Update => InstallsEvent::Updated(machine.clone()),
                });
                self.run_check(machine, cx);
            }
            Err(error) => {
                let why = error.to_string().into();
                self.states
                    .insert(machine.clone(), State::Failed { change, why });
                cx.notify();
            }
        }
    }
}

/// What to offer, from what is known of a machine's rho, the upgrade its
/// daemon waits to run, and how its connection is doing.
fn offer(
    state: Option<&State>,
    upgrade: Option<&PendingUpgrade>,
    status: Option<&MachineStatus>,
) -> Option<Offer> {
    match state {
        Some(State::Changing(change)) => return Some(Offer::Changing(*change)),
        Some(State::Failed { change, why }) => {
            return Some(Offer::Failed {
                change: *change,
                why: why.clone(),
            });
        }
        _ => {}
    }
    if let Some(upgrade) = upgrade {
        return Some(Offer::Restart {
            version: upgrade.version.clone().into(),
            waiting_for: upgrade.waiting_for,
        });
    }
    let Some(State::Checked { check, .. }) = state else {
        return None;
    };
    match check {
        // A daemon answers, so rho is there, where the check didn't look:
        // a `PATH` only a login shell sets, say.
        RhoCheck::Missing if matches!(status, Some(MachineStatus::Ready { .. })) => None,
        RhoCheck::Missing => Some(Offer::Install),
        RhoCheck::Installed { update, .. } => match update {
            Update::Available { latest } => Some(Offer::Update {
                latest: latest.clone().into(),
            }),
            Update::InTerminal { latest, command } => Some(Offer::InTerminal {
                latest: latest.clone().into(),
                command: command.clone().into(),
            }),
            Update::UpToDate | Update::Unknown(_) => None,
        },
    }
}

/// What `offer` says, and the button that acts on it, for `machine`,
/// named `tag` where machines need telling apart: the sidebar's footer and
/// the About page draw it.
pub(crate) fn render_offer(
    installs: &Entity<Installs>,
    machine: &MachineId,
    tag: Option<&str>,
    offer: &Offer,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    // " on box", or nothing with no tag.
    let on = |preposition: &str| tag.map_or_default(|tag| format!(" {preposition} {tag}"));
    let line = |text: String| div().text_xs().text_color(muted).child(text);
    let button = |action: &str, label: &'static str| {
        Button::new(SharedString::from(format!("{action}-rho-{machine}")))
            .outline()
            .xsmall()
            .label(label)
    };
    let act = |run: fn(&mut Installs, &MachineId, &mut Context<'_, Installs>)| {
        let (installs, machine) = (installs.clone(), machine.clone());
        move |_: &gpui_kit::ClickEvent, _: &mut gpui_kit::Window, cx: &mut App| {
            installs.update(cx, |installs, cx| run(installs, &machine, cx));
        }
    };
    let row = |text: AnyElement, button: Option<AnyElement>| {
        h_flex()
            .gap_2()
            .justify_between()
            .child(div().flex_1().min_w_0().child(text))
            .children(button)
            .into_any_element()
    };
    match offer {
        Offer::Install => row(
            line(format!("Rho isn't installed{}", on("on"))).into_any_element(),
            Some(
                button("install", "Install")
                    .icon(IconName::ArrowDown)
                    .on_click(act(Installs::install))
                    .into_any_element(),
            ),
        ),
        Offer::Changing(change) => {
            let doing = match change {
                Change::Install => "Installing",
                Change::Update => "Updating",
            };
            h_flex()
                .gap_1()
                .child(Spinner::new().xsmall())
                .child(line(format!("{doing} Rho{}…", on("on"))))
                .into_any_element()
        }
        Offer::Failed { change, why } => {
            let retry: fn(&mut Installs, &MachineId, &mut Context<'_, Installs>) = match change {
                Change::Install => Installs::install,
                Change::Update => Installs::update,
            };
            render_failure(machine, *change, &on("on"), why)
                .child(
                    h_flex().justify_end().child(
                        button("retry", "Try again")
                            .icon(IconName::RotateCw)
                            .on_click(act(retry)),
                    ),
                )
                .into_any_element()
        }
        Offer::Update { latest } => row(
            line(format!("Rho v{latest} available{}", on("for"))).into_any_element(),
            Some(
                button("update", "Update")
                    .icon(IconName::ArrowDown)
                    .on_click(act(Installs::update))
                    .into_any_element(),
            ),
        ),
        Offer::InTerminal { latest, command } => v_flex()
            .gap_1()
            .child(line(format!(
                "Rho v{latest} available{}. Update it in a terminal:",
                on("for")
            )))
            .child(render_command(machine, command, cx))
            .into_any_element(),
        Offer::Restart {
            version,
            waiting_for,
        } => {
            let text = match waiting_for {
                0 => format!("Rho {version} starts{} in a moment", on("on")),
                1 => format!("Rho {version} starts{} once 1 task ends", on("on")),
                n => format!("Rho {version} starts{} once {n} tasks end", on("on")),
            };
            row(
                line(text).into_any_element(),
                (*waiting_for > 0).then(|| {
                    button("restart", "Restart now")
                        .icon(IconName::RotateCw)
                        .tooltip(
                            "Running turns stop, and carry on after the restart in a Rho \
                             that resumes them. Background processes stop.",
                        )
                        .on_click(act(Installs::restart))
                        .into_any_element()
                }),
            )
        }
    }
}

/// Why `change` failed on `machine`, named by `on`, in an alert.
fn render_failure(machine: &MachineId, change: Change, on: &str, why: &str) -> Div {
    let title = match change {
        Change::Install => format!("Couldn't install Rho{on}"),
        Change::Update => format!("Couldn't update Rho{on}"),
    };
    v_flex().gap_1().child(
        Alert::error(
            SharedString::from(format!("rho-failed-{machine}")),
            last_lines(why),
        )
        .title(title)
        .small(),
    )
}

/// `command`, which updates rho in a terminal, with a button to copy it.
fn render_command(machine: &MachineId, command: &SharedString, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_1()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .child(command.clone()),
        )
        .child(
            Clipboard::new(SharedString::from(format!("copy-rho-update-{machine}")))
                .value(command.clone())
                .tooltip("Copy"),
        )
}

/// The main pane while this machine has no rho to connect to: `offer`,
/// one that [`Offer::replaces_connection_error`], in full. One block
/// whose button turns into its own progress, so nothing moves while rho
/// installs; a failure puts the script's last words above Try again.
pub(crate) fn render_setup(installs: &Entity<Installs>, offer: &Offer, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (title, failure) = match offer {
        Offer::Failed { why, .. } => ("Couldn't install Rho", Some(last_lines(why))),
        _ => ("Install Rho", None),
    };
    let installing = matches!(offer, Offer::Changing(_));
    let label = match (installing, &failure) {
        (true, _) => "Installing…",
        (false, Some(_)) => "Try again",
        (false, None) => "Install",
    };
    let dir = installs.read(cx).local_install_dir(cx);
    let installs = installs.clone();
    let button = Button::new("setup-install-rho")
        .primary()
        .label(label)
        .loading(installing)
        .on_click(move |_, _, cx| {
            installs.update(cx, |installs, cx| installs.install(&MachineId::local(), cx));
        });
    let said = match failure {
        Some(output) => div()
            .w_full()
            .p_3()
            .rounded(theme.radius)
            .bg(theme.muted)
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .text_color(theme.muted_foreground)
            .child(output),
        None => div()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child("Sessions run in Rho, which isn't on this machine yet."),
    };
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(
            v_flex()
                .w_96()
                .items_center()
                .gap_2()
                .child(div().text_xl().font_semibold().child(title))
                .child(said)
                .child(
                    v_flex().pt_4().gap_2().items_center().child(button).child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("Installs to {dir}")),
                    ),
                ),
        )
        .into_any_element()
}

/// The last lines of a long error, as an install script's output is: the
/// part that says what went wrong.
fn last_lines(why: &str) -> SharedString {
    /// About as many as a small alert shows before it grows tall.
    const SHOWN: usize = 6;
    let lines: Vec<&str> = why.trim().lines().collect();
    lines[lines.len().saturating_sub(SHOWN)..].join("\n").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A change under way or failed shows over everything; a pending
    /// upgrade over an update the check found; then what the check found,
    /// but never an install to a machine whose daemon answers.
    #[test]
    fn the_offer_follows_the_most_pressing_state() {
        let checked = |check| State::Checked {
            check,
            at: Instant::now(),
        };
        let installed = |update| {
            checked(RhoCheck::Installed {
                current: "3.1.0".to_owned(),
                update,
            })
        };
        let available = installed(Update::Available {
            latest: "3.2.0".to_owned(),
        });
        let upgrade = PendingUpgrade {
            version: "3.2.0".to_owned(),
            waiting_for: 2,
        };
        let restart = Offer::Restart {
            version: "3.2.0".into(),
            waiting_for: 2,
        };
        let cases = [
            (None, None, None),
            (Some(State::Checking), None, None),
            (Some(State::Checking), Some(&upgrade), Some(restart.clone())),
            (
                Some(State::Changing(Change::Update)),
                Some(&upgrade),
                Some(Offer::Changing(Change::Update)),
            ),
            (
                Some(State::Failed {
                    change: Change::Install,
                    why: "boom".into(),
                }),
                None,
                Some(Offer::Failed {
                    change: Change::Install,
                    why: "boom".into(),
                }),
            ),
            (Some(State::CheckFailed("unreachable".into())), None, None),
            (Some(available.clone()), Some(&upgrade), Some(restart)),
            (
                Some(available),
                None,
                Some(Offer::Update {
                    latest: "3.2.0".into(),
                }),
            ),
            (Some(checked(RhoCheck::Missing)), None, None),
            (Some(installed(Update::UpToDate)), None, None),
            (
                Some(installed(Update::Unknown("offline".to_owned()))),
                None,
                None,
            ),
        ];
        let ready = MachineStatus::Ready {
            version: "Rho 3.1.0".into(),
        };
        for (state, upgrade, want) in cases {
            assert_eq!(
                offer(state.as_ref(), upgrade, Some(&ready)),
                want,
                "connected, state {state:?}, upgrade {upgrade:?}"
            );
        }
        let failed = MachineStatus::Failed("no program rho".into());
        assert_eq!(
            offer(Some(&checked(RhoCheck::Missing)), None, Some(&failed)),
            Some(Offer::Install),
            "only a machine that can't connect is offered rho"
        );
    }
}
