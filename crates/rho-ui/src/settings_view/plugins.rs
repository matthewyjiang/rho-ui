//! The Plugins page: the agent plugins the chosen machine's rho finds for a
//! session in the chosen project (see [`crate::configured`]), in the order
//! a session loads them, the project's then the user's. Each says whether
//! it loads and, when it doesn't, why; where it came from; and what it
//! brings. Its switch enables or disables it; its menu updates a git
//! install, or removes the plugin once the user confirms. Add plugin…
//! installs one ([`add`]); Update all fetches every git install again.
//!
//! The page shows the machine and project MCP & hooks shows, from the same
//! read, which every change reads again, as a plugin's MCP servers change
//! with it. Changes apply to sessions opened afterwards.

mod add;

use std::path::PathBuf;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    button::{Button, ButtonVariant, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    spinner::Spinner,
    switch::Switch,
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _,
};
use gpui_kit::{
    rems, Anchor, App, Context, Entity, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, Window,
};
use rho_ui_core::{Plugin, PluginScope, PluginSource, PluginStatus, PluginTarget};

use super::{
    configured::{
        project_picker, redacted_url, refresh_button, too_old, trust_hint, unlisted, Shown,
    },
    described, machine_picker, page_header, status_mark, unreachable_note, value, Mark,
    SettingsView, Tone, CONTROL_REMS,
};
use crate::{
    configured::{short, Answer, Change, ChangeState, Configured, UpdateAllState},
    machines::MachineId,
    workspaces::tilde,
};

/// The machine a page shows, and the project it reads there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Spot {
    machine: MachineId,
    workspace: Option<PathBuf>,
}

/// One plugin, as the page showed it on a machine for a project.
#[derive(Clone, Debug)]
struct Row {
    spot: Spot,
    target: PluginTarget,
}

impl Row {
    /// Asks for `change` to the row's plugin.
    fn send(&self, configured: &Entity<Configured>, change: Change, cx: &mut App) {
        configured.update(cx, |configured, cx| {
            configured.change_plugin(
                &self.spot.machine,
                self.spot.workspace.clone(),
                self.target.clone(),
                change,
                cx,
            );
        });
    }
}

/// A row's status while `change` is on its way.
fn running(change: Change) -> &'static str {
    match change {
        Change::Enable => "Enabling…",
        Change::Disable => "Disabling…",
        Change::Update => "Updating…",
        Change::Remove => "Removing…",
    }
}

/// A row's status once `change` failed.
fn failed(change: Change) -> &'static str {
    match change {
        Change::Enable | Change::Disable => "Not changed",
        Change::Update => "Update failed",
        Change::Remove => "Not removed",
    }
}

impl SettingsView {
    pub(super) fn plugins_page(&self, cx: &mut Context<'_, Self>) -> SettingPage {
        let (machine, workspace) = self.configured_target(cx);
        let spot = Spot { machine, workspace };
        let view = cx.entity().downgrade();
        let machines = self.machine_list.read(cx);
        let page = SettingPage::new("Plugins")
            .icon(Lucide::Puzzle)
            .header_style(&page_header());
        let mut head = Vec::new();
        if machines.several() {
            head.push(machine_picker(
                machines,
                &spot.machine,
                "Each machine's Rho has its own plugins.",
                view.clone(),
                Self::pick_configured_machine,
            ));
        }
        if let Some(why) = unreachable_note(machines, &spot.machine) {
            head.push(SettingItem::new("Status", value(why, Tone::Muted)));
            return page.group(SettingGroup::new().items(head));
        }
        let fields = &self.configured;
        let home = fields.workspaces.read(cx).home(&spot.machine);
        let label = machines.label(&spot.machine);
        head.push(project_picker(
            fields.projects.read(cx),
            &spot.machine,
            spot.workspace.as_deref(),
            home.as_deref(),
            view,
        ));
        let page = page.title_suffix(refresh_button(
            &fields.configured,
            &spot.machine,
            spot.workspace.clone(),
        ));
        let configured = fields.configured.read(cx);
        let reading = configured
            .reading(&spot.machine)
            .filter(|reading| reading.workspace == spot.workspace);
        let Some(reading) = reading else {
            head.push(SettingItem::new("Status", value("Reading…", Tone::Muted)));
            return page.group(SettingGroup::new().items(head));
        };
        let plugins = match &reading.plugins {
            Answer::Listed(plugins) => plugins,
            Answer::TooOld => {
                let status = too_old(&label, "its plugins");
                return page
                    .group(SettingGroup::new().items(head))
                    .group(SettingGroup::new().title("Status").item(status));
            }
            unread @ Answer::Failed(_) => {
                let status = unlisted(unread, "plugins", &label);
                return page
                    .group(SettingGroup::new().items(head))
                    .group(SettingGroup::new().title("Status").items(status));
            }
        };
        let shown = Shown {
            machine: &spot.machine,
            label: &label,
            home: home.as_deref(),
        };
        let rows = Rows {
            configured,
            entity: &fields.configured,
            spot: &spot,
            plugins,
            shown: &shown,
        };
        head.push(rows.actions());
        let mut page = page.group(SettingGroup::new().items(head));
        if spot.workspace.is_some() {
            page = page.group(rows.group(PluginScope::Project));
        }
        page.group(rows.group(PluginScope::User))
    }
}

/// The machine a sentence names, as `shown`.
fn on(shown: &Shown<'_>) -> String {
    if shown.machine.is_local() {
        "this machine".to_owned()
    } else {
        shown.label.to_owned()
    }
}

/// What the plugin rows are drawn from.
struct Rows<'a> {
    configured: &'a Configured,
    entity: &'a Entity<Configured>,
    spot: &'a Spot,
    plugins: &'a [Plugin],
    shown: &'a Shown<'a>,
}

impl Rows<'_> {
    /// The row that says changes apply to new sessions, with Update all
    /// when there are git installs, and Add plugin….
    fn actions(&self) -> SettingItem {
        let git: Vec<PluginTarget> = self
            .plugins
            .iter()
            .filter(|plugin| matches!(plugin.source, PluginSource::Git { .. }))
            .map(|plugin| PluginTarget::new(plugin.name.clone(), plugin.scope))
            .collect();
        let on = on(self.shown);
        let update_all = self
            .configured
            .update_all_state(&self.spot.machine, self.spot.workspace.as_ref());
        let mut lines = vec![format!(
            "Sessions already open keep the plugins they loaded. Installs and updates run \
             git on {on}."
        )];
        if let Some(UpdateAllState::Said(said)) = &update_all {
            lines.push(said.to_string());
        }
        let running = matches!(update_all, Some(UpdateAllState::Running));
        let (configured, spot, home) = (
            self.entity.clone(),
            self.spot.clone(),
            self.shown.home.map(PathBuf::from),
        );
        let field = SettingField::render(move |options, _, _| {
            let update = (!git.is_empty()).then(|| {
                let (configured, spot, git) = (configured.clone(), spot.clone(), git.clone());
                Button::new("plugins-update-all")
                    .outline()
                    .with_size(options.size())
                    .label("Update all")
                    .loading(running)
                    .disabled(running)
                    .on_click(move |_, _, cx| {
                        let workspace = spot.workspace.clone();
                        configured.update(cx, |configured, cx| {
                            configured.update_all_plugins(&spot.machine, workspace, &git, cx);
                        });
                    })
            });
            let (configured, spot, on, home) =
                (configured.clone(), spot.clone(), on.clone(), home.clone());
            h_flex().gap_2().children(update).child(
                Button::new("plugins-add")
                    .primary()
                    .with_size(options.size())
                    .icon(Lucide::Plus)
                    .label("Add plugin…")
                    .on_click(move |_, window, cx| {
                        let target = add::Target {
                            machine: spot.machine.clone(),
                            workspace: spot.workspace.clone(),
                            label: on.clone(),
                            home: home.clone(),
                        };
                        add::open(target, configured.clone(), window, cx);
                    }),
            )
        });
        described(
            SettingItem::new("Changes apply to new sessions", field),
            Some(lines.join("\n")),
        )
    }

    /// The group of `scope`'s plugins.
    fn group(&self, scope: PluginScope) -> SettingGroup {
        let listed: Vec<&Plugin> = self
            .plugins
            .iter()
            .filter(|plugin| plugin.scope == scope)
            .collect();
        let (title, none) = match scope {
            PluginScope::Project => (
                "Project plugins",
                "None in this project. A project's plugins live in its .agents/plugins.",
            ),
            _ => (
                "Your plugins",
                "None yet. Add one from a folder or a git repository.",
            ),
        };
        let group = SettingGroup::new().title(title);
        let untrusted = listed
            .iter()
            .any(|plugin| plugin.status == PluginStatus::Untrusted);
        let group = if untrusted {
            group.description(format!(
                "Sessions load none of them. To trust project plugins, {}",
                trust_hint(
                    self.shown,
                    "RHO_TRUST_PROJECT_PLUGINS",
                    "every project's plugins"
                )
            ))
        } else {
            group
        };
        if listed.is_empty() {
            return group.item(SettingItem::new("No plugins", value(none, Tone::Muted)));
        }
        let items: Vec<SettingItem> = listed
            .iter()
            .enumerate()
            .map(|(ix, plugin)| {
                // rho acts on the nearest root's plugin of a name, so a
                // farther one of the same name can only be shown.
                let unreachable = listed[..ix]
                    .iter()
                    .any(|other| other.name == plugin.name)
                    .then_some(Unreachable(ix));
                self.item(plugin, unreachable)
            })
            .collect();
        group.items(items)
    }

    /// `plugin`'s row; with no actions when it is `unreachable`.
    fn item(&self, plugin: &Plugin, unreachable: Option<Unreachable>) -> SettingItem {
        let row = Row {
            spot: self.spot.clone(),
            target: PluginTarget::new(plugin.name.clone(), plugin.scope),
        };
        let state = unreachable
            .is_none()
            .then(|| {
                self.configured.change_state(
                    &self.spot.machine,
                    self.spot.workspace.as_ref(),
                    &row.target,
                )
            })
            .flatten();
        let mut lines = self.lines(plugin, state.as_ref());
        if unreachable.is_some() {
            lines.push("Rho can't change it from here: it acts on the nearer one.".to_owned());
        }
        let field = row_field(
            plugin,
            row,
            unreachable,
            state.as_ref(),
            self.shown,
            self.entity.clone(),
        );
        let keywords = [SharedString::from(plugin.path.display().to_string())];
        described(
            SettingItem::new(plugin.name.clone(), field).keywords(keywords),
            Some(lines.join("\n")),
        )
    }

    /// What the row says under its name: what the plugin is, why it doesn't
    /// load, where it came from, what it brings, its problems, and how its
    /// last change went.
    fn lines(&self, plugin: &Plugin, state: Option<&ChangeState>) -> Vec<String> {
        let home = self.shown.home;
        let about = match (&plugin.version, &plugin.description) {
            (Some(version), Some(description)) => Some(format!("{version} · {description}")),
            (Some(about), None) | (None, Some(about)) => Some(about.clone()),
            (None, None) => None,
        };
        let why = match plugin.status {
            PluginStatus::Untrusted => {
                Some("The project isn't trusted, so sessions skip it.".to_owned())
            }
            PluginStatus::Shadowed => Some(self.shadowed_by(plugin)),
            PluginStatus::Rejected => Some(format!(
                "{}\nSessions skip it until it's fixed.",
                plugin
                    .error
                    .as_deref()
                    .unwrap_or("Its plugin.json doesn't load.")
            )),
            _ => None,
        };
        let source = match &plugin.source {
            PluginSource::Git {
                url,
                git_ref,
                path,
                commit,
            } => {
                let at = path
                    .as_ref()
                    .map_or_else(String::new, |path| format!(" {path}"));
                let on = git_ref
                    .as_ref()
                    .map_or_else(String::new, |git_ref| format!(" ({git_ref})"));
                format!("From {}{at}{on} at {}", redacted_url(url), short(commit))
            }
            PluginSource::Link { target } => format!("Linked to {}", tilde(target, home)),
            PluginSource::Install => format!("Copied into {}", tilde(&plugin.path, home)),
            _ => format!("In {}", tilde(&plugin.path, home)),
        };
        let outcome = match state {
            Some(ChangeState::Failed(_, why) | ChangeState::Updated(why)) => Some(why.to_string()),
            Some(ChangeState::Busy(_)) | None => None,
        };
        about
            .into_iter()
            .chain(why)
            .chain(std::iter::once(source))
            .chain(brings(plugin))
            .chain(plugin.problems.iter().cloned())
            .chain(outcome)
            .collect()
    }

    /// Which plugin of the same name loads, or stays disabled, in place of
    /// shadowed `plugin`.
    fn shadowed_by(&self, plugin: &Plugin) -> String {
        let nearer = self.plugins.iter().find(|other| {
            other.name == plugin.name
                && matches!(other.status, PluginStatus::Loaded | PluginStatus::Disabled)
        });
        match nearer {
            Some(nearer) => format!(
                "{} has the same name and takes its place.",
                tilde(&nearer.path, self.shown.home)
            ),
            None => "A nearer plugin of the same name takes its place.".to_owned(),
        }
    }
}

/// What `plugin` brings to a session, as one line, if anything: none is
/// read for a shadowed or rejected one.
fn brings(plugin: &Plugin) -> Option<String> {
    let names = |kind: &str, names: &[String]| {
        (!names.is_empty()).then(|| format!("{kind}: {}", names.join(", ")))
    };
    let servers: Vec<String> = plugin
        .mcp_servers
        .iter()
        .map(|server| format!("{}/{server}", plugin.name))
        .collect();
    let parts: Vec<String> = [
        names("Skills", &plugin.skills),
        names("MCP", &servers),
        names("Agents", &plugin.agent_files),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// How `status` reads in a row's status column.
fn status_of(status: PluginStatus) -> (Mark, &'static str) {
    match status {
        PluginStatus::Loaded => (Mark::On, "Loaded"),
        PluginStatus::Disabled => (Mark::Off, "Disabled"),
        PluginStatus::Untrusted => (Mark::Warn, "Untrusted"),
        PluginStatus::Rejected => (Mark::Bad, "Rejected"),
        PluginStatus::Shadowed => (Mark::Off, "Shadowed"),
        _ => (Mark::Off, "Unknown"),
    }
}

/// Marks a row the page can't act on: a nearer plugin of its scope has
/// its name, and rho acts on that one. It holds the row's place in its
/// group, which tells its ids from the nearer one's.
#[derive(Clone, Copy)]
struct Unreachable(usize);

/// A row's controls: its status, its switch, and its menu, which offers
/// Update for a git install, and Remove…; a rejected plugin has only
/// Remove…. While a change is on its way they wait, and the status says
/// so.
fn row_field(
    plugin: &Plugin,
    row: Row,
    unreachable: Option<Unreachable>,
    state: Option<&ChangeState>,
    shown: &Shown<'_>,
    configured: Entity<Configured>,
) -> SettingField<SharedString> {
    let scope = match plugin.scope {
        PluginScope::Project => "project",
        _ => "user",
    };
    let far = unreachable.map_or_else(String::new, |Unreachable(ix)| format!("#{ix}"));
    let id = |part: &str| SharedString::from(format!("plugin-{part}{scope}-{}{far}", plugin.name));
    let (status_id, switch_id, menu_id) = (id(""), id("enabled-"), id("menu-"));
    let name = plugin.name.clone();
    let (busy, enabled, (mark, status)) = match state {
        Some(&ChangeState::Busy(change)) => {
            let enabled = match change {
                Change::Enable => true,
                Change::Disable => false,
                Change::Update | Change::Remove => plugin.enabled,
            };
            (true, enabled, (Mark::Off, running(change)))
        }
        Some(&ChangeState::Failed(change, _)) => {
            (false, plugin.enabled, (Mark::Bad, failed(change)))
        }
        _ => (false, plugin.enabled, status_of(plugin.status)),
    };
    let still = busy || unreachable.is_some();
    // rho can't enable, disable or fetch a plugin whose plugin.json doesn't
    // load, only remove it.
    let rejected = plugin.status == PluginStatus::Rejected;
    let git = !rejected && matches!(plugin.source, PluginSource::Git { .. });
    let removal = Removal::of(plugin, shown);
    SettingField::render(move |options, _, cx| {
        let label = format!("{name}: {status}");
        let status = if busy {
            h_flex()
                .gap_1p5()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Spinner::new().xsmall())
                .child(status)
                .into_any_element()
        } else {
            status_mark(status_id.clone(), label, mark, status, cx).into_any_element()
        };
        let toggle = (!rejected).then(|| {
            let (configured, row) = (configured.clone(), row.clone());
            Switch::new(switch_id.clone())
                .accessibility_label(format!("Enable {name}"))
                .checked(enabled)
                .with_size(options.size())
                .disabled(still)
                .on_click(move |on: &bool, _, cx| {
                    let change = if *on { Change::Enable } else { Change::Disable };
                    row.send(&configured, change, cx);
                })
        });
        let menu = {
            let (configured, row, removal) = (configured.clone(), row.clone(), removal.clone());
            Button::new(menu_id.clone())
                .ghost()
                .xsmall()
                .icon(Lucide::Ellipsis)
                .accessibility_label(format!("{name} actions"))
                .disabled(still)
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    let menu = if git {
                        let (configured, row) = (configured.clone(), row.clone());
                        menu.item(PopupMenuItem::new("Update").on_click(move |_, _, cx| {
                            row.send(&configured, Change::Update, cx);
                        }))
                    } else {
                        menu
                    };
                    let (configured, row, removal) =
                        (configured.clone(), row.clone(), removal.clone());
                    menu.item(
                        PopupMenuItem::new("Remove…").on_click(move |_, window, cx| {
                            confirm_removal(&configured, &row, &removal, window, cx);
                        }),
                    )
                })
        };
        h_flex()
            .w(rems(CONTROL_REMS))
            .justify_between()
            .child(status)
            .child(h_flex().gap_1().children(toggle).child(menu))
    })
}

/// What the confirmation of a removal says.
#[derive(Clone)]
struct Removal {
    name: String,
    /// What it does on the machine.
    effect: String,
}

impl Removal {
    fn of(plugin: &Plugin, shown: &Shown<'_>) -> Self {
        let (path, on) = (tilde(&plugin.path, shown.home), on(shown));
        let effect = match &plugin.source {
            PluginSource::Link { target } => format!(
                "Rho removes the link at {path} on {on}. {} stays.",
                tilde(target, shown.home)
            ),
            _ => format!("Rho deletes {path} on {on}. Its data folder stays."),
        };
        Self {
            name: plugin.name.clone(),
            effect,
        }
    }
}

/// Asks before removing `row`'s plugin; the daemon removes it only once
/// the user confirms.
fn confirm_removal(
    configured: &Entity<Configured>,
    row: &Row,
    removal: &Removal,
    window: &mut Window,
    cx: &mut App,
) {
    let (configured, row, removal) = (configured.clone(), row.clone(), removal.clone());
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let (configured, row) = (configured.clone(), row.clone());
        alert
            .icon(Icon::new(Lucide::Trash).text_color(cx.theme().danger))
            .title(format!("Remove {}?", removal.name))
            .description(format!(
                "{} Sessions opened afterwards won't load it.",
                removal.effect
            ))
            .confirm()
            .ok_text("Remove")
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, _, cx| {
                row.send(&configured, Change::Remove, cx);
                true
            })
    });
}
