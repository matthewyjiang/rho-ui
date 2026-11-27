//! The MCP & hooks page: what the chosen machine's rho configuration sets
//! up for a session in the chosen project, read-only (see
//! [`crate::configured`]). Its MCP servers, from `config.toml` and the
//! plugins that apply, each with where it comes from, how a session
//! reaches it, and whether it starts; then its hooks files, the user's and
//! the project's, each saying whether it loads and, when it doesn't, why
//! and what to do, with the hooks it defines.
//!
//! A project's plugins and hooks file apply only to sessions in it, so the
//! page reads them for one project: the one of the session or draft on
//! screen when Settings opened, on its machine, which the page then shows
//! first; else none, which lists the user's own alone. A picker offers the
//! machine's other projects. Opening Settings, or picking a machine or a
//! project, reads afresh; the refresh beside the title reads again.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
};
use gpui_kit::{
    rems, App, Context, Entity, InteractiveElement as _, ParentElement as _, SharedString,
    Styled as _, Subscription, WeakEntity,
};
use rho_ui_core::{
    ConfiguredMcpServer, HookContract, HookEvent, HookStatus, HookTools, HookWarning, HooksFile,
    HooksFileScope, HooksFileStatus, HooksListResponse, McpListResponse, McpServerSource,
    McpTransport,
};

use super::{
    described, machine_picker, page_header, status_field, unreachable_note, value, Mark,
    PickedMachine, SettingsView, Tone, CONTROL_REMS,
};
use crate::{
    configured::{Answer, Configured, Reading},
    machines::{MachineId, Machines, Place},
    projects::Projects,
    workspaces::{tilde, Workspaces},
};

/// The machine a page shows, as its rows name it.
pub(super) struct Shown<'a> {
    pub(super) machine: &'a MachineId,
    pub(super) label: &'a str,
    /// Its home, which paths show as `~`.
    pub(super) home: Option<&'a Path>,
}

/// The page's own state: which machine it shows, and which project on
/// each. Plugins shows the same.
pub(super) struct ConfiguredFields {
    pub(super) configured: Entity<Configured>,
    pub(super) projects: Entity<Projects>,
    pub(super) workspaces: Entity<Workspaces>,
    machine: PickedMachine,
    /// The project on screen when Settings opened, which its machine shows
    /// until another is picked there.
    shown: Option<Place>,
    /// The project picked on each machine; `None` picks none.
    picked: Vec<(MachineId, Option<PathBuf>)>,
    _subscriptions: [Subscription; 3],
}

impl ConfiguredFields {
    /// Shows `shown`'s machine, or this one without it, and reads it
    /// afresh.
    pub(super) fn new(
        machines: &Entity<Machines>,
        configured: Entity<Configured>,
        shown: Option<Place>,
        projects: Entity<Projects>,
        workspaces: Entity<Workspaces>,
        cx: &mut Context<'_, SettingsView>,
    ) -> Self {
        let machine = shown.as_ref().map_or_else(PickedMachine::new, |place| {
            PickedMachine(place.machine.clone())
        });
        let fields = Self {
            _subscriptions: [
                cx.observe(&configured, |_, _, cx| cx.notify()),
                cx.observe(&projects, |_, _, cx| cx.notify()),
                // A machine connecting: read it, if it is the one shown.
                cx.observe(machines, |this, _, cx| {
                    let (machine, workspace) = this.configured_target(cx);
                    this.configured.configured.update(cx, |configured, cx| {
                        configured.ensure_read(&machine, workspace, cx);
                    });
                    cx.notify();
                }),
            ],
            configured,
            projects,
            workspaces,
            machine,
            shown,
            picked: Vec::new(),
        };
        let (machine, workspace) = fields.target(machines.read(cx));
        fields.configured.update(cx, |configured, cx| {
            configured.read(&machine, workspace, cx);
        });
        fields
    }

    /// The project `machine` shows: the one picked there, else the one on
    /// screen if it is there.
    fn workspace(&self, machine: &MachineId) -> Option<PathBuf> {
        match self.picked.iter().find(|(on, _)| on == machine) {
            Some((_, picked)) => picked.clone(),
            None => self
                .shown
                .as_ref()
                .filter(|place| place.machine == *machine)
                .map(|place| place.path.clone()),
        }
    }

    /// The machine shown, and its project.
    fn target(&self, machines: &Machines) -> (MachineId, Option<PathBuf>) {
        let machine = self.machine.shown(machines);
        let workspace = self.workspace(&machine);
        (machine, workspace)
    }
}

impl SettingsView {
    /// The machine the page shows, and the project it reads there.
    pub(super) fn configured_target(&self, cx: &App) -> (MachineId, Option<PathBuf>) {
        self.configured.target(self.machine_list.read(cx))
    }

    /// Shows `machine`, read afresh for its project.
    pub(super) fn pick_configured_machine(
        &mut self,
        machine: MachineId,
        cx: &mut Context<'_, Self>,
    ) {
        let workspace = self.configured.workspace(&machine);
        self.configured.configured.update(cx, |configured, cx| {
            configured.read(&machine, workspace, cx);
        });
        if self.configured.machine.pick(machine) {
            cx.notify();
        }
    }

    /// Reads the machine shown for `workspace`, and keeps it picked there.
    fn pick_configured_project(&mut self, workspace: Option<PathBuf>, cx: &mut Context<'_, Self>) {
        let (machine, _) = self.configured_target(cx);
        let fields = &mut self.configured;
        fields.picked.retain(|(on, _)| *on != machine);
        fields.picked.push((machine.clone(), workspace.clone()));
        fields.configured.update(cx, |configured, cx| {
            configured.read(&machine, workspace, cx);
        });
        cx.notify();
    }

    pub(super) fn configured_page(&self, cx: &mut Context<'_, Self>) -> SettingPage {
        let (machine, workspace) = self.configured_target(cx);
        let view = cx.entity().downgrade();
        let machines = self.machine_list.read(cx);
        let mut page = SettingPage::new("MCP & hooks")
            .icon(Lucide::Plug)
            .header_style(&page_header());
        let mut pickers = Vec::new();
        if machines.several() {
            pickers.push(machine_picker(
                machines,
                &machine,
                "Each machine's Rho has its own MCP servers and hooks.",
                view.clone(),
                Self::pick_configured_machine,
            ));
        }
        if let Some(why) = unreachable_note(machines, &machine) {
            pickers.push(SettingItem::new("Status", value(why, Tone::Muted)));
            return page.group(SettingGroup::new().items(pickers));
        }
        let fields = &self.configured;
        let home = fields.workspaces.read(cx).home(&machine);
        let home = home.as_deref();
        let label = machines.label(&machine);
        pickers.push(project_picker(
            fields.projects.read(cx),
            &machine,
            workspace.as_deref(),
            home,
            view,
        ));
        let state = fields.configured.read(cx);
        page = page.title_suffix(refresh_button(
            &fields.configured,
            &machine,
            workspace.clone(),
        ));
        let reading = state
            .reading(&machine)
            .filter(|reading| reading.workspace == workspace);
        let Some(reading) = reading else {
            pickers.push(SettingItem::new("Status", value("Reading…", Tone::Muted)));
            return page.group(SettingGroup::new().items(pickers));
        };
        if let Reading {
            mcp: Answer::TooOld,
            hooks: Answer::TooOld,
            ..
        } = reading
        {
            return page.group(SettingGroup::new().items(pickers)).group(
                SettingGroup::new()
                    .title("Status")
                    .item(too_old(&label, "its MCP servers and hooks")),
            );
        }
        let shown = Shown {
            machine: &machine,
            label: &label,
            home,
        };
        page.group(SettingGroup::new().items(pickers))
            .group(mcp_group(&reading.mcp, &shown))
            .groups(hooks_groups(&reading.hooks, &shown))
    }
}

/// The quiet line of a rho too old to list `what`.
pub(super) fn too_old(machine: &str, what: &str) -> SettingItem {
    let text = format!("Rho on {machine} is too old to list {what}. Update it there to see them.");
    SettingItem::render(move |_, _, cx| {
        h_flex()
            .id("configured-too-old")
            .test_support()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text.clone())
    })
}

/// The picker of the project to read for, among `machine`'s projects,
/// set to `workspace`; "None" reads for no project.
pub(super) fn project_picker(
    projects: &Projects,
    machine: &MachineId,
    workspace: Option<&Path>,
    home: Option<&Path>,
    view: WeakEntity<SettingsView>,
) -> SettingItem {
    let mut paths: Vec<PathBuf> = projects
        .roots()
        .filter(|root| root.machine == *machine)
        .map(|root| root.path.clone())
        .collect();
    // The project on screen, even if it is no longer on the list.
    if let Some(workspace) = workspace.filter(|shown| !paths.iter().any(|path| path == shown)) {
        paths.push(workspace.to_path_buf());
    }
    // Options are keyed by position, "0" for none, so no path can stand
    // for another, or for none.
    let options: Vec<(SharedString, SharedString)> = std::iter::once("None".to_owned())
        .chain(paths.iter().map(|path| tilde(path, home)))
        .enumerate()
        .map(|(ix, label)| (ix.to_string().into(), label.into()))
        .collect();
    let current = SharedString::from(
        workspace
            .and_then(|shown| paths.iter().position(|path| path == shown))
            .map_or(0, |ix| ix + 1)
            .to_string(),
    );
    SettingItem::new(
        "Project",
        SettingField::dropdown(
            options,
            move |_: &App| current.clone(),
            move |picked: SharedString, cx: &mut App| {
                let workspace = picked
                    .parse::<usize>()
                    .ok()
                    .and_then(|ix| ix.checked_sub(1))
                    .and_then(|ix| paths.get(ix))
                    .cloned();
                let _ = view.update(cx, |this, cx| this.pick_configured_project(workspace, cx));
            },
        )
        .w(rems(CONTROL_REMS))
        .justify_between(),
    )
    .description(
        "A project's plugins and its hooks file apply only to sessions in it. None lists \
         your own alone.",
    )
}

/// Reads `machine`'s again for `workspace`; it spins while a read runs.
pub(super) fn refresh_button(
    configured: &Entity<Configured>,
    machine: &MachineId,
    workspace: Option<PathBuf>,
) -> impl Fn(&mut gpui_kit::Window, &mut App) -> Button + 'static {
    let (configured, machine) = (configured.clone(), machine.clone());
    let read = {
        let (configured, machine) = (configured.clone(), machine.clone());
        Rc::new(move |cx: &mut App| {
            configured.update(cx, |configured, cx| {
                configured.read(&machine, workspace.clone(), cx);
            });
        })
    };
    move |_, cx| {
        let asking = configured.read(cx).asking(&machine);
        let read = Rc::clone(&read);
        Button::new("configured-refresh")
            .ghost()
            .xsmall()
            .icon(IconName::RotateCw)
            .tooltip("Read again")
            .loading(asking)
            .disabled(asking)
            .on_click(move |_, _, cx| read(cx))
    }
}

/// A row titled `title`, whose control says `status` as `mark` reads, its
/// `lines` under the title. `id` names the status for tests, labelled
/// "title: status".
fn status_item(
    id: String,
    title: impl Into<SharedString>,
    (mark, status): (Mark, &'static str),
    lines: impl IntoIterator<Item = String>,
) -> SettingItem {
    let lines: Vec<String> = lines.into_iter().collect();
    let title: SharedString = title.into();
    let field = status_field(id, format!("{title}: {status}"), mark, status);
    let description = (!lines.is_empty()).then(|| lines.join("\n"));
    described(SettingItem::new(title, field), description)
}

/// A section that couldn't be read: `failed`'s reason, or that the rho is
/// too old for it, as `what` names it.
pub(super) fn unlisted<T>(listed: &Answer<T>, what: &str, machine: &str) -> Option<SettingItem> {
    match listed {
        Answer::Listed(_) => None,
        Answer::TooOld => Some(too_old(machine, what)),
        Answer::Failed(why) => Some(status_item(
            format!("configured-failed-{what}"),
            format!("Couldn't list {what}"),
            (Mark::Bad, "Failed"),
            vec![why.to_string()],
        )),
    }
}

/// The MCP servers, each with where it comes from and how a session
/// reaches it.
fn mcp_group(listed: &Answer<McpListResponse>, shown: &Shown<'_>) -> SettingGroup {
    let group = SettingGroup::new().title("MCP servers");
    if let Some(item) = unlisted(listed, "MCP servers", shown.label) {
        return group.item(item);
    }
    let Answer::Listed(list) = listed else {
        return group;
    };
    let config = tilde(&list.config_path, shown.home);
    if list.servers.is_empty() {
        return group.item(SettingItem::new(
            "None configured",
            value(
                format!("Add them in {config}, or with a plugin."),
                Tone::Muted,
            ),
        ));
    }
    group.items(list.servers.iter().map(|server| mcp_item(server, &config)))
}

fn mcp_item(server: &ConfiguredMcpServer, config: &str) -> SettingItem {
    let source = match &server.source {
        McpServerSource::Config => config.to_owned(),
        McpServerSource::Plugin { plugin } => format!("Plugin {plugin}"),
        _ => "Another source".to_owned(),
    };
    // An invalid server has no transport: its error stands in for it. A
    // command line shows as written, as no rule tells a secret argument
    // from another; a URL hides its credentials and query values. Should
    // rho list a server's environment or headers, show their names only.
    let (source, reach) = match &server.transport {
        Some(McpTransport::Stdio { command, args }) => (
            format!("{source} · stdio"),
            Some(
                std::iter::once(command.as_str())
                    .chain(args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        ),
        Some(McpTransport::StreamableHttp { url }) => {
            (format!("{source} · HTTP"), Some(redacted_url(url)))
        }
        Some(_) => (source, Some("Another transport".to_owned())),
        None => (source, server.error.clone()),
    };
    let status = match (&server.error, server.enabled) {
        (Some(_), _) => (Mark::Bad, "Invalid"),
        (None, true) => (Mark::On, "Enabled"),
        (None, false) => (Mark::Off, "Disabled"),
    };
    status_item(
        format!("mcp-config-{}", server.identity),
        server.identity.clone(),
        status,
        std::iter::once(source).chain(reach),
    )
}

/// A group per hooks file: its status, then the hooks it defines.
fn hooks_groups(listed: &Answer<HooksListResponse>, shown: &Shown<'_>) -> Vec<SettingGroup> {
    if let Some(item) = unlisted(listed, "hooks", shown.label) {
        return vec![SettingGroup::new().title("Hooks").item(item)];
    }
    let Answer::Listed(list) = listed else {
        return Vec::new();
    };
    list.files
        .iter()
        .map(|file| hooks_group(file, shown))
        .collect()
}

fn hooks_group(file: &HooksFile, shown: &Shown<'_>) -> SettingGroup {
    let (title, scope) = match file.scope {
        HooksFileScope::User => ("Your hooks", "user"),
        HooksFileScope::Project => ("Project hooks", "project"),
        _ => ("Hooks", "other"),
    };
    let file_item = status_item(
        format!("hooks-file-{scope}"),
        tilde(&file.path, shown.home),
        file_status(&file.status),
        file_lines(file, shown),
    );
    SettingGroup::new()
        .title(title)
        .item(file_item)
        .items(file.hooks.iter().map(hook_item))
}

fn file_status(status: &HooksFileStatus) -> (Mark, &'static str) {
    match status {
        HooksFileStatus::Absent => (Mark::Off, "No file"),
        HooksFileStatus::Loaded => (Mark::On, "Loaded"),
        HooksFileStatus::Untrusted { .. } => (Mark::Warn, "Untrusted"),
        HooksFileStatus::Invalid { .. } => (Mark::Bad, "Invalid"),
        HooksFileStatus::Disabled { .. } => (Mark::Warn, "Disabled"),
        _ => (Mark::Off, "Unknown"),
    }
}

/// What a hooks file's status means for sessions, and what to do about it.
fn file_lines(file: &HooksFile, shown: &Shown<'_>) -> Vec<String> {
    match &file.status {
        HooksFileStatus::Absent => Vec::new(),
        HooksFileStatus::Loaded => vec![match file.hooks.len() {
            0 => "Defines no hooks.".to_owned(),
            1 => "Sessions run its 1 hook.".to_owned(),
            n => format!("Sessions run its {n} hooks."),
        }],
        HooksFileStatus::Untrusted { error } => {
            let mut lines = vec![format!(
                "Sessions skip it. To trust project hooks, {}",
                trust_hint(
                    shown,
                    "RHO_TRUST_PROJECT_HOOKS",
                    "every project's hooks file"
                )
            )];
            lines.extend(
                error
                    .iter()
                    .map(|error| format!("It can't be loaded: {error}")),
            );
            lines
        }
        HooksFileStatus::Invalid { error } => {
            vec![
                error.clone(),
                "Sessions run no hooks until it's fixed.".to_owned(),
            ]
        }
        HooksFileStatus::Disabled { by } => vec![format!(
            "{} is invalid, so sessions run no hooks.",
            tilde(by, shown.home)
        )],
        _ => vec!["A status this rho-ui doesn't know.".to_owned()],
    }
}

/// How to trust what a project brings on the machine shown: set
/// `variable` where its sessions get their environment, which `trusts`.
pub(super) fn trust_hint(shown: &Shown<'_>, variable: &str, trusts: &str) -> String {
    let place = if shown.machine.is_local() {
        "rho-ui's environment".to_owned()
    } else {
        format!("the login environment on {}", shown.label)
    };
    format!("set {variable}=1 in {place} and restart rho-ui. That trusts {trusts}.")
}

fn hook_item(hook: &HookContract) -> SettingItem {
    let mut status = match hook.status {
        HookStatus::Active => (Mark::On, "Active"),
        HookStatus::Untrusted => (Mark::Off, "Untrusted"),
        HookStatus::Disabled => (Mark::Off, "Disabled"),
        _ => (Mark::Off, "Unknown"),
    };
    if !hook.warnings.is_empty() && matches!(status.0, Mark::On) {
        status.0 = Mark::Warn;
    }
    let when = match &hook.tools {
        HookTools::Matching { patterns } => {
            format!("{} on {}", event_name(hook.event), patterns.join(", "))
        }
        _ => event_name(hook.event).to_owned(),
    };
    let lines = [when, hook.command.join(" ")]
        .into_iter()
        .chain(hook.warnings.iter().filter_map(warning_line));
    status_item(format!("hook-{}", hook.id), hook.id.clone(), status, lines)
}

/// What `warning` means, if this rho-ui knows it.
fn warning_line(warning: &HookWarning) -> Option<String> {
    match warning {
        HookWarning::MissingEditTools { tools } => Some(format!(
            "Doesn't name {}, which a session may edit with instead.",
            tools.join(", ")
        )),
        _ => None,
    }
}

/// `event` by the name a hooks file gives it.
fn event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::SessionStarted => "session_started",
        HookEvent::BeforeToolUse => "before_tool_use",
        HookEvent::AfterToolUse => "after_tool_use",
        HookEvent::RunCompleted => "run_completed",
        HookEvent::RunFailed => "run_failed",
        HookEvent::InputRequested => "input_requested",
        HookEvent::SessionCompleted => "session_completed",
        HookEvent::SessionFailed => "session_failed",
        HookEvent::WorkflowStarted => "workflow_started",
        HookEvent::WorkflowNodeStarted => "workflow_node_started",
        HookEvent::WorkflowNodeFinished => "workflow_node_finished",
        HookEvent::WorkflowCompleted => "workflow_completed",
        HookEvent::WorkflowFailed => "workflow_failed",
        HookEvent::WorkflowCancelled => "workflow_cancelled",
        _ => "another event",
    }
}

/// `url` with what most often holds a token by accident left out, as the
/// page may be screen-shared: its user and password, its query's values,
/// and its fragment, each as "…". The host, path and query keys stay, so
/// servers can still be told apart. A string that isn't a URL keeps its
/// query and fragment the same way.
pub(super) fn redacted_url(url: &str) -> String {
    const HIDDEN: &str = "…";
    let (rest, fragment) = match url.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (url, None),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((rest, query)) => (rest, Some(query)),
        None => (rest, None),
    };
    let mut out = match rest.split_once("://") {
        Some((scheme, after)) => {
            let (authority, path) = after.split_at(after.find('/').unwrap_or(after.len()));
            let host = match authority.rsplit_once('@') {
                Some((_, host)) => format!("{HIDDEN}@{host}"),
                None => authority.to_owned(),
            };
            format!("{scheme}://{host}{path}")
        }
        None => rest.to_owned(),
    };
    if let Some(query) = query {
        let pairs: Vec<String> = query
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((key, "")) => format!("{key}="),
                Some((key, _)) => format!("{key}={HIDDEN}"),
                // A bare item may be the token itself.
                None if pair.is_empty() => String::new(),
                None => HIDDEN.to_owned(),
            })
            .collect();
        out.push('?');
        out.push_str(&pairs.join("&"));
    }
    if let Some(fragment) = fragment {
        out.push('#');
        if !fragment.is_empty() {
            out.push_str(HIDDEN);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::redacted_url;

    /// A token in an HTTP server's URL doesn't show; what tells servers
    /// apart does.
    #[test]
    fn urls_hide_their_credentials_and_query_values() {
        let cases = [
            ("https://api.example.com/mcp", "https://api.example.com/mcp"),
            ("http://localhost:8080", "http://localhost:8080"),
            (
                "https://api.example.com/mcp?key=sk-123",
                "https://api.example.com/mcp?key=…",
            ),
            (
                "https://h.example/mcp?a=1&b=&token",
                "https://h.example/mcp?a=…&b=&…",
            ),
            (
                "https://user:pass@h.example:443/mcp",
                "https://…@h.example:443/mcp",
            ),
            ("https://tok@h.example?x=1", "https://…@h.example?x=…"),
            // An @ in the path is no user.
            ("https://h.example/@team/mcp", "https://h.example/@team/mcp"),
            (
                "https://h.example/cb#access_token=abc",
                "https://h.example/cb#…",
            ),
            ("https://h.example/mcp?", "https://h.example/mcp?"),
        ];
        for (url, shown) in cases {
            assert_eq!(redacted_url(url), shown, "{url}");
        }
    }
}
