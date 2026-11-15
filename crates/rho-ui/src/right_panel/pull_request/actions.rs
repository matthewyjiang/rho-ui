//! What the Pull request surface does to a PR besides commenting: marks a
//! draft ready for review, turns an open PR back into a draft, and merges
//! it, a second click confirming, in a way its repository allows.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _, DropdownButton},
    h_flex,
    menu::PopupMenuItem,
    v_flex, ActiveTheme as _, Disableable as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Task,
};

use super::PullRequestSurface;
use crate::pull_requests::{MergeMethod, Mergeable, PrAction, PrDetail, PrState};

/// Where an action on the PR stands.
pub(super) enum Acting {
    Idle,
    /// A merge this way was asked for, of the head commit then on screen;
    /// a second click does it.
    Confirming {
        method: MergeMethod,
        head: String,
    },
    /// Doing it; dropping the task stops waiting for it.
    Running(Task<()>),
    /// It wasn't done, and why.
    Failed(SharedString),
}

impl PullRequestSurface {
    /// Does `action` to the PR on screen.
    fn act(&mut self, action: PrAction, cx: &mut Context<'_, Self>) {
        let Some(id) = self.shown_pr(cx) else {
            return;
        };
        let done = self
            .prs
            .update(cx, |prs, cx| prs.act(id.clone(), action, cx));
        let task = cx.spawn(async move |this, cx| {
            let result = done.await;
            let _ = this.update(cx, |this, cx| {
                if this.shown_pr(cx).as_ref() != Some(&id) {
                    // What it says is about a PR no longer on screen.
                    return;
                }
                this.acting = match result {
                    Ok(()) => Acting::Idle,
                    Err(why) => Acting::Failed(why),
                };
                cx.notify();
            });
        });
        self.acting = Acting::Running(task);
        cx.notify();
    }

    /// Whether an action runs, or no machine's account can act: the
    /// comment box says why.
    fn busy(&self, cx: &Context<'_, Self>) -> bool {
        matches!(self.acting, Acting::Running(_)) || self.prs.read(cx).poster(cx).is_err()
    }

    /// Asks to confirm a merge `method`'s way of the PR on screen's head.
    fn confirm_merge(&mut self, method: MergeMethod, cx: &mut Context<'_, Self>) {
        let Some(id) = self.shown_pr(cx) else {
            return;
        };
        let prs = self.prs.read(cx);
        let Some(detail) = prs.detail(&id) else {
            return;
        };
        let head = detail.summary.change.head_oid.clone();
        self.acting = Acting::Confirming { method, head };
        cx.notify();
    }

    /// The PR's actions, while it is open or a draft: none once it merged
    /// or closed. They are off while [`Self::busy`].
    pub(super) fn actions(&self, detail: &PrDetail, cx: &Context<'_, Self>) -> Option<AnyElement> {
        let busy = self.busy(cx);
        let buttons = match (detail.summary.state, &self.acting) {
            (PrState::Merged | PrState::Closed, _) => return None,
            (PrState::Draft, _) => h_flex().child(
                Button::new("pull-request-ready")
                    .small()
                    .primary()
                    .label("Ready for review")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.act(PrAction::Ready, cx))),
            ),
            (PrState::Open, Acting::Confirming { method, head }) => {
                confirm(*method, head, detail, cx)
            }
            (PrState::Open, _) => h_flex()
                .gap_2()
                .children(self.merge_button(detail, cx))
                .child(
                    Button::new("pull-request-draft")
                        .small()
                        .ghost()
                        .label("Convert to draft")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.act(PrAction::Draft, cx))),
                ),
        };
        let theme = cx.theme();
        let note = match &self.acting {
            Acting::Failed(why) => Some((why.clone(), theme.red)),
            _ if detail.mergeable == Mergeable::Conflicting
                && detail.summary.state == PrState::Open =>
            {
                let why = format!("It conflicts with {}.", detail.base);
                Some((why.into(), theme.muted_foreground))
            }
            _ => None,
        };
        Some(
            v_flex()
                .gap_2()
                .child(buttons)
                .when_some(note, |this, (why, color)| {
                    this.child(
                        div()
                            .id("pull-request-action-note")
                            .test_support()
                            .text_xs()
                            .text_color(color)
                            .child(why),
                    )
                })
                .into_any_element(),
        )
    }
}

impl PullRequestSurface {
    /// Merge, in the viewer's default way, with the repository's other
    /// ways in its menu; each asks to confirm. Off while it conflicts.
    fn merge_button(&self, detail: &PrDetail, cx: &Context<'_, Self>) -> Option<AnyElement> {
        let (&first, others) = detail.merge_methods.split_first()?;
        let disabled = self.busy(cx) || detail.mergeable == Mergeable::Conflicting;
        let button = Button::new("pull-request-merge")
            .small()
            .primary()
            .label(first.label())
            .disabled(disabled)
            .on_click(cx.listener(move |this, _, _, cx| this.confirm_merge(first, cx)));
        if others.is_empty() {
            return Some(button.into_any_element());
        }
        let view = cx.weak_entity();
        let others = others.to_vec();
        let menu = DropdownButton::new("pull-request-merge-ways")
            .small()
            .primary()
            .button(button)
            .menu_accessibility_label("Other ways to merge")
            .disabled(disabled)
            .dropdown_menu(move |mut menu, _, _| {
                for &method in &others {
                    let view = view.clone();
                    menu = menu.item(PopupMenuItem::new(method.label()).on_click(
                        move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| this.confirm_merge(method, cx));
                        },
                    ));
                }
                menu
            });
        Some(menu.into_any_element())
    }
}

/// Asks to confirm a merge `method`'s way of commit `head`.
fn confirm(
    method: MergeMethod,
    head: &str,
    detail: &PrDetail,
    cx: &Context<'_, PullRequestSurface>,
) -> gpui_kit::Div {
    h_flex()
        .gap_2()
        .child(div().flex_1().min_w_0().truncate().text_sm().child(format!(
            "{} into {}?",
            method.verb(),
            detail.base
        )))
        .child(
            Button::new("pull-request-merge-confirm")
                .small()
                .primary()
                .label("Confirm")
                .on_click(cx.listener({
                    let head = head.to_owned();
                    move |this, _, _, cx| {
                        let head = head.clone();
                        this.act(PrAction::Merge { method, head }, cx);
                    }
                })),
        )
        .child(
            Button::new("pull-request-merge-cancel")
                .small()
                .ghost()
                .label("Cancel")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.acting = Acting::Idle;
                    cx.notify();
                })),
        )
}
