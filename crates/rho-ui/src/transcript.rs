//! The transcript: a session's entries as rows in a scroller. Messages and
//! notices are drawn here; runs of tool calls and thoughts by
//! [`crate::work`]. A finished turn's working folds behind one "Worked for"
//! line. This owns what the user opened and folded, and the turns' lengths.

use std::{ops::Range, rc::Rc, time::Duration};

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    alert::{Alert, AlertVariant},
    bubble::{Bubble, BubbleVariant},
    message::{Message, MessageAlignment, MessageContent},
    message_scroller::MessageScrollerState,
    text::TextView,
    v_flex, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, ElementId, Entity, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, Styled as _, Window,
};
use rho_ui_core::{Entry, NoticeLevel, Row, Timeline};

use crate::{
    time,
    turn_times::TurnTimes,
    work::{self, DiffView, Disclosure, Fold, FoldHandler, WorkRow},
};

/// What a click in the transcript asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Open or fold part of work row `row`.
    Fold { row: usize, fold: Fold },
    /// Open or fold a finished turn's working.
    Worked { turn: usize },
}

/// Carries out an [`Action`]; the session view owns the transcript.
pub(crate) type ActionHandler = Rc<dyn Fn(Action, &mut Window, &mut App)>;

/// The turn in progress, by its index among the timeline's turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Running {
    /// A turn a prompt starts. It may not have its prompt row yet: the row
    /// comes when the daemon echoes the prompt, after the turn began.
    Prompted(usize),
    /// A turn with no prompt, as the daemon starts one when a session's
    /// background work finishes. Its rows join this turn, the newest, which
    /// keeps the time it took itself.
    Joined(usize),
}

impl Running {
    fn turn(self) -> usize {
        match self {
            Self::Prompted(turn) | Self::Joined(turn) => turn,
        }
    }
}

/// A session's transcript state. Flat arrays indexed by row, entry or turn.
pub(crate) struct Transcript {
    scroller: Entity<MessageScrollerState>,
    timeline: Timeline,
    /// Per row: whether a work row lists its steps.
    steps: Vec<Disclosure>,
    /// Per entry: whether a step shows its details.
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
        self.steps
            .resize(self.timeline.rows().len(), Disclosure::Folded);
        self.details.resize(entries.len(), Disclosure::Folded);
        self.worked.resize(turns, Disclosure::Folded);
        self.took.resize(turns, None);
        let times = cx.global::<TurnTimes>();
        for turn in known..turns {
            self.took[turn] = self.prompt_id(turn, entries).and_then(|id| times.get(id));
        }
        // Rows came for a turn that began without a prompt row to open it:
        // they joined the newest turn, which unfolds to show them.
        let grew = synced.added > 0 || synced.grew.is_some();
        if let (Some(Running::Prompted(expected)), Some(newest)) =
            (self.running, turns.checked_sub(1))
        {
            if expected == turns && grew {
                self.running = Some(Running::Joined(newest));
                self.remeasure(self.timeline.turn_rows(newest), cx);
            }
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

    /// A turn began, here or in another client: it is the next one, though
    /// its prompt row comes only when the daemon echoes the prompt. The
    /// turns before it stay folded.
    pub(crate) fn expect_turn(&mut self) {
        self.running = Some(Running::Prompted(self.timeline.turns().len()));
    }

    /// The session was attached with a turn under way: it is the newest
    /// one, which unfolds.
    pub(crate) fn resume_turn(&mut self, cx: &mut App) {
        let newest = self.timeline.turns().len().saturating_sub(1);
        self.running = Some(Running::Prompted(newest));
        self.remeasure(self.timeline.turn_rows(newest), cx);
    }

    /// The running turn finished after `took`, and folds. Its time is saved
    /// for when the session replays. A prompt that never arrived leaves no
    /// turn to time, and a turn without a prompt none to time it under.
    pub(crate) fn end_turn(&mut self, took: Duration, entries: &[Entry], cx: &mut App) {
        let Some(running) = self.running.take() else {
            return;
        };
        let turn = running.turn();
        if turn >= self.took.len() {
            return;
        }
        if let Running::Prompted(_) = running {
            self.took[turn] = Some(took);
            if let Some(prompt) = self.prompt_id(turn, entries) {
                cx.global_mut::<TurnTimes>().record(prompt, took);
            }
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
        };
        self.remeasure(rows, cx);
    }

    /// How long `turn` took, if this view ran it.
    #[cfg(test)]
    pub(crate) fn took(&self, turn: usize) -> Option<Duration> {
        self.took.get(turn).copied().flatten()
    }

    fn is_finished(&self, turn: usize) -> bool {
        self.running.is_none_or(|running| turn < running.turn())
    }

    fn remeasure(&self, rows: Range<usize>, cx: &mut App) {
        self.scroller
            .update(cx, |scroller, cx| scroller.remeasure_items(rows, cx));
    }

    /// Row `ix`. A row inside a folded turn is empty, so it takes no space;
    /// the turn's first working row shows the "Worked for" line.
    pub(crate) fn render_row(
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
        let fold = turn
            .filter(|&turn| self.is_finished(turn))
            .and_then(|turn| Some((turn, self.timeline.working(turn)?)))
            .filter(|(_, working)| working.contains(&ix));
        if let Some((turn, working)) = &fold {
            if !self.worked[*turn].is_open() && ix != working.start {
                return div().into_any_element();
            }
        }
        let running = turn.is_some() && turn == self.running.map(Running::turn);
        let phase = if running && ix + 1 == self.timeline.rows().len() {
            Phase::Live
        } else {
            Phase::Settled
        };
        let content = || match row {
            Row::Message(entry) => render_message(&entries[*entry], *entry, phase),
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
        };
        let row = match fold {
            Some((turn, working)) if ix == working.start => {
                let open = self.worked[turn];
                let took = self.took[turn].map(|took| time::took(took).into());
                let act = Rc::clone(act);
                let line = work::worked_line(
                    turn,
                    took,
                    open,
                    move |window, cx| act(Action::Worked { turn }, window, cx),
                    cx,
                );
                v_flex()
                    .gap_3()
                    .child(line)
                    // A folded turn builds none of its rows.
                    .when(open.is_open(), |this| this.child(content()))
                    .into_any_element()
            }
            _ => content(),
        };
        // A readable measure on wide windows. The gap below is here, not in
        // the scroller's row style (which the session view zeroes), so rows in
        // a folded turn take no space.
        div()
            .w_full()
            .max_w(rems(48.))
            .mx_auto()
            .pb_3()
            .child(row)
            .into_any_element()
    }
}

/// Whether a row is still being written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The newest row of a running turn.
    Live,
    Settled,
}

/// Builds the row for a user message, a reply or a notice: entry `ix`.
/// Tool calls and thoughts are drawn by [`crate::work`].
fn render_message(entry: &Entry, ix: usize, phase: Phase) -> AnyElement {
    let key = ElementId::named_usize("row", ix);
    match entry {
        Entry::User(message) => Message::new()
            .id(key)
            .alignment(MessageAlignment::End)
            .content(
                MessageContent::new().bubble(
                    Bubble::new().with_variant(BubbleVariant::Muted).child(
                        div()
                            .id(SharedString::from(format!("user-{ix}")))
                            .test_support()
                            .child(
                                TextView::markdown(
                                    ElementId::named_usize("user-markdown", ix),
                                    message.text().to_owned(),
                                )
                                .selectable(true),
                            ),
                    ),
                ),
            )
            .into_any_element(),
        Entry::Agent(message) => div()
            .id(key)
            .w_full()
            .child(
                TextView::markdown(
                    ElementId::named_usize("markdown", ix),
                    message.text().to_owned(),
                )
                .selectable(true)
                .stream_fade(phase == Phase::Live),
            )
            .into_any_element(),
        Entry::Notice(notice) => {
            let variant = match notice.level() {
                NoticeLevel::Info => AlertVariant::Info,
                NoticeLevel::Warning => AlertVariant::Warning,
                NoticeLevel::Error => AlertVariant::Error,
            };
            let alert = match notice.description() {
                Some(description) => Alert::new(key, SharedString::from(description.to_owned()))
                    .title(notice.title().to_owned()),
                None => Alert::new(key, SharedString::from(notice.title().to_owned())),
            };
            alert.with_variant(variant).small().into_any_element()
        }
        // The timeline puts these in work rows, never message rows.
        Entry::Thought(_) | Entry::Tool(_) => div().into_any_element(),
    }
}
