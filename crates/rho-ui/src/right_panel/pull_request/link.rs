//! Linking PRs to a session by hand, for those the branch doesn't find: a
//! fork's, one from another branch, or one in another repository. Each link
//! is kept in [`PrLinks`], so it outlasts restarts, until it is unlinked,
//! and shows in a tab of its own ([`LinkEvent`]).

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme as _, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, AppContext as _, BorrowAppContext as _, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    Subscription, Window,
};

use super::PullRequestSurface;
use crate::pull_requests::{PrId, PrLinks};

/// What [`super::RightPanel`] hears as the session's links change, to open
/// and close their tabs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::right_panel) enum LinkEvent {
    /// The user linked this PR, or named one linked already: its tab shows.
    Linked(PrId),
    /// The PR the surface showed was unlinked: its tab closes.
    Unlinked,
}

/// The form to link a PR, while it is open.
pub(super) struct LinkForm {
    input: Entity<InputState>,
    /// Why what was given doesn't name a PR.
    error: Option<SharedString>,
    _events: Subscription,
}

impl LinkForm {
    pub(super) fn has_keyboard(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        self.input.focus_handle(cx).is_focused(window)
    }
}

impl PullRequestSurface {
    /// Opens the form to link a PR, with the keyboard in it.
    pub(super) fn start_linking(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.link_form.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder("Pull request URL, owner/name#12, or #12")
            });
            let events =
                cx.subscribe_in(
                    &input,
                    window,
                    |this, _, event: &InputEvent, _, cx| match event {
                        InputEvent::PressEnter { .. } => this.link(cx),
                        InputEvent::Change => {
                            if let Some(form) = &mut this.link_form {
                                form.error = None;
                            }
                            cx.notify();
                        }
                        _ => {}
                    },
                );
            self.link_form = Some(LinkForm {
                input,
                error: None,
                _events: events,
            });
        }
        if let Some(form) = &self.link_form {
            window.focus(&form.input.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Links the PR the form names to the session, beside those linked
    /// already, or says why it can't.
    fn link(&mut self, cx: &mut Context<'_, Self>) {
        let Some(form) = &self.link_form else {
            return;
        };
        let (text, key) = (form.input.read(cx).value(), self.session.clone());
        match PrId::parse(&text, self.origin(cx).as_ref()) {
            Ok(id) => {
                cx.update_global::<PrLinks, _>(|links, _| links.add(&key, id.clone()));
                self.link_form = None;
                cx.emit(LinkEvent::Linked(id));
                cx.notify();
            }
            Err(why) => {
                if let Some(form) = &mut self.link_form {
                    form.error = Some(why.into());
                }
                cx.notify();
            }
        }
    }

    /// Unlinks the PR it shows, which closes its tab.
    pub(super) fn unlink(&mut self, cx: &mut Context<'_, Self>) {
        let Some(id) = self.linked() else {
            return;
        };
        let (key, id) = (self.session.clone(), id.clone());
        cx.update_global::<PrLinks, _>(|links, _| links.remove(&key, &id));
        cx.emit(LinkEvent::Unlinked);
    }

    /// The form, under the toolbar, while it is open.
    pub(super) fn link_form(&self, cx: &Context<'_, Self>) -> Option<AnyElement> {
        let form = self.link_form.as_ref()?;
        let theme = cx.theme();
        let row = h_flex()
            .gap_2()
            .child(
                div()
                    .id("pull-request-link-input")
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&form.input).small()),
            )
            .child(
                Button::new("pull-request-link-submit")
                    .primary()
                    .small()
                    .label("Link")
                    .on_click(cx.listener(|this, _, _, cx| this.link(cx))),
            )
            .child(
                Button::new("pull-request-link-cancel")
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .tooltip("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.link_form = None;
                        cx.notify();
                    })),
            );
        let form = v_flex()
            .flex_shrink_0()
            .gap_1()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(row)
            .when_some(form.error.clone(), |this, why| {
                this.child(
                    div()
                        .id("pull-request-link-error")
                        .test_support()
                        .text_xs()
                        .text_color(theme.red)
                        .child(why),
                )
            });
        Some(form.into_any_element())
    }
}

/// The button that opens the form, where no PR shows.
pub(super) fn link_button(cx: &Context<'_, PullRequestSurface>) -> AnyElement {
    Button::new("pull-request-link-here")
        .outline()
        .small()
        .icon(Lucide::Link)
        .label("Link a pull request")
        .on_click(cx.listener(|this, _, window, cx| this.start_linking(window, cx)))
        .into_any_element()
}
