//! The card pinned above a session's composer while a turn waits for a
//! person: permission to run a tool, approval of a plan, or a question.

use std::rc::Rc;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, App, FocusHandle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window,
};
use rho_ui_core::{ChoiceKind, InputRequest};

/// Answers the request with a choice ID.
type ChoiceHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// A snapshot of the oldest open [`InputRequest`].
#[derive(IntoElement)]
pub(crate) struct ApprovalCard {
    title: SharedString,
    description: Option<SharedString>,
    /// (choice id, label, kind)
    choices: Vec<(SharedString, SharedString, ChoiceKind)>,
    waiting: usize,
    default_focus: FocusHandle,
    on_choose: ChoiceHandler,
}

impl ApprovalCard {
    /// `waiting` counts every queued request, this one included.
    /// `default_focus` is tracked by the default choice so the owner can move
    /// keyboard focus to it.
    pub(crate) fn new(
        request: &InputRequest,
        waiting: usize,
        default_focus: FocusHandle,
        on_choose: impl Fn(&str, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            title: request.title().to_owned().into(),
            description: request.description().map(|text| text.to_owned().into()),
            choices: request
                .choices()
                .iter()
                .map(|choice| {
                    (
                        choice.id().to_owned().into(),
                        choice.label().to_owned().into(),
                        choice.kind(),
                    )
                })
                .collect(),
            waiting,
            default_focus,
            on_choose: Rc::new(on_choose),
        }
    }
}

/// The choice Enter should pick: the one that lets the turn go on once,
/// else the first one.
fn default_choice(choices: &[(SharedString, SharedString, ChoiceKind)]) -> usize {
    choices
        .iter()
        .position(|(_, _, kind)| *kind == ChoiceKind::Accept)
        .unwrap_or(0)
}

impl RenderOnce for ApprovalCard {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let default_ix = default_choice(&self.choices);
        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().warning)
            .bg(cx.theme().secondary)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .small()
                            .text_color(cx.theme().warning),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.title),
                    )
                    .when(self.waiting > 1, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{} more waiting", self.waiting - 1)),
                        )
                    }),
            )
            .when_some(self.description, |this, description| {
                this.child(
                    // A plan runs long: it scrolls inside the card, so the
                    // answers stay on screen.
                    div()
                        .id("approval-detail")
                        .max_h(rems(12.))
                        .overflow_y_scroll()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(description),
                )
            })
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .children(self.choices.into_iter().enumerate().map(
                        |(ix, (id, label, kind))| {
                            let on_choose = Rc::clone(&self.on_choose);
                            let choice = id.clone();
                            Button::new(SharedString::from(format!("approval-{id}")))
                                .small()
                                .label(label)
                                .map(|button| {
                                    if ix == default_ix {
                                        button.primary().track_focus(&self.default_focus)
                                    } else if kind == ChoiceKind::Reject {
                                        button.outline()
                                    } else {
                                        button
                                    }
                                })
                                .on_click(move |_, window, cx| on_choose(&choice, window, cx))
                        },
                    )),
            )
    }
}
