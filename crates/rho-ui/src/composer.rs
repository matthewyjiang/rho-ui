//! The prompt box under the transcript. Enter sends, Shift+Enter starts a new
//! line; while a turn runs the box is disabled and Send becomes Stop. Its
//! footer holds the session settings on both sides, then context use and
//! cost, then the round Send button.

use std::rc::Rc;

use gpui_kit::component::{
    button::{Button, ButtonRounded, ButtonVariants as _},
    h_flex,
    input::{Textarea, TextareaState},
    v_flex, ActiveTheme as _, Disableable as _, IconName,
};
use gpui_kit::{
    div, AnyElement, App, AppContext as _, Entity, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Window,
};

/// Grows from one line to this many before it scrolls: enough for a short
/// multi-paragraph prompt without pushing the transcript off a laptop screen.
const MAX_ROWS: usize = 8;

/// The text state, owned by the session view. Its `PressEnter` event (without
/// Shift) is the send request.
pub(crate) fn new_state(window: &mut Window, cx: &mut App) -> Entity<TextareaState> {
    cx.new(|cx| {
        TextareaState::new(window, cx)
            .auto_grow(1, MAX_ROWS)
            .submit_on_enter(true)
            .placeholder("Ask rho to do something")
    })
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Whether the composer can send, or the turn it sent is still running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposerMode {
    Send,
    /// The session is being created: the box is disabled and Send spins.
    Starting,
    /// A turn is running: the box is disabled and Stop replaces Send.
    Stop,
}

#[derive(IntoElement)]
pub(crate) struct Composer {
    state: Entity<TextareaState>,
    mode: ComposerMode,
    /// Session settings at the left of the footer.
    leading: Option<AnyElement>,
    /// Session settings just before Send.
    trailing: Option<AnyElement>,
    /// Context use and cost, before Send.
    meta: Option<String>,
    on_send: Handler,
    on_stop: Handler,
}

impl Composer {
    pub(crate) fn new(
        state: &Entity<TextareaState>,
        mode: ComposerMode,
        on_send: impl Fn(&mut Window, &mut App) + 'static,
        on_stop: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            state: state.clone(),
            mode,
            leading: None,
            trailing: None,
            meta: None,
            on_send: Rc::new(on_send),
            on_stop: Rc::new(on_stop),
        }
    }

    pub(crate) fn leading(mut self, controls: impl IntoElement) -> Self {
        self.leading = Some(controls.into_any_element());
        self
    }

    pub(crate) fn trailing(mut self, controls: Option<impl IntoElement>) -> Self {
        self.trailing = controls.map(IntoElement::into_any_element);
        self
    }

    pub(crate) fn meta(mut self, meta: Option<String>) -> Self {
        self.meta = meta;
        self
    }
}

impl RenderOnce for Composer {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let empty = self.state.read(cx).value().trim().is_empty();
        let action = if self.mode == ComposerMode::Stop {
            let on_stop = self.on_stop;
            Button::new("composer-stop")
                .danger()
                .icon(IconName::Square)
                .rounded(ButtonRounded::Large)
                .tooltip("Stop (Esc)")
                .on_click(move |_, window, cx| on_stop(window, cx))
        } else {
            let on_send = self.on_send;
            Button::new("composer-send")
                .primary()
                .icon(IconName::ArrowUp)
                .rounded(ButtonRounded::Large)
                .tooltip("Send (Enter)")
                .disabled(empty)
                .loading(self.mode == ComposerMode::Starting)
                .on_click(move |_, window, cx| on_send(window, cx))
        };
        v_flex()
            .w_full()
            .gap_2()
            .pt_3()
            .px_3()
            .pb_2()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().input)
            .bg(cx.theme().secondary)
            .child(
                Textarea::new(&self.state)
                    .appearance(false)
                    .disabled(self.mode != ComposerMode::Send)
                    .aria_label("Prompt"),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .children(self.leading),
                    )
                    .children(self.meta.map(|meta| {
                        div()
                            .flex_shrink_0()
                            .px_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(meta)
                    }))
                    .children(self.trailing)
                    .child(action),
            )
    }
}
