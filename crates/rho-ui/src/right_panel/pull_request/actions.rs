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

use rho_ui_core::{
    PullRequestDetail, PullRequestMergeMethod, PullRequestMergeable, PullRequestState,
};

use super::PullRequestSurface;
use crate::forge::PrAction;

/// Where an action on the PR stands.
#[derive(Default)]
pub(super) enum Acting {
    #[default]
    Idle,
    /// A merge this way was asked for, of the head commit then on screen;
    /// a second click does it.
    Confirming {
        method: PullRequestMergeMethod,
        head: String,
    },
    /// Doing it; dropping the task stops waiting for it.
    Running { _task: Task<()> },
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
                this.pr.acting = match result {
                    Ok(()) => Acting::Idle,
                    Err(why) => Acting::Failed(why),
                };
                cx.notify();
            });
        });
        self.pr.acting = Acting::Running { _task: task };
        cx.notify();
    }

    /// Whether an action runs, or no machine's account can act: the
    /// comment box says why.
    fn busy(&self, cx: &Context<'_, Self>) -> bool {
        matches!(self.pr.acting, Acting::Running { .. }) || self.prs.read(cx).poster(cx).is_err()
    }

    /// Asks to confirm a merge `method`'s way of the head of the PR on
    /// screen as last read whole, whose checks show: the merge is of that
    /// commit only.
    fn confirm_merge(&mut self, method: PullRequestMergeMethod, cx: &mut Context<'_, Self>) {
        let Some(detail) = self.shown_detail(cx) else {
            return;
        };
        let head = detail.summary.head_oid.clone();
        self.pr.acting = Acting::Confirming { method, head };
        cx.notify();
    }

    /// The PR's actions, while it is open or a draft: none once it merged
    /// or closed. They are off while [`Self::busy`].
    pub(super) fn actions(
        &self,
        detail: &PullRequestDetail,
        cx: &Context<'_, Self>,
    ) -> Option<AnyElement> {
        let busy = self.busy(cx);
        let buttons = match (detail.summary.state, &self.pr.acting) {
            (PullRequestState::Draft, _) => h_flex().child(
                Button::new("pull-request-ready")
                    .small()
                    .primary()
                    .label("Ready for review")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.act(PrAction::Ready, cx))),
            ),
            (PullRequestState::Open, Acting::Confirming { method, head }) => {
                confirm(*method, head, detail, cx)
            }
            (PullRequestState::Open, _) => h_flex()
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
            // Merged, closed, or a state a newer rho sends: nothing to do.
            _ => return None,
        };
        let theme = cx.theme();
        let note = match &self.pr.acting {
            Acting::Failed(why) => Some((why.clone(), theme.red)),
            _ if detail.mergeable == PullRequestMergeable::Conflicting
                && detail.summary.state == PullRequestState::Open =>
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
    /// Methods a newer rho sends aren't offered.
    fn merge_button(
        &self,
        detail: &PullRequestDetail,
        cx: &Context<'_, Self>,
    ) -> Option<AnyElement> {
        let methods: Vec<(PullRequestMergeMethod, MergeWords)> = detail
            .merge_methods
            .iter()
            .filter_map(|&method| Some((method, merge_words(method)?)))
            .collect();
        let ((first, first_words), others) = methods.split_first()?;
        let first = *first;
        let disabled = self.busy(cx) || detail.mergeable == PullRequestMergeable::Conflicting;
        let button = Button::new("pull-request-merge")
            .small()
            .primary()
            .label(first_words.label)
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
                for &(method, words) in &others {
                    let view = view.clone();
                    menu = menu.item(PopupMenuItem::new(words.label).on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| this.confirm_merge(method, cx));
                    }));
                }
                menu
            });
        Some(menu.into_any_element())
    }
}

/// What a merge method is called: its button's label, and what its
/// confirmation asks to do.
#[derive(Clone, Copy, Debug)]
struct MergeWords {
    label: &'static str,
    verb: &'static str,
}

/// `method`'s words; none for a method a newer rho sends, which isn't
/// offered.
fn merge_words(method: PullRequestMergeMethod) -> Option<MergeWords> {
    let (label, verb) = match method {
        PullRequestMergeMethod::Merge => ("Merge", "Merge"),
        PullRequestMergeMethod::Squash => ("Squash and merge", "Squash"),
        PullRequestMergeMethod::Rebase => ("Rebase and merge", "Rebase"),
        _ => return None,
    };
    Some(MergeWords { label, verb })
}

/// Asks to confirm a merge `method`'s way of commit `head`.
fn confirm(
    method: PullRequestMergeMethod,
    head: &str,
    detail: &PullRequestDetail,
    cx: &Context<'_, PullRequestSurface>,
) -> gpui_kit::Div {
    let verb = merge_words(method).map_or("Merge", |words| words.verb);
    h_flex()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .child(format!("{verb} into {}?", detail.base)),
        )
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
                    this.pr.acting = Acting::Idle;
                    cx.notify();
                })),
        )
}
