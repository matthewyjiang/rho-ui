//! The About page: rho-ui's version, with a newer one to update to, its
//! source and license, then Rho on each machine: its daemon's version and
//! process ID, with Restart and Stop, or why there is none to show yet,
//! with what would fix it, and under it what its rho needs: installing,
//! updating, or a restart into the newer one.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    IconName, Sizable as _,
};
use gpui_kit::{div, App, Entity, ParentElement as _, SharedString, Styled as _};

use super::{page_header, value, Tone};
use crate::{
    app_update::{render_offer as render_app_offer, AppUpdate},
    machines::{self, DaemonAction, MachineId, MachineStatus, Machines},
    rho_install::{render_offer, Installs},
};

/// rho-ui's source, from its `Cargo.toml`.
const SOURCE: &str = env!("CARGO_PKG_REPOSITORY");

/// A button of a machine's row: element ID `about-{name}-{machine}`, and
/// what it asks of the machine's daemon.
struct DaemonButton {
    name: &'static str,
    label: &'static str,
    icon: Lucide,
    action: DaemonAction,
}

/// The About page: rho-ui, with what `app_update` offers under its
/// version, then a row for each of `machines`, with what `installs` offers
/// for its rho under it.
pub(super) fn about_page(
    machines: &Entity<Machines>,
    installs: &Entity<Installs>,
    app_update: &Entity<AppUpdate>,
    cx: &App,
) -> SettingPage {
    let machines_read = machines.read(cx);
    let update = app_update.read(cx).offer().cloned().map(|offer| {
        let app_update = app_update.clone();
        SettingItem::render(move |_, _, cx| render_app_offer(&app_update, &offer, cx))
    });
    let version = SettingItem::new("Version", value(env!("CARGO_PKG_VERSION"), Tone::Plain));
    let app = SettingGroup::new().items(std::iter::once(version).chain(update).chain([
        SettingItem::new(
            "Source",
            SettingField::render(|options, _, _| {
                Button::new("about-source")
                    .link()
                    .with_size(options.size())
                    .label(SOURCE.trim_start_matches("https://"))
                    .on_click(|_, _, cx| cx.open_url(SOURCE))
            }),
        ),
        SettingItem::new("License", value(env!("CARGO_PKG_LICENSE"), Tone::Plain)),
    ]));
    let rho = SettingGroup::new()
        .title("Rho")
        .items(machines_read.iter().flat_map(|machine| {
            let label = machine.label.clone();
            let id = &machine.id;
            let restart = || DaemonButton {
                name: "restart",
                label: "Restart…",
                icon: Lucide::RotateCw,
                action: DaemonAction::Restart(id.clone()),
            };
            let item = match machines_read.status(id) {
                Some(MachineStatus::Ready {
                    version,
                    pid,
                    another_build,
                }) => {
                    let stop = DaemonButton {
                        name: "stop",
                        label: "Stop…",
                        icon: Lucide::Square,
                        action: DaemonAction::Stop(id.clone()),
                    };
                    let text = format!("Rho {version}").into();
                    let field = controls(machines, id, text, vec![restart(), stop]);
                    let pid = if *another_build {
                        format!("Process {pid}, another build than rho-ui's")
                    } else {
                        format!("Process {pid}")
                    };
                    SettingItem::new(label.clone(), field).description(pid)
                }
                Some(MachineStatus::Stopped) => {
                    let start = DaemonButton {
                        name: "start",
                        label: "Start",
                        icon: Lucide::Play,
                        action: DaemonAction::Connect(id.clone()),
                    };
                    let field = controls(machines, id, "Stopped".into(), vec![start]);
                    SettingItem::new(label.clone(), field)
                }
                Some(MachineStatus::OlderDaemon { .. }) => {
                    let buttons = if machines_read.restarts_older(id) {
                        vec![restart()]
                    } else {
                        Vec::new()
                    };
                    let field = controls(machines, id, "Older than rho-ui".into(), buttons);
                    SettingItem::new(label.clone(), field).description(machines::OLDER_DAEMON)
                }
                Some(MachineStatus::OlderRho) => {
                    let field = controls(machines, id, "Older than rho-ui".into(), Vec::new());
                    SettingItem::new(label.clone(), field).description(machines::OLDER_RHO)
                }
                None | Some(MachineStatus::Connecting) => {
                    SettingItem::new(label.clone(), value("Connecting…", Tone::Muted))
                }
                Some(MachineStatus::Failed(why)) => {
                    SettingItem::new(label.clone(), value("Can't reach", Tone::Muted))
                        .description(why.clone())
                }
            };
            // What its rho needs, on a row of its own under it.
            let offer = installs.read(cx).offer(id, cx).map(|offer| {
                let (installs, id) = (installs.clone(), id.clone());
                SettingItem::render(move |_, _, cx| {
                    render_offer(&installs, &id, Some(&label), &offer, cx)
                })
            });
            std::iter::once(item).chain(offer)
        }));
    SettingPage::new("About")
        .icon(IconName::Info)
        .header_style(&page_header())
        .group(app)
        .group(rho)
}

/// `text` about `machine`'s daemon, with a button for each of `buttons`,
/// which asks `machines` for its action.
fn controls(
    machines: &Entity<Machines>,
    machine: &MachineId,
    text: SharedString,
    buttons: Vec<DaemonButton>,
) -> SettingField<SharedString> {
    let (machines, machine) = (machines.clone(), machine.clone());
    SettingField::render(move |options, _, _| {
        let buttons = buttons.iter().map(|control| {
            let (machines, action) = (machines.clone(), control.action.clone());
            Button::new(SharedString::from(format!(
                "about-{}-{machine}",
                control.name
            )))
            .outline()
            .with_size(options.size())
            .icon(control.icon)
            .label(control.label)
            .on_click(move |_, _, cx| machines::ask(&machines, action.clone(), cx))
        });
        h_flex()
            .gap_2()
            .child(div().text_sm().child(text.clone()))
            .children(buttons)
    })
}
