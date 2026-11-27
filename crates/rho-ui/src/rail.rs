//! The timeline rail: the cards rho sends between two replies, such as a
//! background process that ended, a session that finished or asks, or a
//! watched pull request that changed, on a thin rail in the transcript's
//! left gutter. A card takes one row for each event it tells of (two on
//! one pull request take two), or one for its title from a rho that
//! doesn't say. The rail stops at each reply: a run of cards is one stretch
//! of it.
//!
//! A row's dot says what its event asks of the reader ([`Urgency`]): a
//! ring for news, filled when someone must act (a question, a failure), so
//! the urgency doesn't rest on color alone. A card that bundles routine
//! news with a failure draws each row as its own. Clicking a row opens the
//! card's body under it; its pull request's chip shows the pull request,
//! and a session it is about opens from the row. Past the first
//! [`FOLD_AFTER`] rows of one run, the cards that don't need the reader
//! fold behind one row, whole: a card rho says needs the reader, as it
//! does when one of its events does, never folds, routine rows and all
//! (see [`places`]).
//!
//! Each card is one of the transcript's rows, so only the rows near the
//! window are drawn: each draws its own stretch of the line, which meets
//! its neighbors' as the rows touch. The transcript places a run's cards
//! ([`places`]) once, when the run changes, not each frame: a folded card
//! is an empty row, so every one in view draws each frame.
//!
//! The sizes come from the C1 mockup (`wake-rail.html`) the design was
//! picked from.
//!
//! The rail and its dots are layout, drawn with GPUI's divs: GPUI Kit has
//! no timeline element.

use std::rc::Rc;

use chrono::{DateTime, Utc};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::{ObservedElement, TestSupportExt as _};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    tag::Tag,
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Icon, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, AnyElement, App, Div, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, Window,
};
use rho_ui_core::{
    Notice, NoticeTone, NotificationItem, NotificationOutcome, NotificationSubject, PullRequestId,
    SessionId, Urgency,
};

use crate::{time, transcript::find::Marks, work::Disclosure};

/// How many rows of a run always show before the rest may fold. A card
/// folds whole, so the budget rounds up: a card that starts inside it
/// shows all its rows, even past it. It comes from the C1 mockup the
/// design was picked from, not from measuring real sessions: tune it once
/// leads' runs of cards are seen in use.
pub(crate) const FOLD_AFTER: usize = 4;

/// The fewest rows a fold hides: a fold row in place of one row would
/// save no room.
const FOLD_AT_LEAST: usize = 2;

/// The gutter's width, which the rail sits in.
const GUTTER: f32 = 24.;
/// The rail's distance from the gutter's left edge.
const RAIL_X: f32 = 7.;
/// A row's line height; its dot centers on it.
const ROW_HEIGHT: f32 = 22.;
/// The padding at each end of a row, which its hover shows. A row reaches
/// this far past the measure on both sides, so its time ends at the
/// measure's edge, where a reply's does.
const ROW_PAD: f32 = 6.;
/// A dot's diameter, and its ring's width.
const DOT: f32 = 9.;
const DOT_RING: f32 = 2.;
/// A fold row's small dots, one for each folded row up to
/// [`STACKED_DOTS`], each overlapping the next by [`STACKED_OVERLAP`]: the
/// mockup's stack, which reads as "several" without counting them.
const STACKED_DOT: f32 = 7.;
const STACKED_RING: f32 = 1.5;
const STACKED_OVERLAP: f32 = 2.;
const STACKED_DOTS: usize = 4;
/// How many kinds of outcome a fold row's tally names before it counts the
/// rest as "other": three keep the line short in the mockup's measure.
const TALLY_KINDS: usize = 3;

/// The group a card's hover shows its chevron and its links through.
const CARD_GROUP: &str = "rail-card";

/// Whether a card that asks `urgency`, that of its most urgent row, may
/// fold: only one that needs the reader never does.
fn may_fold(urgency: Urgency) -> bool {
    urgency != Urgency::NeedsYou
}

/// What placing a card of a run takes: what it asks of the reader, and
/// how many rows it draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RunCard {
    urgency: Urgency,
    rows: usize,
}

impl RunCard {
    pub(crate) fn of(notice: &Notice) -> Self {
        Self {
            urgency: notice.urgency(),
            rows: rows(notice),
        }
    }
}

/// How many rows `notice` draws: one per item, or its title alone from a
/// rho that doesn't say ([`Card::of`]).
fn rows(notice: &Notice) -> usize {
    notice.items().len().max(1)
}

/// The places in a run of these cards that fold behind one row: those
/// that may fold and start past the first [`FOLD_AFTER`] rows, once they
/// draw at least [`FOLD_AT_LEAST`] rows. None fold otherwise.
fn folded(cards: &[RunCard]) -> Vec<usize> {
    let (mut above, mut hidden) = (0, 0);
    let mut folds = Vec::new();
    for (at, card) in cards.iter().enumerate() {
        if above >= FOLD_AFTER && may_fold(card.urgency) {
            folds.push(at);
            hidden += card.rows;
        }
        above += card.rows;
    }
    if hidden < FOLD_AT_LEAST {
        return Vec::new();
    }
    folds
}

/// What a card of a run draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// Its own rows.
    Card(Ends),
    /// The fold row, in place of the first card it folds: the places in the
    /// run of the cards it folds, this one first.
    Fold { ends: Ends, folded: Vec<usize> },
    /// Nothing: it is folded.
    Hidden,
}

/// Where each of a run's cards goes, while the user has the run's folded
/// cards `shown` or not: on its own rows, as the fold row, or
/// nowhere. The line stops at the last card that shows.
pub(crate) fn places(cards: &[RunCard], shown: Disclosure) -> Vec<Place> {
    let folds = if shown.is_open() {
        Vec::new()
    } else {
        folded(cards)
    };
    // The fold row shows in the first folded card's place; the rest hide.
    let hidden = |card: usize| {
        folds
            .get(1..)
            .is_some_and(|rest| rest.binary_search(&card).is_ok())
    };
    let last_shown = (0..cards.len()).rev().find(|&card| !hidden(card));
    (0..cards.len())
        .map(|at| {
            let ends = Ends {
                first: at == 0,
                last: Some(at) == last_shown,
            };
            if folds.first() == Some(&at) {
                Place::Fold {
                    ends,
                    folded: folds.clone(),
                }
            } else if hidden(at) {
                Place::Hidden
            } else {
                Place::Card(ends)
            }
        })
        .collect()
}

/// Where a card sits on its stretch of the rail: the line runs up from its
/// first dot unless it is the first card, and down from its last unless it
/// is the last shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ends {
    pub(crate) first: bool,
    pub(crate) last: bool,
}

/// What clicking a card's subject opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    /// A pull request, in the session's Pull request surface, or at `url`
    /// once the session no longer links it.
    PullRequest { id: PullRequestId, url: String },
    /// Another session, such as one this one started.
    Session(SessionId),
    /// A subagent's session, with the line that names it, its title until
    /// the session says.
    Subagent {
        session: SessionId,
        title: SharedString,
    },
}

/// Opens a [`Link`]; the session view carries it out.
pub(crate) type OpenHandler = Rc<dyn Fn(&Link, &mut Window, &mut App)>;

/// Opens or closes a card's body.
pub(crate) type ToggleHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// One row of a card: one event it tells of, or its title from a rho that
/// doesn't say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line<'a> {
    /// The event in words.
    pub(crate) text: &'a str,
    /// What the event asks of the reader.
    urgency: Urgency,
    /// What happened, for a fold row's tally.
    outcome: Option<NotificationOutcome>,
    /// What clicking its subject opens.
    link: Option<Link>,
}

impl<'a> Line<'a> {
    /// The row of `item`, which asks `urgency`.
    fn of(item: &'a NotificationItem, urgency: Urgency) -> Self {
        let link = match &item.subject {
            NotificationSubject::PullRequest { id, url } => Some(Link::PullRequest {
                id: id.clone(),
                url: url.clone(),
            }),
            NotificationSubject::Session { id } => Some(Link::Session(id.clone().into())),
            NotificationSubject::Subagent {
                session: Some(session),
                ..
            } => Some(Link::Subagent {
                session: session.clone().into(),
                title: SharedString::from(item.text.clone()),
            }),
            // A process, a workflow, a subagent with no session, or a kind
            // a newer rho adds: nothing here opens it.
            _ => None,
        };
        Self {
            text: &item.text,
            urgency,
            outcome: Some(item.outcome),
            link,
        }
    }

    /// The pull request it is about, as its chip names it: `rho-ui#12`.
    /// The owner is rarely what tells two apart.
    pub(crate) fn chip(&self) -> Option<String> {
        match &self.link {
            Some(Link::PullRequest { id, .. }) => Some(format!("{}#{}", id.name, id.number)),
            _ => None,
        }
    }
}

/// A card as the rail draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Card<'a> {
    /// What its elements' IDs start with: `card-` and its notification's
    /// id, which stays put however often rho sends it, or its entry's
    /// index from a rho that sends none ([`card_key`]).
    pub(crate) key: SharedString,
    /// The card's tone, which each row's dot takes. rho tones only the
    /// card, as its most pressing event: rho-protocol has no tone per item.
    /// So a row worth a look draws in the card's color even when another
    /// of its events gave it that color; a routine row draws faint whatever
    /// the tone.
    tone: NoticeTone,
    arrived: Option<DateTime<Utc>>,
    pub(crate) lines: Vec<Line<'a>>,
}

impl<'a> Card<'a> {
    /// Entry `ix`'s card, on [`rows`] rows.
    pub(crate) fn of(ix: usize, notice: &'a Notice) -> Self {
        let mut lines: Vec<Line<'a>> = notice
            .items()
            .iter()
            .map(|item| Line::of(item, notice.item_urgency(item)))
            .collect();
        if lines.is_empty() {
            lines.push(Line {
                text: notice.title(),
                urgency: notice.urgency(),
                outcome: None,
                link: None,
            });
        }
        debug_assert_eq!(lines.len(), rows(notice), "the rows the run is placed by");
        Self {
            key: card_key(ix, notice),
            tone: notice.tone(),
            arrived: notice.arrived(),
            lines,
        }
    }
}

/// What entry `ix`'s card's elements' IDs start with; see [`Card::key`].
fn card_key(ix: usize, notice: &Notice) -> SharedString {
    match notice.id() {
        Some(id) => SharedString::from(format!("card-{id}")),
        None => SharedString::from(format!("card-{ix}")),
    }
}

/// What find marks in a card's row: in its chip, and in its line.
#[derive(Default)]
pub(crate) struct LineMarks {
    pub(crate) chip: Marks,
    pub(crate) text: Marks,
}

/// A card's rows, with what find marks in each line, and under the last,
/// while its body is open, the body.
pub(crate) struct CardRows<'a> {
    pub(crate) card: &'a Card<'a>,
    /// One per line of the card.
    pub(crate) marks: Vec<LineMarks>,
    pub(crate) ends: Ends,
    pub(crate) body: Body,
    pub(crate) on_open: OpenHandler,
}

/// A card's body, which a click on any of its rows opens or closes.
pub(crate) enum Body {
    /// The card has none.
    None,
    Shut(ToggleHandler),
    /// Open, drawn as the element.
    Open(ToggleHandler, AnyElement),
}

/// Whether a card's body is open, for its rows: the first's chevron says
/// which, and a click on any toggles it.
enum Toggle {
    None,
    Shut(ToggleHandler),
    Open(ToggleHandler),
}

impl Toggle {
    fn on_toggle(&self) -> Option<ToggleHandler> {
        match self {
            Self::None => None,
            Self::Shut(on_toggle) | Self::Open(on_toggle) => Some(Rc::clone(on_toggle)),
        }
    }
}

impl CardRows<'_> {
    pub(crate) fn render(self, cx: &App) -> AnyElement {
        let Self {
            card,
            marks,
            ends,
            body,
            on_open,
        } = self;
        let (toggle, mut detail) = match body {
            Body::None => (Toggle::None, None),
            Body::Shut(on_toggle) => (Toggle::Shut(on_toggle), None),
            Body::Open(on_toggle, detail) => (Toggle::Open(on_toggle), Some(detail)),
        };
        let count = card.lines.len();
        let mut marks = marks.into_iter();
        let mut rows = Vec::with_capacity(count);
        for (n, line) in card.lines.iter().enumerate() {
            let first = n == 0;
            let last = n + 1 == count;
            let row = line_row(
                card,
                n,
                line,
                marks.next().unwrap_or_default(),
                &toggle,
                &on_open,
                cx,
            );
            let line_ends = Ends {
                first: ends.first && first,
                last: ends.last && last,
            };
            let content = v_flex().flex_1().min_w_0().child(row).children(
                if last { detail.take() } else { None }.map(|detail| {
                    v_flex()
                        .id(SharedString::from(format!("{}-detail", card.key)))
                        .test_support()
                        .mt(px(2.))
                        .mb(px(6.))
                        .px(px(10.))
                        .py(px(6.))
                        .border_l_2()
                        .border_color(cx.theme().border)
                        .rounded_r(cx.theme().radius)
                        .bg(cx.theme().muted.opacity(0.3))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail)
                }),
            );
            rows.push(on_rail(Dot::of(line.urgency, card.tone, cx), line_ends, cx).child(content));
        }
        v_flex()
            .id(card.key.clone())
            .test_support()
            .group(CARD_GROUP)
            .w_full()
            .children(rows)
            .into_any_element()
    }
}

/// Line `n` of `card`: its chip, its words, and on the first, the card's
/// time. Clicking it opens or closes the card's body.
fn line_row(
    card: &Card<'_>,
    n: usize,
    line: &Line<'_>,
    marks: LineMarks,
    toggle: &Toggle,
    on_open: &OpenHandler,
    cx: &App,
) -> ObservedElement<Stateful<Div>> {
    let theme = cx.theme();
    let key = &card.key;
    // A failure or a question reads in the body text's color; the rest is
    // muted.
    let text = if line.urgency == Urgency::NeedsYou {
        theme.foreground
    } else {
        theme.muted_foreground
    };
    // Shown on hover, so a quiet row is a dot and a line.
    let on_hover = |element: Div| {
        element
            .opacity(0.)
            .group_hover(CARD_GROUP, |style| style.opacity(1.))
    };
    let chevron = match toggle {
        _ if n > 0 => None,
        Toggle::None => None,
        Toggle::Shut(_) => Some(on_hover(chevron(Lucide::ChevronRight, cx))),
        Toggle::Open(_) => Some(chevron(Lucide::ChevronDown, cx)),
    };
    let LineMarks {
        chip: chip_marks,
        text: text_marks,
    } = marks;
    let chip_label = line.chip();
    let chip = chip_label.as_ref().map(|label| {
        let tag = Tag::secondary()
            .small()
            .flex_none()
            .font_family(theme.mono_font_family.clone())
            .child(chip_marks.text(label.clone(), cx));
        // Kit's Tag takes no clicks.
        let chip = div()
            .id(SharedString::from(format!("{key}-chip-{n}")))
            .test_support()
            .flex_none()
            .child(tag);
        match line.link.clone() {
            Some(target) => {
                let on_open = Rc::clone(on_open);
                chip.cursor_pointer().on_click(move |_, window, cx| {
                    // The row's own click would open the body.
                    cx.stop_propagation();
                    on_open(&target, window, cx);
                })
            }
            None => chip,
        }
    });
    // A session opens from a button: it has no chip to click.
    let open_session = match &line.link {
        Some(target @ (Link::Session(_) | Link::Subagent { .. })) => {
            let (target, on_open) = (target.clone(), Rc::clone(on_open));
            let button = Button::new(SharedString::from(format!("{key}-open-{n}")))
                .ghost()
                .xsmall()
                .label("Open session")
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    on_open(&target, window, cx);
                });
            Some(on_hover(div().flex_none().child(button)))
        }
        _ => None,
    };
    let label = match &chip_label {
        Some(chip) => format!("{chip} · {}", line.text),
        None => line.text.to_owned(),
    };
    row_frame(cx)
        .id(SharedString::from(format!("{key}-line-{n}")))
        .test_support()
        .aria_label(SharedString::from(label))
        .text_color(text)
        .children(chip)
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(text_marks.text(line.text.to_owned(), cx)),
        )
        .children(open_session)
        .children(chevron)
        .when(n == 0, |row| {
            row.child(clock(format!("{key}-time"), card.arrived, cx))
        })
        .when_some(toggle.on_toggle(), |row, on_toggle| {
            row.cursor_pointer()
                .hover(|style| style.bg(hovered(cx)))
                .on_click(move |_, window, cx| on_toggle(window, cx))
        })
}

/// A card's chevron, `icon`, faint.
fn chevron(icon: Lucide, cx: &App) -> Div {
    let icon = Icon::new(icon).xsmall().flex_none().text_color(faint(cx));
    div().flex_none().child(icon)
}

/// The row that folds a run's cards, `folded`, in place of the first,
/// whose key it takes: how many rows, a tally of what they say, and when
/// the last came. Clicking it unfolds them.
pub(crate) fn fold_row(
    folded: &[Card<'_>],
    ends: Ends,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let key = folded
        .first()
        .map_or_else(SharedString::default, |card| card.key.clone());
    let dots = folded.iter().flat_map(|card| {
        card.lines
            .iter()
            .map(|line| Dot::of(line.urgency, card.tone, cx))
    });
    let stack = h_flex().flex_none().children(
        dots.take(STACKED_DOTS)
            .map(|kind| dot(kind, STACKED_DOT, STACKED_RING, cx).mr(px(-STACKED_OVERLAP))),
    );
    let lines: Vec<&Line<'_>> = folded.iter().flat_map(|card| &card.lines).collect();
    let tally = tally(&lines);
    let label = if tally.is_empty() {
        format!("+{} more", lines.len())
    } else {
        format!("+{} more · {tally}", lines.len())
    };
    let last = folded.iter().filter_map(|card| card.arrived).max();
    let row = row_frame(cx)
        .id(SharedString::from(format!("rail-fold-{key}")))
        .test_support()
        .aria_label(SharedString::from(label.clone()))
        .text_color(faint(cx))
        .cursor_pointer()
        .hover(|style| style.bg(hovered(cx)))
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(stack)
        .child(div().min_w_0().truncate().child(label))
        .child(clock(format!("rail-fold-{key}-time"), last, cx));
    on_rail(Dot::Folded, ends, cx)
        .child(div().flex_1().min_w_0().child(row))
        .into_any_element()
}

/// What folded rows say, counted by outcome in the order they came:
/// "2 completed, 1 with new comments". Past [`TALLY_KINDS`] kinds the rest
/// count as other; rows that don't say what happened aren't counted.
fn tally(folded: &[&Line<'_>]) -> String {
    let mut kinds: Vec<(&str, usize)> = Vec::new();
    for outcome in folded
        .iter()
        .filter_map(|line| line.outcome.and_then(outcome_words))
    {
        match kinds.iter_mut().find(|(kind, _)| *kind == outcome) {
            Some((_, count)) => *count += 1,
            None => kinds.push((outcome, 1)),
        }
    }
    let other: usize = kinds.iter().skip(TALLY_KINDS).map(|(_, count)| count).sum();
    let mut parts: Vec<String> = kinds
        .iter()
        .take(TALLY_KINDS)
        .map(|(kind, count)| format!("{count} {kind}"))
        .collect();
    if other > 0 {
        parts.push(format!("{other} other"));
    }
    parts.join(", ")
}

/// `outcome` in words that read after a count of rows, "2 failed"; none
/// for one this client doesn't know.
fn outcome_words(outcome: NotificationOutcome) -> Option<&'static str> {
    use NotificationOutcome as O;
    Some(match outcome {
        O::Update => "updated",
        O::Question => "asking",
        O::ActionRequested => "asking you to act",
        O::Running => "running",
        O::Completed => "completed",
        O::Failed => "failed",
        O::FailedToStart => "failed to start",
        O::Exited => "exited",
        O::Stopped => "stopped",
        O::Cancelled => "cancelled",
        O::TimedOut => "timed out",
        O::Interrupted => "interrupted",
        O::CutOff => "cut off",
        O::Refused => "refused",
        O::OutOfSteps => "out of steps",
        O::NewCommits => "with new commits",
        O::ChecksFailed => "with failing checks",
        O::ChecksPassed => "with passing checks",
        O::ChecksFinished => "with checks finished",
        O::NewComments => "with new comments",
        O::MergeConflict => "in conflict",
        O::Merged => "merged",
        O::Closed => "closed",
        O::WatchStopped => "no longer watched",
        O::Unreadable => "unreadable",
        // A value a newer rho adds.
        _ => return None,
    })
}

/// A row's line, card or fold, without its content: one line of small
/// text, reaching [`ROW_PAD`] past the measure on both sides for its hover.
fn row_frame(cx: &App) -> Div {
    h_flex()
        .min_h(px(ROW_HEIGHT))
        .gap_2()
        .px(px(ROW_PAD))
        .ml(px(-ROW_PAD))
        .mr(px(-ROW_PAD))
        .rounded(cx.theme().radius)
        .text_xs()
}

/// A clickable row's background under the pointer.
fn hovered(cx: &App) -> Hsla {
    cx.theme().muted.opacity(0.5)
}

/// A time as the rail and the messages show it, small and faint, which
/// says it in full on hover, and to a screen reader; nothing for an entry
/// with no time. `id` is the element's, which its tooltip needs.
pub(crate) fn clock(
    id: impl Into<SharedString>,
    at: Option<DateTime<Utc>>,
    cx: &App,
) -> ObservedElement<Stateful<Div>> {
    div()
        .id(id.into())
        .test_support()
        .flex_none()
        .ml_auto()
        .pl_2()
        .text_xs()
        .text_color(faint(cx))
        .children(at.map(time::clock))
        .when_some(at, |clock, at| {
            let stamp = SharedString::from(time::stamp(at));
            clock
                .aria_label(stamp.clone())
                .tooltip(move |window, cx| Tooltip::new(stamp.clone()).build(window, cx))
        })
}

/// The faint color of the rail's times and quiet marks: the muted text
/// color, fainter.
fn faint(cx: &App) -> Hsla {
    cx.theme().muted_foreground.opacity(0.7)
}

/// A dot on the rail.
#[derive(Clone, Copy, Debug)]
enum Dot {
    /// News: a ring.
    Ring(Hsla),
    /// Someone must act: filled.
    Filled(Hsla),
    /// A fold row's: a dashed ring.
    Folded,
}

impl Dot {
    /// A row's dot, for its urgency and its card's tone: filled when it
    /// needs the reader, red for a failure and the accent for the rest,
    /// such as a question, whatever its tone; a ring in its tone's color
    /// when it is worth a look; a faint ring for routine news.
    fn of(urgency: Urgency, tone: NoticeTone, cx: &App) -> Self {
        let theme = cx.theme();
        // Kit's `info` is cyan; the design's accent is blue.
        let accent = theme.blue;
        match (urgency, tone) {
            (Urgency::NeedsYou, NoticeTone::Error) => Self::Filled(theme.danger),
            (Urgency::NeedsYou, _) => Self::Filled(accent),
            (Urgency::Notable, NoticeTone::Success) => Self::Ring(theme.success),
            (Urgency::Notable, NoticeTone::Warning) => Self::Ring(theme.warning),
            (Urgency::Notable, NoticeTone::Error) => Self::Ring(theme.danger),
            (Urgency::Notable, NoticeTone::Accent) => Self::Ring(accent),
            (Urgency::Notable, NoticeTone::Neutral) | (Urgency::Routine, _) => {
                Self::Ring(faint(cx))
            }
        }
    }
}

/// A dot `diameter` wide whose ring is `ring` wide.
fn dot(kind: Dot, diameter: f32, ring: f32, cx: &App) -> Div {
    let background = cx.theme().background;
    let base = div()
        .flex_none()
        .size(px(diameter))
        .rounded_full()
        .border(px(ring));
    match kind {
        Dot::Ring(color) => base.border_color(color).bg(background),
        Dot::Filled(color) => base.border_color(color).bg(color),
        Dot::Folded => base.border_dashed().border_color(faint(cx)).bg(background),
    }
}

/// A row on the rail: the gutter with its stretch of line and its dot,
/// then whatever the caller adds beside it.
fn on_rail(kind: Dot, ends: Ends, cx: &App) -> Div {
    let line = cx.theme().border;
    let center = ROW_HEIGHT / 2.;
    let stretch = || div().absolute().left(px(RAIL_X)).w(px(1.)).bg(line);
    let gutter = div()
        .relative()
        .flex_none()
        .w(px(GUTTER))
        // Up to the dot from the row above, and on down to the next.
        .when(!ends.first, |gutter| {
            gutter.child(stretch().top_0().h(px(center)))
        })
        .when(!ends.last, |gutter| {
            gutter.child(stretch().top(px(center)).bottom_0())
        })
        .child(
            dot(kind, DOT, DOT_RING, cx)
                .absolute()
                .left(px(RAIL_X + 0.5 - DOT / 2.))
                .top(px(center - DOT / 2.)),
        );
    h_flex().w_full().items_stretch().child(gutter)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A card of `urgency` that draws `rows` rows.
    fn card(urgency: Urgency, rows: usize) -> RunCard {
        RunCard { urgency, rows }
    }

    /// The rule a run folds by: its first [`FOLD_AFTER`] rows always show,
    /// rounded up to whole cards, then cards fold whole, but only two rows
    /// or more at once, and one that needs the reader never.
    #[test]
    fn a_run_folds_its_cards_past_the_first_rows() {
        use Urgency::{NeedsYou as Act, Notable as Look, Routine as R};
        let one = |urgency| card(urgency, 1);
        let first = [one(R); FOLD_AFTER];
        let run = |rest: &[RunCard]| [&first[..], rest].concat();
        let cases: [(Vec<RunCard>, Vec<usize>); 6] = [
            (run(&[]), vec![]),
            (run(&[one(Look)]), vec![]),
            (run(&[one(Look), one(R)]), vec![FOLD_AFTER, FOLD_AFTER + 1]),
            (
                run(&[one(Act), one(Look), one(Act), one(R)]),
                vec![FOLD_AFTER + 1, FOLD_AFTER + 3],
            ),
            // Two cards of two rows fill the budget.
            (vec![card(R, 2), card(Look, 2), one(R), one(R)], vec![2, 3]),
            // The second card starts inside the budget, so it shows all
            // three rows; the third folds alone, as it draws two.
            (vec![card(R, 3), card(R, 3), card(R, 2)], vec![2]),
        ];
        for (cards, want) in cases {
            assert_eq!(folded(&cards), want, "cards {cards:?}");
        }
    }

    /// The fold row takes the first folded card's place, the rest hide,
    /// and the line stops at the last card that shows, wherever that is;
    /// unfolded, every card shows.
    #[test]
    fn the_line_stops_at_the_last_card_shown() {
        use Urgency::{NeedsYou as Act, Routine as R};
        let mut cards = vec![card(R, 1); FOLD_AFTER];
        cards.extend([R, Act, R].map(|urgency| card(urgency, 1)));
        let folded = places(&cards, Disclosure::Folded);
        let ends = |first, last| Ends { first, last };
        let mut want = vec![Place::Card(ends(true, false))];
        want.extend((1..FOLD_AFTER).map(|_| Place::Card(ends(false, false))));
        want.extend([
            Place::Fold {
                ends: ends(false, false),
                folded: vec![FOLD_AFTER, FOLD_AFTER + 2],
            },
            Place::Card(ends(false, true)),
            Place::Hidden,
        ]);
        assert_eq!(folded, want);
        assert_eq!(
            places(&cards, Disclosure::Open).last(),
            Some(&Place::Card(ends(false, true)))
        );
    }

    /// An item's subject makes its chip and what it opens: a pull request
    /// both, a session or a subagent's session a link, and anything with
    /// no session to open neither.
    #[test]
    fn an_item_reads_its_chip_and_link_off_its_subject() {
        let pr = PullRequestId::new("github.com", "o", "rho", 12);
        let session = || rho_ui_core::SessionId::from("s-2");
        let cases = [
            (
                NotificationSubject::PullRequest {
                    id: pr.clone(),
                    url: "https://github.com/o/rho/pull/12".to_owned(),
                },
                Some("rho#12"),
                Some(Link::PullRequest {
                    id: pr,
                    url: "https://github.com/o/rho/pull/12".to_owned(),
                }),
            ),
            (
                serde_json::from_value(serde_json::json!({"kind": "session", "id": "s-2"}))
                    .expect("a session subject"),
                None,
                Some(Link::Session(session())),
            ),
            (
                serde_json::from_value(
                    serde_json::json!({"kind": "subagent", "run": "r", "session": "s-2"}),
                )
                .expect("a subagent subject"),
                None,
                Some(Link::Subagent {
                    session: session(),
                    title: "done".into(),
                }),
            ),
            (
                serde_json::from_value(serde_json::json!({"kind": "subagent", "run": "r"}))
                    .expect("a subagent subject"),
                None,
                None,
            ),
            (
                serde_json::from_value(serde_json::json!({"kind": "process", "id": "p-1"}))
                    .expect("a process subject"),
                None,
                None,
            ),
        ];
        for (subject, chip, link) in cases {
            let item = serde_json::from_value::<NotificationItem>(serde_json::json!({
                "outcome": "completed", "subject": subject, "text": "done"
            }))
            .expect("an item");
            let line = Line::of(&item, Urgency::Routine);
            assert_eq!(
                (line.chip().as_deref(), line.link),
                (chip, link),
                "{subject:?}"
            );
        }
    }
}
