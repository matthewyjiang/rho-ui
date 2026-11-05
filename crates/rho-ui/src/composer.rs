//! The prompt box under the transcript. Enter sends, Shift+Enter starts a new
//! line; while a turn runs the box is disabled and Send becomes Stop. Its
//! footer holds the session settings, context use and cost.

use std::rc::Rc;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Textarea, TextareaState},
    v_flex, ActiveTheme as _, Disableable as _, IconName, Sizable as _,
};
use gpui_kit::{
    div, AnyElement, App, AppContext as _, Entity, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Window,
};

/// Grows from one line to this many before it scrolls: enough for a short
/// multi-paragraph prompt without pushing the transcript off a laptop screen.
const MAX_ROWS: usize = 8;

/// The text state, owned by the thread view. Its `PressEnter` event (without
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
    /// The thread is being created: the box is disabled and Send spins.
    Starting,
    /// A turn is running: the box is disabled and Stop replaces Send.
    Stop,
}

#[derive(IntoElement)]
pub(crate) struct Composer {
    state: Entity<TextareaState>,
    mode: ComposerMode,
    /// Session settings, left of the footer.
    controls: Option<AnyElement>,
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
            controls: None,
            meta: None,
            on_send: Rc::new(on_send),
            on_stop: Rc::new(on_stop),
        }
    }

    pub(crate) fn controls(mut self, controls: impl IntoElement) -> Self {
        self.controls = Some(controls.into_any_element());
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
                .small()
                .outline()
                .icon(IconName::Square)
                .label("Stop")
                .tooltip("Esc")
                .on_click(move |_, window, cx| on_stop(window, cx))
        } else {
            let on_send = self.on_send;
            Button::new("composer-send")
                .small()
                .primary()
                .icon(IconName::ArrowUp)
                .label("Send")
                .disabled(empty)
                .loading(self.mode == ComposerMode::Starting)
                .on_click(move |_, window, cx| on_send(window, cx))
        };
        v_flex()
            .w_full()
            .gap_2()
            .p_2()
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
                    .gap_2()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .children(self.controls),
                    )
                    .children(self.meta.map(|meta| {
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(meta)
                    }))
                    .child(action),
            )
    }
}
