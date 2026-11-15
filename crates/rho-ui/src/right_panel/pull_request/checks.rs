//! The PR's checks on the Pull request surface: a row each, those that
//! need a look first, and how many stand each way.

use std::collections::BTreeMap;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, relative, AnyElement, App, Context, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _,
};

use super::{header::web_url, row, PullRequestSurface};
use crate::pull_requests::{CheckRun, CheckStatus, PrDetail};

/// A row per check, those that need a look first; each opens its log.
/// Says how many more there are past those read.
pub(super) fn checks(detail: &PrDetail, cx: &Context<'_, PullRequestSurface>) -> AnyElement {
    let theme = cx.theme();
    if detail.checks.is_empty() {
        return div()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child("No checks.")
            .into_any_element();
    }
    let rows = detail
        .checks
        .iter()
        .enumerate()
        .map(|(ix, check)| check_row(ix, check, cx));
    let more = u32::try_from(detail.checks.len())
        .ok()
        .and_then(|shown| detail.checks_total.checked_sub(shown))
        .filter(|&more| more > 0);
    v_flex()
        .id("pull-request-checks")
        .test_support()
        .children(rows)
        .when_some(more, |this, more| {
            this.child(
                div()
                    .pt_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{more} more on GitHub")),
            )
        })
        .into_any_element()
}

/// The icon and color of a check's status.
pub(super) fn status_look(status: CheckStatus, cx: &App) -> (Lucide, Hsla) {
    let theme = cx.theme();
    match status {
        CheckStatus::Failed => (Lucide::CircleX, theme.red),
        CheckStatus::Running => (Lucide::LoaderCircle, theme.yellow),
        CheckStatus::Pending => (Lucide::CircleDashed, theme.muted_foreground),
        CheckStatus::Cancelled => (Lucide::CircleSlash, theme.muted_foreground),
        CheckStatus::Passed => (Lucide::CircleCheck, theme.green),
        CheckStatus::Neutral | CheckStatus::Skipped => {
            (Lucide::CircleMinus, theme.muted_foreground)
        }
    }
}

/// One check: its status, name and workflow. A click opens its log.
pub(super) fn check_row(
    ix: usize,
    check: &CheckRun,
    cx: &Context<'_, PullRequestSurface>,
) -> impl IntoElement {
    let theme = cx.theme();
    let (icon, color) = status_look(check.status, cx);
    let url = check.url.as_deref().and_then(web_url);
    row(format!("pull-request-check-{ix}"), cx)
        .text_sm()
        .when_some(url, |this, url| {
            this.cursor_pointer()
                .hover(|style| style.bg(theme.secondary))
                .on_click(move |_, _, cx| cx.open_url(&url))
        })
        .child(Icon::new(icon).small().text_color(color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(check.name.clone()),
        )
        .when_some(check.workflow.clone(), |this, workflow| {
            this.child(
                div()
                    .flex_shrink_0()
                    .max_w(relative(0.4))
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(workflow),
            )
        })
}

/// How many checks stand each way, most urgent first, each in its
/// status's color: "1 failed  3 passed".
pub(super) fn check_counts(checks: &[CheckRun], cx: &App) -> AnyElement {
    let mut counts: BTreeMap<CheckStatus, usize> = BTreeMap::new();
    for check in checks {
        *counts.entry(check.status).or_default() += 1;
    }
    let word = |status: CheckStatus| match status {
        CheckStatus::Failed => "failed",
        CheckStatus::Running => "running",
        CheckStatus::Pending => "waiting",
        CheckStatus::Cancelled => "cancelled",
        CheckStatus::Passed => "passed",
        CheckStatus::Neutral => "neutral",
        CheckStatus::Skipped => "skipped",
    };
    h_flex()
        .gap_2()
        .children(counts.into_iter().map(|(status, count)| {
            let (_, color) = status_look(status, cx);
            div()
                .text_color(color)
                .child(format!("{count} {}", word(status)))
        }))
        .into_any_element()
}
