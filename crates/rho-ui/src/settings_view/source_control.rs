//! The Source control page: the forge CLIs on the chosen machine, such as
//! GitHub's `gh`, each with where it was found and its version, the
//! program to run instead, and the accounts it is signed in to. Opening the page checks the machine's CLIs if nothing
//! has since it connected; Refresh checks again. Signing in asks questions
//! in a terminal, so the page offers the command to copy rather than
//! running it.
//!
//! Every machine has its own CLIs and sign-ins. With more than one
//! machine, a picker heads the page; this machine shows first.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    clipboard::Clipboard,
    h_flex,
    input::{Input, InputEvent, InputState},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    IconName, Sizable as _,
};
use gpui_kit::{
    div, rems, App, AppContext as _, Context, Entity, InteractiveElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
};

use super::{
    machine_picker, page_header, unreachable_note, update, value, PickedMachine, SettingsView,
    Tone, CONTROL_REMS,
};
use crate::{
    forge::{
        Account, AuthState, Check, CliProbe, CliStatus, ForgeCli, Forges, FoundBy, TokenSource,
    },
    machines::{MachineId, Machines},
    settings::Settings,
    workspaces::{tilde, Workspaces},
};

/// The page's own state: which machine it shows, and each CLI's Program
/// box for that machine.
pub(super) struct SourceControlFields {
    forges: Entity<Forges>,
    workspaces: Entity<Workspaces>,
    machine: PickedMachine,
    /// In [`ForgeCli::ALL`]'s order.
    programs: Vec<ProgramInput>,
    /// The machine whose programs `programs` hold.
    programs_for: Option<MachineId>,
    _subscriptions: [Subscription; 2],
}

/// The Program box of one CLI.
struct ProgramInput {
    cli: ForgeCli,
    state: Entity<InputState>,
    _subscription: Subscription,
}

impl SourceControlFields {
    /// Checks this machine's CLIs, unless they have been since it
    /// connected.
    pub(super) fn new(
        machines: &Entity<Machines>,
        workspaces: Entity<Workspaces>,
        forges: Entity<Forges>,
        cx: &mut Context<'_, SettingsView>,
    ) -> Self {
        forges.update(cx, |forges, cx| {
            forges.ensure_checked(&MachineId::local(), cx);
        });
        let subscriptions = [
            cx.observe(&forges, |_, _, cx| cx.notify()),
            // A machine connecting: check its CLIs, if it is the one shown.
            cx.observe(machines, |this, _, cx| {
                let machine = this.source_control_machine(cx);
                this.source_control
                    .forges
                    .update(cx, |forges, cx| forges.ensure_checked(&machine, cx));
                cx.notify();
            }),
        ];
        Self {
            forges,
            workspaces,
            machine: PickedMachine::new(),
            programs: Vec::new(),
            programs_for: None,
            _subscriptions: subscriptions,
        }
    }
}

impl SettingsView {
    /// The machine the page shows.
    fn source_control_machine(&self, cx: &App) -> MachineId {
        self.source_control
            .machine
            .shown(self.machine_list.read(cx))
    }

    /// Shows `machine`'s CLIs, checking them if nothing has yet.
    fn pick_source_control_machine(&mut self, machine: MachineId, cx: &mut Context<'_, Self>) {
        self.source_control
            .forges
            .update(cx, |forges, cx| forges.ensure_checked(&machine, cx));
        if self.source_control.machine.pick(machine) {
            cx.notify();
        }
    }

    /// Gives each CLI a Program box holding what Settings set for `machine`.
    fn sync_programs(
        &mut self,
        machine: &MachineId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let mut programs = Vec::new();
        for cli in ForgeCli::ALL {
            let path = cx.global::<Settings>().cli_path(machine, cli).to_owned();
            let state = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(format!("Find {} on the machine", cli.program()))
                    .default_value(path)
            });
            let machine = machine.clone();
            let subscription = cx.subscribe_in(
                &state,
                window,
                move |this, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let path = input.read(cx).value().trim().to_owned();
                        this.set_program(&machine, cli, path, cx);
                    }
                },
            );
            programs.push(ProgramInput {
                cli,
                state,
                _subscription: subscription,
            });
        }
        self.source_control.programs = programs;
        self.source_control.programs_for = Some(machine.clone());
    }

    /// Sets the program of `cli` on `machine` and checks it again, unless
    /// that is the program set already.
    fn set_program(
        &mut self,
        machine: &MachineId,
        cli: ForgeCli,
        path: String,
        cx: &mut Context<'_, Self>,
    ) {
        if cx.global::<Settings>().cli_path(machine, cli) == path {
            return;
        }
        update(cx, |settings| settings.set_cli_path(machine, cli, path));
        self.source_control
            .forges
            .update(cx, |forges, cx| forges.recheck(machine, cx));
    }

    /// The page for the shown machine: the picker, then a group per CLI,
    /// or what keeps a remote machine's from showing.
    pub(super) fn source_control_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> SettingPage {
        let machine = self.source_control_machine(cx);
        if self.source_control.programs_for.as_ref() != Some(&machine) {
            self.sync_programs(&machine, window, cx);
        }
        let view = cx.entity().downgrade();
        let machines = self.machine_list.read(cx);
        let mut page = SettingPage::new("Source control")
            .icon(Lucide::GitBranch)
            .header_style(&page_header());
        if machines.several() {
            page = page.group(SettingGroup::new().item(machine_picker(
                machines,
                &machine,
                "Each machine has its own CLIs and sign-ins.",
                view,
                Self::pick_source_control_machine,
            )));
        }
        // This machine's CLIs run whether or not its daemon answers.
        let unreachable = (!machine.is_local())
            .then(|| unreachable_note(machines, &machine))
            .flatten();
        if let Some(why) = unreachable {
            return page.group(
                SettingGroup::new().item(SettingItem::new("Status", value(why, Tone::Muted))),
            );
        }
        let on = if machine.is_local() {
            "this machine".to_owned()
        } else {
            machines.label(&machine).to_string()
        };
        let groups: Vec<SettingGroup> = self
            .source_control
            .programs
            .iter()
            .flat_map(|program| self.cli_groups(program, &machine, &on, cx))
            .collect();
        page.groups(groups)
    }

    /// The groups of `program`'s CLI: its status with Refresh, its Program
    /// box; then, once it runs, the accounts it is
    /// signed in to.
    fn cli_groups(
        &self,
        program: &ProgramInput,
        machine: &MachineId,
        on: &str,
        cx: &App,
    ) -> Vec<SettingGroup> {
        let cli = program.cli;
        let fields = &self.source_control;
        let home = fields.workspaces.read(cx).home(machine);
        let shown = |path: &std::path::Path| tilde(path, home.as_deref());
        let check = fields.forges.read(cx).check(machine, cli);
        let program_name = cli.program();
        let (status, tone, about) = match check {
            None | Some(Check::Checking) => ("Checking…".to_owned(), Tone::Muted, String::new()),
            Some(Check::Failed(why)) => ("Couldn't check".to_owned(), Tone::Muted, why.to_string()),
            Some(Check::Done(CliProbe::Missing)) => (
                "Not found".to_owned(),
                Tone::Muted,
                format!(
                    "Install it from {}.",
                    cli.install_url().trim_start_matches("https://")
                ),
            ),
            Some(Check::Done(CliProbe::Broken {
                path,
                found_by,
                why,
            })) => (
                "Doesn't run".to_owned(),
                Tone::Muted,
                format!("{} · {why}", found(*found_by, &shown(path))),
            ),
            Some(Check::Done(CliProbe::Found(status))) => (
                format!("{program_name} {}", status.version),
                Tone::Plain,
                found(status.found_by, &shown(&status.path)),
            ),
        };
        let forges = fields.forges.clone();
        let refreshed = machine.clone();
        let status_id = SharedString::from(format!("source-control-{program_name}-status"));
        let refresh_id = SharedString::from(format!("source-control-{program_name}-refresh"));
        let status_text = SharedString::from(status);
        let status_item = SettingItem::new(
            "Status",
            SettingField::render(move |_, _, cx| {
                let (forges, machine) = (forges.clone(), refreshed.clone());
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id(status_id.clone())
                            .test_support()
                            .aria_label(status_text.clone())
                            .text_sm()
                            .text_color(tone.color(cx))
                            .child(status_text.clone()),
                    )
                    .child(
                        Button::new(refresh_id.clone())
                            .ghost()
                            .xsmall()
                            .icon(IconName::RotateCw)
                            .tooltip("Check again")
                            .on_click(move |_, _, cx| {
                                forges.update(cx, |forges, cx| forges.recheck(&machine, cx));
                            }),
                    )
            }),
        )
        .description(about);

        let group = SettingGroup::new()
            .title(cli.label())
            .item(status_item)
            .item(program_item(program));
        let Some(Check::Done(CliProbe::Found(status))) = check else {
            return vec![group];
        };
        let accounts = SettingGroup::new()
            .title(format!("{} accounts", cli.forge_name()))
            .items(account_items(cli, status, on));
        vec![group, accounts]
    }
}

/// A CLI's Program box, applied on Enter or when it loses the keyboard.
fn program_item(program: &ProgramInput) -> SettingItem {
    let state = program.state.clone();
    let name = program.cli.program();
    let id = SharedString::from(format!("setting-{name}-program"));
    SettingItem::new(
        "Program",
        SettingField::render(move |options, _, _| {
            Input::new(&state)
                .id(id.clone())
                .with_size(options.size())
                .w(rems(CONTROL_REMS))
        }),
    )
    .description(format!("Leave empty to find {name} on its own."))
}

/// The program's path, and how it was found unless Program set it.
fn found(found_by: FoundBy, path: &str) -> String {
    match found_by {
        FoundBy::Setting => path.to_owned(),
        FoundBy::Path => format!("{path} · on the PATH"),
        FoundBy::LoginShell => format!("{path} · via your login shell"),
    }
}

/// An item per account `status` knows, each saying whose it is and how it
/// is signed in; with none, how to sign in. `on` names the machine, as
/// "this machine" or its label.
fn account_items(cli: ForgeCli, status: &CliStatus, on: &str) -> Vec<SettingItem> {
    if status.accounts.is_empty() {
        let command = SharedString::from(cli.login_command(None));
        let description = format!(
            "Sign in from a terminal on {on} (the button copies the command), then Refresh."
        );
        let field = SettingField::render(move |_, _, cx| {
            h_flex()
                .gap_2()
                .child(
                    div()
                        .id("source-control-no-accounts")
                        .test_support()
                        .text_sm()
                        .text_color(Tone::Muted.color(cx))
                        .child("Not signed in"),
                )
                .child(
                    Clipboard::new("source-control-sign-in")
                        .value(command.clone())
                        .tooltip(format!("Copy {command}")),
                )
        });
        return vec![SettingItem::new("Account", field).description(description)];
    }
    status
        .accounts
        .iter()
        .enumerate()
        .map(|(ix, account)| account_item(cli, ix, account, status, on))
        .collect()
}

/// One account: its host, whose it is, and what `gh` says of it.
fn account_item(
    cli: ForgeCli,
    ix: usize,
    account: &Account,
    status: &CliStatus,
    on: &str,
) -> SettingItem {
    let shared_host = status
        .accounts
        .iter()
        .filter(|other| other.host == account.host)
        .count()
        > 1;
    let who: SharedString = if account.login.is_empty() {
        "Not signed in".into()
    } else {
        format!("@{}", account.login).into()
    };
    // A token from a variable outlasts a new sign-in, as the CLI prefers
    // the variable; it has to change where it is set.
    let (tone, sign_in, description) = match (&account.auth, &account.token_source) {
        (AuthState::Refused(why), Some(TokenSource::Variable(variable))) => (
            Tone::Muted,
            None,
            format!("{why} It comes from {variable} on {on}; change it there."),
        ),
        (AuthState::Refused(why), _) => (
            Tone::Muted,
            Some(cli.login_command(Some(&account.host))),
            format!("{why} Sign in again from a terminal on {on}; the button copies the command."),
        ),
        (AuthState::Unreachable(why), _) => (
            Tone::Muted,
            None,
            format!("Couldn't reach {}: {why}", account.host),
        ),
        (AuthState::SignedIn, source) => {
            let state = if shared_host && account.active {
                "Signed in, active"
            } else {
                "Signed in"
            };
            let protocol = account.git_protocol.as_ref().map(|p| p.to_uppercase());
            let token = source.as_ref().map(token_phrase);
            let line = [Some(state.to_owned()), protocol, token]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            let description = match &account.scopes {
                Some(scopes) => format!("{line}\nScopes: {scopes}"),
                None => line,
            };
            (Tone::Plain, None, description)
        }
    };
    let field = account_field(cli, ix, who, tone, sign_in);
    SettingItem::new(account.host.clone(), field).description(description)
}

/// Where a token is kept, as the account's details say it.
fn token_phrase(source: &TokenSource) -> String {
    match source {
        TokenSource::Variable(name) => format!("token from {name}"),
        TokenSource::Keyring => "token in the keyring".to_owned(),
        TokenSource::File(path) => {
            let name = std::path::Path::new(path.as_ref()).file_name().map_or_else(
                || path.to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            format!("token in {name}")
        }
    }
}

/// Whose account `ix` is, as `who`, with a button that copies `sign_in`,
/// the command that signs it in, when it needs one.
fn account_field(
    cli: ForgeCli,
    ix: usize,
    who: SharedString,
    tone: Tone,
    sign_in: Option<String>,
) -> SettingField<SharedString> {
    let id = SharedString::from(format!("source-control-{}-account-{ix}", cli.program()));
    let copy_id = SharedString::from(format!("source-control-{}-sign-in-{ix}", cli.program()));
    let sign_in = sign_in.map(SharedString::from);
    SettingField::render(move |_, _, cx| {
        h_flex()
            .gap_2()
            .child(
                div()
                    .id(id.clone())
                    .test_support()
                    .aria_label(who.clone())
                    .text_sm()
                    .text_color(tone.color(cx))
                    .child(who.clone()),
            )
            .children(sign_in.clone().map(|command| {
                Clipboard::new(copy_id.clone())
                    .value(command.clone())
                    .tooltip(format!("Copy {command}"))
            }))
    })
}
