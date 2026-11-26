//! The top of the Pull request surface: the toolbar over the PR, and what
//! the PR is: its title, state, author, branches, size and review.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    tag::Tag,
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, Anchor, AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _,
};
use rho_ui_core::{
    LinkedPullRequest, PullRequestDetail, PullRequestId, PullRequestReview, PullRequestSummary,
};

use super::PullRequestSurface;
use crate::{
    pictures::Pictures,
    pr_mark::state_look,
    pull_requests::{PullRequests, Reading, Seen, Which},
};

/// What Unlink says it does to the session's `link`: it ends the agent's
/// watch too, while the agent watches it.
fn unlink_tooltip(link: Option<&LinkedPullRequest>) -> &'static str {
    if link.is_some_and(|link| link.watched) {
        "Unlink and end the agent's watch"
    } else {
        "Unlink"
    }
}

/// The strip above the PR the surface shows, by its `reading`: the PR's
/// repository and number, a menu of the session's other PRs (`choices`,
/// [`PullRequests::choices`]) when it has others, and the buttons to open
/// it on the forge, to link another, to unlink one linked, and to read it
/// again. While the daemon can't be asked, it offers nothing that asks it.
pub(super) fn toolbar(
    reading: &Reading<'_>,
    which: &Which,
    choices: &[(&PullRequestId, Option<&str>)],
    cx: &Context<'_, PullRequestSurface>,
) -> impl IntoElement {
    let theme = cx.theme();
    let pr = match reading.seen {
        Seen::Pr(pr) => Some(pr),
        Seen::Disconnected(_) | Seen::Unserved(_) | Seen::Looking | Seen::Nothing(_) => None,
    };
    let url = pr.and_then(|pr| web_url(&pr.url));
    let linked = match which {
        Which::Branch => None,
        Which::Linked(id) => Some(id),
    };
    // A linked PR is named before it is found; a branch's once it is.
    let id: Option<&PullRequestId> = pr.map(|pr| &pr.id).or(linked);
    let watched = reading.link.is_some_and(|link| link.watched);
    let asks = reading.seen.asks();
    let others = choices.iter().any(|(choice, _)| Some(*choice) != id);
    let name = if others {
        Some(pr_menu(id, choices, cx))
    } else {
        id.map(|id| pr_name(id, cx))
    };
    h_flex()
        .flex_shrink_0()
        .h_8()
        .gap_1()
        .px_4()
        .border_b_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.muted_foreground)
        .children(name)
        .when(linked.is_some(), |this| {
            this.child(div().flex_shrink_0().child("· linked"))
        })
        .when(watched, |this| {
            this.child(div().flex_shrink_0().child("· watched"))
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
        .when(asks, |this| {
            this.child(
                Button::new("pull-request-link")
                    .ghost()
                    .xsmall()
                    .icon(Lucide::Link)
                    .tooltip("Link another pull request")
                    .on_click(cx.listener(|this, _, window, cx| this.start_linking(window, cx))),
            )
        })
        .when(asks && linked.is_some(), |this| {
            this.child(
                Button::new("pull-request-unlink")
                    .ghost()
                    .xsmall()
                    .icon(Lucide::Unlink)
                    .tooltip(unlink_tooltip(reading.link))
                    .on_click(cx.listener(|this, _, _, cx| this.unlink(cx))),
            )
        })
        .when(asks, |this| this.child(refresh_button(cx)))
}

/// The PR `id` names: its repository, then its number.
fn pr_name(id: &PullRequestId, cx: &Context<'_, PullRequestSurface>) -> AnyElement {
    h_flex()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(format!("{}/{}", id.owner, id.name)),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_color(cx.theme().foreground)
                .child(format!("#{}", id.number)),
        )
        .into_any_element()
}

/// The PR `id` names, as a menu of the session's PRs (`choices`), which
/// shows the one picked.
fn pr_menu(
    id: Option<&PullRequestId>,
    choices: &[(&PullRequestId, Option<&str>)],
    cx: &Context<'_, PullRequestSurface>,
) -> AnyElement {
    let label = id.map_or_else(
        || "Pull requests".to_owned(),
        |id| format!("{}/{} #{}", id.owner, id.name, id.number),
    );
    let shown = id.cloned();
    let choices: Vec<(PullRequestId, String)> = choices
        .iter()
        .map(|(choice, title)| {
            let name = format!("{}/{}#{}", choice.owner, choice.name, choice.number);
            let name = if choice.host == "github.com" {
                name
            } else {
                format!("{}/{name}", choice.host)
            };
            let label = title.map_or_else(|| name.clone(), |title| format!("{name}  {title}"));
            ((*choice).clone(), label)
        })
        .collect();
    let surface = cx.entity().downgrade();
    Button::new("pull-request-pick")
        .ghost()
        .xsmall()
        .label(label)
        .dropdown_caret(true)
        .accessibility_label("Pull requests of this session")
        .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
            choices.iter().fold(menu, |menu, (choice, label)| {
                let (surface, choice) = (surface.clone(), choice.clone());
                menu.item(
                    PopupMenuItem::new(label.clone())
                        .checked(shown.as_ref() == Some(&choice))
                        .on_click(move |_, _, cx| {
                            let _ = surface.update(cx, |this, cx| this.pick(choice.clone(), cx));
                        }),
                )
            })
        })
        .into_any_element()
}

/// The title, then its state, author and branches, then its size and
/// review.
pub(super) fn identity(
    summary: &PullRequestSummary,
    detail: Option<&PullRequestDetail>,
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
    detail: &PullRequestDetail,
    cx: &Context<'_, PullRequestSurface>,
) -> impl IntoElement {
    let theme = cx.theme();
    let review = match detail.review {
        PullRequestReview::Required => {
            Some(("Review required", Lucide::CircleDashed, theme.yellow))
        }
        PullRequestReview::ChangesRequested => {
            Some(("Changes requested", Lucide::CircleX, theme.red))
        }
        PullRequestReview::Approved => Some(("Approved", Lucide::CircleCheck, theme.green)),
        // None asked or given, or a review a newer rho sends.
        _ => None,
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

/// Has the daemon read the PR again, its detail too.
pub(super) fn refresh_button(cx: &Context<'_, PullRequestSurface>) -> Button {
    Button::new("pull-request-refresh")
        .ghost()
        .xsmall()
        .icon(IconName::RotateCw)
        .tooltip("Refresh")
        .on_click(cx.listener(|this, _, _, cx| {
            this.prs.update(cx, PullRequests::refresh);
            this.pictures.update(cx, Pictures::retry);
        }))
}

/// `url` if it is a web page: check apps set their own links, and only
/// those open.
pub(super) fn web_url(url: &str) -> Option<String> {
    (url.starts_with("https://") || url.starts_with("http://")).then(|| url.to_owned())
}
