//! Signing in to an auth profile, wherever one is needed: the controls a
//! row of Settings → Providers shows, and [`SignInCard`], which sits above
//! a session's composer while its model's profile is signed out, so the
//! user signs in without leaving the session.
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
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
};
use rho_ui_core::{AuthKind, AuthStatus, AuthSummary, LoginPrompt};

use crate::{
    auths::{Auths, Note},
    machines::MachineId,
};

/// A masked box for an API key.
pub(crate) fn key_box(window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder("API key")
    })
}

/// Saves the key in `key` for `auth` on `machine`, and empties the box.
pub(crate) fn save_key(
    auths: &Entity<Auths>,
    machine: &MachineId,
    auth: &str,
    key: &Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) {
    let text = key.read(cx).value().to_string();
    if text.trim().is_empty() {
        return;
    }
    key.update(cx, |key, cx| key.set_value("", window, cx));
    auths.update(cx, |auths, cx| auths.save_key(machine, auth, &text, cx));
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
                login_progress(ids, auths, machine, login.prompt.as_ref(), cx)
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
    let (auths, machine) = (auths.clone(), machine.clone());
    let cancel = Button::new(SharedString::from(format!("{ids}-cancel")))
        .ghost()
        .small()
        .label("Cancel")
        .on_click(move |_, _, cx| {
            auths.update(cx, |auths, cx| auths.cancel_login(&machine, cx));
        });
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
    let page = SharedString::from(
        prompt
            .url_with_code
            .clone()
            .unwrap_or_else(|| prompt.url.clone()),
    );
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
            h_flex()
                .gap_1()
                .min_w_0()
                .child(
                    Button::new(SharedString::from(format!("{ids}-page")))
                        .link()
                        .small()
                        .label(SharedString::from(prompt.url.clone()))
                        .tooltip("Open the page again")
                        .on_click({
                            let page = page.clone();
                            move |_, _, cx| cx.open_url(&page)
                        }),
                )
                .child(
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

/// The card above a session's composer while its model's auth profile is
/// signed out on its machine. It shows nothing otherwise, as while the
/// profiles are read.
pub(crate) struct SignInCard {
    auths: Entity<Auths>,
    machine: MachineId,
    /// The auth profile the session's model runs on, once known.
    auth: Option<String>,
    key: Entity<InputState>,
    _subscriptions: [Subscription; 2],
}

impl SignInCard {
    pub(crate) fn new(
        auths: Entity<Auths>,
        machine: MachineId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let key = key_box(window, cx);
        let subscriptions = [
            cx.observe(&auths, |_, _, cx| cx.notify()),
            cx.subscribe_in(&key, window, |this, key, event: &InputEvent, window, cx| {
                if let (InputEvent::PressEnter { .. }, Some(auth)) = (event, this.auth.clone()) {
                    save_key(&this.auths, &this.machine, &auth, key, window, cx);
                }
            }),
        ];
        Self {
            auths,
            machine,
            auth: None,
            key,
            _subscriptions: subscriptions,
        }
    }

    /// Follows the session's model to the auth profile it runs on.
    pub(crate) fn set_auth(&mut self, auth: Option<&str>, cx: &mut Context<'_, Self>) {
        if self.auth.as_deref() != auth {
            self.auth = auth.map(str::to_owned);
            cx.notify();
        }
    }

    /// Reads whether the profiles are signed in again, as after a turn
    /// failed, which a sign-out elsewhere makes them do.
    pub(crate) fn refresh(&mut self, cx: &mut Context<'_, Self>) {
        self.auths
            .update(cx, |auths, cx| auths.refresh(&self.machine, cx));
    }

    /// The signed-out profile the card offers to sign in to, if any.
    fn signed_out<'a>(&self, auths: &'a Auths) -> Option<&'a AuthSummary> {
        let profile = auths.profile(&self.machine, self.auth.as_deref()?)?;
        (profile.status == AuthStatus::SignedOut).then_some(profile)
    }

    /// Whether the card has anything to show, so its parent lays it out.
    pub(crate) fn is_shown(&self, cx: &App) -> bool {
        self.signed_out(self.auths.read(cx)).is_some()
    }
}

impl Render for SignInCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let auths = self.auths.read(cx);
        let Some(profile) = self.signed_out(auths) else {
            return gpui_kit::Empty.into_any_element();
        };
        let note = auths.note(&self.machine, &profile.auth).cloned();
        let what = match profile.kind {
            AuthKind::Login => "Sign in to send messages.",
            _ => "Enter its API key to send messages.",
        };
        let theme = cx.theme();
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
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(Icon::new(Lucide::KeyRound).small())
                    .child(SharedString::from(format!(
                        "Not signed in to {}",
                        profile.label
                    ))),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!("This session's model runs on it. {what}")),
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
            .into_any_element()
    }
}
