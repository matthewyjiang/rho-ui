//! The Machines page: this machine's name, then each remote machine rho-ui
//! reaches over SSH, with its name, the `rho` it runs there, and Remove,
//! then a field to add one. Adding a machine connects to it at once;
//! removing one drops its sessions from the sidebar, and they stay on the
//! machine.

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

/// The page's own state: the host being typed, and each remote machine's
/// fields.
pub(super) struct MachineFields {
    new_host: Entity<InputState>,
    /// Why the typed host can't be added, once Add was pressed.
    problem: Option<SharedString>,
    remote: Vec<RemoteFields>,
    _subscriptions: [Subscription; 2],
}

/// One remote machine's fields. Its name shows the host while empty and
/// applies as typed; its `rho` program applies on Enter or when it loses
/// the keyboard, as a change reconnects to the machine.
struct RemoteFields {
    host: String,
    name: Entity<InputState>,
    rho: Entity<InputState>,
    _subscriptions: [Subscription; 2],
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
            this.sync_machine_fields(window, cx);
        });
        Self {
            new_host,
            problem: None,
            remote: Vec::new(),
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

    /// Gives each remote machine its fields, and drops the fields of
    /// machines that are gone.
    pub(super) fn sync_machine_fields(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let machines = cx.global::<Settings>().machines.clone();
        self.machines
            .remote
            .retain(|fields| machines.iter().any(|machine| machine.host == fields.host));
        for machine in machines {
            if self
                .machines
                .remote
                .iter()
                .any(|fields| fields.host == machine.host)
            {
                continue;
            }
            let name = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(machine.host.clone())
                    .default_value(machine.label.clone())
            });
            let rho = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(DEFAULT_REMOTE_RHO)
                    .default_value(machine.rho.clone())
            });
            let name_host = machine.host.clone();
            let rho_host = machine.host.clone();
            let subscriptions = [
                cx.subscribe_in(&name, window, move |_, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let label = input.read(cx).value().trim().to_owned();
                        set_label(&name_host, label, cx);
                    }
                }),
                cx.subscribe_in(&rho, window, move |_, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let rho = input.read(cx).value().trim().to_owned();
                        set_rho(&rho_host, rho, cx);
                    }
                }),
            ];
            self.machines.remote.push(RemoteFields {
                host: machine.host,
                name,
                rho,
                _subscriptions: subscriptions,
            });
        }
    }

    /// The Machines page.
    pub(super) fn machines_page(&self, cx: &mut Context<'_, Self>) -> SettingPage {
        let this_machine = SettingGroup::new().title("This machine").item(
            SettingItem::new(
                "Name",
                SettingField::input(
                    |cx: &App| cx.global::<Settings>().local_label.clone(),
                    |label: SharedString, cx: &mut App| {
                        let label = if label.trim().is_empty() {
                            crate::machines::LOCAL.into()
                        } else {
                            label
                        };
                        update(cx, |settings| settings.local_label = label);
                    },
                )
                .w(rems(CONTROL_REMS)),
            )
            .description("Shown in the sidebar."),
        );
        let remote = self
            .machines
            .remote
            .iter()
            .map(machine_group)
            .collect::<Vec<_>>();
        SettingPage::new("Machines")
            .icon(Lucide::Server)
            .header_style(&super::page_header())
            .group(this_machine)
            .groups(remote)
            .group(self.add_group(cx))
    }

    /// The field to add a machine by its SSH host, with a note that says
    /// which hosts work, or why the typed one can't be added.
    fn add_group(&self, cx: &mut Context<'_, Self>) -> SettingGroup {
        let input = self.machines.new_host.clone();
        let problem = self.machines.problem.clone();
        let view = cx.entity().downgrade();
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        SettingGroup::new()
            .title("Add machine")
            .item(SettingItem::render(move |options, _, _| {
                let view = view.clone();
                let note: SharedString = problem
                    .clone()
                    .unwrap_or_else(|| "Any host ssh reaches without a password prompt.".into());
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
                                    .with_size(options.size())
                                    .label("Add")
                                    .on_click(move |_, window, cx| {
                                        let _ = view
                                            .update(cx, |this, cx| this.add_machine(window, cx));
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("new-machine-note")
                            .test_support()
                            .aria_label(note.clone())
                            .text_xs()
                            .text_color(if problem.is_some() { danger } else { muted })
                            .child(note),
                    )
            }))
    }
}

/// One remote machine, titled by its host: its name, its `rho` program,
/// and Remove.
fn machine_group(fields: &RemoteFields) -> SettingGroup {
    let input = |state: &Entity<InputState>| {
        let state = state.clone();
        SettingField::render(move |options, _, _| {
            Input::new(&state)
                .with_size(options.size())
                .w(rems(CONTROL_REMS))
        })
    };
    let host = fields.host.clone();
    let remove_host = host.clone();
    SettingGroup::new()
        .title(host.clone())
        .item(
            SettingItem::new("Name", input(&fields.name))
                .description("Shown in the sidebar. Empty uses the host."),
        )
        .item(
            SettingItem::new("rho path", input(&fields.rho))
                .description("Use an absolute path if ssh can't find rho."),
        )
        .item(
            SettingItem::new(
                "Remove machine",
                SettingField::render(move |options, _, _| {
                    let host = remove_host.clone();
                    Button::new(SharedString::from(format!("remove-machine-{host}")))
                        .outline()
                        .with_size(options.size())
                        .label("Remove")
                        .on_click(move |_, _, cx| {
                            let host = host.clone();
                            update(cx, |settings| {
                                settings.machines.retain(|machine| machine.host != host);
                            });
                        })
                }),
            )
            .description(format!("Its sessions stay on {host}.")),
        )
}

/// Sets the label of the machine at `host`; empty shows the host.
fn set_label(host: &str, label: String, cx: &mut App) {
    let unchanged = cx
        .global::<Settings>()
        .machine(host)
        .is_some_and(|machine| machine.label == label);
    if unchanged {
        return;
    }
    update(cx, |settings| {
        if let Some(machine) = settings.machine_mut(host) {
            machine.label = label;
        }
    });
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
