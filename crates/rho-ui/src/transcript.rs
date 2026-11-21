//! The transcript: a session's entries as rows in a scroller. Messages and
//! notices are drawn here; runs of tool calls and thoughts by
//! [`crate::work`]. Under each prompt, a header says "Working for 38s"
//! while the turn runs; when it finishes, the same line says "Worked for"
//! and the turn's working folds behind it. This owns what the user opened
//! and folded, the turns' lengths, and the user messages' attachments,
//! ready to draw.
//!
//! A notice's detail is Markdown, and can be long: a subagent's card holds
//! its whole result. It shows its first [`NOTICE_LINES`] lines, with a
//! button to show the rest.
//!
//! A row is one of the scroller's items, but a long reply's row is one for
//! each of its parts ([`crate::parts`]), so the scroller draws only the
//! parts near the window.

use std::{cell::RefCell, ops::Range, rc::Rc, time::Duration};

use web_time::Instant;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::{text::CodeBlock, TestSupportExt as _};
use gpui_kit::component::{
    alert::{Alert, AlertVariant},
    attachment::Attachment as Card,
    bubble::{Bubble, BubbleVariant},
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex,
    message::{Message, MessageAlignment, MessageContent},
    message_scroller::MessageScrollerState,
    text::{MarkdownExtensions, TextView, TextViewState, TextViewStyle},
    v_flex, ActiveTheme as _, Colorize as _, Icon, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, rems, transparent_white, AnyElement, App, AppContext as _, ClipboardItem, ElementId,
    Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled, Subscription, Window,
};
use rho_ui_core::{
    surface::SurfaceRef, Delay, Entry, Notice, NoticeLevel, Row, Timeline, ToolContent, UserMessage,
};

use crate::{
    attachments::Preview,
    figures,
    fold::Folding,
    lightbox,
    parts::{self, Edit, Items, Place},
    pictures::{self, PictureRead, Pictures},
    surface_view::Surfaces,
    turn_times::TurnTimes,
    work::{self, DiffView, Disclosure, Fold, FoldHandler, Header, WorkRow},
};

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

/// How many lines of a notice's detail show until it is opened. Kit caps
/// the height at this many body lines, so headings and code blocks take
/// more room: a subagent's result cut at 6 lines shows its first line and
/// heading, and measured 202px tall, card and all, in the UI test's window
/// (12 lines measured 340px and read as most of the result). A short card,
/// such as a finished command's, still shows whole.
const NOTICE_LINES: usize = 6;

/// The gap between a notice's paragraphs, in rems: Kit's alerts' own, so a
/// detail sets as tight as the card it is in.
const NOTICE_PARAGRAPH_GAP: f32 = 0.2;

/// How messages and notices draw their Markdown.
fn message_style() -> TextViewStyle {
    TextViewStyle {
        code_block: StyleRefinement::default()
            .pt(px(CODE_BLOCK_PAD_TOP))
            .pb(px(CODE_BLOCK_PAD_BOTTOM)),
        ..TextViewStyle::default()
    }
}

/// How long a code block's copy button shows a check after it copies, as
/// Kit's own `Clipboard` does.
const COPIED_FOR: Duration = Duration::from_secs(2);

/// A code block's corner in a message or notice: its language, and a button
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
    /// Show all of notice `entry`'s detail, or only its first lines.
    Notice { entry: usize },
}

/// Carries out an [`Action`]; the session view owns the transcript.
pub(crate) type ActionHandler = Rc<dyn Fn(Action, &mut Window, &mut App)>;

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
    /// Remeasures the text's item when it lays out anew: its parse arrived,
    /// or a notice's detail started or stopped being cut short, which adds
    /// or drops its button.
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
    /// Per entry: whether a step shows its details, or a notice all of its
    /// detail.
    details: Vec<Disclosure>,
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
    /// Per entry: a user message's attachments, or a tool's images, ready
    /// to draw.
    attachments: Vec<Vec<Preview>>,
    /// The images the session's messages show by path.
    pictures: Entity<Pictures>,
    /// Per entry: a message's text, or a notice's detail, with one for each
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
            worked: Vec::new(),
            took: Vec::new(),
            running: None,
            delay: None,
            diffs: Vec::new(),
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
        for (ix, entry) in entries.iter().enumerate().skip(self.markdown.len()) {
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
            let prompt = self.prompt(turn, entries);
            self.took[turn] = prompt.and_then(|prompt| cx.global::<TurnTimes>().get(prompt.id()));
            if prompt.is_some_and(UserMessage::is_steer) {
                self.steered(turn, entries, cx);
            }
        }
        // A finished turn that grows, as history replays, folds more rows.
        if let Some(newest) = turns.checked_sub(1) {
            if (synced.added > 0 || synced.grew.is_some()) && self.is_finished(newest) {
                self.remeasure(self.timeline.turn_rows(newest), cx);
            }
        }
    }

    /// Forgets what it built for entries from `from` on, which went or
    /// moved, as a discarded response's do; [`Self::sync`] builds what is
    /// there now.
    pub(crate) fn truncate(&mut self, from: usize, cx: &mut App) {
        let rows = self.timeline.truncate(from);
        let turns = self.timeline.turns().len();
        self.steps.truncate(rows);
        self.details.truncate(from);
        self.attachments.truncate(from);
        self.markdown.truncate(from);
        self.pictures
            .update(cx, |pictures, _| pictures.forget_from(from));
        self.diffs.retain(|diff| diff.entry < from);
        self.worked.truncate(turns);
        self.took.truncate(turns);
        let gone = self.items.borrow_mut().truncate(rows);
        self.splice(gone, cx);
        // A run of work may have lost steps.
        if let Some(last) = rows.checked_sub(1) {
            self.remeasure(last..rows, cx);
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
            match self.diffs.iter_mut().find(|diff| diff.entry == ix) {
                Some(diff) => diff.update(tool, cx),
                // Open before its patch came: the viewer is made now.
                None if self.details.get(ix).is_some_and(|open| open.is_open()) => {
                    self.diffs.extend(DiffView::new(ix, tool, cx));
                }
                None => {}
            }
        }
        if let Some(row) = self.timeline.row_of(ix) {
            self.remeasure(row..row + 1, cx);
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

    /// A steer opened `turn` while another ran. The daemon's replay can't
    /// tell a steer from a prompt, so the steer opens a turn of its own, as
    /// it will when the session replays: the turn it joined finishes there,
    /// and the running one is the steer's, timed from now. Two steers the
    /// model read at once leave the first one's turn at about 0s.
    ///
    /// A steer that came while the session was being attached arrives
    /// before [`Self::resume_turn`], so the turn it joined goes untimed and
    /// the steer's counts from the turn's start.
    fn steered(&mut self, turn: usize, entries: &[Entry], cx: &mut App) {
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
        let row = *self.timeline.turns().get(turn)?;
        let Row::Message(ix) = self.timeline.rows().get(row)? else {
            return None;
        };
        match entries.get(*ix)? {
            Entry::User(prompt) => Some(prompt),
            _ => None,
        }
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
                if let Some(Entry::Tool(tool)) = entries.get(ix) {
                    if !self.diffs.iter().any(|diff| diff.entry == ix) {
                        self.diffs.extend(DiffView::new(ix, tool, cx));
                    }
                }
                row..row + 1
            }
            Action::Worked { turn } => {
                let Some(worked) = self.worked.get_mut(turn) else {
                    return;
                };
                worked.toggle();
                self.timeline.turn_rows(turn)
            }
            Action::Notice { entry } => {
                let Some(shows) = self.details.get_mut(entry) else {
                    return;
                };
                shows.toggle();
                let Some(row) = self.timeline.row_of(entry) else {
                    return;
                };
                row..row + 1
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
        act: &ActionHandler,
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
        let Some(turn) = self.timeline.turn_of(row) else {
            return measure
                .when(ends_row, Styled::pb_3)
                .child(self.render_content(place, entries, act, cx))
                .into_any_element();
        };
        let working = self.timeline.working(turn);
        if working.as_ref().is_some_and(|rows| rows.contains(&row)) {
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
                            .when(ends_row, Styled::pb_3)
                            .child(content(place, window, cx))
                            .into_any_element()
                    },
                ))
                .into_any_element();
        }
        let opens_turn = self.timeline.turns().get(turn) == Some(&row);
        measure
            .when(ends_row, Styled::pb_3)
            .child(self.render_content(place, entries, act, cx))
            .when(opens_turn, |this| {
                this.child(self.render_header(turn, act, cx))
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
        act: &ActionHandler,
        cx: &App,
    ) -> AnyElement {
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
                Entry::Notice(notice) => self.render_notice(*entry, notice, act, cx),
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
    /// Notice `ix`: its title, and its detail in Markdown, cut to
    /// [`NOTICE_LINES`] lines until opened with the button under it.
    fn render_notice(
        &self,
        ix: usize,
        notice: &Notice,
        act: &ActionHandler,
        cx: &App,
    ) -> AnyElement {
        let key = ElementId::named_usize("row", ix);
        let variant = match notice.level() {
            NoticeLevel::Info => AlertVariant::Info,
            NoticeLevel::Warning => AlertVariant::Warning,
            NoticeLevel::Error => AlertVariant::Error,
        };
        let Some(detail) = self.text(ix, 0) else {
            return Alert::new(key, SharedString::from(notice.title().to_owned()))
                .with_variant(variant)
                .small()
                .into_any_element();
        };
        let open = self.details.get(ix).is_some_and(|shows| shows.is_open());
        let mut text = TextView::new(detail)
            .style(message_style().paragraph_gap(rems(NOTICE_PARAGRAPH_GAP)))
            .code_block_actions(code_actions)
            .selectable(true);
        if !open {
            text = text.max_lines(NOTICE_LINES);
        }
        // Kit's text view says whether the last frame cut it short.
        let toggle = (open || detail.read(cx).is_clamped()).then(|| {
            let act = Rc::clone(act);
            let (label, icon) = if open {
                ("Show less", Lucide::ChevronUp)
            } else {
                ("Show more", Lucide::ChevronDown)
            };
            Button::new(SharedString::from(format!("notice-{ix}-toggle")))
                .ghost()
                .xsmall()
                .icon(icon)
                .label(label)
                .on_click(move |_, window, cx| act(Action::Notice { entry: ix }, window, cx))
        });
        v_flex()
            .id(SharedString::from(format!("notice-{ix}")))
            .test_support()
            .aria_label(notice.title().to_owned())
            .w_full()
            .gap_1()
            .child(notice_card(key, notice, text, cx))
            .children(toggle)
            .into_any_element()
    }
}

/// A notice with a detail: Kit's small [`Alert`], drawn here because Kit's
/// sets its own style on its message, which drops the code-block padding
/// [`message_style`] gives `text`. Sizes and colors are Kit 0.9.1's.
fn notice_card(key: ElementId, notice: &Notice, text: TextView, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let color = match notice.level() {
        NoticeLevel::Info => theme.info,
        NoticeLevel::Warning => theme.warning,
        NoticeLevel::Error => theme.danger,
    };
    h_flex()
        .id(key)
        .w_full()
        .items_start()
        .gap(px(6.))
        .px(px(12.))
        .py(px(8.))
        .text_sm()
        .text_color(color)
        .bg(color.mix_oklab(transparent_white(), 0.04))
        .border_1()
        .border_color(color.mix_oklab(transparent_white(), 0.3))
        .rounded(theme.radius)
        .child(div().mt(px(5.)).child(Icon::new(Lucide::Info)))
        .child(
            v_flex()
                .flex_1()
                .overflow_hidden()
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .font_semibold()
                        .child(notice.title().to_owned()),
                )
                .child(text),
        )
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
fn attachment_cell(ix: usize, n: usize, preview: &Preview, card: Card) -> AnyElement {
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
        Entry::Thought(_) | Entry::Tool(_) | Entry::Surface(_) | Entry::Wake => None,
    };
    whole.into_iter().collect()
}

/// Builds the row for a user message or a reply: entry `ix`, with what
/// `shown` holds. Tool calls and thoughts are drawn by [`crate::work`].
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
        Entry::User(_) => {
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
                                .style(message_style())
                                .code_block_actions(code_actions)
                                .markdown_extensions(message_extensions(pictures, ix))
                                .selectable(true),
                        ),
                )
            });
            let mut content = MessageContent::new().children(images).children(files);
            if let Some(text) = text {
                content = content.bubble(text);
            }
            Message::new()
                .id(key)
                .alignment(MessageAlignment::End)
                .content(content)
                .into_any_element()
        }
        Entry::Agent(_) => {
            let style = message_style();
            // The gap Kit would leave after the part's last block, were
            // the reply one text.
            let gap = style.paragraph_gap;
            let id = match part {
                0 => format!("reply-{ix}"),
                part => format!("reply-{ix}-{part}"),
            };
            div()
                .id(SharedString::from(id))
                .test_support()
                .w_full()
                .when(!ends_message, |this| this.pb(gap))
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
        // The timeline puts these in work rows, never message rows, and
        // the transcript draws notices and surfaces itself.
        Entry::Thought(_) | Entry::Tool(_) | Entry::Notice(_) | Entry::Surface(_) => {
            div().into_any_element()
        }
    }
}
