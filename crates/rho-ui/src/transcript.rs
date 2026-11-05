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
    div, AnyElement, App, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, Window,
};
use rho_ui_core::{Entry, NoticeLevel};

use crate::tool_card::ToolCard;

/// The element ID of the row for `entry`. Messages and tools use
/// [`Entry::key`]; notices have no agent ID, but they are only ever
/// appended, so their position is stable.
pub(crate) fn row_key(entry: &Entry, ix: usize) -> SharedString {
    match entry.key() {
        key if key.is_empty() => format!("notice:{ix}").into(),
        key => key.into(),
    }
}

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
    pub(crate) key: SharedString,
    /// Thoughts and tool output start collapsed.
    pub(crate) disclosure: Disclosure,
    /// The entry is the newest one and the turn is still running.
    pub(crate) live: bool,
    pub(crate) on_toggle: ToggleHandler,
}

pub(crate) fn render_row(entry: Entry, row: RowContext, cx: &App) -> AnyElement {
    let RowContext {
        key,
        disclosure,
        live,
        on_toggle,
    } = row;
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
            .id(key.clone())
            .w_full()
            .child(
                TextView::markdown(key, message.text().to_owned())
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
        Entry::Tool(tool) => ToolCard::new(key, tool, disclosure, on_toggle).into_any_element(),
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
