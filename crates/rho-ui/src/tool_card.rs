//! A tool call in the transcript: status, title, and output that expands on
//! request.

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _,
};
use rho_ui_core::{ToolCall, ToolContent, ToolStatus};

use crate::transcript::{Disclosure, ToggleHandler};

/// Output past this many lines shows only its tail. At `text_xs` that is
/// about seven screens of text, more than anyone reads inline, and it keeps
/// a huge `cat` from stalling layout.
const MAX_OUTPUT_LINES: usize = 400;

fn status_icon(status: ToolStatus, cx: &App) -> AnyElement {
    match status {
        ToolStatus::Pending | ToolStatus::Running => Spinner::new().xsmall().into_any_element(),
        ToolStatus::Done => Icon::new(IconName::Check)
            .xsmall()
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
        ToolStatus::Failed => Icon::new(IconName::CircleX)
            .xsmall()
            .text_color(cx.theme().danger)
            .into_any_element(),
        ToolStatus::Cancelled => Icon::new(IconName::Ban)
            .xsmall()
            .text_color(cx.theme().muted_foreground)
            .into_any_element(),
    }
}

fn status_label(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Pending => "Pending",
        ToolStatus::Running => "Running",
        ToolStatus::Done => "Done",
        ToolStatus::Failed => "Failed",
        ToolStatus::Cancelled => "Cancelled",
    }
}

/// The last `MAX_OUTPUT_LINES` lines of `text`, and how many were hidden.
fn tail(text: &str) -> (&str, usize) {
    let total = text.lines().count();
    if total <= MAX_OUTPUT_LINES {
        return (text, 0);
    }
    let hidden = total - MAX_OUTPUT_LINES;
    let start = text
        .match_indices('\n')
        .nth(hidden - 1)
        .map_or(0, |(at, _)| at + 1);
    (&text[start..], hidden)
}

fn monospace(text: &str, cx: &App) -> impl IntoElement {
    let (shown, hidden) = tail(text.trim_end());
    v_flex()
        .gap_1()
        .when(hidden > 0, |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{hidden} earlier lines hidden")),
            )
        })
        .child(
            div()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .child(shown.to_owned()),
        )
}

/// The card for `tool`; `on_toggle` flips `disclosure`. Its element ID is
/// `tool:<tool call id>`.
pub(crate) fn render(
    tool: &ToolCall,
    disclosure: Disclosure,
    on_toggle: ToggleHandler,
    cx: &App,
) -> AnyElement {
    let expanded = disclosure == Disclosure::Expanded;
    let has_output = !tool.content().is_empty();
    let title = if tool.title().is_empty() {
        "Tool call".to_owned()
    } else {
        tool.title().to_owned()
    };

    v_flex()
        .id(SharedString::from(format!("tool:{}", tool.id())))
        .w_full()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .gap_2()
                .px_2()
                .py_1()
                .child(
                    div()
                        .id("status")
                        .flex_shrink_0()
                        .size_4()
                        .flex()
                        .items_center()
                        .justify_center()
                        .tooltip({
                            let label = status_label(tool.status());
                            move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(label).build(window, cx)
                            }
                        })
                        .child(status_icon(tool.status(), cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(title),
                )
                .when(has_output, |this| {
                    this.child(
                        Button::new("toggle-output")
                            .ghost()
                            .xsmall()
                            .icon(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .label(if expanded { "Hide output" } else { "Output" })
                            .on_click(move |_, window, cx| on_toggle(window, cx)),
                    )
                }),
        )
        .when(expanded && has_output, |this| {
            this.child(
                v_flex()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .rounded_b(cx.theme().radius)
                    .children(tool.content().iter().map(|content| {
                        match content {
                            ToolContent::Text(text) => monospace(text, cx).into_any_element(),
                            ToolContent::Diff { paths, patch } => v_flex()
                                .gap_1()
                                .children(paths.iter().map(|path| {
                                    div()
                                        .text_xs()
                                        .font_semibold()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(path.display().to_string())
                                }))
                                .when_some(patch.as_deref(), |this, patch| {
                                    this.child(monospace(patch, cx))
                                })
                                .into_any_element(),
                        }
                    })),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    #[test]
    fn tail_keeps_the_last_lines() {
        let text = (0..MAX_OUTPUT_LINES + 3).fold(String::new(), |mut text, n| {
            writeln!(text, "line {n}").expect("writing to a String cannot fail");
            text
        });
        let (shown, hidden) = tail(text.trim_end());
        assert_eq!(hidden, 3);
        assert!(shown.starts_with("line 3\n"));
        assert_eq!(tail("short"), ("short", 0));
    }
}
