//! The Machines page: what the sidebar calls this machine, and the remote
//! machines rho-ui reaches over SSH, each with its label and the `rho` it
//! runs there. Adding a machine connects to it at once; removing one drops
//! its sessions from the sidebar, and they stay on the machine.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    v_flex, ActiveTheme as _, Sizable as _,
};
use gpui_kit::{
    div, rems, App, AppContext as _, Context, Entity, InteractiveElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
};

use super::{update, SettingsView, CONTROL_REMS};
use crate::{
    machines::{host_problem, RemoteMachine, DEFAULT_REMOTE_RHO},
    settings::Settings,
};

/// The page's own state: the host being typed, and each machine's `rho`
/// program field, which applies on Enter or when it loses the keyboard, as
/// a change reconnects to the machine.
pub(super) struct MachineFields {
    new_host: Entity<InputState>,
    /// Why the typed host can't be added, once Add was pressed.
    problem: Option<SharedString>,
    rho: Vec<RhoField>,
    _subscriptions: [Subscription; 2],
}

struct RhoField {
    host: String,
    input: Entity<InputState>,
    _subscription: Subscription,
}

impl MachineFields {
    pub(super) fn new(window: &mut Window, cx: &mut Context<'_, SettingsView>) -> Self {
        let new_host =
            cx.new(|cx| InputState::new(window, cx).placeholder("SSH host, such as workstation"));
        let subscription = cx.subscribe_in(
            &new_host,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.add_machine(window, cx),
                InputEvent::Change => {
                    if this.machines.problem.take().is_some() {
                        cx.notify();
                    }
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        // Machines come and go from this page, or from another window's.
        let machines = cx.observe_global_in::<Settings>(window, |this, window, cx| {
            this.sync_rho_fields(window, cx);
        });
        Self {
            new_host,
            problem: None,
            rho: Vec::new(),
            _subscriptions: [subscription, machines],
        }
    }
}

impl SettingsView {
    /// Adds the typed host as a machine, or says why it can't be one.
    fn add_machine(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let host = self.machines.new_host.read(cx).value().trim().to_owned();
        let known = cx.global::<Settings>().machine(&host).is_some();
        let problem = host_problem(&host).or(known.then_some("That machine is on the list"));
        if let Some(problem) = problem {
            self.machines.problem = Some(problem.into());
            cx.notify();
            return;
        }
        update(cx, |settings| {
            settings.machines.push(RemoteMachine::new(&host));
        });
        self.machines.problem = None;
        self.machines
            .new_host
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// Gives each machine its `rho` program field, and drops the fields of
    /// machines that are gone.
    pub(super) fn sync_rho_fields(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let machines = cx.global::<Settings>().machines.clone();
        self.machines
            .rho
            .retain(|field| machines.iter().any(|machine| machine.host == field.host));
        for machine in machines {
            if self
                .machines
                .rho
                .iter()
                .any(|field| field.host == machine.host)
            {
                continue;
            }
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(DEFAULT_REMOTE_RHO)
                    .default_value(machine.rho.clone())
            });
            let host = machine.host.clone();
            let subscription = cx.subscribe_in(
                &input,
                window,
                move |_, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let rho = input.read(cx).value().trim().to_owned();
                        set_rho(&host, rho, cx);
                    }
                },
            );
            self.machines.rho.push(RhoField {
                host: machine.host,
                input,
                _subscription: subscription,
            });
        }
    }

    /// The Machines page.
    pub(super) fn machines_page(&self, cx: &mut Context<'_, Self>) -> SettingPage {
        let control =
            |field: SettingField<SharedString>| field.w(rems(CONTROL_REMS)).justify_between();
        let this_machine = SettingGroup::new().title("This machine").item(
            SettingItem::new(
                "Label",
                control(SettingField::input(
                    |cx: &App| cx.global::<Settings>().local_label.clone(),
                    |label: SharedString, cx: &mut App| {
                        let label = if label.trim().is_empty() {
                            crate::machines::LOCAL.into()
                        } else {
                            label
                        };
                        update(cx, |settings| settings.local_label = label);
                    },
                )),
            )
            .description("What the sidebar calls this machine."),
        );
        let remote = cx
            .global::<Settings>()
            .machines
            .iter()
            .map(|machine| self.machine_group(machine, cx))
            .collect::<Vec<_>>();
        SettingPage::new("Machines")
            .icon(Lucide::Server)
            .header_style(&super::page_header())
            .group(this_machine)
            .groups(remote)
            .group(self.add_group(cx))
    }

    /// One remote machine: its host with Remove, its label, and its `rho`
    /// program.
    fn machine_group(&self, machine: &RemoteMachine, cx: &App) -> SettingGroup {
        let control =
            |field: SettingField<SharedString>| field.w(rems(CONTROL_REMS)).justify_between();
        let host = machine.host.clone();
        let label_host = host.clone();
        let read_host = host.clone();
        let rho = self
            .machines
            .rho
            .iter()
            .find(|field| field.host == host)
            .map(|field| field.input.clone());
        let remove_host = host.clone();
        let danger = cx.theme().danger;
        SettingGroup::new()
            .title(host.clone())
            .item(
                SettingItem::new(
                    "SSH host",
                    SettingField::render(move |options, _, _| {
                        let host = remove_host.clone();
                        h_flex()
                            .w(rems(CONTROL_REMS))
                            .justify_between()
                            .gap_2()
                            .child(div().truncate().child(host.clone()))
                            .child(
                                Button::new(SharedString::from(format!("remove-machine-{host}")))
                                    .ghost()
                                    .with_size(options.size())
                                    .text_color(danger)
                                    .label("Remove")
                                    .on_click(move |_, _, cx| {
                                        let host = host.clone();
                                        update(cx, |settings| {
                                            settings.machines.retain(|machine| machine.host != host);
                                        });
                                    }),
                            )
                    }),
                )
                .description("To reach it at another host, remove it and add that one."),
            )
            .item(
                SettingItem::new(
                    "Label",
                    control(SettingField::input(
                        move |cx: &App| {
                            cx.global::<Settings>()
                                .machine(&read_host)
                                .map(|machine| machine.label.clone().into())
                                .unwrap_or_default()
                        },
                        move |label: SharedString, cx: &mut App| {
                            update(cx, |settings| {
                                if let Some(machine) = settings.machine_mut(&label_host) {
                                    label.trim().clone_into(&mut machine.label);
                                }
                            });
                        },
                    )),
                )
                .description("What the sidebar calls it. Empty shows the host."),
            )
            .item(
                SettingItem::new(
                    "rho program",
                    SettingField::render(move |options, _, _| {
                        div().w(rems(CONTROL_REMS)).children(rho.clone().map(|input| {
                            Input::new(&input).with_size(options.size())
                        }))
                    }),
                )
                .description(format!(
                    "The rho on {host}. An SSH command may not see your shell's PATH, so give an absolute or ~/ path if rho isn't found."
                )),
            )
    }

    /// The field to add a machine by its SSH host.
    fn add_group(&self, cx: &mut Context<'_, Self>) -> SettingGroup {
        let input = self.machines.new_host.clone();
        let problem = self.machines.problem.clone();
        let view = cx.entity().downgrade();
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        SettingGroup::new()
            .title("Add a machine")
            .item(SettingItem::render(move |options, _, _| {
                let view = view.clone();
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div().flex_1().child(
                                    Input::new(&input)
                                        .id("new-machine-host")
                                        .with_size(options.size()),
                                ),
                            )
                            .child(
                                Button::new("add-machine")
                                    .primary()
                                    .small()
                                    .label("Add")
                                    .on_click(move |_, window, cx| {
                                        let _ = view
                                            .update(cx, |this, cx| this.add_machine(window, cx));
                                    }),
                            ),
                    )
                    .child({
                        let note: SharedString = problem.clone().unwrap_or_else(|| {
                            "Any host ssh reaches, such as one in ~/.ssh/config. Its key must \
                             work without a password prompt, as from an SSH agent."
                                .into()
                        });
                        div()
                            .id("new-machine-note")
                            .test_support()
                            .aria_label(note.clone())
                            .text_xs()
                            .text_color(if problem.is_some() { danger } else { muted })
                            .child(note)
                    })
            }))
    }
}

/// Sets the `rho` program of the machine at `host`; empty means
/// [`DEFAULT_REMOTE_RHO`].
fn set_rho(host: &str, rho: String, cx: &mut App) {
    let rho = if rho.is_empty() {
        DEFAULT_REMOTE_RHO.to_owned()
    } else {
        rho
    };
    let unchanged = cx
        .global::<Settings>()
        .machine(host)
        .is_some_and(|machine| machine.rho == rho);
    if unchanged {
        return;
    }
    update(cx, |settings| {
        if let Some(machine) = settings.machine_mut(host) {
            machine.rho = rho;
        }
    });
}
