//! Find in the transcript: the matches of the find bar's query in the
//! session's entries, the one the user is on, and how the transcript marks
//! and reveals them.
//!
//! It searches what the user can read, in the session's data rather than in
//! laid-out rows, so it finds what lies off screen or folded away: prompts,
//! replies and notices as their Markdown renders (the text Kit's view of
//! each holds, parsed whether or not it is drawn), and each step's line,
//! command, notes and output as [`work::searched`] lists them. A match the
//! user moves to opens the folds around it, which stay open, as a browser's
//! hidden-until-found text does. Diffs, compactions, surfaces and images
//! aren't searched.
//!
//! A match is known by its entry, the [`Field`] of the entry it lies in,
//! and its byte range there. Markdown is marked with Kit's range
//! highlights; text rho-ui draws itself, through [`Marks`].
//!
//! A step's Markdown body (a thought, a tool's Markdown) has no text view
//! until its details open, so it is searched in its source, unlike a
//! reply. So a match in its Markdown syntax, such as a link's address,
//! counts but marks nothing, and one that spans emphasis isn't found.

use std::{cell::Cell, collections::BTreeSet, ops::Range, rc::Rc, time::Duration};

use gpui_kit::base::text::RangeHighlight;
use gpui_kit::component::{text::TextViewState, ActiveTheme as _};
use gpui_kit::{
    canvas, combine_highlights, div, px, size, AnyElement, App, Bounds, Entity, HighlightStyle,
    Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, StyledText,
};
use regex::{Regex, RegexBuilder};
use rho_ui_core::{Entry, Row};
use web_time::Instant;

use super::{markdown_parts, Transcript};
use crate::{
    rail::{self, Line},
    work::{self, Disclosure},
};

/// Where in an entry a match lies, in the order the transcript draws them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Field {
    /// What a card is about, as its chip shows it ([`Line::subject`]).
    Subject,
    /// A card's title: what happened, as its line shows it
    /// ([`Line::outcome`]).
    Title,
    /// Who or what it happened to, as its line names it ([`Line::name`]).
    Name,
    /// Part `n` of the entry's Markdown: a reply's part, a prompt's text,
    /// or a notice's detail. Its ranges index the text Kit renders.
    Part(usize),
    /// A tool call's line.
    Line,
    /// A tool call's command as written.
    Command,
    /// A tool card's note or error `n`.
    Note(usize),
    /// Text `n` of a tool call's output: its text body, a line of its
    /// search results, or a text it read without a card.
    Output(usize),
    /// A thought, or a tool card's Markdown or code body. Its ranges index
    /// the text [`work::searched`] gives.
    Body,
}

impl Field {
    /// The scroller's item, of those its entry's row takes, the field is in.
    fn item(self) -> usize {
        match self {
            Self::Part(part) => part,
            _ => 0,
        }
    }

    /// Whether it lies in a step's details, which fold.
    fn in_details(self) -> bool {
        matches!(
            self,
            Self::Command | Self::Note(_) | Self::Output(_) | Self::Body
        )
    }
}

/// A match: where it lies in its entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) field: Field,
    pub(crate) range: Range<usize>,
}

impl Hit {
    /// What the match is known by while its entry changes around it.
    fn key(&self) -> (Field, usize) {
        (self.field, self.range.start)
    }
}

/// A match, by its entry and its index among the entry's matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct At {
    pub(crate) entry: usize,
    pub(crate) ix: usize,
}

/// Which way to move from the current match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Next,
    Previous,
}

/// What a query finds: its text anywhere, ignoring case.
pub(crate) struct Matcher(Regex);

impl Matcher {
    /// The matcher for `query`; `None` for an empty one, which finds
    /// nothing.
    pub(crate) fn new(query: &str) -> Option<Self> {
        if query.is_empty() {
            return None;
        }
        // An escaped literal compiles unless it outgrows the regex size
        // limit (10 MiB), far beyond anything typed in the bar.
        let regex = RegexBuilder::new(&regex::escape(query))
            .case_insensitive(true)
            .build()
            .ok()?;
        Some(Self(regex))
    }

    /// The byte ranges of the matches in `text`, in order.
    pub(crate) fn find<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
        self.0.find_iter(text).map(|found| found.range())
    }
}

/// Every match, per entry, and the one the user is on.
#[derive(Debug, Default)]
pub(crate) struct Hits {
    /// Per entry: its matches, in drawing order.
    entries: Vec<Vec<Hit>>,
    total: usize,
    /// `Some` whenever there is a match.
    current: Option<At>,
}

impl Hits {
    /// Sets entry `entry`'s matches. The current match stays the one it
    /// was, known by its field and start; if it went, the next one after
    /// it is current. With no current match, the first becomes current.
    pub(crate) fn set(&mut self, entry: usize, hits: Vec<Hit>) {
        if self.entries.len() <= entry {
            self.entries.resize_with(entry + 1, Vec::new);
        }
        let key = self
            .current
            .filter(|at| at.entry == entry)
            .and_then(|at| self.hit(at))
            .map(Hit::key);
        self.total = self.total - self.entries[entry].len() + hits.len();
        self.entries[entry] = hits;
        if let Some(key) = key {
            let hits = &self.entries[entry];
            self.current = match hits.iter().position(|hit| hit.key() >= key) {
                Some(ix) => Some(At { entry, ix }),
                None => self.after(entry).or_else(|| self.last()),
            };
        }
        // With none current, no entry had a match: this one's first is it.
        if self.current.is_none() && !self.entries[entry].is_empty() {
            self.current = Some(At { entry, ix: 0 });
        }
    }

    /// Forgets the matches in entries from `from` on.
    pub(crate) fn truncate(&mut self, from: usize) {
        if from >= self.entries.len() {
            return;
        }
        let gone: usize = self.entries[from..].iter().map(Vec::len).sum();
        self.entries.truncate(from);
        self.total -= gone;
        if self.current.is_some_and(|at| at.entry >= from) {
            self.current = self.last();
        }
    }

    pub(crate) fn of(&self, entry: usize) -> &[Hit] {
        self.entries.get(entry).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn hit(&self, at: At) -> Option<&Hit> {
        self.entries.get(at.entry)?.get(at.ix)
    }

    pub(crate) fn current(&self) -> Option<At> {
        self.current
    }

    /// Makes current the first match `wanted` takes, or the last match if
    /// it takes none.
    pub(crate) fn pick(&mut self, wanted: impl Fn(usize, &Hit) -> bool) {
        let first = self.entries.iter().enumerate().find_map(|(entry, hits)| {
            let ix = hits.iter().position(|hit| wanted(entry, hit))?;
            Some(At { entry, ix })
        });
        self.current = first.or_else(|| self.last());
    }

    /// Moves the current match one way, wrapping around at either end.
    pub(crate) fn step(&mut self, direction: Direction) {
        let Some(At { entry, ix }) = self.current else {
            return;
        };
        self.current = match direction {
            Direction::Next if ix + 1 < self.of(entry).len() => Some(At { entry, ix: ix + 1 }),
            Direction::Next => self.after(entry).or_else(|| self.first()),
            Direction::Previous if ix > 0 => Some(At { entry, ix: ix - 1 }),
            Direction::Previous => self.before(entry).or_else(|| self.last()),
        };
    }

    /// Which match is current, counting from 1, and how many there are;
    /// `None` with none.
    pub(crate) fn count(&self) -> Option<(usize, usize)> {
        self.current.map(|At { entry, ix }| {
            let before: usize = self.entries[..entry].iter().map(Vec::len).sum();
            (before + ix + 1, self.total)
        })
    }

    fn first(&self) -> Option<At> {
        self.after_or_at(0)
    }

    fn last(&self) -> Option<At> {
        self.before(self.entries.len())
    }

    /// The first match of the first entry after `entry` that has any.
    fn after(&self, entry: usize) -> Option<At> {
        self.after_or_at(entry + 1)
    }

    fn after_or_at(&self, entry: usize) -> Option<At> {
        let mut found = self.entries.iter().enumerate().skip(entry);
        let (entry, _) = found.find(|(_, hits)| !hits.is_empty())?;
        Some(At { entry, ix: 0 })
    }

    /// The last match of the last entry before `entry` that has any.
    fn before(&self, entry: usize) -> Option<At> {
        let end = entry.min(self.entries.len());
        let mut found = self.entries[..end].iter().enumerate().rev();
        let (entry, hits) = found.find(|(_, hits)| !hits.is_empty())?;
        Some(At {
            entry,
            ix: hits.len() - 1,
        })
    }
}

/// How long the transcript keeps trying to scroll a match into view: as
/// long as Kit's text view tries for its own reveals.
const REVEAL_FOR: Duration = Duration::from_secs(1);

/// Since when the current match is to be scrolled into view, until it is.
/// Shared with the [`Marks`] anchor that scrolls it, in text rho-ui draws.
type Reveal = Rc<Cell<Option<Instant>>>;

/// The find bar's state in the transcript.
#[derive(Default)]
pub(crate) struct Find {
    matcher: Option<Matcher>,
    hits: Hits,
    /// Entries whose Kit text views it set highlights on.
    painted: BTreeSet<usize>,
    /// Entries whose Markdown hadn't parsed its latest text when searched:
    /// they are searched again once it has ([`Transcript::find_reparsed`]).
    pending: BTreeSet<usize>,
    reveal: Reveal,
}

impl Find {
    /// The current match's field in entry `entry`, if it lies there.
    pub(crate) fn current_in(&self, entry: usize) -> Option<Field> {
        let at = self.hits.current().filter(|at| at.entry == entry)?;
        Some(self.hits.hit(at)?.field)
    }

    /// What to mark in `field` of entry `entry`.
    pub(crate) fn marks(&self, entry: usize, field: Field) -> Marks {
        let current = self.hits.current().filter(|at| at.entry == entry);
        let mut marks = Marks::default();
        for (ix, hit) in self.hits.of(entry).iter().enumerate() {
            if hit.field != field {
                continue;
            }
            let is_current = current.is_some_and(|at| at.ix == ix);
            if is_current {
                marks.reveal = Some((hit.range.start, Rc::clone(&self.reveal)));
            }
            marks.ranges.push((hit.range.clone(), is_current));
        }
        marks
    }

    pub(crate) fn count(&self) -> Count {
        match (&self.matcher, self.hits.count()) {
            (None, _) => Count::NoQuery,
            (Some(_), None) => Count::NoMatches,
            (Some(_), Some((current, total))) => Count::At { current, total },
        }
    }
}

/// What find found, as its bar says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Count {
    /// Nothing is typed.
    NoQuery,
    NoMatches,
    /// Match `current` of `total` is current, counting from 1.
    At {
        current: usize,
        total: usize,
    },
}

/// The backgrounds of find's matches, as Kit's editor search paints them:
/// the current match as a selection, the others in a greyer one.
fn backgrounds(cx: &App) -> (Hsla, Hsla) {
    let selection = cx.theme().selection;
    (
        Hsla {
            s: 0.1,
            ..selection
        },
        selection,
    )
}

/// What find marks in one text rho-ui draws: its matches, and whether one
/// is current.
#[derive(Default)]
pub(crate) struct Marks {
    /// Each match's range, and whether it is the current one.
    ranges: Vec<(Range<usize>, bool)>,
    /// Where the current match starts, if it is here, and the reveal it
    /// carries out.
    reveal: Option<(usize, Reveal)>,
}

impl Marks {
    /// `text`, with the matches marked.
    pub(crate) fn text(self, text: impl Into<SharedString>, cx: &App) -> AnyElement {
        self.text_over(text.into(), Vec::new(), cx)
    }

    /// `text` with `base` highlights, such as a search result's own, and
    /// the matches marked over them.
    pub(crate) fn text_over(
        self,
        text: SharedString,
        base: Vec<(Range<usize>, HighlightStyle)>,
        cx: &App,
    ) -> AnyElement {
        let (other, current) = backgrounds(cx);
        let marks = self.ranges.into_iter().map(|(range, is_current)| {
            let background = if is_current { current } else { other };
            let style = HighlightStyle {
                background_color: Some(background),
                ..HighlightStyle::default()
            };
            (range, style)
        });
        let highlights: Vec<_> = combine_highlights(base, marks).collect();
        // Raw GPUI: Kit's `Label` marks a substring, not ranges.
        let text = StyledText::new(text).with_highlights(highlights);
        let Some((start, reveal)) = self.reveal else {
            return text.into_any_element();
        };
        let layout = text.layout().clone();
        // An empty box after the text, laid out after it, that scrolls the
        // transcript to the current match while a reveal is due.
        let anchor = canvas(
            move |_, window, cx| {
                let Some(since) = reveal.get() else {
                    return;
                };
                let now = cx.background_executor().now();
                if now.saturating_duration_since(since) > REVEAL_FOR {
                    reveal.set(None);
                    return;
                }
                let Some(origin) = layout.position_for_index(start) else {
                    return;
                };
                // A sliver as tall as the match's line: what must show.
                let target = Bounds::new(origin, size(px(1.), layout.line_height()));
                let visible = window.content_mask().bounds;
                if visible.top() <= target.top() && target.bottom() <= visible.bottom() {
                    reveal.set(None);
                } else {
                    window.request_autoscroll(target);
                    window.request_animation_frame();
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_0();
        div()
            .relative()
            .child(text)
            .child(anchor)
            .into_any_element()
    }
}

/// Sets `marks` on `text`'s view, or clears its marks when there are none.
/// The ranges come from its rendered text as it is: a view whose text has
/// changed since is searched again once its parse lands
/// ([`Transcript::find_reparsed`]), so a stale set is only dropped.
fn set_marks(text: &Entity<TextViewState>, marks: Vec<RangeHighlight>, cx: &mut App) {
    text.update(cx, |state, cx| {
        if marks.is_empty() {
            state.clear_range_highlights(cx);
        } else {
            let _stale = state.set_range_highlights(marks, cx);
        }
    });
}

impl Transcript {
    /// Finds `query` in `entries`, and makes current the first match from
    /// the top of the transcript's view on, or the last above it, which it
    /// scrolls to.
    pub(crate) fn find(&mut self, query: &str, entries: &[Entry], cx: &mut App) {
        self.find.matcher = Matcher::new(query);
        self.find.hits = Hits::default();
        self.find.pending.clear();
        for entry in 0..entries.len() {
            self.search(entry, entries, cx);
        }
        let top = self.scroller.read(cx).logical_scroll_top().item_ix;
        let items = Rc::clone(&self.items);
        let timeline = &self.timeline;
        self.find.hits.pick(|entry, hit| {
            timeline
                .row_of(entry)
                .is_some_and(|row| items.borrow().of(row..row + 1).start + hit.field.item() >= top)
        });
        self.paint_all(cx);
        self.reveal(entries, None, cx);
    }

    /// Moves to the next or previous match, and scrolls to it.
    pub(crate) fn find_step(&mut self, direction: Direction, entries: &[Entry], cx: &mut App) {
        let was = self.find.hits.current();
        self.find.hits.step(direction);
        self.repaint_current(was, cx);
        self.reveal(entries, was, cx);
    }

    /// Clears the matches; what they opened stays open.
    pub(crate) fn close_find(&mut self, cx: &mut App) {
        let was = self.find.hits.current();
        self.find.matcher = None;
        self.find.hits = Hits::default();
        self.find.pending.clear();
        self.find.reveal.set(None);
        self.paint_all(cx);
        if let Some(row) = was.and_then(|at| self.timeline.row_of(at.entry)) {
            // Its details part lets the current match grow it no more.
            self.remeasure(row..row + 1, cx);
        }
    }

    pub(crate) fn find_count(&self) -> Count {
        self.find.count()
    }

    /// The current match's entry and field, for UI tests.
    #[cfg(test)]
    pub(crate) fn find_current(&self) -> Option<(usize, Field)> {
        let at = self.find.hits.current()?;
        Some((at.entry, self.find.hits.hit(at)?.field))
    }

    /// Entries from `from` on are new, or entry `from` changed: they are
    /// searched, keeping the current match.
    pub(super) fn find_again(&mut self, entries: Range<usize>, all: &[Entry], cx: &mut App) {
        if self.find.matcher.is_none() {
            return;
        }
        for entry in entries {
            self.search(entry, all, cx);
            self.paint(entry, cx);
        }
    }

    /// Forgets the matches in entries from `from` on.
    pub(super) fn find_truncate(&mut self, from: usize) {
        self.find.hits.truncate(from);
        self.find.painted.retain(|&entry| entry < from);
        self.find.pending.retain(|&entry| entry < from);
    }

    /// Searches again the entries whose Markdown was still parsing, once
    /// it has. Call it whenever the transcript's scroller changes: a text
    /// view whose parse lands remeasures its item.
    pub(crate) fn find_reparsed(&mut self, entries: &[Entry], cx: &mut App) {
        let parsed: Vec<usize> = self
            .find
            .pending
            .iter()
            .copied()
            .filter(|&entry| !self.is_stale(entry, entries, cx))
            .collect();
        for entry in parsed {
            self.find.pending.remove(&entry);
            self.find_again(entry..entry + 1, entries, cx);
            if self.find.hits.current().is_some_and(|at| at.entry == entry) {
                self.reveal_text(cx);
            }
        }
    }

    /// Whether a text view of entry `entry` hasn't parsed its latest text
    /// yet. Its rendered text, which find searches and marks, trails it
    /// until then.
    fn is_stale(&self, entry: usize, entries: &[Entry], cx: &App) -> bool {
        let Some(shown) = entries.get(entry) else {
            return false;
        };
        let parts = self.markdown.get(entry).map_or(&[][..], Vec::as_slice);
        let stale_part = parts
            .iter()
            .zip(markdown_parts(shown))
            .any(|(markdown, source)| markdown.text.read(cx).rendered_text().source() != source);
        let body = self.bodies.iter().find(|body| body.entry == entry);
        stale_part || body.is_some_and(|body| body.rendered(cx).is_none())
    }

    /// Searches entry `entry`. One whose Markdown hasn't parsed its latest
    /// text is pending: its matches are in the text it has, and it is
    /// searched again once it has parsed ([`Self::find_reparsed`]).
    fn search(&mut self, entry: usize, entries: &[Entry], cx: &App) {
        let hits = self.search_entry(entry, entries, cx);
        self.find.hits.set(entry, hits);
        if self.is_stale(entry, entries, cx) {
            self.find.pending.insert(entry);
        }
    }

    /// Entry `entry`'s matches.
    fn search_entry(&self, entry: usize, entries: &[Entry], cx: &App) -> Vec<Hit> {
        let (Some(matcher), Some(shown)) = (&self.find.matcher, entries.get(entry)) else {
            return Vec::new();
        };
        if self.timeline.row_of(entry).is_none() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        let mut found = |field, text: &str| {
            hits.extend(matcher.find(text).map(|range| Hit { field, range }));
        };
        if let Entry::Notice(notice) = shown {
            let line = Line::of(notice);
            if let Some(subject) = &line.subject {
                found(Field::Subject, subject.label());
            }
            found(Field::Title, line.outcome);
            if let Some(name) = line.name {
                found(Field::Name, name);
            }
        }
        let parts = self.markdown.get(entry).map_or(&[][..], Vec::as_slice);
        for (n, markdown) in parts.iter().enumerate() {
            found(
                Field::Part(n),
                markdown.text.read(cx).rendered_text().as_str(),
            );
        }
        if let Some(step) = shown.as_step() {
            for (field, text) in work::searched(step) {
                found(field, &text);
            }
        }
        hits.sort_by_key(Hit::key);
        hits
    }

    /// Marks every entry's matches in its text views, and clears those
    /// with none any more.
    fn paint_all(&mut self, cx: &mut App) {
        let mut entries: BTreeSet<usize> = std::mem::take(&mut self.find.painted);
        let with_hits = self.find.hits.entries.iter().enumerate();
        entries.extend(
            with_hits
                .filter(|(_, hits)| !hits.is_empty())
                .map(|(entry, _)| entry),
        );
        for entry in entries {
            self.paint(entry, cx);
        }
    }

    /// Paints the old and the new current match, after a step.
    fn repaint_current(&mut self, was: Option<At>, cx: &mut App) {
        let now = self.find.hits.current();
        for at in [was, now].into_iter().flatten() {
            self.paint(at.entry, cx);
        }
    }

    /// Sets entry `entry`'s matches on its text views: its Markdown parts,
    /// and its step body if its details made one. Text rho-ui draws marks
    /// them as it draws ([`Find::marks`]).
    fn paint(&mut self, entry: usize, cx: &mut App) {
        let (other, current) = backgrounds(cx);
        let at = self.find.hits.current().filter(|at| at.entry == entry);
        let hits = self.find.hits.of(entry);
        let background = |ix: usize| {
            if at.is_some_and(|at| at.ix == ix) {
                current
            } else {
                other
            }
        };
        let mut painted = false;
        let parts = self.markdown.get(entry).map_or(&[][..], Vec::as_slice);
        for (n, markdown) in parts.iter().enumerate() {
            let marks: Vec<RangeHighlight> = hits
                .iter()
                .enumerate()
                .filter(|(_, hit)| hit.field == Field::Part(n))
                .map(|(ix, hit)| RangeHighlight::new(hit.range.clone(), background(ix)))
                .collect();
            painted |= !marks.is_empty();
            set_marks(&markdown.text, marks, cx);
        }
        // A body still parsing keeps its marks until it has: Kit moves them
        // with its text. They may still need clearing.
        let body = self.bodies.iter().find(|body| body.entry == entry);
        let rendered = body.map(|body| (body, body.rendered(cx)));
        painted |= matches!(rendered, Some((_, None)));
        if let Some((body, Some(rendered))) = rendered {
            let marks: Vec<RangeHighlight> = hits
                .iter()
                .enumerate()
                .filter(|(_, hit)| hit.field == Field::Body)
                .filter_map(|(ix, hit)| {
                    let range = body.rendered_range(&rendered, &hit.range)?;
                    Some(RangeHighlight::new(range, background(ix)))
                })
                .collect();
            painted |= !marks.is_empty();
            set_marks(body.state(), marks, cx);
        }
        if painted {
            self.find.painted.insert(entry);
        } else {
            self.find.painted.remove(&entry);
        }
    }

    /// Opens what folds the current match away, and scrolls to it. The
    /// list scrolls to its item only when `was`, the match before, was in
    /// another: a match further down a tall item it is on shows by
    /// scrolling no more than it takes.
    fn reveal(&mut self, entries: &[Entry], was: Option<At>, cx: &mut App) {
        let Some(at) = self.find.hits.current() else {
            return;
        };
        let Some(field) = self.find.hits.hit(at).map(|hit| hit.field) else {
            return;
        };
        let Some(row) = self.timeline.row_of(at.entry) else {
            return;
        };
        let mut opened = row..row + 1;
        if let Some(turn) = self.timeline.turn_of(row) {
            let folds = self.is_finished(turn) && self.folds_in_working(turn, row, entries);
            if let Some(worked) = self.worked.get_mut(turn).filter(|_| folds) {
                if !worked.is_open() {
                    *worked = Disclosure::Open;
                    opened = self.timeline.turn_rows(turn);
                }
            }
        }
        let open = |shows: Option<&mut Disclosure>| {
            if let Some(shows) = shows {
                *shows = Disclosure::Open;
            }
        };
        match self.timeline.rows().get(row) {
            Some(Row::Work(_)) => {
                open(self.steps.get_mut(row));
                if field.in_details() {
                    open(self.details.get_mut(at.entry));
                    self.open_views(at.entry, entries, cx);
                }
            }
            // A card: its body opens for a match there, and its run unfolds
            // if it folds the card away.
            Some(Row::Message(_)) if matches!(entries.get(at.entry), Some(Entry::Notice(_))) => {
                if matches!(field, Field::Part(_)) {
                    open(self.details.get_mut(at.entry));
                }
                let placed = self.placed(row, entries);
                let shown = matches!(
                    placed.places.get(row - placed.start),
                    Some(rail::Place::Card(_))
                );
                let run = self.run_of(row, entries);
                let first = self.card_at(run.start, entries).map(|(first, _)| first);
                if !shown && first.is_some_and(|first| self.unfold_run(first)) {
                    opened = opened.start.min(run.start)..opened.end.max(run.end);
                }
            }
            Some(Row::Message(_)) | None => {}
        }
        // Its row grows to hold the current match, and the last one's
        // shrinks back.
        self.remeasure(opened, cx);
        if let Some(last) = was.and_then(|was| self.timeline.row_of(was.entry)) {
            self.remeasure(last..last + 1, cx);
        }
        let item = self.items.borrow().of(row..row + 1).start + field.item();
        let item_of = |at: At| {
            let row = self.timeline.row_of(at.entry)?;
            let field = self.find.hits.hit(at)?.field;
            Some(self.items.borrow().of(row..row + 1).start + field.item())
        };
        if was.and_then(item_of) != Some(item) {
            self.scroller
                .update(cx, |scroller, cx| scroller.scroll_to_item(item, cx));
        }
        self.find.reveal.set(Some(cx.background_executor().now()));
        self.reveal_text(cx);
    }

    /// Asks the Kit text view holding the current match, if one does, to
    /// scroll it into view. Text rho-ui draws does so itself.
    fn reveal_text(&self, cx: &mut App) {
        let Some(since) = self.find.reveal.get() else {
            return;
        };
        let Some(at) = self.find.hits.current() else {
            return;
        };
        let Some(hit) = self.find.hits.hit(at) else {
            return;
        };
        let now = cx.background_executor().now();
        if now.saturating_duration_since(since) > REVEAL_FOR {
            return;
        }
        let (state, range) = match hit.field {
            Field::Part(n) => {
                let Some(markdown) = self.markdown.get(at.entry).and_then(|parts| parts.get(n))
                else {
                    return;
                };
                (markdown.text.clone(), hit.range.clone())
            }
            Field::Body => {
                let Some(body) = self.bodies.iter().find(|body| body.entry == at.entry) else {
                    return;
                };
                let Some(range) = body
                    .rendered(cx)
                    .and_then(|rendered| body.rendered_range(&rendered, &hit.range))
                else {
                    return;
                };
                (body.state().clone(), range)
            }
            Field::Subject
            | Field::Title
            | Field::Name
            | Field::Line
            | Field::Command
            | Field::Note(_)
            | Field::Output(_) => return,
        };
        let revealed = state.update(cx, |state, cx| state.reveal_range(range, cx));
        if revealed.is_ok() {
            self.find.reveal.set(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(field: Field, range: Range<usize>) -> Hit {
        Hit { field, range }
    }

    /// Matches by entry: entry 0 has two, entry 1 none, entry 2 one.
    fn hits() -> Hits {
        let mut hits = Hits::default();
        hits.set(
            0,
            vec![hit(Field::Part(0), 0..2), hit(Field::Part(0), 5..7)],
        );
        hits.set(1, Vec::new());
        hits.set(2, vec![hit(Field::Body, 3..5)]);
        hits
    }

    #[test]
    fn matcher_ignores_case_and_gives_byte_ranges() {
        // A query, a text, and the byte ranges found, as (start, end).
        type Case = (&'static str, &'static str, &'static [(usize, usize)]);
        let cases: [Case; 5] = [
            ("walk", "Walk the WALKER", &[(0, 4), (9, 13)]),
            // Non-ASCII before and inside the match: byte offsets.
            ("über", "Grüße, ÜBER alles", &[(9, 14)]),
            // Regex syntax is literal text.
            ("a.b(", "axb( a.b(", &[(5, 9)]),
            ("[x]*", "[x]* x", &[(0, 4)]),
            ("", "anything", &[]),
        ];
        for (query, text, expected) in cases {
            let found: Vec<_> = Matcher::new(query)
                .map(|matcher| {
                    let found = matcher.find(text);
                    found.map(|range| (range.start, range.end)).collect()
                })
                .unwrap_or_default();
            assert_eq!(found, expected, "{query:?} in {text:?}");
        }
    }

    #[test]
    fn steps_wrap_around_and_pick_starts_from_a_position() {
        let mut hits = hits();
        assert_eq!(hits.count(), Some((1, 3)));
        let mut seen = Vec::new();
        for _ in 0..4 {
            hits.step(Direction::Next);
            seen.push(hits.count().map(|(current, _)| current));
        }
        // Entry 1, with none, is skipped.
        assert_eq!(seen, [Some(2), Some(3), Some(1), Some(2)]);
        hits.step(Direction::Previous);
        hits.step(Direction::Previous);
        assert_eq!(hits.current(), Some(At { entry: 2, ix: 0 }));

        hits.pick(|_, hit| hit.range.start >= 5);
        assert_eq!(hits.current(), Some(At { entry: 0, ix: 1 }));
        // Nothing from there on: the last match above.
        hits.pick(|entry, _| entry >= 3);
        assert_eq!(hits.current(), Some(At { entry: 2, ix: 0 }));
    }

    #[test]
    fn current_match_stays_put_as_entries_change() {
        let mut hits = Hits::default();
        hits.set(0, Vec::new());
        assert_eq!(hits.count(), None);
        // Matches arrive: the first is current.
        hits.set(
            0,
            vec![hit(Field::Part(0), 0..2), hit(Field::Part(0), 5..7)],
        );
        hits.set(2, vec![hit(Field::Body, 3..5)]);
        hits.step(Direction::Next);
        // A match comes before it in its entry, and more in a new entry.
        let grown = vec![
            hit(Field::Part(0), 0..2),
            hit(Field::Part(0), 3..4),
            hit(Field::Part(0), 5..7),
        ];
        hits.set(0, grown);
        hits.set(3, vec![hit(Field::Line, 0..1)]);
        assert_eq!(hits.current(), Some(At { entry: 0, ix: 2 }));
        assert_eq!(hits.count(), Some((3, 5)));
        // It went: the next match after it is current.
        hits.set(0, vec![hit(Field::Part(0), 0..2)]);
        assert_eq!(hits.current(), Some(At { entry: 2, ix: 0 }));
        // Its entry went: the last match left is.
        hits.truncate(1);
        assert_eq!(hits.count(), Some((1, 1)));
        hits.truncate(0);
        assert_eq!(hits.count(), None);
    }
}
