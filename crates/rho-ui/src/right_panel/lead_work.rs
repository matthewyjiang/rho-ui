//! The Work surface: a lead's work at a glance. It lists the sessions the
//! lead started, as the sidebar lists them, and the pull requests linked to
//! the lead, as [`PullRequests`] holds them, in three parts by [`Stage`]:
//! what needs the user, what is in progress, and what is finished. A
//! session's row opens it, as its row in the sidebar does; a PR linked to
//! the lead shows in the lead's Pull request surface. It redraws as the
//! sidebar's list and the PRs change.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{h_flex, list::ListItem, v_flex, ActiveTheme as _, StyledExt as _};
use gpui_kit::{
    div, AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, WeakEntity, Window,
};
use rho_ui_core::{LinkedPullRequest, PullRequestId, PullRequestState};

use super::{message, RightPanel};
use crate::{
    machines::SessionKey,
    pr_mark::{pr_mark, pr_pill},
    pull_requests::{Mark, PullRequests},
    session_list::Status,
    sidebar::SidebarView,
};

/// What a session's row says while it has no PR.
const NO_PULL_REQUEST: &str = "No pull request yet";

/// What the row of a linked PR the daemon couldn't read says. Its Pull
/// request surface says why.
const UNREADABLE: &str = "Can't read this pull request";

/// Which part of the surface a row is in, in the order they show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Its session waits for the user.
    NeedsYou,
    InProgress,
    /// Its PR is merged or closed.
    Finished,
}

impl Stage {
    const ALL: [Stage; 3] = [Stage::NeedsYou, Stage::InProgress, Stage::Finished];

    /// Its header's element ID.
    fn id(self) -> &'static str {
        match self {
            Stage::NeedsYou => "lead-work-needs-you",
            Stage::InProgress => "lead-work-in-progress",
            Stage::Finished => "lead-work-finished",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Stage::NeedsYou => "Needs you",
            Stage::InProgress => "In progress",
            Stage::Finished => "Finished",
        }
    }
}

/// A session the lead started, as the sidebar lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Worker {
    id: SessionKey,
    title: SharedString,
    /// What its row in the sidebar says of it.
    status: Option<Status>,
}

/// Whose a started session's PR is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Link {
    /// The lead links it too, so the lead's Pull request surface can show
    /// it.
    Lead,
    /// The session's alone.
    Session,
}

/// A started session's PR, as its sidebar row marks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pr<'a> {
    mark: Mark<'a>,
    link: Link,
}

/// A row of the surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item<'a> {
    /// A session the lead started, with its PR if it has one.
    Session(&'a Worker, Option<Pr<'a>>),
    /// A PR linked to the lead that no session it started has.
    PullRequest(Mark<'a>),
    /// A PR linked to the lead that no session it started has, and that
    /// the daemon couldn't read: it has no title or state to show.
    Unreadable(&'a PullRequestId),
}

impl Item<'_> {
    /// Waiting for the user comes first, whatever the PR; then a merged
    /// or closed PR is finished; anything else is in progress.
    fn stage(self) -> Stage {
        let ended = |mark: Mark<'_>| {
            matches!(
                mark.pr.state,
                PullRequestState::Merged | PullRequestState::Closed
            )
        };
        match self {
            Item::Session(worker, _) if matches!(worker.status, Some(Status::NeedsYou(_))) => {
                Stage::NeedsYou
            }
            Item::Session(_, Some(pr)) if ended(pr.mark) => Stage::Finished,
            Item::PullRequest(mark) if ended(mark) => Stage::Finished,
            Item::Session(..) | Item::PullRequest(_) | Item::Unreadable(_) => Stage::InProgress,
        }
    }

    /// What its row is titled: its PR's title if it has one, else its
    /// session's; a PR not read, by its ID.
    fn title(self) -> SharedString {
        match self {
            Item::Session(_, Some(pr)) => pr.mark.pr.title.clone().into(),
            Item::Session(worker, None) => worker.title.clone(),
            Item::PullRequest(mark) => mark.pr.title.clone().into(),
            Item::Unreadable(id) => id.to_string().into(),
        }
    }
}

/// The rows of one stage.
#[derive(Debug, PartialEq, Eq)]
struct Section<'a> {
    stage: Stage,
    items: Vec<Item<'a>>,
}

/// The surface's parts, from `workers`, the sessions the lead started with
/// each one's PR, and `linked`, the PRs linked to the lead. A row for each
/// session, then one for each linked PR no session has. A linked PR not
/// read yet has no title or state to show, and waits until it is, unless
/// the daemon says it couldn't read it. Each part keeps that order, and a
/// part with no rows is left out.
fn sections<'a>(
    workers: &'a [(Worker, Option<Mark<'a>>)],
    linked: &'a [LinkedPullRequest],
) -> Vec<Section<'a>> {
    let to_lead = |id: &PullRequestId| linked.iter().any(|link| link.id == *id);
    let theirs = |id: &PullRequestId| {
        workers
            .iter()
            .any(|(_, mark)| mark.is_some_and(|mark| mark.pr.id == *id))
    };
    let started = workers.iter().map(|(worker, mark)| {
        let pr = mark.map(|mark| Pr {
            mark,
            link: if to_lead(&mark.pr.id) {
                Link::Lead
            } else {
                Link::Session
            },
        });
        Item::Session(worker, pr)
    });
    let unclaimed = linked
        .iter()
        .filter(|link| !theirs(&link.id))
        .filter_map(|link| match (&link.summary, &link.trouble) {
            (Some(pr), _) => Some(Item::PullRequest(Mark {
                pr,
                watched: link.watched,
                more: 0,
            })),
            (None, Some(_)) => Some(Item::Unreadable(&link.id)),
            (None, None) => None,
        });
    let items: Vec<Item<'a>> = started.chain(unclaimed).collect();
    Stage::ALL
        .into_iter()
        .filter_map(|stage| {
            let items: Vec<Item<'a>> = items
                .iter()
                .copied()
                .filter(|item| item.stage() == stage)
                .collect();
            (!items.is_empty()).then_some(Section { stage, items })
        })
        .collect()
}

/// The Work surface of [`super::RightPanel`], for one lead.
pub(super) struct WorkSurface {
    lead: SessionKey,
    /// Lists the sessions the lead started, and opens them.
    sidebar: Entity<SidebarView>,
    prs: Entity<PullRequests>,
    /// Shows a PR linked to the lead in its Pull request surface.
    // Called, not sent an event: showing a PR needs a window, and the
    // panel makes this tab where it has none to subscribe with.
    panel: WeakEntity<RightPanel>,
    _subscriptions: [Subscription; 2],
}

impl WorkSurface {
    pub(super) fn new(
        lead: SessionKey,
        sidebar: Entity<SidebarView>,
        prs: Entity<PullRequests>,
        panel: WeakEntity<RightPanel>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let subscriptions = [
            cx.observe(&sidebar, |_, _, cx| cx.notify()),
            cx.observe(&prs, |_, _, cx| cx.notify()),
        ];
        Self {
            lead,
            sidebar,
            prs,
            panel,
            _subscriptions: subscriptions,
        }
    }

    /// Opens session `id`, as its row in the sidebar does.
    fn open_session(&self, id: &SessionKey, cx: &mut Context<'_, Self>) {
        self.sidebar.update(cx, |sidebar, cx| sidebar.open(id, cx));
    }

    /// Shows PR `id`, linked to the lead, in the lead's Pull request
    /// surface.
    fn show_pull_request(&self, id: &PullRequestId, window: &Window, cx: &mut Context<'_, Self>) {
        let _ = self
            .panel
            .update(cx, |panel, cx| panel.show_pull_request(id, window, cx));
    }

    /// `mark` as a button that shows its PR in the lead's Pull request
    /// surface.
    fn pill(mark: Mark<'_>, cx: &Context<'_, Self>) -> AnyElement {
        let id = mark.pr.id.clone();
        pr_pill(
            SharedString::from(format!("lead-work-pr-{id}")),
            &mark,
            cx.theme(),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            // Kit's Button passes the click on, and the row's own click
            // would open its session.
            cx.stop_propagation();
            this.show_pull_request(&id, window, cx);
        }))
        .into_any_element()
    }

    /// `item`'s row: its PR's mark and its title over what its session is
    /// doing, as the sidebar words it, or why its PR can't be read. A
    /// session's opens it; a PR's alone shows it.
    fn render_item(item: Item<'_>, cx: &Context<'_, Self>) -> AnyElement {
        let title = item.title();
        match item {
            Item::Session(worker, pr) => {
                let mark = pr.map(|pr| match pr.link {
                    Link::Lead => Self::pill(pr.mark, cx),
                    Link::Session => div()
                        .text_xs()
                        .child(pr_mark(&pr.mark, cx.theme()))
                        .into_any_element(),
                });
                let detail: Option<SharedString> = match (worker.status.map(Status::word), pr) {
                    (Some(word), Some(_)) => Some(word.into()),
                    (Some(word), None) => Some(format!("{word} · {NO_PULL_REQUEST}").into()),
                    (None, Some(_)) => None,
                    (None, None) => Some(NO_PULL_REQUEST.into()),
                };
                let id = worker.id.clone();
                row(
                    format!("lead-work-item-{}", worker.id),
                    title,
                    mark,
                    detail,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_session(&id, cx)))
                .into_any_element()
            }
            Item::PullRequest(mark) => {
                let id = mark.pr.id.clone();
                let pill = Self::pill(mark, cx);
                row(format!("lead-work-item-{id}"), title, Some(pill), None, cx)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_pull_request(&id, window, cx);
                    }))
                    .into_any_element()
            }
            Item::Unreadable(id) => {
                let id = id.clone();
                row(
                    format!("lead-work-item-{id}"),
                    title,
                    None,
                    Some(UNREADABLE.into()),
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.show_pull_request(&id, window, cx);
                }))
                .into_any_element()
            }
        }
    }

    /// `section`'s header over its rows.
    fn render_section(section: Section<'_>, cx: &Context<'_, Self>) -> AnyElement {
        let (id, label) = (section.stage.id(), section.stage.label());
        v_flex()
            .gap_0p5()
            .child(
                div()
                    .id(id)
                    .test_support()
                    .aria_label(label)
                    .pb_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .children(
                section
                    .items
                    .into_iter()
                    .map(|item| Self::render_item(item, cx)),
            )
            .into_any_element()
    }
}

/// A row with element ID `id`: `mark` and `title` on its first line, and
/// `detail`, muted and small, under them.
fn row(
    id: String,
    title: SharedString,
    mark: Option<AnyElement>,
    detail: Option<SharedString>,
    cx: &App,
) -> ListItem {
    let theme = cx.theme();
    ListItem::new(SharedString::from(id))
        .accessibility_label(title.clone())
        .px_2()
        .py_1()
        .rounded(theme.radius)
        .child(
            v_flex()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .text_sm()
                        .children(mark)
                        .child(div().min_w_0().truncate().child(title)),
                )
                .children(detail.map(|detail| {
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(detail)
                })),
        )
}

impl Render for WorkSurface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let cx = &*cx;
        let prs = self.prs.read(cx);
        let workers: Vec<(Worker, Option<Mark<'_>>)> = self
            .sidebar
            .read(cx)
            .started_by(&self.lead)
            .map(|row| {
                let worker = Worker {
                    id: row.id.clone(),
                    title: row.title(),
                    status: row.status(),
                };
                (worker, prs.mark(&row.id))
            })
            .collect();
        let linked = prs
            .of(&self.lead)
            .map_or(&[][..], |set| set.linked.as_slice());
        let sections = sections(&workers, linked);
        if sections.is_empty() {
            return message("No sessions or pull requests yet.", None);
        }
        v_flex()
            .id("lead-work")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .gap_4()
            .px_4()
            .py_3()
            .children(
                sections
                    .into_iter()
                    .map(|section| Self::render_section(section, cx)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use rho_ui_core::{
        PullRequestLinkOrigin, PullRequestRollup, PullRequestSummary, PullRequestTrouble,
    };
    use web_time::Instant;

    use super::*;

    fn id(number: u32) -> PullRequestId {
        PullRequestId::new("github.com", "acme", "widgets", number)
    }

    fn summary(number: u32, state: PullRequestState) -> PullRequestSummary {
        PullRequestSummary::new(
            id(number),
            format!("PR {number}"),
            format!("https://github.com/acme/widgets/pull/{number}"),
            state,
            PullRequestRollup::None,
            "abc",
            "2026-11-25T00:00:00Z",
        )
    }

    /// PR `number` linked to the lead, read as `state`, or not read yet.
    fn linked(number: u32, state: Option<PullRequestState>) -> LinkedPullRequest {
        let url = format!("https://github.com/acme/widgets/pull/{number}");
        let origin = PullRequestLinkOrigin::Agent;
        let mut link = LinkedPullRequest::new(id(number), url, origin, 1_767_312_100);
        link.summary = state.map(|state| summary(number, state));
        link
    }

    /// A session titled by its ID, which its sidebar row says `status` of.
    fn worker(id: &str, status: Option<Status>) -> Worker {
        Worker {
            id: SessionKey::local(id),
            title: id.into(),
            status,
        }
    }

    fn mark(pr: &PullRequestSummary) -> Mark<'_> {
        Mark {
            pr,
            watched: false,
            more: 0,
        }
    }

    /// The row of `worker`, with its PR as `link`'s when it has one.
    fn session<'a>(worker: &'a (Worker, Option<Mark<'a>>), link: Option<Link>) -> Item<'a> {
        let pr = worker.1.zip(link).map(|(mark, link)| Pr { mark, link });
        Item::Session(&worker.0, pr)
    }

    /// The row of `link`, read, that no session has.
    fn linked_row(link: &LinkedPullRequest) -> Item<'_> {
        Item::PullRequest(mark(link.summary.as_ref().expect("a read link")))
    }

    /// The sessions a lead started and the PRs linked to it join into rows:
    /// a linked PR that is a session's own is that session's row, one no
    /// session has is a row of its own, one not read yet waits, and one
    /// the daemon couldn't read is a row by its ID. A session waiting for
    /// the user needs you whatever its PR; a merged or closed PR is
    /// finished; the rest are in progress. Each part keeps the sessions'
    /// order, then the links', and an empty part is left out. A row is
    /// titled by its PR, else its session.
    #[test]
    fn rows_join_sessions_and_links_by_stage() {
        use Link::{Lead, Session};
        use PullRequestState::{Closed, Merged, Open};
        let now = Instant::now();
        let (pr1, pr3) = (summary(1, Open), summary(3, Merged));
        let (pr4, pr5) = (summary(4, Closed), summary(5, Merged));
        let workers = [
            (worker("w1", Some(Status::Working(now))), Some(mark(&pr1))),
            (worker("w2", None), None),
            (worker("w3", Some(Status::NeedsYou(now))), Some(mark(&pr3))),
            (worker("w4", Some(Status::Done)), Some(mark(&pr4))),
            (worker("w5", Some(Status::Background)), Some(mark(&pr5))),
        ];
        let mut unreadable = linked(9, None);
        unreadable.trouble = Some(PullRequestTrouble::Failed {
            message: "HTTP 502".to_owned(),
        });
        let links = [
            linked(1, Some(Open)),
            linked(6, Some(Open)),
            linked(7, Some(Merged)),
            linked(8, None),
            unreadable,
        ];
        let table: [(&[_], &[LinkedPullRequest], _, &[&str]); 2] = [
            (
                &workers,
                &links,
                vec![
                    Section {
                        stage: Stage::NeedsYou,
                        items: vec![session(&workers[2], Some(Session))],
                    },
                    Section {
                        stage: Stage::InProgress,
                        items: vec![
                            session(&workers[0], Some(Lead)),
                            session(&workers[1], None),
                            linked_row(&links[1]),
                            Item::Unreadable(&links[4].id),
                        ],
                    },
                    Section {
                        stage: Stage::Finished,
                        items: vec![
                            session(&workers[3], Some(Session)),
                            session(&workers[4], Some(Session)),
                            linked_row(&links[2]),
                        ],
                    },
                ],
                &[
                    "PR 3",
                    "PR 1",
                    "w2",
                    "PR 6",
                    "acme/widgets#9",
                    "PR 4",
                    "PR 5",
                    "PR 7",
                ],
            ),
            (
                &workers[1..2],
                &[],
                vec![Section {
                    stage: Stage::InProgress,
                    items: vec![session(&workers[1], None)],
                }],
                &["w2"],
            ),
        ];
        for (workers, links, expected, titles) in table {
            let got = sections(workers, links);
            let got_titles: Vec<String> = got
                .iter()
                .flat_map(|section| &section.items)
                .map(|item| item.title().to_string())
                .collect();
            assert_eq!(got, expected);
            assert_eq!(got_titles, titles);
        }
    }
}
