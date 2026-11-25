//! The panel's section for the sessions a session starts: the mode and model
//! they open with, each `Default` until chosen, from the choices the session
//! itself offers.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    label::Label,
    v_flex, ActiveTheme as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, WeakEntity,
};
use rho_ui_core::{
    Choices, SessionOptions, StartedMode, StartedSessionChange, StartedSessionDefaults,
};

use super::{choice_menu, footer_button, Availability, Change, MenuChoice, SessionSettings};

/// The first entry of each menu: no choice, so the sessions take the
/// config's.
const DEFAULT: &str = "Default";

/// When a change to the choice applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Applies {
    /// At once, or no change is on its way.
    Now,
    /// When the session's turn under way ends: the sessions the turn starts
    /// until then open with the choice as it was.
    WhenTurnEnds,
}

/// The section: `started` as the controls show it, with a warning for a
/// mode the daemon reports as unknown, and a note while a change waits for
/// the turn to end.
pub(super) fn render(
    started: &StartedSessionDefaults,
    applies: Applies,
    options: &SessionOptions,
    availability: Availability,
    settings: &WeakEntity<SessionSettings>,
    cx: &App,
) -> impl IntoElement {
    let row = |name: &'static str, control: AnyElement| {
        h_flex().px_3().justify_between().child(name).child(control)
    };
    v_flex()
        .gap_1()
        .child(
            Label::new("Sessions it starts")
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .px_3()
                .pb_1(),
        )
        // The daemon says `unknown` for a saved choice it can't read, under
        // which it starts no session, and for a mode it or this client
        // doesn't know, under which sessions start; rho-ui can't tell which.
        .when(started.mode() == StartedMode::Unknown, |this| {
            this.child(note(
                "config-started-unreadable",
                "rho-ui can't read the saved mode. If sessions don't start, choose one.",
                cx.theme().warning,
            ))
        })
        .child(row(
            "Mode",
            mode_menu(started, options, availability, settings, cx),
        ))
        .child(row(
            "Model",
            model_menu(started, options, availability, settings, cx),
        ))
        .when(applies == Applies::WhenTurnEnds, |this| {
            this.child(note(
                "config-started-waiting",
                "Applies when the current turn ends.",
                cx.theme().muted_foreground,
            ))
        })
}

/// One line of small text in the section, in `color`.
fn note(id: &'static str, text: &'static str, color: Hsla) -> impl IntoElement {
    h_flex()
        .id(id)
        .test_support()
        .px_3()
        .pb_1()
        .child(Label::new(text).text_xs().text_color(color))
}

/// The button naming the mode the sessions this one starts open in, with a
/// menu of `Default` and the session's own modes. An unknown mode checks
/// none.
fn mode_menu(
    started: &StartedSessionDefaults,
    options: &SessionOptions,
    availability: Availability,
    settings: &WeakEntity<SessionSettings>,
    cx: &App,
) -> AnyElement {
    let label: SharedString = match started.mode() {
        StartedMode::Default => DEFAULT.into(),
        StartedMode::Chosen(mode) => mode.label().into(),
        StartedMode::Unknown => "Choose a mode".into(),
    };
    let modes = options
        .permission_mode()
        .map_or(&[][..], Choices::available);
    let entries = menu_entries(
        modes
            .iter()
            .map(|&mode| (SharedString::from(mode.label()), mode)),
        |mode| started.mode() == StartedMode::from(mode.copied()),
        StartedSessionChange::PermissionMode,
    );
    choice_menu(
        Button::new("config-started-mode")
            .custom(footer_button(cx))
            .small()
            .label(label),
        availability,
        Anchor::BottomRight,
        entries,
        settings,
    )
    .into_any_element()
}

/// The button naming the model the sessions this one starts open on, with
/// a menu of `Default` and the models the session offers. It names a model
/// the session doesn't offer by its id.
fn model_menu(
    started: &StartedSessionDefaults,
    options: &SessionOptions,
    availability: Availability,
    settings: &WeakEntity<SessionSettings>,
    cx: &App,
) -> AnyElement {
    let chosen = started.model();
    let offered = options.model().available();
    let label: SharedString = chosen.map_or_else(
        || DEFAULT.into(),
        |model| {
            offered
                .iter()
                .find(|choice| choice.model() == model)
                .map_or_else(|| model.to_string(), |choice| choice.label().to_owned())
                .into()
        },
    );
    let entries = menu_entries(
        offered.iter().map(|choice| {
            (
                SharedString::from(choice.label().to_owned()),
                choice.model().clone(),
            )
        }),
        |model| model == chosen,
        StartedSessionChange::Model,
    );
    choice_menu(
        Button::new("config-started-model")
            .custom(footer_button(cx))
            .small()
            .label(label),
        availability,
        Anchor::BottomRight,
        entries,
        settings,
    )
    .into_any_element()
}

/// A menu's entries: `Default`, which clears the choice, then `choices`,
/// each a label and its value. `checked` says whether a value, `None` for
/// `Default`, is the one in effect, and `change` makes the change that
/// chooses it.
fn menu_entries<T>(
    choices: impl IntoIterator<Item = (SharedString, T)>,
    checked: impl Fn(Option<&T>) -> bool,
    change: fn(Option<T>) -> StartedSessionChange,
) -> Vec<MenuChoice> {
    std::iter::once((SharedString::from(DEFAULT), None))
        .chain(
            choices
                .into_iter()
                .map(|(label, value)| (label, Some(value))),
        )
        .map(|(label, value)| MenuChoice {
            checked: checked(value.as_ref()),
            change: Change::Started(change(value)),
            label,
        })
        .collect()
}
