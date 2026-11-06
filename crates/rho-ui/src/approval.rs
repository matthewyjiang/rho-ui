//! The approval card pinned above a session's composer while the agent waits
//! for permission to run a tool.

use std::rc::Rc;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, App, FocusHandle, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window,
};
use rho_ui_core::{PermissionKind, PermissionRequest};

/// Answers the request with a choice ID.
type ChoiceHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// A snapshot of the oldest waiting [`PermissionRequest`].
#[derive(IntoElement)]
pub(crate) struct ApprovalCard {
    title: SharedString,
    description: Option<SharedString>,
    /// (choice id, label, kind)
    choices: Vec<(SharedString, SharedString, PermissionKind)>,
    waiting: usize,
    default_focus: FocusHandle,
    on_choose: ChoiceHandler,
}

impl ApprovalCard {
    /// `waiting` counts every queued request, this one included.
    /// `default_focus` is tracked by the default choice so the owner can move
    /// keyboard focus to it.
    pub(crate) fn new(
        request: &PermissionRequest,
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

/// The choice Enter should pick: allow once, else the first one.
fn default_choice(choices: &[(SharedString, SharedString, PermissionKind)]) -> usize {
    choices
        .iter()
        .position(|(_, _, kind)| *kind == PermissionKind::AllowOnce)
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
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(description),
                )
            })
            .child(
                h_flex()
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
                                    } else if matches!(
                                        kind,
                                        PermissionKind::RejectOnce | PermissionKind::RejectAlways
                                    ) {
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
