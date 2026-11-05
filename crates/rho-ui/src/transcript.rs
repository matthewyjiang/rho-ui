//! One transcript row per thread [`Entry`].

use std::rc::Rc;

use gpui_kit::component::{
    alert::{Alert, AlertVariant},
    bubble::{Bubble, BubbleVariant},
    button::{Button, ButtonVariants as _},
    message::{Message, MessageAlignment, MessageContent},
    text::TextView,
    v_flex, ActiveTheme as _, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, App, ElementId, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, Window,
};
use rho_ui_core::{Entry, NoticeLevel};

use crate::tool_card;

/// Whether a thought or a tool card shows its body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disclosure {
    Collapsed,
    Expanded,
}

/// Flips a row's [`Disclosure`].
pub(crate) type ToggleHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Everything a row needs beyond its entry.
pub(crate) struct RowContext {
    /// The entry's index. Entries are only appended or changed in place, so
    /// it names the row for the thread's lifetime.
    pub(crate) ix: usize,
    /// Thoughts and tool output start collapsed.
    pub(crate) disclosure: Disclosure,
    /// The entry is the newest one and the turn is still running.
    pub(crate) live: bool,
    pub(crate) on_toggle: ToggleHandler,
}

/// Builds the row for `entry`. Copies only the text the row shows: a
/// collapsed thought or tool card copies none of its body.
pub(crate) fn render_row(entry: &Entry, row: RowContext, cx: &App) -> AnyElement {
    let RowContext {
        ix,
        disclosure,
        live,
        on_toggle,
    } = row;
    let key = ElementId::named_usize("row", ix);
    let expanded = disclosure == Disclosure::Expanded;
    match entry {
        Entry::User(message) => Message::new()
            .id(key)
            .alignment(MessageAlignment::End)
            .content(
                MessageContent::new().bubble(
                    Bubble::new()
                        .with_variant(BubbleVariant::Muted)
                        .child(message.text().to_owned()),
                ),
            )
            .into_any_element(),
        Entry::Agent(message) => div()
            .id(key)
            .w_full()
            .child(
                TextView::markdown(
                    ElementId::named_usize("markdown", ix),
                    message.text().to_owned(),
                )
                .selectable(true)
                .stream_fade(live),
            )
            .into_any_element(),
        Entry::Thought(message) => v_flex()
            .id(key)
            .w_full()
            .gap_1()
            .text_color(cx.theme().muted_foreground)
            .child(
                div().child(
                    Button::new("toggle-thought")
                        .ghost()
                        .xsmall()
                        .icon(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .label(if live { "Thinking…" } else { "Thought" })
                        .on_click(move |_, window, cx| on_toggle(window, cx)),
                ),
            )
            .when(expanded, |this| {
                this.child(
                    div()
                        .pl_3()
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .text_sm()
                        .child(message.text().trim().to_owned()),
                )
            })
            .into_any_element(),
        Entry::Tool(tool) => tool_card::render(tool, disclosure, on_toggle, cx),
        Entry::Notice(notice) => {
            let variant = match notice.level() {
                NoticeLevel::Info => AlertVariant::Info,
                NoticeLevel::Warning => AlertVariant::Warning,
                NoticeLevel::Error => AlertVariant::Error,
            };
            let alert = match notice.description() {
                Some(description) => Alert::new(key, SharedString::from(description.to_owned()))
                    .title(notice.title().to_owned()),
                None => Alert::new(key, SharedString::from(notice.title().to_owned())),
            };
            alert.with_variant(variant).small().into_any_element()
        }
    }
}
