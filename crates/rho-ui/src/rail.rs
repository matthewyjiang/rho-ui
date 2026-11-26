//! The timeline rail: the cards rho sends between two replies, such as a
//! background process that ended, a session that finished or asks, or a
//! watched pull request that changed, each one line on a thin rail in the
//! transcript's left gutter. The rail stops at each reply: a run of cards
//! is one stretch of it.
//!
//! A row's dot says how the card reads: a ring for routine news, filled
//! when someone must look (an error, or an accent such as a question), so
//! the urgency doesn't rest on color alone. Clicking a row opens its body
//! under it. Past [`FOLD_AFTER`] cards in one run, the routine ones fold
//! behind one row (see [`place`]).
//!
//! Each card row is one of the transcript's rows, so only the rows near
//! the window are drawn: each draws its own stretch of the line, which
//! meets its neighbors' as the rows touch. Placing a row reads its run's
//! tones, which costs the run's length, not the transcript's.
//!
//! The sizes come from the C1 mockup (`wake-rail.html`) the design was
//! picked from.
//!
//! The rail and its dots are layout, drawn with GPUI's divs: GPUI Kit has
//! no timeline element.

use chrono::{DateTime, Utc};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{h_flex, tag::Tag, v_flex, ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, AnyElement, App, Div, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled, Window,
};
use rho_ui_core::{Notice, NoticeTone};

use crate::{time, work::Disclosure};

/// How many cards of a run always show before its routine ones fold. It
/// comes from the C1 mockup the design was picked from, not from measuring
/// real sessions: tune it once leads' runs of cards are seen in use.
pub(crate) const FOLD_AFTER: usize = 4;

/// The fewest cards a fold hides: a fold row in place of one card would
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
/// A fold row's small dots, one for each folded card up to
/// [`STACKED_DOTS`], each overlapping the next by [`STACKED_OVERLAP`]: the
/// mockup's stack, which reads as "several" without counting them.
const STACKED_DOT: f32 = 7.;
const STACKED_RING: f32 = 1.5;
const STACKED_OVERLAP: f32 = 2.;
const STACKED_DOTS: usize = 4;
/// How many kinds of outcome a fold row's tally names before it counts the
/// rest as "other": three keep the line short in the mockup's measure.
const TALLY_KINDS: usize = 3;

/// The group a card row's hover shows its chevron through.
const ROW_GROUP: &str = "rail-row";

/// Whether a card of `tone` may fold: routine news may, an error or a
/// question never does.
fn is_routine(tone: NoticeTone) -> bool {
    match tone {
        NoticeTone::Neutral | NoticeTone::Success | NoticeTone::Warning => true,
        NoticeTone::Error | NoticeTone::Accent => false,
    }
}

/// The places in a run of cards, with these tones, that fold behind one
/// row: the routine ones after the first [`FOLD_AFTER`], once there are at
/// least [`FOLD_AT_LEAST`] of them. None fold otherwise.
fn folded(tones: &[NoticeTone]) -> Vec<usize> {
    let folds: Vec<usize> = tones
        .iter()
        .enumerate()
        .skip(FOLD_AFTER)
        .filter(|(_, tone)| is_routine(**tone))
        .map(|(at, _)| at)
        .collect();
    if folds.len() < FOLD_AT_LEAST {
        return Vec::new();
    }
    folds
}

/// What a card of a run draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// Its own row.
    Card(Ends),
    /// The fold row, in place of the first card it folds: the places in the
    /// run of the cards it folds, this one first.
    Fold { ends: Ends, folded: Vec<usize> },
    /// Nothing: it is folded.
    Hidden,
}

/// Where card `at` of a run with these tones goes, while the user has the
/// run's folded cards `shown` or not: on its own row, as the fold row, or
/// nowhere. The line stops at the last row that shows. It costs the run's
/// length.
pub(crate) fn place(tones: &[NoticeTone], shown: Disclosure, at: usize) -> Place {
    let folds = if shown.is_open() {
        Vec::new()
    } else {
        folded(tones)
    };
    // The fold row shows in the first folded card's place; the rest hide.
    let hidden = |card: usize| {
        folds
            .get(1..)
            .is_some_and(|rest| rest.binary_search(&card).is_ok())
    };
    let last_shown = (0..tones.len()).rev().find(|&card| !hidden(card));
    let ends = Ends {
        first: at == 0,
        last: Some(at) == last_shown,
    };
    if folds.first() == Some(&at) {
        Place::Fold {
            ends,
            folded: folds,
        }
    } else if hidden(at) {
        Place::Hidden
    } else {
        Place::Card(ends)
    }
}

/// Where a row sits on its stretch of the rail: the line runs up from its
/// dot unless it is the first row, and down unless it is the last shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ends {
    pub(crate) first: bool,
    pub(crate) last: bool,
}

/// What a card is about, shown as a chip before its line.
///
/// rho's card names it only by its `reference`, a label with no kind, so
/// this reads a pull request off its shape (`owner/repo#12`). Once rho
/// sends the subject as data, this is where it lands, and the chip can
/// open it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Subject<'a> {
    /// A pull request, by its full reference and the label shown.
    PullRequest { reference: &'a str, label: &'a str },
}

impl<'a> Subject<'a> {
    fn of(reference: &'a str) -> Option<Self> {
        let (repo, number) = reference.rsplit_once('#')?;
        let is_number = !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit());
        if repo.is_empty() || !is_number {
            return None;
        }
        // `rho#12`: the owner is rarely what tells two apart.
        let label = reference
            .rsplit_once('/')
            .map_or(reference, |(_, label)| label);
        Some(Self::PullRequest { reference, label })
    }

    fn reference(&self) -> &'a str {
        match self {
            Self::PullRequest { reference, .. } => reference,
        }
    }
}

/// One row of the rail: what a card says, in a line. A card makes one row
/// today; once rho sends a card's events as items (two comments on one
/// pull request), each makes a row of its own from the same parts.
///
/// rho titles its notification cards "Outcome · name", which [`Line::of`]
/// splits: a stand-in, as [`Subject`] is, until rho sends the two apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line<'a> {
    pub(crate) tone: NoticeTone,
    pub(crate) subject: Option<Subject<'a>>,
    /// What happened: rho titles a card "Outcome · name".
    pub(crate) outcome: &'a str,
    /// Who or what it happened to, unless the chip names it already.
    pub(crate) name: Option<&'a str>,
    pub(crate) arrived: Option<DateTime<Utc>>,
}

impl<'a> Line<'a> {
    pub(crate) fn of(notice: &'a Notice) -> Self {
        let subject = notice.reference().and_then(Subject::of);
        let (outcome, name) = match notice.title().split_once(" · ") {
            Some((outcome, name)) => (outcome, Some(name)),
            None => (notice.title(), None),
        };
        let named_by_chip = |name: &&str| subject.as_ref().is_some_and(|s| s.reference() == *name);
        Self {
            tone: notice.tone(),
            outcome,
            name: name.filter(|name| !named_by_chip(name)),
            subject,
            arrived: notice.arrived(),
        }
    }
}

/// A card's row: entry `ix`'s line, and under it, while `detail` is
/// `Some`, its body. `on_click` opens or closes the body; a card with no
/// body has none.
pub(crate) fn card_row(
    ix: usize,
    line: &Line<'_>,
    ends: Ends,
    detail: Option<AnyElement>,
    on_click: Option<impl Fn(&mut Window, &mut App) + 'static>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let open = detail.is_some();
    // A failure or a question reads in the body text's color; routine news
    // is muted.
    let text = if is_routine(line.tone) {
        theme.muted_foreground
    } else {
        theme.foreground
    };
    let chevron = on_click.is_some().then(|| {
        Icon::new(if open {
            Lucide::ChevronDown
        } else {
            Lucide::ChevronRight
        })
        .xsmall()
        .flex_none()
        .text_color(faint(cx))
    });
    // Shown on hover, so a quiet row is a dot and a line.
    let chevron = chevron.map(|icon| {
        div().flex_none().child(icon).when(!open, |chevron| {
            chevron
                .opacity(0.)
                .group_hover(ROW_GROUP, |style| style.opacity(1.))
        })
    });
    let chip = line.subject.as_ref().map(|subject| {
        let Subject::PullRequest { label, .. } = subject;
        Tag::secondary()
            .small()
            .flex_none()
            .font_family(theme.mono_font_family.clone())
            .child(SharedString::from(label.to_string()))
    });
    let row = row_frame(cx)
        .group(ROW_GROUP)
        .id(SharedString::from(format!("card-{ix}")))
        .test_support()
        .aria_label(SharedString::from(match line.name {
            Some(name) => format!("{} · {name}", line.outcome),
            None => line.outcome.to_owned(),
        }))
        .text_color(text)
        .children(chip)
        .child(
            div()
                .min_w_0()
                .when(line.name.is_some(), Styled::flex_none)
                .truncate()
                .child(line.outcome.to_owned()),
        )
        .children(line.name.map(|name| {
            // Names read in the body text's color.
            div()
                .min_w_0()
                .truncate()
                .text_color(theme.foreground)
                .child(name.to_owned())
        }))
        .children(chevron)
        .child(clock(line.arrived, cx))
        .when_some(on_click, |row, on_click| {
            row.cursor_pointer()
                .hover(|style| style.bg(hovered(cx)))
                .on_click(move |_, window, cx| on_click(window, cx))
        });
    let detail = detail.map(|detail| {
        v_flex()
            .id(SharedString::from(format!("card-{ix}-detail")))
            .test_support()
            .mt(px(2.))
            .mb(px(6.))
            .px(px(10.))
            .py(px(6.))
            .border_l_2()
            .border_color(theme.border)
            .rounded_r(theme.radius)
            .bg(theme.muted.opacity(0.3))
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(detail)
    });
    on_rail(Dot::of(line.tone, cx), ends, cx)
        .child(v_flex().flex_1().min_w_0().child(row).children(detail))
        .into_any_element()
}

/// The row that folds a run's routine cards, `folded`, in place of the
/// first, entry `ix`: how many, a tally of what they say, and when the last
/// came. Clicking it unfolds them.
pub(crate) fn fold_row(
    ix: usize,
    folded: &[Line<'_>],
    ends: Ends,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let stack = h_flex()
        .flex_none()
        .children(folded.iter().take(STACKED_DOTS).map(|line| {
            dot(Dot::of(line.tone, cx), STACKED_DOT, STACKED_RING, cx).mr(px(-STACKED_OVERLAP))
        }));
    let label = format!("+{} more · {}", folded.len(), tally(folded));
    let last = folded.iter().filter_map(|line| line.arrived).max();
    let row = row_frame(cx)
        .id(SharedString::from(format!("rail-fold-{ix}")))
        .test_support()
        .aria_label(SharedString::from(label.clone()))
        .text_color(faint(cx))
        .cursor_pointer()
        .hover(|style| style.bg(hovered(cx)))
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(stack)
        .child(div().min_w_0().truncate().child(label))
        .child(clock(last, cx));
    on_rail(Dot::Folded, ends, cx)
        .child(div().flex_1().min_w_0().child(row))
        .into_any_element()
}

/// What folded cards say, counted by outcome in the order they came:
/// "2 completed, 1 new comments". Past [`TALLY_KINDS`] kinds the rest
/// count as other.
fn tally(folded: &[Line<'_>]) -> String {
    let mut kinds: Vec<(String, usize)> = Vec::new();
    for line in folded {
        let outcome = line.outcome.to_lowercase();
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

/// A time as the rail and the messages show it, small and faint; nothing
/// for an entry with no time.
pub(crate) fn clock(at: Option<DateTime<Utc>>, cx: &App) -> Div {
    div()
        .flex_none()
        .ml_auto()
        .pl_2()
        .text_xs()
        .text_color(faint(cx))
        .children(at.map(time::clock))
}

/// The faint color of the rail's times and quiet marks: the muted text
/// color, fainter.
fn faint(cx: &App) -> Hsla {
    cx.theme().muted_foreground.opacity(0.7)
}

/// A dot on the rail.
#[derive(Clone, Copy, Debug)]
enum Dot {
    /// Routine: a ring.
    Ring(Hsla),
    /// Someone must look: filled.
    Filled(Hsla),
    /// A fold row's: a dashed ring.
    Folded,
}

impl Dot {
    fn of(tone: NoticeTone, cx: &App) -> Self {
        let theme = cx.theme();
        match tone {
            NoticeTone::Neutral => Self::Ring(faint(cx)),
            NoticeTone::Success => Self::Ring(theme.success),
            NoticeTone::Warning => Self::Ring(theme.warning),
            NoticeTone::Error => Self::Filled(theme.danger),
            // Kit's `info` is cyan; the design's accent is blue.
            NoticeTone::Accent => Self::Filled(theme.blue),
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

    /// The rule a run folds by: the first [`FOLD_AFTER`] always show, then
    /// routine cards fold, but only two or more at once, and an error or a
    /// question never.
    #[test]
    fn a_run_folds_its_routine_cards_past_the_first_few() {
        use NoticeTone::{Accent as Ask, Error as Err, Neutral as N, Success as Ok};
        let first = [N; FOLD_AFTER];
        let run = |rest: &[NoticeTone]| [&first[..], rest].concat();
        let cases: [(Vec<NoticeTone>, Vec<usize>); 4] = [
            (run(&[]), vec![]),
            (run(&[Ok]), vec![]),
            (run(&[Ok, Ok]), vec![FOLD_AFTER, FOLD_AFTER + 1]),
            (
                run(&[Err, Ok, Ask, N]),
                vec![FOLD_AFTER + 1, FOLD_AFTER + 3],
            ),
        ];
        for (tones, want) in cases {
            assert_eq!(folded(&tones), want, "tones {tones:?}");
        }
    }

    /// The fold row takes the first folded card's place, the rest hide,
    /// and the line stops at the last row that shows, wherever that is;
    /// unfolded, every card shows.
    #[test]
    fn the_line_stops_at_the_last_row_shown() {
        use NoticeTone::{Error as Err, Neutral as N};
        let mut tones = vec![N; FOLD_AFTER];
        tones.extend([N, Err, N]);
        let places: Vec<Place> = (0..tones.len())
            .map(|at| place(&tones, Disclosure::Folded, at))
            .collect();
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
        assert_eq!(places, want);
        assert_eq!(
            place(&tones, Disclosure::Open, tones.len() - 1),
            Place::Card(ends(false, true))
        );
    }
}
