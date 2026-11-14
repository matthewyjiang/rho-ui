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

use std::{
    ops::Range,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    alert::{Alert, AlertVariant},
    attachment::Attachment as Card,
    bubble::{Bubble, BubbleVariant},
    button::{Button, ButtonVariants as _},
    h_flex,
    message::{Message, MessageAlignment, MessageContent},
    message_scroller::MessageScrollerState,
    text::{TextView, TextViewState, TextViewStyle},
    v_flex, ActiveTheme as _, Colorize as _, Icon, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, rems, transparent_white, AnyElement, App, AppContext as _, ElementId, Entity,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription, Window,
};
use rho_ui_core::{Entry, Notice, NoticeLevel, Row, Timeline};

use crate::{
    attachments::Preview,
    figures,
    fold::Folding,
    turn_times::TurnTimes,
    work::{self, DiffView, Disclosure, Fold, FoldHandler, Header, WorkRow},
};

/// How tall an image attached to a user message is, in rems: a
/// screenshot's layout reads at this size, and two side by side fit the
/// transcript's measure.
const IMAGE_HEIGHT: f32 = 10.;

/// A code block's padding above its code. Kit sets the language name in the
/// top corner, its letters 10px to 17px down; this starts the code's letters
/// about 4px under them, so the name sits in the padding rather than reading
/// as a line of the block, as Kit's own 28px makes it.
const CODE_BLOCK_PAD_TOP: f32 = 19.;

/// A code block's padding below its code: 2px more than above, as a line's
/// letters sit about 2px lower in its box than its bottom, so the code is as
/// far from both edges.
const CODE_BLOCK_PAD_BOTTOM: f32 = 21.;

/// How many lines of a notice's detail show until it is opened. Kit counts
/// body lines, so headings and code blocks take more room: a subagent's
/// result cut at 12 lines, with two headings and a code block, measured
/// 340px tall card and all in the UI test's 800px window, leaving most of
/// the window to the conversation, while a short card, such as a finished
/// command's, shows whole.
const NOTICE_LINES: usize = 12;

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

/// Builds row `ix`'s content: [`Transcript::render_content`], reached
/// through the session view, because a fold builds its rows only while they
/// show, after [`Transcript::render_row`] returned.
pub(crate) type RowContent = Rc<dyn Fn(usize, &mut Window, &mut App) -> AnyElement>;

/// The turn in progress.
#[derive(Clone, Copy, Debug)]
struct Running {
    /// By its index among the timeline's turns. It may not have its opening
    /// row yet: the prompt's comes when the daemon echoes it, and a turn
    /// the daemon began itself opens with its first entry ([`Entry::Wake`]).
    turn: usize,
    since: Instant,
}

/// An entry's Markdown, parsed once and kept while the transcript lives.
///
/// Kit's `TextView::markdown` keeps its parse only while it is drawn in
/// consecutive frames. A message the list scrolled out of its drawn range
/// was parsed anew when it came back, in the background for one over 4 KiB,
/// so it drew empty for a frame, then at its height: a 2,884px reply drew
/// 15px, then 2,884px, and the transcript jumped as it scrolled up.
struct Markdown {
    text: Entity<TextViewState>,
    /// Remeasures the entry's row when the text lays out anew: its parse
    /// arrived, or a notice's detail started or stopped being cut short,
    /// which adds or drops its button.
    _relayout: Subscription,
}

/// A session's transcript state. Flat arrays indexed by row, entry or turn.
pub(crate) struct Transcript {
    scroller: Entity<MessageScrollerState>,
    timeline: Timeline,
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
    /// Diff viewers, made when a step with patches first opens.
    diffs: Vec<DiffView>,
    /// Per entry: a user message's attachments, ready to draw.
    attachments: Vec<Vec<Preview>>,
    /// Per entry: a message's text, or a notice's detail; `None` for
    /// anything else.
    markdown: Vec<Option<Markdown>>,
}

impl Transcript {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            timeline: Timeline::default(),
            steps: Vec::new(),
            details: Vec::new(),
            worked: Vec::new(),
            took: Vec::new(),
            running: None,
            diffs: Vec::new(),
            attachments: Vec::new(),
            markdown: Vec::new(),
        }
    }

    pub(crate) fn scroller(&self) -> &Entity<MessageScrollerState> {
        &self.scroller
    }

    /// Places entries added since the last call in rows.
    pub(crate) fn sync(&mut self, entries: &[Entry], cx: &mut App) {
        let synced = self.timeline.sync(entries);
        if let Some(row) = synced.grew {
            self.remeasure(row..row + 1, cx);
        }
        if synced.added > 0 {
            self.scroller
                .update(cx, |scroller, cx| scroller.append(synced.added, cx));
        }
        let known = self.took.len();
        let turns = self.timeline.turns().len();
        let seen = self.attachments.len();
        self.attachments
            .extend(entries.iter().skip(seen).map(previews));
        for (ix, entry) in entries.iter().enumerate().skip(self.markdown.len()) {
            let markdown = self.markdown(ix, entry, cx);
            self.markdown.push(markdown);
        }
        self.steps
            .resize(self.timeline.rows().len(), Disclosure::Folded);
        self.details.resize(entries.len(), Disclosure::Folded);
        self.worked.resize(turns, Disclosure::Folded);
        self.took.resize(turns, None);
        let times = cx.global::<TurnTimes>();
        for turn in known..turns {
            self.took[turn] = self.prompt_id(turn, entries).and_then(|id| times.get(id));
        }
        // A finished turn that grows, as history replays, folds more rows.
        if let Some(newest) = turns.checked_sub(1) {
            if (synced.added > 0 || synced.grew.is_some()) && self.is_finished(newest) {
                self.remeasure(self.timeline.turn_rows(newest), cx);
            }
        }
    }

    /// Entry `ix` was added or changed. A new entry gets its row in
    /// [`Self::sync`].
    pub(crate) fn changed(&mut self, entries: &[Entry], ix: usize, cx: &mut App) {
        // A new entry gets its attachments in `sync`.
        if let (Some(attachments), Some(entry)) = (self.attachments.get_mut(ix), entries.get(ix)) {
            *attachments = previews(entry);
        }
        // A reply streams in.
        if let (Some(Some(markdown)), Some(text)) = (
            self.markdown.get(ix),
            entries.get(ix).and_then(markdown_text),
        ) {
            markdown
                .text
                .update(cx, |state, cx| state.set_text(text, cx));
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
    fn markdown(&self, ix: usize, entry: &Entry, cx: &mut App) -> Option<Markdown> {
        let text = markdown_text(entry)?;
        let row = self.timeline.row_of(ix)?;
        let text = cx.new(|cx| TextViewState::markdown(text, cx));
        let scroller = self.scroller.clone();
        let relayout = cx.observe(&text, move |_, cx| {
            scroller.update(cx, |scroller, cx| {
                scroller.remeasure_items(row..row + 1, cx)
            });
        });
        Some(Markdown {
            text,
            _relayout: relayout,
        })
    }

    /// Entry `ix`'s Markdown, if it has any, to draw with Kit's
    /// `TextView::new`.
    fn text(&self, ix: usize) -> Option<&Entity<TextViewState>> {
        Some(&self.markdown.get(ix)?.as_ref()?.text)
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

    /// The running turn finished, and folds. Its time is saved for when the
    /// session replays, under its prompt: a turn the daemon began itself
    /// has none, and a prompt that never arrived leaves no turn to time.
    pub(crate) fn end_turn(&mut self, entries: &[Entry], cx: &mut App) {
        let Some(Running { turn, since }) = self.running.take() else {
            return;
        };
        let took = since.elapsed();
        if turn >= self.took.len() {
            return;
        }
        self.took[turn] = Some(took);
        if let Some(prompt) = self.prompt_id(turn, entries) {
            cx.global_mut::<TurnTimes>().record(prompt, took);
        }
        self.remeasure(self.timeline.turn_rows(turn), cx);
    }

    /// The message ID of `turn`'s prompt.
    fn prompt_id<'a>(&self, turn: usize, entries: &'a [Entry]) -> Option<&'a str> {
        let row = *self.timeline.turns().get(turn)?;
        let Row::Message(ix) = self.timeline.rows().get(row)? else {
            return None;
        };
        match entries.get(*ix)? {
            Entry::User(prompt) => Some(prompt.id()),
            _ => None,
        }
    }

    pub(crate) fn act(&mut self, action: Action, entries: &[Entry], cx: &mut App) {
        let rows = match action {
            Action::Fold {
                row,
                fold: Fold::Steps,
            } => {
                self.steps[row].toggle();
                row..row + 1
            }
            Action::Fold {
                row,
                fold: Fold::Details(ix),
            } => {
                if let Some(Entry::Tool(tool)) = entries.get(ix) {
                    if !self.diffs.iter().any(|diff| diff.entry == ix) {
                        self.diffs.extend(DiffView::new(ix, tool, cx));
                    }
                }
                self.details[ix].toggle();
                row..row + 1
            }
            Action::Worked { turn } => {
                self.worked[turn].toggle();
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

    fn remeasure(&self, rows: Range<usize>, cx: &mut App) {
        self.scroller
            .update(cx, |scroller, cx| scroller.remeasure_items(rows, cx));
    }

    /// Row `ix`, at a readable measure on wide windows. A turn's working
    /// is open while the turn runs and folds once it finishes, so its rows
    /// are built through `content`; a folded row takes no space. The gap
    /// below each row is here, inside any fold, not in the scroller's row
    /// style (which the session view zeroes).
    pub(crate) fn render_row(
        &self,
        ix: usize,
        entries: &[Entry],
        act: &ActionHandler,
        content: &RowContent,
        cx: &App,
    ) -> AnyElement {
        let measure = div().w_full().max_w(rems(48.)).mx_auto();
        let Some(turn) = self.timeline.turn_of(ix) else {
            return measure
                .pb_3()
                .child(self.render_content(ix, entries, act, cx))
                .into_any_element();
        };
        let working = self.timeline.working(turn);
        if working.as_ref().is_some_and(|rows| rows.contains(&ix)) {
            let shows = if self.is_finished(turn) {
                self.worked[turn]
            } else {
                Disclosure::Open
            };
            let content = Rc::clone(content);
            return measure
                .child(Folding::new(
                    ElementId::named_usize("working", ix),
                    shows,
                    move |window, cx| {
                        div()
                            .pb_3()
                            .child(content(ix, window, cx))
                            .into_any_element()
                    },
                ))
                .into_any_element();
        }
        let opens_turn = self.timeline.turns().get(turn) == Some(&ix);
        measure
            .pb_3()
            .child(self.render_content(ix, entries, act, cx))
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
            Some(running) if !finished => Header::Working(running.since.elapsed()),
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

    /// Row `ix`'s own content: a message, or a run of work.
    pub(crate) fn render_content(
        &self,
        ix: usize,
        entries: &[Entry],
        act: &ActionHandler,
        cx: &App,
    ) -> AnyElement {
        let Some(row) = self.timeline.rows().get(ix) else {
            return div().into_any_element();
        };
        let turn = self.timeline.turn_of(ix);
        let running = turn.is_some() && turn == self.running.map(|running| running.turn);
        let phase = if running && ix + 1 == self.timeline.rows().len() {
            Phase::Live
        } else {
            Phase::Settled
        };
        match row {
            Row::Message(entry) => match &entries[*entry] {
                Entry::Notice(notice) => self.render_notice(*entry, notice, act, cx),
                message => {
                    let attachments = self.attachments.get(*entry).map_or(&[][..], Vec::as_slice);
                    let text = self.text(*entry);
                    render_message(message, *entry, text, attachments, phase, cx)
                }
            },
            Row::Work(run) => {
                let act = Rc::clone(act);
                let on_fold: FoldHandler = Rc::new(move |fold, window, cx| {
                    act(Action::Fold { row: ix, fold }, window, cx);
                });
                work::render(
                    WorkRow {
                        entries: &entries[run.clone()],
                        start: run.start,
                        steps: self.steps[ix],
                        details: &self.details[run.clone()],
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
        let Some(detail) = self.text(ix) else {
            return Alert::new(key, SharedString::from(notice.title().to_owned()))
                .with_variant(variant)
                .small()
                .into_any_element();
        };
        let open = self.details.get(ix).is_some_and(|shows| shows.is_open());
        let mut text = TextView::new(detail)
            .style(message_style().paragraph_gap(rems(NOTICE_PARAGRAPH_GAP)))
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

/// The previews of `entry`'s attachments, if it is a user message; an
/// image whose header doesn't read is left out.
fn previews(entry: &Entry) -> Vec<Preview> {
    match entry {
        Entry::User(message) => message
            .attachments()
            .iter()
            .filter_map(Preview::new)
            .collect(),
        _ => Vec::new(),
    }
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

/// Attachment `n` of user message `ix`, drawn as `card`. Wrapped so UI
/// tests can find it: Kit's Attachment does not register its ID with them.
fn attachment_cell(ix: usize, n: usize, preview: &Preview, card: Card) -> AnyElement {
    let id = SharedString::from(format!("user-{ix}-attachment-{n}"));
    div()
        .id(id.clone())
        .test_support()
        .aria_label(preview.label())
        .child(card.id((ElementId::from(id), "card")))
        .into_any_element()
}

/// The Markdown `entry` shows: a message's text, or a notice's detail.
fn markdown_text(entry: &Entry) -> Option<&str> {
    match entry {
        Entry::User(message) => Some(message.text()).filter(|text| !text.is_empty()),
        Entry::Agent(message) => Some(message.text()),
        Entry::Notice(notice) => notice.description(),
        Entry::Thought(_) | Entry::Tool(_) | Entry::Wake => None,
    }
}

/// Builds the row for a user message or a reply: entry `ix`, whose text
/// `text` holds.
/// `attachments` are a user message's. Tool calls and thoughts are drawn
/// by [`crate::work`].
fn render_message(
    entry: &Entry,
    ix: usize,
    text: Option<&Entity<TextViewState>>,
    attachments: &[Preview],
    phase: Phase,
    cx: &App,
) -> AnyElement {
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
                                .markdown_extensions(figures::extensions())
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
        Entry::Agent(_) => div()
            .id(SharedString::from(format!("reply-{ix}")))
            .test_support()
            .w_full()
            .children(text.map(|text| {
                TextView::new(text)
                    .style(message_style())
                    .markdown_extensions(figures::extensions())
                    .selectable(true)
                    .stream_fade(phase == Phase::Live)
            }))
            .into_any_element(),
        Entry::Wake => h_flex()
            .id(SharedString::from(format!("wake-{ix}")))
            .test_support()
            .gap_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(Lucide::Bell).xsmall())
            .child("rho continued on its own")
            .into_any_element(),
        // The timeline puts these in work rows, never message rows, and
        // the transcript draws notices itself.
        Entry::Thought(_) | Entry::Tool(_) | Entry::Notice(_) => div().into_any_element(),
    }
}
