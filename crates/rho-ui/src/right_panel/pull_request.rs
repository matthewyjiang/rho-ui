//! The Pull request surface: one of a session's PRs at a time, as its
//! machine's rho daemon reads it ([`PullRequests`]): the one whose head is
//! its branch, or one linked to the session, by hand ([`link`]) or by its
//! agent. Its toolbar switches between them ([`Which`],
//! [`PullRequests::which`]). It shows what the PR is ([`header`]), its
//! checks ([`checks`]), its description, and its threads on the code and
//! conversation, with a box to comment in ([`conversation`]). It asks the
//! daemon to keep reading the PR only while it shows; the session's turn
//! ending reads it again, and so does Refresh. What it shows is
//! [`PullRequests::reading`]'s to say; the surface draws it.

mod actions;
mod checks;
mod conversation;
mod diagrams;
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
    div, relative, AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity,
    Focusable as _, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
};
use rho_ui_core::{PullRequestDetail, PullRequestId, PullRequestSummary};

use self::{
    actions::Acting,
    checks::{check_counts, checks},
    conversation::{open_threads, Posting},
    diagrams::DiagramState,
    header::{identity, toolbar, web_url},
    link::{link_button, LinkForm, Unlinking},
};
use crate::{
    machines::SessionKey,
    pull_requests::{every_post, PullRequests, Seen, Shown, Which},
    time,
};

/// Whether a surface is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Visibility {
    Shown,
    Hidden,
}

/// The Pull request surface of [`super::RightPanel`]: one PR of the
/// session its tab belongs to.
pub(super) struct PullRequestSurface {
    prs: Entity<PullRequests>,
    /// Its session.
    session: SessionKey,
    /// What it shows, kept by [`PullRequests::which`] as the session's
    /// PRs change.
    which: Which,
    /// Whether it is on screen: the tab shown in a panel that shows.
    visibility: Visibility,
    /// The Markdown of the PR on screen, parsed once per text it has: its
    /// description under [`DESCRIPTION`], and each post under its URL.
    texts: HashMap<SharedString, (SharedString, Entity<TextViewState>)>,
    /// The diagrams in those texts.
    diagrams: DiagramState,
    /// What the user has under way on the PR on screen.
    pr: PrState,
    /// The form to link a PR by hand, while it is open.
    link_form: Option<LinkForm>,
    _subscriptions: [Subscription; 1],
}

/// What the user has under way on the one PR on screen, which the next PR
/// starts without, so nothing meant for one lands on another. Dropping it
/// stops waiting on what it asked.
#[derive(Default)]
struct PrState {
    /// The comment threads on the code the user opened or closed, by id:
    /// those resolved start closed, the rest open.
    flipped: HashSet<String>,
    /// The box to write a comment in, and its events. It is made on the
    /// first render, which has the window it needs.
    comment_box: Option<(Entity<TextareaState>, Subscription)>,
    posting: Posting,
    unlinking: Unlinking,
    acting: Acting,
}

impl PullRequestSurface {
    pub(super) fn new(
        prs: Entity<PullRequests>,
        session: SessionKey,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let subscriptions = [cx.observe(&prs, |this, _, cx| {
            this.sync_which(cx);
            this.sync_texts(cx);
            this.forget_stale_refusal(cx);
            cx.notify();
        })];
        // Closing the tab stops the daemon reading it for this surface.
        let surface = cx.entity_id();
        cx.on_release(move |this, cx| {
            this.prs.update(cx, |prs, cx| prs.hide(surface, cx));
        })
        .detach();
        let which = prs.read(cx).which(&session, &Which::Branch);
        let mut this = Self {
            prs,
            session,
            which,
            visibility: Visibility::Hidden,
            texts: HashMap::new(),
            diagrams: DiagramState::new(),
            pr: PrState::default(),
            link_form: None,
            _subscriptions: subscriptions,
        };
        this.watch(cx);
        this
    }

    /// Reads the PR again as the session's turn ended, if it shows.
    pub(super) fn turn_ended(&mut self, cx: &mut Context<'_, Self>) {
        if self.visibility == Visibility::Shown {
            self.prs.update(cx, PullRequests::show_fresh);
        }
    }

    /// Shows the surface or hides it: only the one on screen is read.
    pub(super) fn set_visibility(&mut self, visibility: Visibility, cx: &mut Context<'_, Self>) {
        if self.visibility != visibility {
            self.visibility = visibility;
            self.watch(cx);
        }
    }

    /// Shows PR `id` of the session's: its branch's or a linked one.
    pub(super) fn pick(&mut self, id: PullRequestId, cx: &mut Context<'_, Self>) {
        self.show(&Which::Linked(id), cx);
    }

    /// Follows [`PullRequests::which`] as the session's PRs change: a PR
    /// unlinked falls back to another, and the branch's lookup finding
    /// none shows the PR linked last.
    fn sync_which(&mut self, cx: &mut Context<'_, Self>) {
        self.show(&self.which.clone(), cx);
    }

    /// Shows `which`, as [`PullRequests::which`] has it. What was under
    /// way on the PR it showed is dropped, so a comment or merge meant for
    /// it can't land on the next.
    fn show(&mut self, which: &Which, cx: &mut Context<'_, Self>) {
        let which = self.prs.read(cx).which(&self.session, which);
        if which == self.which {
            return;
        }
        self.which = which;
        self.pr = PrState::default();
        self.watch(cx);
    }

    /// The linked PR that it shows, if it shows one.
    fn linked(&self) -> Option<&PullRequestId> {
        match &self.which {
            Which::Branch => None,
            Which::Linked(id) => Some(id),
        }
    }

    /// What its tab says: the PR's number, once its branch's is found.
    pub(super) fn label(&self, cx: &App) -> SharedString {
        let number = match &self.which {
            Which::Linked(id) => Some(id.number),
            Which::Branch => self
                .prs
                .read(cx)
                .summary(&self.shown())
                .map(|summary| summary.id.number),
        };
        number.map_or_else(
            || "Pull request".into(),
            |number| format!("#{number}").into(),
        )
    }

    /// What the surface shows, as [`PullRequests`] names it.
    fn shown(&self) -> Shown {
        Shown {
            session: self.session.clone(),
            which: self.which.clone(),
        }
    }

    /// Tells [`PullRequests`] whether this surface is on screen, so the
    /// daemon reads what it shows.
    fn watch(&mut self, cx: &mut Context<'_, Self>) {
        let surface = cx.entity_id();
        let shown = self.shown();
        let visibility = self.visibility;
        self.prs.update(cx, |prs, cx| match visibility {
            Visibility::Shown => prs.show(surface, shown, cx),
            Visibility::Hidden => prs.hide(surface, cx),
        });
        self.sync_texts(cx);
        cx.notify();
    }

    /// Whether the comment box, or the form to link a PR, has the keyboard.
    pub(super) fn has_keyboard(&self, window: &Window, cx: &App) -> bool {
        let comment = self
            .pr
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
    fn shown_pr(&self, cx: &App) -> Option<PullRequestId> {
        let prs = self.prs.read(cx);
        Some(prs.summary(&self.shown())?.id.clone())
    }

    /// The whole PR on screen, once the daemon read it.
    fn shown_detail<'a>(&self, cx: &'a App) -> Option<&'a PullRequestDetail> {
        let prs = self.prs.read(cx);
        let summary = prs.summary(&self.shown())?;
        prs.detail(&self.session.machine, &summary.id)
    }

    /// Parses the Markdown of the PR on screen that changed, and drops
    /// what it no longer has. Its diagrams are drawn while it shows: again
    /// when any changed, and not while hidden, as a hidden tab's backlog
    /// would hold up the diagrams of the PR on screen.
    fn sync_texts(&mut self, cx: &mut Context<'_, Self>) {
        let wanted: Vec<(SharedString, SharedString)> = self
            .shown_detail(cx)
            .map(|detail| {
                let posts = every_post(detail).map(|post| {
                    (
                        SharedString::from(post.url.clone()),
                        SharedString::from(post.body.clone()),
                    )
                });
                let description = (
                    SharedString::from(DESCRIPTION),
                    SharedString::from(detail.body.clone()),
                );
                std::iter::once(description)
                    .chain(posts)
                    .filter(|(_, text)| !text.trim().is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let mut parsed = std::mem::take(&mut self.texts);
        let mut changed = false;
        let order = wanted.iter().map(|(_, text)| text.clone()).collect();
        for (key, text) in wanted {
            let state = match parsed.remove(&key) {
                Some((shown, state)) if shown == text => state,
                Some((_, state)) => {
                    changed = true;
                    state.update(cx, |state, cx| state.set_text(&text, cx));
                    state
                }
                None => {
                    changed = true;
                    cx.new(|cx| TextViewState::markdown(&text, cx))
                }
            };
            self.texts.insert(key, (text, state));
        }
        match self.visibility {
            Visibility::Hidden => self.diagrams.stop(),
            Visibility::Shown if changed || self.diagrams.stopped() => {
                self.draw_diagrams(order, cx);
            }
            Visibility::Shown => {}
        }
    }

    /// `key`'s Markdown, as [`Self::texts`] holds it, with its diagrams and
    /// formulas drawn.
    fn text(&self, key: &str) -> Option<AnyElement> {
        let (_, state) = self.texts.get(key)?;
        let view = TextView::new(state)
            .markdown_extensions(self.diagrams.extensions())
            // Its links are anyone's: only web ones open, as Kit opens
            // links without a handler.
            .on_link_click(|url, event, _, cx| {
                let opens = match event {
                    ClickEvent::Mouse(click) => {
                        matches!(click.up.button, MouseButton::Left | MouseButton::Middle)
                    }
                    ClickEvent::Keyboard(_) => true,
                    ClickEvent::Touch(click) => !click.long_press,
                };
                if let Some(url) = web_url(url).filter(|_| opens) {
                    cx.open_url(&url);
                }
            });
        Some(div().text_sm().child(view).into_any_element())
    }

    /// The PR: what it is, then once its detail is read, its checks and
    /// description, each under a rule.
    fn render_pr(
        &self,
        summary: &PullRequestSummary,
        detail: Option<&PullRequestDetail>,
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
        let prs = self.prs.read(cx);
        let reading = prs.reading(&self.shown());
        // Why what shows may be stale: what this surface was just refused
        // first, then what the reading says.
        let trouble = match &reading.seen {
            Seen::Disconnected(_) | Seen::Unserved(_) => None,
            Seen::Looking | Seen::Nothing(_) | Seen::Pr(_) => self
                .pr
                .unlinking
                .refusal()
                .cloned()
                .or_else(|| reading.trouble.clone()),
        };
        let content = match &reading.seen {
            Seen::Disconnected(text) => Some(message(text.clone(), None, None)),
            Seen::Unserved(text) => Some(
                div()
                    .id("pull-request-unserved")
                    .test_support()
                    .aria_label(text.clone())
                    .h_full()
                    .child(message(text.clone(), None, None))
                    .into_any_element(),
            ),
            // The trouble says why there is nothing yet.
            Seen::Looking if trouble.is_some() => None,
            Seen::Looking => Some(message(
                "Looking for a pull request…",
                Some(Spinner::new().into_any_element()),
                None,
            )),
            Seen::Nothing(why) => Some(message(why.clone(), None, Some(link_button(cx)))),
            Seen::Pr(summary) => {
                Some(self.render_pr(summary, prs.detail(&self.session.machine, &summary.id), cx))
            }
        };
        v_flex()
            .id("pull-request")
            .test_support()
            .size_full()
            .child(toolbar(
                &reading,
                &self.which,
                &prs.choices(&self.session),
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
