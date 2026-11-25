//! Linking PRs to a session by hand, for those the branch doesn't find: a
//! fork's, one from another branch, or one in another repository. The
//! session's daemon keeps each link, the agent's too, until it is
//! unlinked, and the surface's toolbar switches between them. Unlinking is
//! the toolbar's alone: closing the tab never unlinks. A refusal shows on
//! the surface.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme as _, Disableable as _, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, App, AppContext as _, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    Subscription, Task, Window,
};
use rho_ui_core::LinkedPullRequest;

use super::PullRequestSurface;

/// The form to link a PR, while it is open.
pub(super) struct LinkForm {
    input: Entity<InputState>,
    /// Why the daemon refused what was given.
    error: Option<SharedString>,
    /// The link asked of the daemon, until it answers; dropping it stops
    /// waiting.
    linking: Option<Task<()>>,
    _events: Subscription,
}

impl LinkForm {
    pub(super) fn has_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.input.focus_handle(cx).is_focused(window)
    }
}

/// The surface's last unlink of the PR it shows.
#[derive(Default)]
pub(super) enum Unlinking {
    #[default]
    Idle,
    /// Asked of the daemon, until it answers; dropping it stops waiting.
    Asking { _answer: Task<()> },
    /// The daemon refused, and why, until it unlinks again or the
    /// daemon's link to the PR changes from `link`, as the refusal may no
    /// longer hold.
    Refused {
        why: SharedString,
        link: Option<Box<LinkedPullRequest>>,
    },
}

impl Unlinking {
    /// Why the daemon refused the last unlink, if it did.
    pub(super) fn refusal(&self) -> Option<&SharedString> {
        match self {
            Unlinking::Refused { why, .. } => Some(why),
            Unlinking::Idle | Unlinking::Asking { .. } => None,
        }
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
                linking: None,
                _events: events,
            });
        }
        if let Some(form) = &self.link_form {
            window.focus(&form.input.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Asks the session's daemon to link the PR the form names, as typed:
    /// the daemon reads it. Once linked, the form closes and the PR shows;
    /// a refusal shows under the form.
    fn link(&mut self, cx: &mut Context<'_, Self>) {
        let Some(form) = &self.link_form else {
            return;
        };
        let text = form.input.read(cx).value().trim().to_owned();
        if text.is_empty() || form.linking.is_some() {
            return;
        }
        let session = self.session.clone();
        let linked = self.prs.update(cx, |prs, cx| prs.link(&session, text, cx));
        let task = cx.spawn(async move |this, cx| {
            let result = linked.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(id) => {
                        this.link_form = None;
                        this.pick(id, cx);
                    }
                    Err(why) => {
                        if let Some(form) = &mut this.link_form {
                            form.linking = None;
                            form.error = Some(why);
                        }
                    }
                }
                cx.notify();
            });
        });
        if let Some(form) = &mut self.link_form {
            form.error = None;
            form.linking = Some(task);
        }
        cx.notify();
    }

    /// Asks the session's daemon to unlink the PR it shows, after which
    /// another of the session's shows; a refusal shows as its trouble.
    pub(super) fn unlink(&mut self, cx: &mut Context<'_, Self>) {
        let Some(id) = self.linked().cloned() else {
            return;
        };
        if matches!(self.pr.unlinking, Unlinking::Asking { .. }) {
            return;
        }
        let session = self.session.clone();
        let unlinked = self.prs.update(cx, |prs, cx| prs.unlink(&session, &id, cx));
        let task = cx.spawn(async move |this, cx| {
            let result = unlinked.await;
            let _ = this.update(cx, |this, cx| {
                this.pr.unlinking = match result {
                    Ok(()) => Unlinking::Idle,
                    Err(why) => Unlinking::Refused {
                        why,
                        link: this.shown_link(cx).cloned().map(Box::new),
                    },
                };
                cx.notify();
            });
        });
        self.pr.unlinking = Unlinking::Asking { _answer: task };
        cx.notify();
    }

    /// Forgets the refusal of the last unlink once the daemon's link to
    /// the PR changed since.
    pub(super) fn forget_stale_refusal(&mut self, cx: &App) {
        let stale = match &self.pr.unlinking {
            Unlinking::Refused { link, .. } => self.shown_link(cx) != link.as_deref(),
            Unlinking::Idle | Unlinking::Asking { .. } => false,
        };
        if stale {
            self.pr.unlinking = Unlinking::Idle;
        }
    }

    /// The session's link to the linked PR it shows, as its daemon last
    /// said.
    fn shown_link<'a>(&self, cx: &'a App) -> Option<&'a LinkedPullRequest> {
        let id = self.linked()?;
        self.prs.read(cx).of(&self.session)?.link(id)
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
                    .loading(form.linking.is_some())
                    .disabled(form.linking.is_some())
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
