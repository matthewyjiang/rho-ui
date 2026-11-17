//! Signing in to an auth profile, wherever one is needed: the controls a
//! row of Settings → Providers shows, and the card a session's view shows
//! above its composer while its model's profile is signed out
//! ([`SignInSlot`]), so the user signs in without leaving the session.
//!
//! An API-key profile takes its key in a masked box, saved with Save or
//! Enter; the box empties as it goes, so the key doesn't linger on screen.
//! A login profile has Sign in, which starts the daemon's login: rho-ui
//! opens its page in the browser and, until the login ends, shows the page
//! and any code to enter there, both to copy, with Cancel.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    clipboard::Clipboard,
    h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    v_flex, ActiveTheme as _, Disableable as _, Icon, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, Styled as _, Subscription, Window,
};
use rho_ui_core::{AuthKind, AuthStatus, AuthSummary, LoginPrompt};

use crate::{
    auths::{login_page, opens_in_browser, Auths, Note},
    machines::MachineId,
    workspaces,
};

/// A masked box for an API key.
pub(crate) fn key_box(window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder("API key")
    })
}

/// Saves the key in `key` for `auth` on `machine`, and empties the box;
/// unless the box is empty, or a request for `auth` is on its way, which
/// leaves the box as it is.
pub(crate) fn save_key(
    auths: &Entity<Auths>,
    machine: &MachineId,
    auth: &str,
    key: &Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) {
    let text = key.read(cx).value().to_string();
    if text.trim().is_empty() || auths.read(cx).is_busy(machine, auth) {
        return;
    }
    key.update(cx, |key, cx| key.set_value("", window, cx));
    auths.update(cx, |auths, cx| auths.save_key(machine, auth, text, cx));
}

/// The controls that sign `profile` in on `machine`, by its kind: `key`
/// and Save for an API key, or Sign in for a login, which gives way to
/// the login's page and code while it waits. `ids` starts each element's
/// ID, so two places that show one profile tell theirs apart.
pub(crate) fn controls(
    ids: &str,
    auths: &Entity<Auths>,
    machine: &MachineId,
    profile: &AuthSummary,
    key: Option<&Entity<InputState>>,
    cx: &App,
) -> AnyElement {
    let state = auths.read(cx);
    let busy = state.is_busy(machine, &profile.auth);
    let auth = profile.auth.clone();
    match (profile.kind, key) {
        (AuthKind::ApiKey, Some(key)) => {
            let (auths, machine, key_state) = (auths.clone(), machine.clone(), key.clone());
            h_flex()
                .gap_2()
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(key)
                            .id(SharedString::from(format!("{ids}-key")))
                            .small()
                            .mask_toggle(),
                    ),
                )
                .child(
                    Button::new(SharedString::from(format!("{ids}-save")))
                        .primary()
                        .small()
                        .label("Save")
                        .loading(busy)
                        .on_click(move |_, window, cx| {
                            save_key(&auths, &machine, &auth, &key_state, window, cx);
                        }),
                )
                .into_any_element()
        }
        (AuthKind::Login, _) => match state.login(machine) {
            Some(login) if login.auth == profile.auth => {
                login_progress(ids, auths, machine, login.prompt(), cx)
            }
            other => {
                let (auths, machine) = (auths.clone(), machine.clone());
                Button::new(SharedString::from(format!("{ids}-login")))
                    .primary()
                    .small()
                    .label("Sign in")
                    // One login at a time per machine.
                    .disabled(other.is_some())
                    .on_click(move |_, _, cx| {
                        auths.update(cx, |auths, cx| auths.login_to(&machine, &auth, cx));
                    })
                    .into_any_element()
            }
        },
        _ => gpui_kit::Empty.into_any_element(),
    }
}

/// Why a login without a device code may not finish, for `machine`'s
/// daemon reached through a link, a remote one or in a browser this
/// machine's: it signs in with a device code where the provider has one
/// (`Daemon::login`), and a provider without sends the browser back to the
/// daemon's own machine, which a browser elsewhere can't reach. `None` for
/// a daemon rho-ui reaches here.
fn browser_elsewhere(machine: &MachineId) -> Option<String> {
    if workspaces::reads_here(machine) {
        return None;
    }
    let on = if machine.is_local() {
        "the machine `rho-ui serve` runs on".to_owned()
    } else {
        machine.to_string()
    };
    Some(format!(
        "If the browser can't finish, this provider signs in only from a browser on {on}; use \
         an API key instead."
    ))
}

/// A login under way: until its prompt arrives, that it is starting;
/// then what to do, the page with a button to copy it, any code to enter
/// there, also to copy, and a spinner while it waits. Cancel stops it.
fn login_progress(
    ids: &str,
    auths: &Entity<Auths>,
    machine: &MachineId,
    prompt: Option<&LoginPrompt>,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let cancel = {
        let (auths, machine) = (auths.clone(), machine.clone());
        Button::new(SharedString::from(format!("{ids}-cancel")))
            .ghost()
            .small()
            .label("Cancel")
            .on_click(move |_, _, cx| {
                auths.update(cx, |auths, cx| auths.cancel_login(&machine, cx));
            })
    };
    let waiting = |text: &'static str| {
        h_flex()
            .gap_2()
            .text_sm()
            .text_color(muted)
            .child(Spinner::new().small())
            .child(text)
    };
    let Some(prompt) = prompt else {
        return h_flex()
            .gap_2()
            .justify_between()
            .child(waiting("Starting the sign-in…"))
            .child(cancel)
            .into_any_element();
    };
    let page = SharedString::from(login_page(prompt).to_owned());
    let shown = SharedString::from(prompt.url.clone());
    // Only a web page opens from here; anything else is there to copy.
    let link = if opens_in_browser(&page) {
        let page = page.clone();
        Button::new(SharedString::from(format!("{ids}-page")))
            .link()
            .small()
            .label(shown)
            .tooltip("Open the page again")
            .on_click(move |_, _, cx| cx.open_url(&page))
            .into_any_element()
    } else {
        div().text_sm().child(shown).into_any_element()
    };
    let elsewhere = (prompt.user_code.is_none())
        .then(|| browser_elsewhere(machine))
        .flatten();
    v_flex()
        .id(SharedString::from(format!("{ids}-prompt")))
        .test_support()
        .gap_2()
        .child(
            div()
                .text_sm()
                .text_color(muted)
                .child(SharedString::from(prompt.instruction.clone())),
        )
        .child(
            h_flex().gap_1().min_w_0().child(link).child(
                Clipboard::new(SharedString::from(format!("{ids}-copy-page")))
                    .value(page)
                    .tooltip("Copy the page's address"),
            ),
        )
        .when_some(prompt.user_code.clone(), |this, code| {
            let code = SharedString::from(code);
            this.child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id(SharedString::from(format!("{ids}-code")))
                            .test_support()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(code.clone()),
                    )
                    .child(
                        Clipboard::new(SharedString::from(format!("{ids}-copy-code")))
                            .value(code)
                            .tooltip("Copy the code"),
                    ),
            )
        })
        .when_some(elsewhere, |this, elsewhere| {
            this.child(div().text_sm().text_color(muted).child(elsewhere))
        })
        .child(
            h_flex()
                .gap_2()
                .justify_between()
                .child(waiting("Waiting for you to finish in the browser"))
                .child(cancel),
        )
        .into_any_element()
}

/// What came of the user's last request for a profile, on one line.
pub(crate) fn note_line(ids: &str, note: &Note, cx: &App) -> AnyElement {
    let (text, color) = match note {
        Note::Failed(why) => (why.clone(), cx.theme().danger),
        Note::Unchecked(why) => (why.clone(), cx.theme().warning),
    };
    div()
        .id(SharedString::from(format!("{ids}-note")))
        .test_support()
        .text_sm()
        .text_color(color)
        .child(text)
        .into_any_element()
}

/// What a session's view keeps to offer the sign-in its model needs: the
/// key box, and what keeps the view drawn as the sign-ins change. The view
/// names its model's auth profile each time it asks, so nothing here
/// follows the model.
pub(crate) struct SignInSlot {
    auths: Entity<Auths>,
    machine: MachineId,
    key: Entity<InputState>,
    _subscriptions: [Subscription; 2],
}

impl SignInSlot {
    /// The slot of a view of type `V` on `machine`, whose model's auth
    /// profile `auth` reads, for Enter in the key box.
    pub(crate) fn new<V: 'static>(
        auths: Entity<Auths>,
        machine: MachineId,
        auth: fn(&V) -> Option<&str>,
        window: &mut Window,
        cx: &mut Context<'_, V>,
    ) -> Self {
        let key = key_box(window, cx);
        let (saved_auths, saved_machine) = (auths.clone(), machine.clone());
        let subscriptions = [
            cx.observe(&auths, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &key,
                window,
                move |view, key, event: &InputEvent, window, cx| {
                    if let (InputEvent::PressEnter { .. }, Some(auth)) = (event, auth(view)) {
                        let auth = auth.to_owned();
                        save_key(&saved_auths, &saved_machine, &auth, key, window, cx);
                    }
                },
            ),
        ];
        Self {
            auths,
            machine,
            key,
            _subscriptions: subscriptions,
        }
    }

    /// Reads whether the profiles are signed in again, as after a turn
    /// failed, which a sign-out elsewhere makes them do.
    /// Whether its key box holds nothing typed, so dropping it loses
    /// nothing; a login under way lives on in [`Auths`].
    pub(crate) fn is_empty(&self, cx: &App) -> bool {
        self.key.read(cx).value().trim().is_empty()
    }

    pub(crate) fn refresh(&self, cx: &mut App) {
        self.auths
            .update(cx, |auths, cx| auths.refresh(&self.machine, cx));
    }

    /// The card for auth profile `auth`, while it needs the user: signed
    /// out, or signed in with a key its provider couldn't check, which the
    /// card says until dismissed, as the key may be wrong.
    pub(crate) fn card(&self, auth: Option<&str>, cx: &App) -> Option<AnyElement> {
        let auths = self.auths.read(cx);
        let profile = auths.profile(&self.machine, auth?)?;
        let note = auths.note(&self.machine, &profile.auth).cloned();
        let unchecked = matches!(note, Some(Note::Unchecked(_)));
        if profile.status != AuthStatus::SignedOut && !unchecked {
            return None;
        }
        let (title, what) = match (unchecked, profile.kind) {
            (true, _) => (
                format!("Signed in to {}, unchecked", profile.label),
                "If replies fail, enter the key again.",
            ),
            (false, AuthKind::Login) => (
                format!("Not signed in to {}", profile.label),
                "This session's model runs on it. Sign in to send messages.",
            ),
            (false, _) => (
                format!("Not signed in to {}", profile.label),
                "This session's model runs on it. Enter its API key to send messages.",
            ),
        };
        let dismiss = unchecked.then(|| {
            let (auths, machine, auth) = (
                self.auths.clone(),
                self.machine.clone(),
                profile.auth.clone(),
            );
            Button::new("sign-in-dismiss")
                .ghost()
                .xsmall()
                .label("Dismiss")
                .on_click(move |_, _, cx| {
                    auths.update(cx, |auths, cx| auths.dismiss(&machine, &auth, cx));
                })
        });
        let theme = cx.theme();
        Some(
            v_flex()
                .id("sign-in-card")
                .test_support()
                .w_full()
                .gap_2()
                .p_3()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.warning)
                .bg(theme.secondary)
                .child(
                    h_flex()
                        .gap_2()
                        .justify_between()
                        .child(
                            h_flex()
                                .gap_2()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(Icon::new(Lucide::KeyRound).small())
                                .child(SharedString::from(title)),
                        )
                        .children(dismiss),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(what),
                )
                .child(controls(
                    "sign-in",
                    &self.auths,
                    &self.machine,
                    profile,
                    Some(&self.key),
                    cx,
                ))
                .when_some(note, |this, note| {
                    this.child(note_line("sign-in", &note, cx))
                })
                .into_any_element(),
        )
    }
}
