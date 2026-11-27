//! The find bar: Cmd/Ctrl+F's field over the transcript, saying which match
//! is current of how many, with buttons to move between them. Docked above
//! the transcript rather than floating over it, so it hides no match. Its
//! look follows Kit's editor search panel.
//!
//! The transcript does the finding ([`crate::transcript::find`]); the bar
//! tells the session view what the user typed and pressed.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Escape, Input, InputEvent, InputState},
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use gpui_kit::{
    div, rems, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window,
};

use super::SessionView;
use crate::transcript::find::{Count, Direction};

/// The find bar of a session view, while open.
pub(super) struct FindBar {
    input: Entity<InputState>,
    _events: Subscription,
}

impl FindBar {
    /// A bar whose field reports to the session view: each edit finds
    /// again, Enter moves to the next match and Shift+Enter the previous.
    pub(super) fn new(window: &mut Window, cx: &mut Context<'_, SessionView>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in transcript"));
        let events =
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.find_query(cx),
                    InputEvent::PressEnter { shift: false, .. } => {
                        this.find_step(Direction::Next, cx);
                    }
                    InputEvent::PressEnter { shift: true, .. } => {
                        this.find_step(Direction::Previous, cx);
                    }
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            );
        Self {
            input,
            _events: events,
        }
    }

    pub(super) fn query(&self, cx: &Context<'_, SessionView>) -> SharedString {
        self.input.read(cx).value()
    }

    /// Puts `query` in the field, without finding it: the session view
    /// finds it where it wants the current match.
    pub(super) fn set_query(
        &self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<'_, SessionView>,
    ) {
        self.input.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx);
        });
    }

    /// Puts the keyboard in the field, its text selected to type over.
    pub(super) fn focus(&self, window: &mut Window, cx: &mut Context<'_, SessionView>) {
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    /// The bar, given what the transcript found.
    pub(super) fn render(
        &self,
        count: Count,
        cx: &mut Context<'_, SessionView>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let label: SharedString = match count {
            Count::NoQuery => SharedString::default(),
            Count::NoMatches => "No results".into(),
            Count::At { current, total } => format!("{current} of {total}").into(),
        };
        let none = !matches!(count, Count::At { .. });
        let step = |id: &'static str, icon, tooltip: &'static str, direction| {
            Button::new(id)
                .xsmall()
                .ghost()
                .icon(icon)
                .tooltip(tooltip)
                .disabled(none)
                .on_click(cx.listener(move |this, _, _, cx| this.find_step(direction, cx)))
        };
        let previous = step(
            "find-previous",
            IconName::ChevronUp,
            "Previous match",
            Direction::Previous,
        );
        let next = step(
            "find-next",
            IconName::ChevronDown,
            "Next match",
            Direction::Next,
        );
        div()
            .id("find-bar")
            .test_support()
            // The field lets Escape go up to here, which closes the bar
            // rather than stopping the turn.
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.close_find(window, cx)))
            .w_full()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                // At the transcript's measure, over its text.
                h_flex()
                    .w_full()
                    .max_w(rems(48.))
                    .mx_auto()
                    .gap_2()
                    .child(
                        Input::new(&self.input)
                            .small()
                            .prefix(
                                Icon::new(IconName::Search)
                                    .small()
                                    .text_color(theme.muted_foreground),
                            )
                            .flex_1(),
                    )
                    .child(
                        div()
                            .id("find-count")
                            .aria_label(label.clone())
                            .test_support()
                            .min_w_16()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(label),
                    )
                    .child(previous)
                    .child(next)
                    .child(
                        Button::new("find-close")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip("Close")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_find(window, cx);
                            })),
                    ),
            )
    }
}
