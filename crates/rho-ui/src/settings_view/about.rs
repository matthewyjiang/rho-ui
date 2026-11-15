//! The About page: rho-ui's version, source and license, then the version
//! of Rho on each machine, or why there is none to show yet.

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    ActiveTheme as _, IconName, Sizable as _,
};
use gpui_kit::{div, ParentElement as _, SharedString, Styled as _};

use super::page_header;
use crate::machines::{MachineStatus, Machines};

/// rho-ui's source, from its `Cargo.toml`.
const SOURCE: &str = env!("CARGO_PKG_REPOSITORY");

/// The About page, with a row for each of `machines`.
pub(super) fn about_page(machines: &Machines) -> SettingPage {
    let app = SettingGroup::new().items([
        SettingItem::new("Version", value(env!("CARGO_PKG_VERSION"), Tone::Plain)),
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
    ]);
    let rho = SettingGroup::new()
        .title("Rho versions")
        .items(machines.iter().map(|machine| {
            let label = machine.label.clone();
            match machines.status(&machine.id) {
                Some(MachineStatus::Ready { version }) => {
                    SettingItem::new(label, value(version.clone(), Tone::Plain))
                }
                None | Some(MachineStatus::Connecting) => {
                    SettingItem::new(label, value("Connecting…", Tone::Muted))
                }
                Some(MachineStatus::Failed(why)) => {
                    SettingItem::new(label, value("Can't reach", Tone::Muted))
                        .description(why.clone())
                }
            }
        }));
    SettingPage::new("About")
        .icon(IconName::Info)
        .header_style(&page_header())
        .group(app)
        .group(rho)
}

/// How a value reads: muted when it stands in for one not known yet.
#[derive(Clone, Copy)]
enum Tone {
    Plain,
    Muted,
}

/// A value to read, not change.
fn value(text: impl Into<SharedString>, tone: Tone) -> SettingField<SharedString> {
    let text = text.into();
    SettingField::render(move |_, _, cx| {
        let theme = cx.theme();
        let color = match tone {
            Tone::Plain => theme.foreground,
            Tone::Muted => theme.muted_foreground,
        };
        div().text_sm().text_color(color).child(text.clone())
    })
}
