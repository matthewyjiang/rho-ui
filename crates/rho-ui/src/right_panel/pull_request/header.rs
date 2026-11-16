//! The top of the Pull request surface: the toolbar over the PR, and what
//! the PR is: its title, state, author, branches, size and review.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    tag::Tag,
    v_flex, ActiveTheme as _, ColorName, Icon, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _,
};

use super::{PullRequestSurface, Shows};
use crate::pull_requests::{PrDetail, PrState, PrSummary, Refresh, RepoId, Review};

/// The strip above the PR: its repository and number, and the buttons to
/// open it on the forge, to link another by hand, to unlink one linked so,
/// and to read it again.
pub(super) fn toolbar(
    repo: Option<&RepoId>,
    pr: Option<&PrSummary>,
    shows: &Shows,
    cx: &Context<'_, PullRequestSurface>,
) -> impl IntoElement {
    let theme = cx.theme();
    let url = pr.and_then(|pr| web_url(&pr.url));
    h_flex()
        .flex_shrink_0()
        .h_8()
        .gap_1()
        .px_4()
        .border_b_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.muted_foreground)
        .children(repo.map(|repo| {
            div()
                .min_w_0()
                .truncate()
                .child(format!("{}/{}", repo.owner, repo.name))
        }))
        .when_some(pr, |this, pr| {
            this.child(
                div()
                    .flex_shrink_0()
                    .text_color(theme.foreground)
                    .child(format!("#{}", pr.id.number)),
            )
        })
        .when(matches!(shows, Shows::Linked(_)), |this| {
            this.child(div().flex_shrink_0().child("· linked"))
        })
        .child(div().flex_1())
        .when_some(url, |this, url| {
            this.child(
                Button::new("pull-request-open")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ExternalLink)
                    .tooltip("Open on GitHub")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
        })
        .child(
            Button::new("pull-request-link")
                .ghost()
                .xsmall()
                .icon(Lucide::Link)
                .tooltip("Link another pull request")
                .on_click(cx.listener(|this, _, window, cx| this.start_linking(window, cx))),
        )
        .when(matches!(shows, Shows::Linked(_)), |this| {
            this.child(
                Button::new("pull-request-unlink")
                    .ghost()
                    .xsmall()
                    .icon(Lucide::Unlink)
                    .tooltip("Unlink, closing its tab")
                    .on_click(cx.listener(|this, _, _, cx| this.unlink(cx))),
            )
        })
        .when(repo.is_some(), |this| this.child(refresh_button(cx)))
}

/// How a PR's state reads, here and on its sidebar row: its word, icon
/// and color.
pub(crate) fn state_look(state: PrState) -> (&'static str, Lucide, ColorName) {
    match state {
        PrState::Open => ("Open", Lucide::GitPullRequest, ColorName::Green),
        PrState::Draft => ("Draft", Lucide::GitPullRequestDraft, ColorName::Gray),
        PrState::Merged => ("Merged", Lucide::GitMerge, ColorName::Purple),
        PrState::Closed => ("Closed", Lucide::GitPullRequestClosed, ColorName::Red),
    }
}

/// The title, then its state, author and branches, then its size and
/// review.
pub(super) fn identity(
    summary: &PrSummary,
    detail: Option<&PrDetail>,
    cx: &Context<'_, PullRequestSurface>,
) -> impl IntoElement {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let (label, icon, color) = state_look(summary.state);
    let byline = h_flex()
        .flex_wrap()
        .items_center()
        .gap_x_2()
        .gap_y_1()
        .text_xs()
        .text_color(muted)
        .child(
            Tag::color(color).small().child(
                h_flex()
                    .gap_1()
                    .child(Icon::new(icon).xsmall())
                    .child(label),
            ),
        )
        .when_some(detail, |this, detail| {
            this.child(
                div()
                    .text_color(theme.foreground)
                    .child(format!("@{}", detail.author)),
            )
            .child(
                h_flex()
                    .min_w_0()
                    .gap_1()
                    .child(Icon::new(Lucide::GitBranch).xsmall())
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(format!("{} → {}", detail.head, detail.base)),
                    ),
            )
        });
    v_flex()
        .gap_2()
        .child(
            div()
                .id("pull-request-title")
                .test_support()
                .aria_label(summary.title.clone())
                .text_base()
                .font_semibold()
                .child(summary.title.clone()),
        )
        .child(byline)
        .when_some(detail, |this, detail| {
            this.child(size_and_review(detail, cx))
        })
}

/// "+10 −2 · 3 files · Approved".
pub(super) fn size_and_review(
    detail: &PrDetail,
    cx: &Context<'_, PullRequestSurface>,
) -> impl IntoElement {
    let theme = cx.theme();
    let review = match detail.review {
        Review::None => None,
        Review::Required => Some(("Review required", Lucide::CircleDashed, theme.yellow)),
        Review::ChangesRequested => Some(("Changes requested", Lucide::CircleX, theme.red)),
        Review::Approved => Some(("Approved", Lucide::CircleCheck, theme.green)),
    };
    let files = match detail.changed_files {
        1 => "1 file".to_owned(),
        count => format!("{count} files"),
    };
    h_flex()
        .flex_wrap()
        .items_center()
        .gap_x_2()
        .gap_y_1()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(
            div()
                .text_color(theme.green)
                .child(format!("+{}", detail.additions)),
        )
        .child(
            div()
                .text_color(theme.red)
                .child(format!("−{}", detail.deletions)),
        )
        .child(files)
        .when_some(review, |this, (review, icon, color)| {
            this.child("·").child(
                h_flex()
                    .gap_1()
                    .text_color(color)
                    .child(Icon::new(icon).xsmall())
                    .child(review),
            )
        })
}

/// Reads the PR again, its detail too, and checks again a CLI that
/// couldn't read it.
pub(super) fn refresh_button(cx: &Context<'_, PullRequestSurface>) -> Button {
    Button::new("pull-request-refresh")
        .ghost()
        .xsmall()
        .icon(IconName::RotateCw)
        .tooltip("Refresh")
        .on_click(cx.listener(|this, _, _, cx| {
            this.prs
                .update(cx, |prs, cx| prs.refresh(Refresh::Everything, cx));
        }))
}

/// `url` if it is a web page: check apps set their own links, and only
/// those open.
pub(super) fn web_url(url: &str) -> Option<String> {
    (url.starts_with("https://") || url.starts_with("http://")).then(|| url.to_owned())
}
