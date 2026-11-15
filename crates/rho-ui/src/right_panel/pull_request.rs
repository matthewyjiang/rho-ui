//! The Pull request surface: the PR whose head is the branch of the session
//! on screen, as [`PullRequests`] reads it: what it is ([`header`]), its
//! checks ([`checks`]), its description, and its threads on the code and
//! conversation, with a box to comment in ([`conversation`]). It watches the
//! branch only while it shows, so a hidden panel or another tab costs no
//! queries; the session's turn ending reads it again, and so does Refresh.

mod actions;
mod checks;
mod conversation;
mod header;
mod link;

use std::collections::{HashMap, HashSet};

use gpui_kit::base::{ObservedElement, TestSupportExt as _};
use gpui_kit::component::{
    alert::Alert,
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia},
    h_flex,
    input::TextareaState,
    skeleton::Skeleton,
    spinner::Spinner,
    text::{TextView, TextViewState},
    v_flex, ActiveTheme as _, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, relative, AnyElement, App, AppContext as _, Context, Div, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window,
};

pub(crate) use self::header::state_look;
use self::{
    actions::Acting,
    checks::{check_counts, checks},
    conversation::{open_threads, Posting},
    header::{identity, toolbar, web_url, Linked},
    link::{link_button, LinkForm},
};
use crate::{
    machines::{Place, SessionKey},
    pull_requests::{
        Found, Lookup, NoTarget, PrDetail, PrId, PrLinks, PrSummary, PullRequests, Refresh, RepoId,
        Target, Trouble, Watch, BUDGET_FLOOR,
    },
    session_view::{SessionEvent, SessionView},
    time,
    workspaces::{WorkspaceRef, Workspaces},
};

/// Whether the surface is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Visibility {
    Shown,
    Hidden,
}

/// The session on screen, as the surface follows it.
struct Followed {
    key: SessionKey,
    workspace: WorkspaceRef,
    /// Reads the PR again when the session's turn ends.
    _turns: Subscription,
}

/// The Pull request surface of [`super::RightPanel`]: it follows the
/// session the panel shows.
pub(super) struct PullRequestSurface {
    prs: Entity<PullRequests>,
    workspaces: Entity<Workspaces>,
    session: Option<Followed>,
    /// Whether it is on screen: the tab shown in a panel that shows.
    visibility: Visibility,
    /// The Markdown of the PR on screen, parsed once per text it has: its
    /// description under [`DESCRIPTION`], and each post under its URL.
    texts: HashMap<SharedString, (SharedString, Entity<TextViewState>)>,
    /// The comment threads on the code the user opened or closed, by id:
    /// those resolved start closed, the rest open.
    flipped: HashSet<SharedString>,
    /// The box to write a comment in, and its events. It is made on the
    /// first render, which has the window it needs.
    comment_box: Option<(Entity<TextareaState>, Subscription)>,
    posting: Posting,
    /// The form to link a PR by hand, while it is open.
    link_form: Option<LinkForm>,
    acting: Acting,
    _subscriptions: [Subscription; 2],
}

impl PullRequestSurface {
    pub(super) fn new(
        prs: Entity<PullRequests>,
        workspaces: Entity<Workspaces>,
        session: Option<&Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let subscriptions = [
            cx.observe(&prs, |this, _, cx| {
                this.sync_texts(cx);
                cx.notify();
            }),
            // A branch switch shows in the workspace once it is read again.
            cx.observe(&workspaces, |this, _, cx| this.watch(cx)),
        ];
        // Closing the tab stops the polling for it.
        cx.on_release(|this, cx| {
            this.prs.update(cx, |prs, cx| prs.set_on_screen(None, cx));
        })
        .detach();
        let mut this = Self {
            prs,
            workspaces,
            session: None,
            visibility: Visibility::Hidden,
            texts: HashMap::new(),
            flipped: HashSet::new(),
            comment_box: None,
            posting: Posting::Idle,
            link_form: None,
            acting: Acting::Idle,
            _subscriptions: subscriptions,
        };
        this.set_session(session, cx);
        this
    }

    /// Shows the PR of `session`'s branch, or nothing without one.
    pub(super) fn set_session(
        &mut self,
        session: Option<&Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) {
        let key = session.map(|view| view.read(cx).live().id);
        if key.as_ref() != self.session.as_ref().map(|followed| &followed.key) {
            // A form open for the last session doesn't link this one, nor
            // does a merge asked for there, or a comment written there, go
            // to this one's PR. One under way still finishes.
            self.link_form = None;
            if !matches!(self.acting, Acting::Running { .. }) {
                self.acting = Acting::Idle;
            }
            if !matches!(self.posting, Posting::Sending { .. }) {
                self.comment_box = None;
                self.posting = Posting::Idle;
            }
        }
        self.session = session.map(|view| {
            let live = view.read(cx).live();
            let place = Place::new(live.id.machine.clone(), live.cwd);
            let workspace = WorkspaceRef::new(&self.workspaces, &place, cx);
            let turns = cx.subscribe(view, |this, _, event: &SessionEvent, cx| {
                if matches!(event, SessionEvent::Saved) && this.visibility == Visibility::Shown {
                    this.prs
                        .update(cx, |prs, cx| prs.refresh(Refresh::Changed, cx));
                }
            });
            Followed {
                key: live.id,
                workspace,
                _turns: turns,
            }
        });
        self.watch(cx);
    }

    /// Shows the surface or hides it: only one on screen polls.
    pub(super) fn set_visibility(&mut self, visibility: Visibility, cx: &mut Context<'_, Self>) {
        if self.visibility != visibility {
            self.visibility = visibility;
            self.watch(cx);
        }
    }

    /// What the surface follows: the PR linked to the session, else its
    /// branch's, in a repository on a forge; or why there is none.
    fn target(&self, cx: &App) -> Result<Target, SharedString> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| SharedString::from("Open a session to see its pull request."))?;
        let workspace = session.workspace.read(cx);
        Target::of(&session.key, workspace, cx.global::<PrLinks>()).map_err(|why| match why {
            NoTarget::NotGit => "This session's folder isn't in a git repository.".into(),
            NoTarget::Detached(hash) => {
                format!("This session's checkout is on no branch, at {hash}.").into()
            }
            NoTarget::NoForge => "This repository has no origin remote on a forge.".into(),
        })
    }

    /// The repository of the session's origin remote, if it is on a forge.
    fn origin(&self, cx: &App) -> Option<RepoId> {
        let workspace = self.session.as_ref()?.workspace.read(cx);
        RepoId::from_url(workspace.origin()?)
    }

    /// Tells [`PullRequests`] what to watch for this surface.
    fn watch(&mut self, cx: &mut Context<'_, Self>) {
        let watch = self
            .target(cx)
            .ok()
            .filter(|_| self.visibility == Visibility::Shown)
            .and_then(|target| {
                let machine = self.session.as_ref()?.key.machine.clone();
                Some(Watch { machine, target })
            });
        self.prs.update(cx, |prs, cx| prs.set_on_screen(watch, cx));
        self.sync_texts(cx);
        cx.notify();
    }

    /// Whether the comment box, or the form to link a PR, has the keyboard.
    pub(super) fn has_keyboard(&self, window: &Window, cx: &App) -> bool {
        let comment = self
            .comment_box
            .as_ref()
            .is_some_and(|(input, _)| input.focus_handle(cx).is_focused(window));
        let link = self
            .link_form
            .as_ref()
            .is_some_and(|form| form.has_keyboard(window, cx));
        comment || link
    }

    /// The PR on screen, if one is.
    fn shown_pr(&self, cx: &App) -> Option<PrId> {
        let target = self.target(cx).ok()?;
        Some(self.prs.read(cx).pr(&target)?.id.clone())
    }

    /// Parses the Markdown of the PR on screen that changed, and drops
    /// what it no longer has.
    fn sync_texts(&mut self, cx: &mut Context<'_, Self>) {
        let wanted: Vec<(SharedString, SharedString)> = self
            .target(cx)
            .ok()
            .and_then(|target| {
                let prs = self.prs.read(cx);
                let summary = prs.pr(&target)?;
                let detail = prs.detail(&summary.id)?;
                let posts = detail
                    .posts
                    .iter()
                    .chain(detail.threads.iter().flat_map(|thread| &thread.comments))
                    .map(|post| (post.url.clone(), post.body.clone()));
                Some(
                    std::iter::once((DESCRIPTION.into(), detail.body.clone()))
                        .chain(posts)
                        .filter(|(_, text)| !text.trim().is_empty())
                        .collect(),
                )
            })
            .unwrap_or_default();
        let mut parsed = std::mem::take(&mut self.texts);
        for (key, text) in wanted {
            let state = match parsed.remove(&key) {
                Some((shown, state)) if shown == text => state,
                Some((_, state)) => {
                    state.update(cx, |state, cx| state.set_text(&text, cx));
                    state
                }
                None => cx.new(|cx| TextViewState::markdown(&text, cx)),
            };
            self.texts.insert(key, (text, state));
        }
    }

    /// `key`'s Markdown, as [`Self::texts`] holds it.
    fn text(&self, key: &str) -> Option<AnyElement> {
        let (_, state) = self.texts.get(key)?;
        Some(
            div()
                .text_sm()
                // Its links are anyone's: only web ones open.
                .child(TextView::new(state).on_link_click(|url, _, _, cx| {
                    if let Some(url) = web_url(url) {
                        cx.open_url(&url);
                    }
                }))
                .into_any_element(),
        )
    }

    /// The PR: what it is, then once its detail is read, its checks and
    /// description, each under a rule.
    fn render_pr(
        &self,
        summary: &PrSummary,
        detail: Option<&PrDetail>,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let content = v_flex().gap_6().child(identity(summary, detail, cx));
        let Some(detail) = detail else {
            // Where the checks and description go, while they are read.
            return content
                .child(
                    v_flex()
                        .gap_2()
                        .child(Skeleton::new().h_4().w(relative(0.35)))
                        .child(Skeleton::new().h_4().w(relative(0.8)))
                        .child(Skeleton::new().h_4().w(relative(0.6))),
                )
                .into_any_element();
        };
        let description = self
            .text(DESCRIPTION)
            .unwrap_or_else(|| quiet("No description.", cx));
        let now = time::now_unix();
        content
            .child(section(
                "Checks",
                (!detail.checks.is_empty()).then(|| check_counts(&detail.checks, cx)),
                checks(detail, cx),
                cx,
            ))
            .children(self.actions(detail, cx))
            .child(section("Description", None, description, cx))
            .when(!detail.threads.is_empty(), |this| {
                this.child(section(
                    "On the code",
                    Some(quiet(open_threads(detail), cx)),
                    self.threads(detail, now, cx),
                    cx,
                ))
            })
            // Last, as it ends in the box to comment in.
            .child(section(
                "Conversation",
                None,
                self.conversation(detail, now, cx),
                cx,
            ))
            .into_any_element()
    }
}

/// The key of a PR's description in [`PullRequestSurface::texts`].
const DESCRIPTION: &str = "description";

/// A row in a list that a click acts on: its fill on hover reaches past
/// the list's edges, so what it starts with lines up with the heading.
fn row(id: impl Into<SharedString>, cx: &App) -> ObservedElement<Stateful<Div>> {
    let theme = cx.theme();
    h_flex()
        .id(id.into())
        .test_support()
        .h_7()
        .mx_neg_2()
        .px_2()
        .gap_2()
        .rounded(theme.radius)
}

/// Small muted text, for what isn't there and for counts.
fn quiet(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
        .into_any_element()
}

/// A part of the PR: its heading, with `aside` to the right, over `body`.
fn section(
    title: &'static str,
    aside: Option<AnyElement>,
    body: AnyElement,
    cx: &Context<'_, PullRequestSurface>,
) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .font_semibold()
                        .text_color(theme.muted_foreground)
                        .child(title),
                )
                .children(aside),
        )
        .child(body)
}

/// Why what shows may be out of date, as a person reads it.
fn trouble_text(trouble: &Trouble<'_>) -> SharedString {
    match trouble {
        Trouble::NoReader(why) | Trouble::Failed(why) => (*why).clone(),
        Trouble::Paused(budget) => {
            // An upper bound, which stays true as the minutes pass unredrawn.
            // GitHub's limit resets hourly; hours are for a forge that doesn't.
            let minutes = (budget.resets_at - time::now_unix())
                .max(0)
                .unsigned_abs()
                .div_ceil(60);
            let wait = if minutes <= 60 {
                format!("{minutes} min")
            } else {
                format!("{} h", minutes.div_ceil(60))
            };
            format!(
                "Paused: this GitHub account has {} API points left, and rho-ui leaves the last \
                 {BUDGET_FLOOR} for other uses. Its limit resets within {wait}.",
                budget.remaining
            )
            .into()
        }
    }
}

/// A message in place of the PR, with `action` under it.
fn message(
    text: impl Into<SharedString>,
    media: Option<AnyElement>,
    action: Option<AnyElement>,
) -> AnyElement {
    let mut header = EmptyHeader::new().description(EmptyDescription::new().child(text.into()));
    if let Some(media) = media {
        header = header.media(EmptyMedia::new().child(media));
    }
    Empty::new()
        .h_full()
        .header(header)
        .when_some(action, |this, action| {
            this.content(EmptyContent::new().child(action))
        })
        .into_any_element()
}

impl Render for PullRequestSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.ensure_comment_box(window, cx);
        let cx = &*cx;
        if self.session.is_none() {
            return message("Open a session to see its pull request.", None, None);
        }
        let target = self.target(cx);
        let prs = self.prs.read(cx);
        let (pr, trouble, content) = match &target {
            Err(why) => (
                None,
                None,
                Some(message(why.clone(), None, Some(link_button(cx)))),
            ),
            Ok(target) => {
                let trouble = prs.trouble(target).map(|trouble| trouble_text(&trouble));
                let lookup = prs.lookup(target);
                let pr = prs.pr(target);
                let content = match lookup {
                    // The trouble says why there is nothing yet.
                    Lookup::Looking if trouble.is_some() => None,
                    Lookup::Looking => Some(message(
                        "Looking for a pull request…",
                        Some(Spinner::new().into_any_element()),
                        None,
                    )),
                    Lookup::Found(Found::Pr(summary)) => {
                        Some(self.render_pr(summary, prs.detail(&summary.id), cx))
                    }
                    lookup => Some(message(
                        nothing_found(target, &lookup),
                        None,
                        Some(link_button(cx)),
                    )),
                };
                (pr, trouble, content)
            }
        };
        let linked = match target {
            Ok(Target::Pr(_)) => Linked::ByHand,
            _ => Linked::ByBranch,
        };
        v_flex()
            .id("pull-request")
            .test_support()
            .size_full()
            .child(toolbar(
                target.as_ref().ok().map(Target::repo),
                pr,
                linked,
                cx,
            ))
            .children(self.link_form(cx))
            .child(
                v_flex()
                    .id("pull-request-body")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_4()
                    .px_4()
                    .py_4()
                    .when_some(trouble, |this, why| {
                        this.child(
                            div()
                                .id("pull-request-trouble")
                                .test_support()
                                .child(Alert::warning("trouble", why).small()),
                        )
                    })
                    .children(content),
            )
            .into_any_element()
    }
}

/// What the surface says when `target` has no PR to show, as `lookup` found.
fn nothing_found(target: &Target, lookup: &Lookup<'_>) -> String {
    match (target, lookup) {
        (Target::Pr(id), _) => format!(
            "There's no pull request #{} in {}/{}.",
            id.number, id.repo.owner, id.repo.name
        ),
        (Target::Branch(branch), Lookup::DefaultBranch) => {
            format!(
                "This session works on {}, the default branch.",
                branch.branch
            )
        }
        (Target::Branch(branch), Lookup::Found(Found::OnlyForks(read))) => format!(
            "The {read} newest pull requests from a branch named {} all come from forks. \
             If this repository has its own, it is older.",
            branch.branch
        ),
        (Target::Branch(branch), _) => format!("No pull request for {}.", branch.branch),
    }
}
