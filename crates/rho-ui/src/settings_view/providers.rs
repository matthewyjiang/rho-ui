//! The Providers page: the auth profiles the chosen machine's Rho can run
//! sessions on, signed in first, then those that take an API key, then
//! those that take an account's login, each signed in or out from here
//! (see [`crate::sign_in`]); and the credential store Rho keeps them in,
//! which can be chosen while no setting chose it, or Rho can't reach it.
//!
//! Every machine's Rho has its own sign-ins. With more than one machine, a
//! picker heads the page; this machine shows first. Opening the page, or
//! picking a machine, reads its sign-ins again, as a sign-in made in a
//! terminal reaches rho-ui no other way.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, InputState},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    v_flex, ActiveTheme as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, App, Axis, Context, Entity, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, Subscription, Window,
};
use rho_ui_core::{
    AuthKind, AuthStatus, AuthSummary, CredentialStoreKind, CredentialStoreStatus, StoreSelectedBy,
};

use super::{
    machine_picker, page_header, unreachable_note, value, PickedMachine, SettingsView, Tone,
    CONTROL_REMS,
};
use crate::{
    auths::{Auths, Profiles},
    machines::{MachineId, Machines},
    sign_in,
};

/// The page's own state: which machine it shows, and a key box for each
/// of that machine's API-key profiles.
pub(super) struct ProvidersFields {
    auths: Entity<Auths>,
    machine: PickedMachine,
    keys: Vec<KeyInput>,
    /// The machine whose profiles `keys` are for.
    keys_for: Option<MachineId>,
    _subscriptions: [Subscription; 2],
}

/// The key box of one API-key profile.
struct KeyInput {
    auth: String,
    state: Entity<InputState>,
    _subscription: Subscription,
}

impl KeyInput {
    /// A key box for `auth` on `machine`, which Enter saves.
    fn new(
        machine: &MachineId,
        auth: String,
        window: &mut Window,
        cx: &mut Context<'_, SettingsView>,
    ) -> Self {
        let state = sign_in::key_box(window, cx);
        let (saved_machine, saved_auth) = (machine.clone(), auth.clone());
        let subscription = cx.subscribe_in(
            &state,
            window,
            move |this, key, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let auths = this.providers.auths.clone();
                    sign_in::save_key(&auths, &saved_machine, &saved_auth, key, window, cx);
                }
            },
        );
        Self {
            auth,
            state,
            _subscription: subscription,
        }
    }
}

impl ProvidersFields {
    /// Reads this machine's sign-ins afresh.
    pub(super) fn new(
        machines: &Entity<Machines>,
        auths: Entity<Auths>,
        cx: &mut Context<'_, SettingsView>,
    ) -> Self {
        auths.update(cx, |auths, cx| auths.refresh(&MachineId::local(), cx));
        let subscriptions = [
            cx.observe(&auths, |_, _, cx| cx.notify()),
            // A machine connecting, failing, or leaving the list.
            cx.observe(machines, |_, _, cx| cx.notify()),
        ];
        Self {
            auths,
            machine: PickedMachine::new(),
            keys: Vec::new(),
            keys_for: None,
            _subscriptions: subscriptions,
        }
    }
}

impl SettingsView {
    /// The machine the page shows.
    fn providers_machine(&self, cx: &App) -> MachineId {
        self.providers.machine.shown(self.machine_list.read(cx))
    }

    /// Shows `machine`'s sign-ins, read afresh.
    fn pick_providers_machine(&mut self, machine: MachineId, cx: &mut Context<'_, Self>) {
        self.providers
            .auths
            .update(cx, |auths, cx| auths.refresh(&machine, cx));
        if self.providers.machine.pick(machine) {
            cx.notify();
        }
    }

    /// Gives each API-key profile of `machine` a key box, keeping those it
    /// had, so a key half typed survives the list being read again.
    fn sync_keys(&mut self, machine: &MachineId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let wanted: Vec<String> = match self.providers.auths.read(cx).profiles(machine) {
            Some(Profiles::Ready(profiles)) => profiles
                .iter()
                .filter(|profile| profile.kind == AuthKind::ApiKey)
                .map(|profile| profile.auth.clone())
                .collect(),
            _ => Vec::new(),
        };
        let same_machine = self.providers.keys_for.as_ref() == Some(machine);
        let mut old = if same_machine {
            std::mem::take(&mut self.providers.keys)
        } else {
            Vec::new()
        };
        let mut keys = Vec::with_capacity(wanted.len());
        for auth in wanted {
            match old.iter().position(|key| key.auth == auth) {
                Some(ix) => keys.push(old.swap_remove(ix)),
                None => keys.push(KeyInput::new(machine, auth, window, cx)),
            }
        }
        self.providers.keys = keys;
        self.providers.keys_for = Some(machine.clone());
    }

    pub(super) fn providers_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> SettingPage {
        let machine = self.providers_machine(cx);
        self.sync_keys(&machine, window, cx);
        let view = cx.entity().downgrade();
        let machines = self.machine_list.read(cx);
        let label = machines.label(&machine);
        let mut page = SettingPage::new("Providers")
            .icon(Lucide::KeyRound)
            .header_style(&page_header());
        if machines.several() {
            page = page.group(SettingGroup::new().item(machine_picker(
                machines,
                &machine,
                "Each machine's Rho keeps its own sign-ins.",
                view,
                Self::pick_providers_machine,
            )));
        }
        let status = |text: String| {
            SettingGroup::new().item(SettingItem::new("Status", value(text, Tone::Muted)))
        };
        if let Some(why) = unreachable_note(machines, &machine) {
            return page.group(status(why));
        }
        let auths = self.providers.auths.read(cx);
        let profiles = match auths.profiles(&machine) {
            Some(Profiles::Ready(profiles)) => profiles,
            Some(Profiles::Failed(why)) => {
                return page.group(status(format!(
                    "Couldn't read the sign-ins on {label}: {why}"
                )));
            }
            Some(Profiles::Reading) | None => {
                return page.group(status(format!("Reading the sign-ins on {label}…")));
            }
        };
        if let Some(store) = auths.store(&machine) {
            page = page.group(SettingGroup::new().item(self.store_item(&machine, store, cx)));
        }
        let groups = [
            ("Signed in", Some(AuthStatus::SignedIn), None),
            ("API keys", None, Some(AuthKind::ApiKey)),
            ("Accounts", None, Some(AuthKind::Login)),
        ];
        for (title, status, kind) in groups {
            let items: Vec<SettingItem> = profiles
                .iter()
                .filter(|profile| match (status, kind) {
                    (Some(status), _) => profile.status == status,
                    (None, Some(kind)) => {
                        profile.status != AuthStatus::SignedIn && profile.kind == kind
                    }
                    (None, None) => false,
                })
                .map(|profile| self.profile_item(&machine, profile, cx))
                .collect();
            if !items.is_empty() {
                page = page.group(SettingGroup::new().title(title).items(items));
            }
        }
        page
    }

    /// Where `machine`'s Rho keeps credentials, with a choice of the other
    /// store unless a variable chooses it.
    fn store_item(
        &self,
        machine: &MachineId,
        store: &CredentialStoreStatus,
        cx: &App,
    ) -> SettingItem {
        let chosen_by = match store.selected_by {
            StoreSelectedBy::Default => {
                "Nothing has chosen one, so Rho uses the OS keyring. Choose where new sign-ins go."
            }
            StoreSelectedBy::Environment => "RHO_CREDENTIAL_STORE chooses it; change it there.",
            _ => "Chosen in Rho's config. Sign-ins saved before a change stay where they were.",
        };
        let notes = [
            store
                .unreachable
                .as_ref()
                .map(|why| format!("Rho can't reach it: {why}")),
            self.providers
                .auths
                .read(cx)
                .store_problem(machine)
                .map(|why| format!("Couldn't change it: {why}")),
        ];
        let description = std::iter::once(chosen_by.to_owned())
            .chain(notes.into_iter().flatten())
            .collect::<Vec<_>>()
            .join("\n");
        let field = if store.selected_by == StoreSelectedBy::Environment {
            value(store_name(store.store), Tone::Plain)
        } else {
            let choices = [CredentialStoreKind::Os, CredentialStoreKind::File]
                .map(|kind| (store_value(kind), SharedString::from(store_name(kind))));
            let current = store_value(store.store);
            let (auths, machine) = (self.providers.auths.clone(), machine.clone());
            SettingField::dropdown(
                choices.to_vec(),
                move |_: &App| current.clone(),
                move |chosen: SharedString, cx: &mut App| {
                    let kind = if chosen == store_value(CredentialStoreKind::File) {
                        CredentialStoreKind::File
                    } else {
                        CredentialStoreKind::Os
                    };
                    auths.update(cx, |auths, cx| auths.choose_store(&machine, kind, cx));
                },
            )
            .w(rems(CONTROL_REMS))
            .justify_between()
        };
        SettingItem::new("Credential store", field).description(description)
    }

    /// One profile: its label, provider and how it signs in, with Sign out
    /// once signed in, else the controls that sign it in. A login under
    /// way takes the row's whole width, for its page and code.
    fn profile_item(&self, machine: &MachineId, profile: &AuthSummary, cx: &App) -> SettingItem {
        let auths = &self.providers.auths;
        let ids = format!("providers-{}", profile.auth);
        let how = match profile.kind {
            AuthKind::ApiKey => "API key",
            AuthKind::Login => "account login",
            _ => "other",
        };
        let state = auths.read(cx);
        let note = state.note(machine, &profile.auth).cloned();
        let logging_in = state
            .login(machine)
            .is_some_and(|login| login.auth == profile.auth);
        let keyword = profile_keyword(profile);
        let (auths, machine, profile) = (auths.clone(), machine.clone(), profile.clone());
        let key = self
            .providers
            .keys
            .iter()
            .find(|key| key.auth == profile.auth)
            .map(|key| key.state.clone());
        let description = format!("{} · {how}", profile.provider);
        let title = SharedString::from(profile.label.clone());
        let field = SettingField::render(move |_, _, cx| {
            let controls = if profile.status == AuthStatus::SignedIn {
                signed_in(&ids, &auths, &machine, &profile, cx)
            } else {
                sign_in::controls(&ids, &auths, &machine, &profile, key.as_ref(), cx)
            };
            v_flex()
                .id(SharedString::from(ids.clone()))
                .test_support()
                .gap_1()
                .when(!logging_in, |this| this.w(rems(CONTROL_REMS)))
                .child(controls)
                .when_some(note.as_ref(), |this, note| {
                    this.child(sign_in::note_line(&ids, note, cx))
                })
        });
        let item = SettingItem::new(title, field)
            .description(description)
            .keywords([keyword]);
        if logging_in {
            item.layout(Axis::Vertical)
        } else {
            item
        }
    }
}

/// What finds a profile in the page's search besides its label.
fn profile_keyword(profile: &AuthSummary) -> SharedString {
    SharedString::from(format!("{} {}", profile.provider, profile.auth))
}

/// A signed-in profile's row: that it is, and Sign out.
fn signed_in(
    ids: &str,
    auths: &Entity<Auths>,
    machine: &MachineId,
    profile: &AuthSummary,
    cx: &App,
) -> gpui_kit::AnyElement {
    let busy = auths.read(cx).is_busy(machine, &profile.auth);
    let (auths, machine, auth) = (auths.clone(), machine.clone(), profile.auth.clone());
    h_flex()
        .gap_2()
        .justify_between()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().success)
                .child("Signed in"),
        )
        .child(
            Button::new(SharedString::from(format!("{ids}-logout")))
                .ghost()
                .small()
                .label("Sign out")
                .loading(busy)
                .on_click(move |_, _, cx| {
                    auths.update(cx, |auths, cx| auths.logout(&machine, &auth, cx));
                }),
        )
        .into_any_element()
}

/// A store's value in the dropdown.
fn store_value(kind: CredentialStoreKind) -> SharedString {
    match kind {
        CredentialStoreKind::File => "file".into(),
        _ => "os".into(),
    }
}

/// A store as the page names it.
fn store_name(kind: CredentialStoreKind) -> &'static str {
    match kind {
        CredentialStoreKind::Os => "OS keyring",
        CredentialStoreKind::File => "File",
        _ => "Another store",
    }
}
