//! The transcript: a session's entries as rows in a scroller. Messages are
//! drawn here; runs of tool calls and thoughts by [`crate::work`], and the
//! cards rho sends between replies on a rail by [`crate::rail`]. Under each
//! prompt, a header says "Working for 38s" while the turn runs; when it
//! finishes, the same line says "Worked for" and the turn's working folds
//! behind it. This owns what the user opened and folded, the turns'
//! lengths, and the user messages' attachments, ready to draw.
//!
//! A message shows when it arrived on hover, if this client saw it arrive,
//! in the rail's faint small style: a reply in its top right corner, in
//! the column the rail's times make, and a user message left of its
//! bubble.
//!
//! A row is one of the scroller's items, but a long reply's row is one for
//! each of its parts ([`crate::parts`]), so the scroller draws only the
//! parts near the window.

pub(crate) mod find;

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ops::Range,
    rc::Rc,
    time::Duration,
};

use web_time::Instant;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::{text::CodeBlock, TestSupportExt as _};
use gpui_kit::component::{
    attachment::Attachment,
    bubble::{Bubble, BubbleVariant},
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex,
    message::{Message, MessageAlignment, MessageContent},
    message_scroller::MessageScrollerState,
    separator::Separator,
    text::{MarkdownExtensions, TextView, TextViewState, TextViewStyle},
    ActiveTheme as _, Icon, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, rems, AnyElement, App, AppContext as _, ClipboardItem, ElementId, Entity,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled, Subscription, Window,
};
use rho_ui_core::{
    surface::SurfaceRef, Compaction, Delay, Entry, Notice, Row, Step, Timeline, ToolContent,
    UserMessage,
};

use crate::{
    attachments::Preview,
    figures,
    fold::Folding,
    lightbox,
    parts::{self, Edit, Items, Place},
    pictures::{self, PictureRead, Pictures},
    rail::{self, Card},
    surface_view::Surfaces,
    turn_times::TurnTimes,
    usage,
    work::{self, BodyView, DiffView, Disclosure, Fold, FoldHandler, Header, WorkRow},
};
use find::{CardPart, Field, Find};

/// [`figures::extensions`] and the plugin for images by path, for a
/// `TextView` showing message `entry` of a session whose images `pictures`
/// reads.
fn message_extensions(pictures: &Entity<Pictures>, entry: usize) -> MarkdownExtensions {
    figures::extensions().plugin(pictures::Plugin {
        pictures: pictures.clone(),
        entry,
    })
}

/// How tall an image attached to a user message or made by a tool is, in
/// rems: a screenshot's layout reads at this size, and two side by side
/// fit the transcript's measure.
pub(crate) const IMAGE_HEIGHT: f32 = 10.;

/// A code block's padding above its code. Kit puts [`code_actions`] in the
/// top corner, 8px down; its copy button is 20px tall, its icon 12px to 24px
/// down. This starts the code's letters about 4px under the icon, so the
/// corner sits in the padding rather than over the first line.
const CODE_BLOCK_PAD_TOP: f32 = 26.;

/// A code block's padding below its code, as far from its last line's
/// letters as Kit's corner leaves the first line's under the icon.
const CODE_BLOCK_PAD_BOTTOM: f32 = 21.;

/// The group a message's hover shows its time through.
const MESSAGE_GROUP: &str = "message";

/// A Markdown heading's size relative to the body text, by level. Kit 0.9.1
/// steps down from 1.8 and sets `####` at body size, so the `##` and `###`
/// headings replies use came out barely bigger than their text; these follow
/// GitHub's 2 / 1.5 / 1.25, a step larger below `#`, and keep `####` above
/// the body.
fn heading_scale(level: u8) -> f32 {
    match level {
        1 => 2.,
        2 => 1.6,
        3 => 1.3,
        4 => 1.15,
        5 => 1.,
        _ => 0.9,
    }
}

/// How messages and cards draw their Markdown, around body text of size
/// `body`. Kit's heading sizes are pixels, so they come from `body` to follow
/// the UI scale.
fn message_style(body: Pixels) -> TextViewStyle {
    TextViewStyle {
        heading_base_font_size: body,
        code_block: StyleRefinement::default()
            .pt(px(CODE_BLOCK_PAD_TOP))
            .pb(px(CODE_BLOCK_PAD_BOTTOM)),
        ..TextViewStyle::default()
    }
    .heading_font_size(|level, body| body * heading_scale(level))
}

/// The body size of text in a user's bubble: Kit's `BubbleContent` sets it
/// `text_sm`, 0.875 rem.
fn small_text(cx: &App) -> Pixels {
    cx.theme().font_size * 0.875
}

/// The body size of a card's body: `text_xs`, 0.75 rem, which the rail's
/// detail panel sets for the rest of it.
fn xsmall_text(cx: &App) -> Pixels {
    cx.theme().font_size * 0.75
}

/// How long a code block's copy button shows a check after it copies, as
/// Kit's own `Clipboard` does.
const COPIED_FOR: Duration = Duration::from_secs(2);

/// A code block's corner in a message or card: its language, and a button
/// that copies its code. Kit drops its own language name once a block has
/// actions, so this sets it, as Kit does.
fn code_actions(block: &CodeBlock, window: &mut Window, cx: &mut App) -> impl IntoElement {
    let copied = window.use_keyed_state("copy-code-copied", cx, |_, _| false);
    let shows_check = *copied.read(cx);
    let code = block.code();
    // Not Kit's `Clipboard`: that is a ghost button, whose hover is the
    // theme's accent, the color Kit fills a code block with, so hovering it
    // showed nothing. This is its button and behavior, with a hover the
    // block shows.
    let hover = cx.theme().secondary_active;
    let copy = Button::new("copy-code")
        .custom(
            ButtonCustomVariant::new(cx)
                .foreground(cx.theme().secondary_foreground)
                .hover(hover)
                .active(hover),
        )
        .xsmall()
        .icon(if shows_check {
            Lucide::Check
        } else {
            Lucide::Copy
        })
        .tooltip("Copy code")
        .accessibility_label("Copy code")
        .when(!shows_check, |button| {
            button.on_click(move |_, _, cx| {
                cx.stop_propagation();
                cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                copied.update(cx, |copied, cx| {
                    *copied = true;
                    cx.notify();
                });
                let copied = copied.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(COPIED_FOR).await;
                    copied.update(cx, |copied, cx| {
                        *copied = false;
                        cx.notify();
                    });
                })
                .detach();
            })
        });
    h_flex()
        .gap_1()
        .children(block.lang().filter(|lang| !lang.is_empty()).map(|lang| {
            div()
                .font_family(cx.theme().font_family.clone())
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(lang)
        }))
        .child(copy)
}

/// What a click in the transcript asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Open or fold part of work row `row`.
    Fold { row: usize, fold: Fold },
    /// Open or fold a finished turn's working.
    Worked { turn: usize },
    /// Open or close card `entry`'s body under its row on the rail.
    Card { entry: usize },
    /// Unfold the folded cards of the run that card `entry` opens.
    Unfold { entry: usize },
}

/// Carries out an [`Action`]; the session view owns the transcript.
pub(crate) type ActionHandler = Rc<dyn Fn(Action, &mut Window, &mut App)>;

/// What a click in the transcript calls: `act` for what changes the
/// transcript, and `open` for what a card on the rail is about, which the
/// session view opens.
pub(crate) struct Handlers {
    pub(crate) act: ActionHandler,
    pub(crate) open: rail::OpenHandler,
}

/// Builds the content of the scroller's item at a [`Place`]:
/// [`Transcript::render_content`], reached through the session view, because
/// a fold builds its content only while it shows, after
/// [`Transcript::render_item`] returned.
pub(crate) type ItemContent = Rc<dyn Fn(Place, &mut Window, &mut App) -> AnyElement>;

/// The turn in progress.
#[derive(Clone, Copy, Debug)]
struct Running {
    /// By its index among the timeline's turns. It may not have its opening
    /// row yet: the prompt's comes when the daemon echoes it, and a turn
    /// the daemon began itself opens with its first entry ([`Entry::Wake`]).
    turn: usize,
    since: Instant,
}

/// An entry's Markdown, or one part of a reply's, parsed once and kept
/// while the transcript lives.
///
/// Kit's `TextView::markdown` keeps its parse only while it is drawn in
/// consecutive frames. A message the list scrolled out of its drawn range
/// was parsed anew when it came back, in the background for one over 4 KiB,
/// so it drew empty for a frame, then at its height: a 2,884px reply drew
/// 15px, then 2,884px, and the transcript jumped as it scrolled up.
struct Markdown {
    text: Entity<TextViewState>,
    /// Remeasures the text's item when it lays out anew: its parse arrived.
    _relayout: Subscription,
}

/// A session's transcript state. Flat arrays indexed by row, entry or turn.
pub(crate) struct Transcript {
    scroller: Entity<MessageScrollerState>,
    timeline: Timeline,
    /// Which of the scroller's items each row takes. Shared with each
    /// [`Markdown`], which finds its item as the rows before it change.
    items: Rc<RefCell<Items>>,
    /// Per row: whether a work row lists its steps.
    steps: Vec<Disclosure>,
    /// Per entry: whether a step shows its details, or a card its body.
    details: Vec<Disclosure>,
    /// The runs of cards the user unfolded, by their first card's entry:
    /// one opened stays open.
    unfolded: HashSet<usize>,
    /// Per card row drawn since its run last changed: its run's first row
    /// and where the run's cards go ([`rail::places`]). A folded card is
    /// an empty row, so every one in view draws each frame: placing its
    /// run each time would cost the run's length per row.
    placed: RefCell<HashMap<usize, PlacedRun>>,
    /// Per turn: whether its folded working is open.
    worked: Vec<Disclosure>,
    /// Per turn: how long it took, for turns rho-ui watched. The daemon
    /// replays no turn times, so a replayed turn has one only from
    /// [`TurnTimes`].
    took: Vec<Option<Duration>>,
    running: Option<Running>,
    /// What holds up the running turn, which its header says.
    delay: Option<Delay>,
    /// Diff viewers, made when a step with patches first opens.
    diffs: Vec<DiffView>,
    /// Text views of steps' Markdown details, made when they first open.
    bodies: Vec<BodyView>,
    /// What the find bar found.
    find: Find,
    /// Per entry: a user message's attachments, or a tool's images, ready
    /// to draw.
    attachments: Vec<Vec<Preview>>,
    /// The images the session's messages show by path.
    pictures: Entity<Pictures>,
    /// Per entry: a message's text, or a card's body, with one for each
    /// part of a reply; none for anything else.
    markdown: Vec<Vec<Markdown>>,
}

impl Transcript {
    pub(crate) fn new(pictures: Entity<Pictures>, cx: &mut App) -> Self {
        Self {
            pictures,
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            timeline: Timeline::default(),
            items: Rc::default(),
            steps: Vec::new(),
            details: Vec::new(),
            unfolded: HashSet::new(),
            placed: RefCell::default(),
            worked: Vec::new(),
            took: Vec::new(),
            running: None,
            delay: None,
            diffs: Vec::new(),
            bodies: Vec::new(),
            find: Find::default(),
            attachments: Vec::new(),
            markdown: Vec::new(),
        }
    }

    pub(crate) fn scroller(&self) -> &Entity<MessageScrollerState> {
        &self.scroller
    }

    /// What reads the images the session's messages show by path: it
    /// says when each read ends ([`PictureRead`]).
    pub(crate) fn pictures(&self) -> &Entity<Pictures> {
        &self.pictures
    }

    /// Surface `surface` changed: what it shows, or which of its entries
    /// draws it. Its rows are measured again.
    pub(crate) fn surface_changed(&self, surface: &str, entries: &[Entry], cx: &mut App) {
        for (ix, entry) in entries.iter().enumerate() {
            if matches!(entry, Entry::Surface(shown) if shown.surface() == surface) {
                if let Some(row) = self.timeline.row_of(ix) {
                    self.remeasure(row..row + 1, cx);
                }
            }
        }
    }

    /// Whether row `row` is a compaction's. It stays out of its turn's
    /// folded working: the model sees what came before it only as a
    /// summary, which matters after the turn too.
    /// Whether `row` of `turn` folds behind the turn's "Worked for" line
    /// once the turn ends.
    fn folds_in_working(&self, turn: usize, row: usize, entries: &[Entry]) -> bool {
        let working = self.timeline.working(turn);
        working.is_some_and(|rows| rows.contains(&row)) && !self.is_compaction_row(row, entries)
    }

    /// Unfolds the run of cards whose first is entry `first`; whether it
    /// was folded.
    fn unfold_run(&mut self, first: usize) -> bool {
        let folded = self.unfolded.insert(first);
        if folded {
            self.placed.get_mut().clear();
        }
        folded
    }

    fn is_compaction_row(&self, row: usize, entries: &[Entry]) -> bool {
        matches!(
            self.timeline.rows().get(row),
            Some(Row::Message(entry)) if matches!(entries.get(*entry), Some(Entry::Compaction(_)))
        )
    }

    fn is_wake_row(&self, row: usize, entries: &[Entry]) -> bool {
        matches!(
            self.timeline.rows().get(row),
            Some(Row::Message(entry)) if matches!(entries.get(*entry), Some(Entry::Wake))
        )
    }

    /// The surface row `row` is for, if it is a surface's row.
    fn surface_row<'a>(&self, row: usize, entries: &'a [Entry]) -> Option<&'a SurfaceRef> {
        let Row::Message(entry) = self.timeline.rows().get(row)? else {
            return None;
        };
        match entries.get(*entry)? {
            Entry::Surface(shown) => Some(shown),
            _ => None,
        }
    }

    /// A picture of message `entry` was read, or is read again, which
    /// changes the message's height.
    pub(crate) fn picture_read(&self, read: &PictureRead, cx: &mut App) {
        if let Some(row) = self.timeline.row_of(read.entry) {
            self.remeasure(row..row + 1, cx);
        }
    }

    /// Places entries added since the last call in rows.
    pub(crate) fn sync(&mut self, entries: &[Entry], cx: &mut App) {
        let synced = self.timeline.sync(entries);
        if let Some(row) = synced.grew {
            self.remeasure(row..row + 1, cx);
        }
        let seen_entries = self.markdown.len();
        for (ix, entry) in entries.iter().enumerate().skip(seen_entries) {
            let markdown = self.markdown(ix, entry, cx);
            self.markdown.push(markdown);
        }
        let rows = self.timeline.rows().len();
        let counts: Vec<usize> = (rows - synced.added..rows)
            .map(|row| self.item_count(row))
            .collect();
        let added = self.items.borrow_mut().extend(counts);
        self.splice(added, cx);
        let known = self.took.len();
        let turns = self.timeline.turns().len();
        let seen = self.attachments.len();
        self.attachments
            .extend(entries.iter().skip(seen).map(previews));
        self.steps.resize(rows, Disclosure::Folded);
        self.details.resize(entries.len(), Disclosure::Folded);
        self.worked.resize(turns, Disclosure::Folded);
        self.took.resize(turns, None);
        for turn in known..turns {
            let opener = self.opener(turn, entries);
            let prompt = match opener {
                Some(Entry::User(prompt)) => Some(prompt),
                _ => None,
            };
            self.took[turn] = prompt.and_then(|prompt| cx.global::<TurnTimes>().get(prompt.id()));
            if prompt.is_some_and(UserMessage::is_steer) || matches!(opener, Some(Entry::Wake)) {
                self.split(turn, entries, cx);
            }
        }
        // A finished turn that grows, as history replays, folds more rows.
        if let Some(newest) = turns.checked_sub(1) {
            if (synced.added > 0 || synced.grew.is_some()) && self.is_finished(newest) {
                self.remeasure(self.timeline.turn_rows(newest), cx);
            }
        }
        // A row added after a card changes its run: a card that joins it
        // moves the run's end and may fold some of it, so the run's rows
        // are placed and draw anew; a wake the run explains takes the
        // run's gap ([`Self::keeps_gap_below`]).
        if synced.added > 0 {
            self.placed.get_mut().clear();
        }
        if let Some(before) = (rows - synced.added).checked_sub(1) {
            if synced.added > 0 && self.card_at(before, entries).is_some() {
                self.remeasure(self.run_of(before, entries), cx);
            }
        }
        self.find_changed(seen_entries..entries.len(), entries, cx);
    }

    /// The rail row `row` shows, if it shows one, with its entry: a card,
    /// or a plain notice, which sits on the rail too ([`Notice::is_card`]).
    fn card_at<'a>(&self, row: usize, entries: &'a [Entry]) -> Option<(usize, &'a Notice)> {
        let Row::Message(entry) = self.timeline.rows().get(row)? else {
            return None;
        };
        match entries.get(*entry)? {
            Entry::Notice(notice) => Some((*entry, notice)),
            _ => None,
        }
    }

    /// The rows of the run of cards around `row`: the cards between two
    /// other rows, such as replies. It costs the run's length, not the
    /// transcript's.
    fn run_of(&self, row: usize, entries: &[Entry]) -> Range<usize> {
        let is_card = |row: usize| self.card_at(row, entries).is_some();
        let mut start = row;
        while start > 0 && is_card(start - 1) {
            start -= 1;
        }
        let mut end = row + 1;
        while is_card(end) {
            end += 1;
        }
        start..end
    }

    /// Whether `row` keeps a gap below it. Rows do, but a rail row only at
    /// the end of its run, and not above a wake it explains: that wake's
    /// turn header keeps the gap above itself, so the gap folds away with
    /// the header of a turn that has no working.
    fn keeps_gap_below(&self, row: usize, entries: &[Entry]) -> bool {
        let gap_goes_on =
            self.card_at(row + 1, entries).is_some() || self.is_explained_wake(row + 1, entries);
        self.card_at(row, entries).is_none() || !gap_goes_on
    }

    /// Whether `row` is a wake under a card: the run of rail rows right
    /// above it holds one. The card says why rho continued, so the wake
    /// draws no line of its own: only its turn's header. A plain notice,
    /// the daemon's or rho-ui's own, sits on the rail too, but says nothing
    /// of why rho went on, so a wake with no card above it keeps its line,
    /// and no turn starts unexplained. It costs the run's length.
    fn is_explained_wake(&self, row: usize, entries: &[Entry]) -> bool {
        self.is_wake_row(row, entries)
            && (0..row)
                .rev()
                .map_while(|above| self.card_at(above, entries))
                .any(|(_, notice)| notice.is_card())
    }

    /// Forgets what it built for entries from `from` on, which went or
    /// moved, as a discarded response's do; [`Self::sync`] builds what is
    /// there now.
    pub(crate) fn truncate(&mut self, from: usize, entries: &[Entry], cx: &mut App) {
        let rows = self.timeline.truncate(from);
        let turns = self.timeline.turns().len();
        self.steps.truncate(rows);
        self.details.truncate(from);
        self.unfolded.retain(|&first| first < from);
        self.placed.get_mut().clear();
        self.attachments.truncate(from);
        self.markdown.truncate(from);
        self.pictures
            .update(cx, |pictures, _| pictures.forget_from(from));
        self.diffs.retain(|diff| diff.entry < from);
        self.bodies.retain(|body| body.entry < from);
        self.find_truncate(from);
        self.worked.truncate(turns);
        self.took.truncate(turns);
        let gone = self.items.borrow_mut().truncate(rows);
        self.splice(gone, cx);
        // A run of work may have lost steps, and a run of cards the cards
        // that folded.
        if let Some(last) = rows.checked_sub(1) {
            let run = match self.card_at(last, entries) {
                Some(_) => self.run_of(last, entries),
                None => last..rows,
            };
            self.remeasure(run, cx);
        }
    }

    /// Entry `ix` was added or changed. A new entry gets its row in
    /// [`Self::sync`].
    pub(crate) fn changed(&mut self, entries: &[Entry], ix: usize, cx: &mut App) {
        // A new entry gets its attachments in `sync`. A tool call's status
        // or text changes far more often than its images, and a preview
        // copies and hashes the whole image, so it is made again only for
        // images that changed.
        if let (Some(attachments), Some(entry)) = (self.attachments.get_mut(ix), entries.get(ix)) {
            if !shows_images_of(attachments, entry) {
                *attachments = previews(entry);
            }
        }
        if let Some(entry) = entries.get(ix) {
            self.set_markdown(ix, entry, cx);
        }
        if let Some(Entry::Tool(tool)) = entries.get(ix) {
            if let Some(diff) = self.diffs.iter_mut().find(|diff| diff.entry == ix) {
                diff.update(tool, cx);
            }
        }
        if let Some(step) = entries.get(ix).and_then(Entry::as_step) {
            if let Some(body) = self.bodies.iter_mut().find(|body| body.entry == ix) {
                body.update(step, cx);
            }
        }
        self.find_changed(ix..ix + 1, entries, cx);
        // Open before its patch or body came: their views are made now.
        if self.details.get(ix).is_some_and(|open| open.is_open()) {
            self.open_views(ix, entries, cx);
        }
        if let Some(row) = self.timeline.row_of(ix) {
            self.remeasure(row..row + 1, cx);
        }
    }

    /// Makes step `ix`'s diff viewer and the text view of its Markdown
    /// details, those it has and lacks, as its details open.
    fn open_views(&mut self, ix: usize, entries: &[Entry], cx: &mut App) {
        let (Some(step), Some(row)) = (
            entries.get(ix).and_then(Entry::as_step),
            self.timeline.row_of(ix),
        ) else {
            return;
        };
        if let Step::Tool(tool) = step {
            if !self.diffs.iter().any(|diff| diff.entry == ix) {
                self.diffs.extend(DiffView::new(ix, tool, cx));
            }
        }
        if self.bodies.iter().any(|body| body.entry == ix) {
            return;
        }
        let scroller = self.scroller.clone();
        let items = Rc::clone(&self.items);
        let relayout = move |cx: &mut App| {
            let items = items.borrow().of(row..row + 1);
            scroller.update(cx, |scroller, cx| scroller.remeasure_items(items, cx));
        };
        if let Some(body) = BodyView::new(ix, step, relayout, cx) {
            self.bodies.push(body);
            self.find_opened(ix, entries, cx);
        }
    }

    /// Entry `ix`'s Markdown, parsing from now, if it has any.
    fn markdown(&self, ix: usize, entry: &Entry, cx: &mut App) -> Vec<Markdown> {
        let Some(row) = self.timeline.row_of(ix) else {
            return Vec::new();
        };
        let sources = markdown_parts(entry);
        let parts = sources.iter().enumerate();
        parts
            .map(|(part, source)| self.part(row, part, source, cx))
            .collect()
    }

    /// Part `part` of the Markdown of `row`'s entry, parsing `source` from
    /// now.
    fn part(&self, row: usize, part: usize, source: &str, cx: &mut App) -> Markdown {
        let text = cx.new(|cx| TextViewState::markdown(source, cx));
        let scroller = self.scroller.clone();
        let items = Rc::clone(&self.items);
        let relayout = cx.observe(&text, move |_, cx| {
            let item = items.borrow().of(row..row + 1).start + part;
            scroller.update(cx, |scroller, cx| {
                scroller.remeasure_items(item..item + 1, cx)
            });
        });
        Markdown {
            text,
            _relayout: relayout,
        }
    }

    /// Shows entry `ix`'s Markdown as it is now. A reply that streams in
    /// grows its last part, and its row takes an item more for each part
    /// more.
    fn set_markdown(&mut self, ix: usize, entry: &Entry, cx: &mut App) {
        let (Some(row), Some(shown)) = (
            self.timeline.row_of(ix),
            self.markdown.get(ix).map(Vec::len),
        ) else {
            return;
        };
        let sources = markdown_parts(entry);
        for (markdown, source) in self.markdown[ix].iter().zip(&sources) {
            markdown
                .text
                .update(cx, |state, cx| state.set_text(source, cx));
        }
        for (part, source) in sources.iter().enumerate().skip(shown) {
            let markdown = self.part(row, part, source, cx);
            self.markdown[ix].push(markdown);
        }
        self.markdown[ix].truncate(sources.len());

        let count = self.item_count(row);
        let resized = self.items.borrow_mut().resize(row, count);
        self.splice(resized, cx);
    }

    /// Changes the scroller's items as [`Items`] changed.
    fn splice(&self, edit: Edit, cx: &mut App) {
        let Edit { old, count } = edit;
        if !old.is_empty() || count > 0 {
            self.scroller
                .update(cx, |scroller, cx| scroller.splice(old, count, cx));
        }
        debug_assert_eq!(
            self.scroller.read(cx).item_count(),
            self.items.borrow().len(),
            "the scroller has an item for each of the rows' items"
        );
    }

    /// How many of the scroller's items `row` takes: one, but one for each
    /// part of a reply.
    fn item_count(&self, row: usize) -> usize {
        match self.timeline.rows().get(row) {
            Some(Row::Message(entry)) => self.markdown.get(*entry).map_or(1, Vec::len).max(1),
            Some(Row::Work(_)) | None => 1,
        }
    }

    /// Part `part` of entry `ix`'s Markdown, if it has that part, to draw
    /// with Kit's `TextView::new`. Only a reply has more than one.
    fn text(&self, ix: usize, part: usize) -> Option<&Entity<TextViewState>> {
        Some(&self.markdown.get(ix)?.get(part)?.text)
    }

    /// What holds up the running turn, if anything: its header says so.
    pub(crate) fn set_delay(&mut self, delay: Option<Delay>) {
        self.delay = delay;
    }

    /// A turn began `since`, here or in another client: it is the next
    /// one, though its prompt row comes only when the daemon echoes the
    /// prompt. The turns before it stay folded.
    pub(crate) fn expect_turn(&mut self, since: Instant) {
        self.running = Some(Running {
            turn: self.timeline.turns().len(),
            since,
        });
    }

    /// The session was attached with a turn under way since `since`: it is
    /// the newest one, which unfolds.
    pub(crate) fn resume_turn(&mut self, since: Instant, cx: &mut App) {
        let newest = self.timeline.turns().len().saturating_sub(1);
        self.running = Some(Running {
            turn: newest,
            since,
        });
        self.remeasure(self.timeline.turn_rows(newest), cx);
    }

    /// Forgets the running turn without timing it, as when the session is
    /// followed again: the attach says whether a turn runs, and since
    /// when.
    pub(crate) fn forget_turn(&mut self, cx: &mut App) {
        if let Some(running) = self.running.take() {
            self.remeasure(self.timeline.turn_rows(running.turn), cx);
        }
    }

    /// The running turn finished, and folds.
    pub(crate) fn end_turn(&mut self, entries: &[Entry], cx: &mut App) {
        // The turn may have written files its messages showed before
        // they were there.
        self.pictures.update(cx, Pictures::retry);
        if let Some(running) = self.running.take() {
            self.time(running, Instant::now(), entries, cx);
        }
    }

    /// A steer or a wake opened `turn` while another ran. The daemon's
    /// replay can't tell a steer from a prompt, nor a result delivered
    /// after the reply from a turn of the daemon's own, so each opens a
    /// turn of its own, as it will when the session replays: the turn it
    /// joined finishes there, and the running one is the new one, timed
    /// from now. Two steers the model read at once leave the first one's
    /// turn at about 0s. A wake that opens the running turn itself joins
    /// none.
    ///
    /// A steer that came while the session was being attached arrives
    /// before [`Self::resume_turn`], so the turn it joined goes untimed and
    /// the steer's counts from the turn's start.
    fn split(&mut self, turn: usize, entries: &[Entry], cx: &mut App) {
        let Some(joined) = self.running.filter(|running| running.turn < turn) else {
            return;
        };
        let now = Instant::now();
        self.time(joined, now, entries, cx);
        self.running = Some(Running { turn, since: now });
    }

    /// Times `running`, which finished at `end`, and folds it. Its time is
    /// saved for when the session replays, under its prompt: a turn the
    /// daemon began itself has none, and a prompt that never arrived
    /// leaves no turn to time.
    fn time(&mut self, running: Running, end: Instant, entries: &[Entry], cx: &mut App) {
        let Running { turn, since } = running;
        let Some(slot) = self.took.get_mut(turn) else {
            return;
        };
        let took = end.saturating_duration_since(since);
        *slot = Some(took);
        if let Some(prompt) = self.prompt(turn, entries) {
            cx.global_mut::<TurnTimes>().record(prompt.id(), took);
        }
        self.remeasure(self.timeline.turn_rows(turn), cx);
    }

    /// `turn`'s prompt, or steer.
    fn prompt<'a>(&self, turn: usize, entries: &'a [Entry]) -> Option<&'a UserMessage> {
        match self.opener(turn, entries)? {
            Entry::User(prompt) => Some(prompt),
            _ => None,
        }
    }

    /// The entry that opens `turn`: its prompt, steer, or wake.
    fn opener<'a>(&self, turn: usize, entries: &'a [Entry]) -> Option<&'a Entry> {
        let row = *self.timeline.turns().get(turn)?;
        let Row::Message(ix) = self.timeline.rows().get(row)? else {
            return None;
        };
        entries.get(*ix)
    }

    /// Does what a click asked for. `action` names rows, entries and turns
    /// as last drawn; one the transcript has lost since is ignored.
    pub(crate) fn act(&mut self, action: Action, entries: &[Entry], cx: &mut App) {
        let rows = match action {
            Action::Fold {
                row,
                fold: Fold::Steps,
            } => {
                let Some(steps) = self.steps.get_mut(row) else {
                    return;
                };
                steps.toggle();
                row..row + 1
            }
            Action::Fold {
                row,
                fold: Fold::Details(ix),
            } => {
                let Some(details) = self.details.get_mut(ix) else {
                    return;
                };
                details.toggle();
                self.open_views(ix, entries, cx);
                row..row + 1
            }
            Action::Worked { turn } => {
                let Some(worked) = self.worked.get_mut(turn) else {
                    return;
                };
                worked.toggle();
                self.timeline.turn_rows(turn)
            }
            Action::Card { entry } => {
                let Some(shows) = self.details.get_mut(entry) else {
                    return;
                };
                shows.toggle();
                let Some(row) = self.timeline.row_of(entry) else {
                    return;
                };
                row..row + 1
            }
            Action::Unfold { entry } => {
                let Some(row) = self.timeline.row_of(entry) else {
                    return;
                };
                self.unfold_run(entry);
                self.run_of(row, entries)
            }
        };
        self.remeasure(rows, cx);
    }

    /// How long `turn` took, if this view ran it.
    #[cfg(test)]
    pub(crate) fn took(&self, turn: usize) -> Option<Duration> {
        self.took.get(turn).copied().flatten()
    }

    fn is_finished(&self, turn: usize) -> bool {
        self.running.is_none_or(|running| turn < running.turn)
    }

    /// Measures `rows` again.
    fn remeasure(&self, rows: Range<usize>, cx: &mut App) {
        let items = self.items.borrow().of(rows);
        self.scroller
            .update(cx, |scroller, cx| scroller.remeasure_items(items, cx));
    }

    /// The scroller's item `item`: a row, or a part of a long reply's, at a
    /// readable measure on wide windows. A turn's working is open while the
    /// turn runs and folds once it finishes, so its rows are built through
    /// `content`; a folded row takes no space. The gap below each row is
    /// here, inside any fold, not in the scroller's row style (which the
    /// session view zeroes).
    pub(crate) fn render_item(
        &self,
        item: usize,
        entries: &[Entry],
        surfaces: &Surfaces,
        handlers: &Handlers,
        content: &ItemContent,
        cx: &App,
    ) -> AnyElement {
        let measure = div().w_full().max_w(rems(48.)).mx_auto();
        let Some(place) = self.items.borrow().locate(item) else {
            return measure.into_any_element();
        };
        let Place {
            row,
            part,
            ends_row,
        } = place;
        // A surface stays usable after its turn: its row lies within the
        // turn's working, and does not fold with it. A surface that moved
        // under a later call, or was deleted, takes no room here.
        if let Some(shown) = self.surface_row(row, entries) {
            let panel = surfaces.panel(shown).cloned();
            return measure
                .when_some(panel, |this, panel| this.pb_3().child(panel))
                .into_any_element();
        }
        let gap_below = ends_row && self.keeps_gap_below(row, entries);
        let Some(turn) = self.timeline.turn_of(row) else {
            return measure
                .when(gap_below, Styled::pb_3)
                .child(self.render_content(place, entries, handlers, cx))
                .into_any_element();
        };
        if self.folds_in_working(turn, row, entries) {
            let shows = if self.is_finished(turn) {
                self.worked[turn]
            } else {
                Disclosure::Open
            };
            let content = Rc::clone(content);
            let id = ElementId::named_usize("working", row);
            return measure
                .child(Folding::new(
                    // A reply's parts fold each on their own.
                    ElementId::from((id, SharedString::from(part.to_string()))),
                    shows,
                    move |window, cx| {
                        div()
                            .when(gap_below, Styled::pb_3)
                            .child(content(place, window, cx))
                            .into_any_element()
                    },
                ))
                .into_any_element();
        }
        let opens_turn = self.timeline.turns().get(turn) == Some(&row);
        let content = (!self.is_explained_wake(row, entries))
            .then(|| self.render_content(place, entries, handlers, cx));
        measure
            .when(gap_below, Styled::pb_3)
            .children(content)
            .when(opens_turn, |this| {
                this.child(self.render_header(turn, &handlers.act, cx))
            })
            .into_any_element()
    }

    /// `turn`'s header, under its opening row. It counts up while the turn
    /// runs, then folds the turn's working; a turn with no working has no
    /// header once it finishes, so the running one's folds away.
    fn render_header(&self, turn: usize, act: &ActionHandler, cx: &App) -> AnyElement {
        let finished = self.is_finished(turn);
        let has_working = self.timeline.working(turn).is_some();
        let header = match self.running {
            Some(running) if !finished => Header::Working {
                running: running.since.elapsed(),
                delay: self.delay,
            },
            _ => Header::Worked {
                took: self.took[turn],
                fold: has_working.then_some(self.worked[turn]),
            },
        };
        let act = Rc::clone(act);
        let line = div()
            .pt_3()
            .child(work::turn_header(
                turn,
                header,
                move |window, cx| act(Action::Worked { turn }, window, cx),
                cx,
            ))
            .into_any_element();
        if finished && has_working {
            // Held still: a finished turn's history may replay its working
            // after its prompt, which shouldn't animate in.
            return line;
        }
        let shows = if finished {
            Disclosure::Folded
        } else {
            Disclosure::Open
        };
        Folding::new(
            ElementId::named_usize("turn-header", turn),
            shows,
            move |_, _| line,
        )
        .into_any_element()
    }

    /// The content of the scroller's item at `place`: a message, a part of
    /// a long reply, or a run of work.
    pub(crate) fn render_content(
        &self,
        place: Place,
        entries: &[Entry],
        handlers: &Handlers,
        cx: &App,
    ) -> AnyElement {
        let act = &handlers.act;
        let Place {
            row,
            part,
            ends_row,
        } = place;
        let Some(drawn) = self.timeline.rows().get(row) else {
            return div().into_any_element();
        };
        let turn = self.timeline.turn_of(row);
        let running = turn.is_some() && turn == self.running.map(|running| running.turn);
        let phase = if running && ends_row && row + 1 == self.timeline.rows().len() {
            Phase::Live
        } else {
            Phase::Settled
        };
        match drawn {
            Row::Message(entry) => match &entries[*entry] {
                Entry::Notice(_) => self.render_card(row, *entry, entries, handlers, cx),
                message => {
                    let attachments = self.attachments.get(*entry).map_or(&[][..], Vec::as_slice);
                    let shown = Shown {
                        text: self.text(*entry, part),
                        part,
                        ends_message: ends_row,
                        attachments,
                        pictures: &self.pictures,
                        phase,
                    };
                    render_message(message, *entry, &shown, cx)
                }
            },
            Row::Work(run) => {
                let act = Rc::clone(act);
                let on_fold: FoldHandler = Rc::new(move |fold, window, cx| {
                    act(Action::Fold { row, fold }, window, cx);
                });
                work::render(
                    WorkRow {
                        entries: &entries[run.clone()],
                        start: run.start,
                        steps: self.steps[row],
                        details: &self.details[run.clone()],
                        images: &self.attachments[run.clone()],
                        diffs: &self.diffs,
                        bodies: &self.bodies,
                        find: &self.find,
                        phase,
                        on_fold,
                    },
                    cx,
                )
            }
        }
    }
}

impl Transcript {
    /// The run of cards at `row`, placed once, then looked up until the
    /// run changes.
    fn placed(&self, row: usize, entries: &[Entry]) -> PlacedRun {
        if let Some(run) = self.placed.borrow().get(&row) {
            return run.clone();
        }
        let run = self.run_of(row, entries);
        let sizes: Vec<_> = run
            .clone()
            .filter_map(|row| self.card_at(row, entries))
            .map(|(_, card)| rail::RunCard::of(card))
            .collect();
        let first = self.card_at(run.start, entries).map(|(entry, _)| entry);
        let shown = if first.is_some_and(|first| self.unfolded.contains(&first)) {
            Disclosure::Open
        } else {
            Disclosure::Folded
        };
        let placed = PlacedRun {
            start: run.start,
            places: rail::places(&sizes, shown).into(),
        };
        let mut cache = self.placed.borrow_mut();
        for row in run {
            cache.insert(row, placed.clone());
        }
        placed
    }

    /// Card `ix`, at `row`, on the rail with the rest of its run: its row,
    /// the row that folds it with the run's other cards that may fold, or nothing
    /// while it is folded. An open card shows its body under its row.
    fn render_card(
        &self,
        row: usize,
        ix: usize,
        entries: &[Entry],
        Handlers { act, open }: &Handlers,
        cx: &App,
    ) -> AnyElement {
        let PlacedRun { start, places } = self.placed(row, entries);
        // A run spans only card rows.
        let card = |at: usize| self.card_at(start + at, entries).map(|(_, card)| card);
        let first = self.card_at(start, entries).map_or(ix, |(entry, _)| entry);
        match &places[row - start] {
            // An empty row. When it ends the run, it carries the gap below
            // the run (see `keeps_gap_below`).
            rail::Place::Hidden => div().into_any_element(),
            rail::Place::Fold { ends, folded } => {
                let folded: Vec<Card<'_>> = folded
                    .iter()
                    .filter_map(|&at| self.card_at(start + at, entries))
                    .map(|(entry, card)| Card::of(entry, card))
                    .collect();
                let act = Rc::clone(act);
                let unfold = move |window: &mut Window, cx: &mut App| {
                    act(Action::Unfold { entry: first }, window, cx);
                };
                rail::fold_row(&folded, *ends, unfold, cx)
            }
            rail::Place::Card(ends) => {
                let Some(notice) = card(row - start) else {
                    return div().into_any_element();
                };
                let card = Card::of(ix, notice);
                let shows = self.details.get(ix).is_some_and(|shows| shows.is_open());
                let body = match self.text(ix, 0) {
                    None => rail::Body::None,
                    Some(text) => {
                        let act = Rc::clone(act);
                        let on_toggle: rail::ToggleHandler = Rc::new(move |window, cx| {
                            act(Action::Card { entry: ix }, window, cx);
                        });
                        if shows {
                            let detail = TextView::new(text)
                                .style(message_style(xsmall_text(cx)))
                                .code_block_actions(code_actions)
                                .selectable(true)
                                .into_any_element();
                            rail::Body::Open(on_toggle, detail)
                        } else {
                            rail::Body::Shut(on_toggle)
                        }
                    }
                };
                let marks = (0..card.lines.len())
                    .map(|n| rail::LineMarks {
                        chip: self.find.marks(ix, Field::Card(n, CardPart::Chip)),
                        text: self.find.marks(ix, Field::Card(n, CardPart::Text)),
                    })
                    .collect();
                rail::CardRows {
                    card: &card,
                    marks,
                    ends: *ends,
                    body,
                    on_open: Rc::clone(open),
                }
                .render(cx)
            }
        }
    }
}

/// A run of cards, placed: its first row, and where each of its cards
/// goes.
#[derive(Clone, Debug)]
struct PlacedRun {
    start: usize,
    places: Rc<[rail::Place]>,
}

/// Whether a row is still being written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The newest row of a running turn.
    Live,
    Settled,
}

/// The previews of `entry`'s attachments, if it is a user message, or of
/// its images, if it is a tool call; an image whose header doesn't read is
/// left out.
fn previews(entry: &Entry) -> Vec<Preview> {
    match entry {
        Entry::User(message) => message
            .attachments()
            .iter()
            .filter_map(Preview::new)
            .collect(),
        Entry::Tool(tool) => tool
            .content()
            .iter()
            .filter_map(|content| match content {
                ToolContent::Image(image) => Preview::of_image(image),
                ToolContent::Text(_) | ToolContent::Diff { .. } => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether `previews` are of tool call `entry`'s images, each still the
/// one it was made from. Only a tool call's are checked: a user message's
/// attachments are made again whenever it changes, which is seldom.
fn shows_images_of(previews: &[Preview], entry: &Entry) -> bool {
    let Entry::Tool(tool) = entry else {
        return false;
    };
    let images = || {
        tool.content().iter().filter_map(|content| match content {
            ToolContent::Image(image) => Some(image),
            ToolContent::Text(_) | ToolContent::Diff { .. } => None,
        })
    };
    previews.len() == images().count()
        && previews
            .iter()
            .zip(images())
            .all(|(preview, image)| preview.image().is_some_and(|shown| shown.shows(image)))
}

/// One row of a user message's attachments, against the message's edge;
/// `None` when there are none.
fn attachment_row(cells: impl Iterator<Item = AnyElement>) -> Option<impl IntoElement> {
    let cells: Vec<_> = cells.collect();
    (!cells.is_empty()).then(|| {
        h_flex()
            .w_full()
            .flex_wrap()
            .justify_end()
            .gap_2()
            .children(cells)
    })
}

/// Attachment `n` of user message `ix`, drawn as `card`; an image opens in
/// the lightbox when clicked. Wrapped so UI tests can find it: Kit's
/// Attachment does not register its ID with them.
fn attachment_cell(ix: usize, n: usize, preview: &Preview, card: Attachment) -> AnyElement {
    let id = SharedString::from(format!("user-{ix}-attachment-{n}"));
    let card = card.id((ElementId::from(id.clone()), "card"));
    match preview.image() {
        Some(image) => lightbox::opens(id, image, SharedString::default(), card).into_any_element(),
        None => div()
            .id(id)
            .aria_label(preview.label())
            .test_support()
            .child(card)
            .into_any_element(),
    }
}

/// What a message row draws besides its entry.
struct Shown<'a> {
    /// The message's text, if it has any: for a reply, its part `part`.
    text: Option<&'a Entity<TextViewState>>,
    part: usize,
    /// Whether no part of the message follows this one.
    ends_message: bool,
    /// A user message's attachments.
    attachments: &'a [Preview],
    pictures: &'a Entity<Pictures>,
    phase: Phase,
}

/// The Markdown `entry` shows, each part to draw as an item of its own: a
/// reply's text in the parts [`parts::split`] gives, a user message's text
/// or a notice's detail whole, in its bubble or card.
fn markdown_parts(entry: &Entry) -> Vec<&str> {
    let whole = match entry {
        Entry::Agent(message) => return parts::split(message.text()),
        Entry::User(message) => Some(message.text()).filter(|text| !text.is_empty()),
        Entry::Notice(notice) => notice.description(),
        Entry::Thought(_)
        | Entry::Tool(_)
        | Entry::Compaction(_)
        | Entry::Surface(_)
        | Entry::Wake => None,
    };
    whole.into_iter().collect()
}

/// Compaction `ix`: a dashed rule across the transcript, since the model
/// now sees what came before it only as a summary, with how far the context
/// shrank. The counts are rho's: messages, and tokens it estimates locally.
fn compaction_divider(ix: usize, compaction: Compaction, cx: &App) -> AnyElement {
    let Compaction { before, after } = compaction;
    let label = if before == after {
        "Context unchanged by compaction".to_owned()
    } else {
        format!(
            "Context compacted · {} → {} messages · {} → {} tokens",
            before.messages,
            after.messages,
            usage::tokens(before.tokens),
            usage::tokens(after.tokens),
        )
    };
    let rule = || Separator::horizontal_dashed().flex_1().min_w_4();
    h_flex()
        .id(SharedString::from(format!("compaction-{ix}")))
        .test_support()
        .aria_label(label.clone())
        .w_full()
        .gap_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(rule())
        .child(Icon::new(Lucide::FoldVertical).xsmall().flex_shrink_0())
        .child(div().min_w_0().truncate().child(label))
        .child(rule())
        .into_any_element()
}

/// The height of a reply's first line, which its time centers on: GPUI's
/// default line height, φ, which Kit's text view keeps.
fn first_line(cx: &App) -> Pixels {
    cx.theme().font_size * 1.618
}

/// When a reply arrived, in its top right corner, in the rail's column of
/// times, over the first line's end on the page's color.
fn reply_time(
    ix: usize,
    at: chrono::DateTime<chrono::Utc>,
    cx: &App,
) -> gpui_kit::base::ObservedElement<gpui_kit::Stateful<gpui_kit::Div>> {
    hover_time("reply", ix, at, cx)
        .absolute()
        .top_0()
        .right_0()
        .h(first_line(cx))
        .flex()
        .items_center()
        .bg(cx.theme().background)
}

/// When a message arrived, shown while the pointer is on it, as faint as
/// the rail's times. Not Kit's `MessageHeader` or `MessageFooter`: each
/// keeps its line while the time is hidden, which would space every
/// message out for a time that shows only on hover. Its ID is
/// `{kind}-{ix}-time`, for entry `ix`.
fn hover_time(
    kind: &str,
    ix: usize,
    at: chrono::DateTime<chrono::Utc>,
    cx: &App,
) -> gpui_kit::base::ObservedElement<gpui_kit::Stateful<gpui_kit::Div>> {
    rail::clock(format!("{kind}-{ix}-time"), Some(at), cx)
        .opacity(0.)
        .group_hover(MESSAGE_GROUP, |style| style.opacity(1.))
}

/// Builds the row for a user message, a reply, a wake no card explains
/// ([`Transcript::is_explained_wake`]), or a compaction:
/// entry `ix`, with what `shown` holds. Tool calls and thoughts are drawn by [`crate::work`].
fn render_message(entry: &Entry, ix: usize, shown: &Shown<'_>, cx: &App) -> AnyElement {
    let Shown {
        text,
        part,
        ends_message,
        attachments,
        pictures,
        phase,
    } = *shown;
    let key = ElementId::named_usize("row", ix);
    match entry {
        Entry::User(message) => {
            // Above the text, the images in their own proportions on one
            // row, then the file chips on the next: one height to a row.
            // IDs number the attachments in the order they were sent.
            let numbered = || attachments.iter().enumerate();
            let images = attachment_row(numbered().filter_map(|(n, preview)| {
                let tile = preview.image()?.tile(IMAGE_HEIGHT);
                Some(attachment_cell(ix, n, preview, tile))
            }));
            let files = attachment_row(
                numbered()
                    .filter(|(_, preview)| preview.image().is_none())
                    .map(|(n, preview)| attachment_cell(ix, n, preview, preview.card())),
            );
            let text = text.map(|text| {
                Bubble::new().with_variant(BubbleVariant::Muted).child(
                    div()
                        .id(SharedString::from(format!("user-{ix}")))
                        .test_support()
                        .child(
                            TextView::new(text)
                                .style(message_style(small_text(cx)))
                                .code_block_actions(code_actions)
                                .markdown_extensions(message_extensions(pictures, ix))
                                .selectable(true),
                        ),
                )
            });
            // The time sits left of the bubble, where the row is empty: a
            // bubble is at most 80% of it wide. (`MessageContent::bubble`
            // only marks a ghost bubble; this one is muted.)
            let text = text.map(|text| {
                h_flex()
                    .group(MESSAGE_GROUP)
                    .w_full()
                    .justify_end()
                    .items_center()
                    .gap_2()
                    .children(message.arrived().map(|at| hover_time("user", ix, at, cx)))
                    .child(text)
            });
            let content = MessageContent::new()
                .children(images)
                .children(files)
                .children(text);
            Message::new()
                .id(key)
                .alignment(MessageAlignment::End)
                .content(content)
                .into_any_element()
        }
        Entry::Agent(message) => {
            let style = message_style(cx.theme().font_size);
            // The gap Kit would leave after the part's last block, were
            // the reply one text.
            let gap = style.paragraph_gap;
            let id = match part {
                0 => format!("reply-{ix}"),
                part => format!("reply-{ix}-{part}"),
            };
            let time = message
                .arrived()
                .filter(|_| part == 0)
                .map(|at| reply_time(ix, at, cx));
            div()
                .id(SharedString::from(id))
                .test_support()
                .group(MESSAGE_GROUP)
                .relative()
                .w_full()
                .when(!ends_message, |this| this.pb(gap))
                .children(time)
                .children(text.map(|text| {
                    TextView::new(text)
                        .style(style)
                        .code_block_actions(code_actions)
                        .markdown_extensions(message_extensions(pictures, ix))
                        .selectable(true)
                        .stream_fade(phase == Phase::Live)
                }))
                .into_any_element()
        }
        Entry::Wake => h_flex()
            .id(SharedString::from(format!("wake-{ix}")))
            .test_support()
            .gap_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(Lucide::Bell).xsmall())
            .child("Rho continued on its own")
            .into_any_element(),
        Entry::Compaction(compaction) => compaction_divider(ix, *compaction, cx),
        // The timeline puts these in work rows, never message rows, and
        // the transcript draws notices and surfaces itself.
        Entry::Thought(_) | Entry::Tool(_) | Entry::Notice(_) | Entry::Surface(_) => {
            div().into_any_element()
        }
    }
}
