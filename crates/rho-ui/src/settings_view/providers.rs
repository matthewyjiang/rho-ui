//! The Providers page: the auth profiles the chosen machine's Rho is
//! signed in to, each with Sign out, and Add provider, a menu of the rest.
//! One picked there joins the list to be signed in (see
//! [`crate::sign_in`]), and leaves it again if removed first; a profile
//! signed out goes back to the menu. Below is the credential store Rho
//! keeps them in, which can be changed here unless `RHO_CREDENTIAL_STORE`
//! chooses it.
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
    menu::{DropdownMenu as _, PopupMenuItem},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    v_flex, ActiveTheme as _, Disableable as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, Anchor, App, Axis, Context, Entity, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, Subscription, WeakEntity, Window,
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

/// The page's own state: which machine it shows, and the profiles being
/// added there.
pub(super) struct ProvidersFields {
    auths: Entity<Auths>,
    machine: PickedMachine,
    adding: Vec<Adding>,
    /// The machine whose profiles `adding` are.
    adding_on: Option<MachineId>,
    _subscriptions: [Subscription; 2],
}

/// A signed-out profile listed to be signed in: one picked from Add
/// provider, or with a login under way. It stays until it is signed in or
/// removed.
struct Adding {
    auth: String,
    /// The key box, of a profile that takes an API key.
    key: Option<KeyInput>,
}

/// The key box of one API-key profile.
struct KeyInput {
    state: Entity<InputState>,
    _subscription: Subscription,
}

impl KeyInput {
    /// A key box for `auth` on `machine`, which Enter saves.
    fn new(
        machine: &MachineId,
        auth: &str,
        window: &mut Window,
        cx: &mut Context<'_, SettingsView>,
    ) -> Self {
        let state = sign_in::key_box(window, cx);
        let (saved_machine, saved_auth) = (machine.clone(), auth.to_owned());
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
            adding: Vec::new(),
            adding_on: None,
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

    /// Keeps the profiles being added those of `machine` that are still
    /// signed out, with any whose login is under way, as one started from
    /// a session is.
    fn sync_adding(&mut self, machine: &MachineId, cx: &App) {
        let fields = &mut self.providers;
        if fields.adding_on.as_ref() != Some(machine) {
            fields.adding.clear();
            fields.adding_on = Some(machine.clone());
        }
        let auths = fields.auths.read(cx);
        // What is listed stays while the profiles are read.
        if !matches!(auths.profiles(machine), Some(Profiles::Ready(_))) {
            return;
        }
        let signed_out = |auth: &str| {
            auths
                .profile(machine, auth)
                .is_some_and(|profile| profile.status != AuthStatus::SignedIn)
        };
        fields.adding.retain(|adding| signed_out(&adding.auth));
        if let Some(login) = auths.login(machine) {
            let listed = fields.adding.iter().any(|adding| adding.auth == login.auth);
            if !listed && signed_out(&login.auth) {
                fields.adding.push(Adding {
                    auth: login.auth.clone(),
                    key: None,
                });
            }
        }
    }

    /// Lists `profile` of `machine`, picked from Add provider, to be signed
    /// in: an API key's box takes the keyboard, and a login starts.
    fn add_provider(
        &mut self,
        machine: &MachineId,
        profile: &AuthSummary,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let key = if profile.kind == AuthKind::ApiKey {
            let key = KeyInput::new(machine, &profile.auth, window, cx);
            key.state.update(cx, |input, cx| input.focus(window, cx));
            Some(key)
        } else {
            self.providers
                .auths
                .update(cx, |auths, cx| auths.login_to(machine, &profile.auth, cx));
            None
        };
        self.providers.adding.push(Adding {
            auth: profile.auth.clone(),
            key,
        });
        cx.notify();
    }

    /// Takes `auth`, not signed in after all, back off the list, with what
    /// came of its last try.
    fn remove_provider(&mut self, machine: &MachineId, auth: &str, cx: &mut Context<'_, Self>) {
        self.providers.adding.retain(|adding| adding.auth != auth);
        self.providers
            .auths
            .update(cx, |auths, cx| auths.dismiss(machine, auth, cx));
        cx.notify();
    }

    pub(super) fn providers_page(
        &mut self,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> SettingPage {
        let machine = self.providers_machine(cx);
        self.sync_adding(&machine, cx);
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
                view.clone(),
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
        let adding = &self.providers.adding;
        let is_adding = |profile: &AuthSummary| adding.iter().any(|a| a.auth == profile.auth);
        // Signed in, in the daemon's order, then those being added, in the
        // order picked, beside the menu that added them.
        let signed_in = profiles
            .iter()
            .filter(|profile| profile.status == AuthStatus::SignedIn);
        let being_added = adding
            .iter()
            .filter_map(|adding| profiles.iter().find(|profile| profile.auth == adding.auth));
        let mut items: Vec<SettingItem> = signed_in
            .chain(being_added)
            .map(|profile| self.profile_item(&machine, profile, &view, cx))
            .collect();
        let offered: Vec<AuthSummary> = profiles
            .iter()
            .filter(|profile| profile.status != AuthStatus::SignedIn && !is_adding(profile))
            .filter(|profile| MenuSection::of(profile).is_some())
            .cloned()
            .collect();
        if !offered.is_empty() {
            let description = if items.is_empty() {
                "Rho isn't signed in to any provider here. Add one with an account or an API key."
            } else {
                "Sign in to another, with an account or an API key."
            };
            items.push(add_item(&machine, offered, description, view));
        }
        if items.is_empty() {
            items.push(SettingItem::new(
                "Status",
                value(format!("Rho on {label} has no providers."), Tone::Muted),
            ));
        }
        page = page.group(SettingGroup::new().title("Sign-ins").items(items));
        if let Some(store) = auths.store(&machine) {
            let store = self.store_item(&machine, store, cx);
            page = page.group(SettingGroup::new().title("Storage").item(store));
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
            StoreSelectedBy::Config => {
                "Chosen in Rho's config. Sign-ins saved before a change stay where they were."
            }
            _ => "Chosen by something this version of rho-ui doesn't know.",
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
        // A store or a choice this version doesn't know is shown, not
        // changed.
        let known = STORES.iter().find(|(kind, _, _)| *kind == store.store);
        let changeable = matches!(
            store.selected_by,
            StoreSelectedBy::Default | StoreSelectedBy::Config
        );
        let field = match known {
            Some(&(_, current, _)) if changeable => {
                let choices = STORES
                    .map(|(_, value, name)| (SharedString::from(value), SharedString::from(name)));
                let (auths, machine) = (self.providers.auths.clone(), machine.clone());
                SettingField::dropdown(
                    choices.to_vec(),
                    move |_: &App| SharedString::from(current),
                    move |chosen: SharedString, cx: &mut App| {
                        let Some(&(kind, _, _)) =
                            STORES.iter().find(|(_, value, _)| *value == &*chosen)
                        else {
                            return;
                        };
                        auths.update(cx, |auths, cx| auths.choose_store(&machine, kind, cx));
                    },
                )
                .w(rems(CONTROL_REMS))
                .justify_between()
            }
            Some(&(_, _, name)) => value(name, Tone::Plain),
            None => value("Another store", Tone::Plain),
        };
        SettingItem::new("Credential store", field).description(description)
    }

    /// One listed profile: its label, provider and how it signs in, with
    /// Sign out once signed in, else the controls that sign it in and a
    /// button that takes it off the list. A login under way takes the
    /// row's whole width, for its page and code.
    fn profile_item(
        &self,
        machine: &MachineId,
        profile: &AuthSummary,
        view: &WeakEntity<Self>,
        cx: &App,
    ) -> SettingItem {
        let auths = &self.providers.auths;
        let ids = format!("providers-{}", profile.auth);
        let state = auths.read(cx);
        let note = state.note(machine, &profile.auth).cloned();
        let logging_in = state
            .login(machine)
            .is_some_and(|login| login.auth == profile.auth);
        let keyword = profile_keyword(profile);
        let description = format!("{} · {}", profile.provider, how(profile.kind));
        let title = SharedString::from(profile.label.clone());
        let key = self
            .providers
            .adding
            .iter()
            .find(|adding| adding.auth == profile.auth)
            .and_then(|adding| adding.key.as_ref())
            .map(|key| key.state.clone());
        let (auths, machine, profile, view) = (
            auths.clone(),
            machine.clone(),
            profile.clone(),
            view.clone(),
        );
        let field = SettingField::render(move |_, _, cx| {
            let controls = if profile.status == AuthStatus::SignedIn {
                signed_in(&ids, &auths, &machine, &profile, cx)
            } else {
                let controls =
                    sign_in::controls(&ids, &auths, &machine, &profile, key.as_ref(), cx);
                // Cancel ends a login; only then can the row go.
                let remove = (!logging_in).then(|| {
                    let busy = auths.read(cx).is_busy(&machine, &profile.auth);
                    let (view, machine, auth) =
                        (view.clone(), machine.clone(), profile.auth.clone());
                    Button::new(SharedString::from(format!("{ids}-remove")))
                        .ghost()
                        .small()
                        .icon(Lucide::Close)
                        .tooltip("Remove from the list")
                        .accessibility_label("Remove")
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.remove_provider(&machine, &auth, cx);
                            });
                        })
                });
                h_flex()
                    .gap_1()
                    .child(div().flex_1().min_w_0().child(controls))
                    .children(remove)
                    .into_any_element()
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

/// How a profile of `kind` signs in, in a few words.
fn how(kind: AuthKind) -> &'static str {
    match kind {
        AuthKind::ApiKey => "API key",
        AuthKind::Login => "account login",
        _ => "other",
    }
}

/// Add provider: a menu of `offered`, the profiles of `machine` neither
/// signed in nor being added, accounts first, then API keys. The page's
/// search finds it by any of them.
fn add_item(
    machine: &MachineId,
    offered: Vec<AuthSummary>,
    description: &'static str,
    view: WeakEntity<SettingsView>,
) -> SettingItem {
    let keywords: Vec<SharedString> = offered.iter().map(profile_keyword).collect();
    let machine = machine.clone();
    let field = SettingField::render(move |_, _, _| {
        let (machine, offered, view) = (machine.clone(), offered.clone(), view.clone());
        Button::new("providers-add")
            .outline()
            .small()
            .w(rems(CONTROL_REMS))
            .icon(Lucide::Plus)
            .label("Add provider")
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, _| {
                // As wide as its button; a long list scrolls.
                let mut menu = menu
                    .min_w(window.rem_size() * CONTROL_REMS)
                    .scrollable(true);
                let mut last = None;
                for section in MenuSection::ALL {
                    for profile in &offered {
                        if MenuSection::of(profile) != Some(section) {
                            continue;
                        }
                        if last != Some(section) {
                            if last.is_some() {
                                menu = menu.separator();
                            }
                            menu = menu.label(section.title());
                            last = Some(section);
                        }
                        let (view, machine, profile) =
                            (view.clone(), machine.clone(), profile.clone());
                        menu = menu.item(PopupMenuItem::new(profile.label.clone()).on_click(
                            move |_, window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.add_provider(&machine, &profile, window, cx);
                                });
                            },
                        ));
                    }
                }
                menu
            })
    });
    SettingItem::new("Add a provider", field)
        .description(description)
        .keywords(keywords)
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

/// The stores the page offers: each one's value in the dropdown, and its
/// name.
const STORES: [(CredentialStoreKind, &str, &str); 2] = [
    (CredentialStoreKind::Os, "os", "OS keyring"),
    (CredentialStoreKind::File, "file", "File"),
];

/// The sections of Add provider's menu, in its order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuSection {
    Account,
    ApiKey,
}

impl MenuSection {
    const ALL: [MenuSection; 2] = [MenuSection::Account, MenuSection::ApiKey];

    /// The section that offers `profile`; none for one that signs in some
    /// way this version doesn't know.
    fn of(profile: &AuthSummary) -> Option<Self> {
        match profile.kind {
            AuthKind::Login => Some(Self::Account),
            AuthKind::ApiKey => Some(Self::ApiKey),
            _ => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Account => "Accounts",
            Self::ApiKey => "API keys",
        }
    }
}
