//! The session's background work. What still runs is listed above the
//! composer: the processes, workflow runs and subagents its turns left
//! running, each with a button to stop it. Work leaves that list as it ends;
//! a button in the header opens what ended and how, as the daemon's turn
//! that tells the model can be missed or scroll away. The right panel's
//! Background surface ([`BackgroundSurface`]) shows both lists at once. A
//! subagent's row, in any of them, opens the subagent's own session once it
//! has one.
//!
//! A process's row says how long it has run, and how long it has been
//! quiet, as the daemon reads them now ([`processes`]), and opens its
//! output in the Background surface ([`output`]), in place of the lists,
//! as Files opens a file in place of its tree.
//!
//! The session's background list says only whether a subagent or workflow
//! run is running or how it ended. The daemon's job list ([`Run`]) adds
//! where a queued run waits and why a failed one failed, which every row
//! shows, and the runs a run started, such as a workflow's agent nodes,
//! which the surface nests under it.

mod output;
pub(crate) mod processes;

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::{ObservedElement, TestSupportExt as _};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyDescription, EmptyHeader},
    h_flex,
    popover::Popover,
    v_flex, ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, Anchor, AnyElement, App, Div, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Window,
};
use rho_ui_core::{
    BackgroundId, BackgroundKind, BackgroundStatus, BackgroundWork, ProcessState, Run, RunStatus,
    SessionId, SessionProcess,
};

use self::output::timing;
pub(crate) use self::output::ProcessOutput;
use self::processes::Processes;

/// Asks the daemon to stop one piece of work.
pub(crate) type StopHandler = Rc<dyn Fn(&BackgroundId, &mut Window, &mut App)>;

/// What a row opens when clicked.
#[derive(Clone, Debug)]
pub(crate) enum Opens {
    /// A subagent's session, with the subagent's task.
    Session {
        session: SessionId,
        title: SharedString,
    },
    /// A process's output, by the process's ID.
    Output(String),
}

/// Opens what a row opens.
pub(crate) type OpenHandler = Rc<dyn Fn(&Opens, &mut Window, &mut App)>;

/// What a row shows of one piece of work.
struct Row {
    kind: BackgroundKind,
    title: SharedString,
    status: SharedString,
    /// Whether it ended badly, which the status also says in words.
    failed: bool,
    /// Why it failed, when the daemon's job list says.
    error: Option<SharedString>,
    /// What a click on the row opens: a subagent's session, or a
    /// process's output.
    opens: Option<Opens>,
    /// The runs it started, which a nesting list shows under it.
    runs: Vec<Run>,
}

impl Row {
    /// `work` as its session lists it, with what the job list says of it,
    /// `run`, if it is a run, and what the daemon read of it, `process`, if
    /// it is a process. A process's row opens its output where `output`
    /// says it can.
    fn new(
        work: &BackgroundWork,
        run: Option<&Run>,
        process: Option<&SessionProcess>,
        output: Output,
        stop: Stop,
    ) -> Self {
        let title: SharedString = work.title().to_owned().into();
        let opens = match (work.session(), work.id().key()) {
            (Some(session), _) => Some(Opens::Session {
                session,
                title: title.clone(),
            }),
            (None, Some((BackgroundKind::Process, id))) if output == Output::Opens => {
                Some(Opens::Output(id.to_owned()))
            }
            _ => None,
        };
        let status = status_label(work, process, stop);
        let status = match process.and_then(timing) {
            Some(timing) => format!("{status} · {timing}"),
            None => status,
        };
        let row = Self {
            kind: work.kind(),
            title,
            status: status.into(),
            failed: failed(work),
            error: None,
            opens,
            runs: run.map_or_else(Vec::new, |run| run.runs().to_vec()),
        };
        // The job list says more while a run waits for a slot, and how a
        // run ended after the session forgot it.
        let says_more = |status: &&RunStatus| match work.status() {
            BackgroundStatus::Running => {
                stop != Stop::Asked && matches!(status, RunStatus::Queued { .. })
            }
            BackgroundStatus::Ended => true,
            _ => false,
        };
        match run.map(Run::status).filter(says_more) {
            Some(status) => row.with_status(status),
            // Where the session's list says it failed, the job list may
            // say why.
            None => Self {
                error: run
                    .filter(|_| row.failed)
                    .and_then(|run| error(run.status())),
                ..row
            },
        }
    }

    /// A run only the job list names, as a workflow's agent node.
    fn of_run(run: &Run) -> Self {
        let title = run.title().unwrap_or(match run.kind() {
            BackgroundKind::Workflow => "Workflow",
            _ => "Subagent",
        });
        Self {
            kind: run.kind(),
            title: title.to_owned().into(),
            status: SharedString::default(),
            failed: false,
            error: None,
            opens: run.session().map(|session| Opens::Session {
                session,
                title: title.to_owned().into(),
            }),
            runs: run.runs().to_vec(),
        }
        .with_status(run.status())
    }

    /// The row as the job list's `status` says.
    fn with_status(self, status: &RunStatus) -> Self {
        Self {
            status: run_label(status).into(),
            failed: matches!(status, RunStatus::Failed { .. }),
            error: error(status),
            ..self
        }
    }

    /// The row named `id`, with `trailing` at its end, and under it why it
    /// failed; with `nest`, then the rows of the runs it started, named
    /// `id-run-N`, each `depth` steps further in.
    fn render_tree(
        &self,
        id: &str,
        depth: u16,
        trailing: Option<AnyElement>,
        nest: Nest,
        open: &OpenHandler,
        cx: &App,
    ) -> Vec<AnyElement> {
        let indent = rems(0.75 + 1.25 * f32::from(depth));
        let mut rows = vec![self
            .render(id.to_owned().into(), open, cx)
            .pl(indent)
            .pr_3()
            .children(trailing)
            .into_any_element()];
        if let Some(error) = self.error.clone() {
            rows.push(
                div()
                    .id(SharedString::from(format!("{id}-error")))
                    .test_support()
                    .aria_label(error.clone())
                    // In line with the title, past the icon and the gap.
                    .ml(indent)
                    .pl(rems(1.25))
                    .pr_3()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(error)
                    .into_any_element(),
            );
        }
        if nest == Nest::Runs {
            for (ix, run) in self.runs.iter().enumerate() {
                let id = format!("{id}-run-{ix}");
                rows.extend(Self::of_run(run).render_tree(&id, depth + 1, None, nest, open, cx));
            }
        }
        rows
    }

    /// Icon, title and status, labelled "title: status" for UI tests. A
    /// row that opens something opens it with `open`.
    fn render(
        &self,
        id: SharedString,
        open: &OpenHandler,
        cx: &App,
    ) -> ObservedElement<Stateful<Div>> {
        let muted = cx.theme().muted_foreground;
        let status_color: Hsla = if self.failed {
            cx.theme().danger
        } else {
            muted
        };
        let hover = cx.theme().secondary;
        h_flex()
            .id(id)
            .test_support()
            .aria_label(format!("{}: {}", self.title, self.status))
            .gap_2()
            .rounded(cx.theme().radius)
            .text_sm()
            .text_color(muted)
            .when_some(self.opens.clone(), |this, opens| {
                let open = Rc::clone(open);
                this.cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(move |_, window, cx| open(&opens, window, cx))
            })
            .child(kind_icon(self.kind).xsmall())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(status_color)
                    .child(self.status.clone()),
            )
    }
}

/// Whether a row lists the runs its work started under it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Nest {
    Runs,
    None,
}

/// Whether a process's row opens its output: not where the output can't
/// show, as in a side chat, whose panel shows its session's surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Output {
    Opens,
    Never,
}

/// Whether a row offers to stop its work.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// It ended, or is a kind this client can't stop.
    Never,
    /// It runs.
    Offered,
    /// The stop was asked for; the work has not ended yet.
    Asked,
}

/// What the rows of a session's background work read besides the work
/// itself.
pub(crate) struct Sources<'a> {
    /// The session's runs, as the daemon's job list says.
    pub(crate) runs: &'a [Run],
    /// The session's processes, as the daemon last read them.
    pub(crate) processes: &'a Processes,
    /// Whether a process's row opens its output.
    pub(crate) output: Output,
}

impl Sources<'_> {
    /// The row of `work`, with what the job list and the daemon's read of
    /// its processes say of it.
    fn row(&self, work: &BackgroundWork, stop: Stop) -> Row {
        Row::new(
            work,
            run_of(self.runs, work),
            process_of(self.processes, work),
            self.output,
            stop,
        )
    }
}

/// The session's running background work, oldest first within each kind,
/// as the daemon lists it.
#[derive(IntoElement)]
pub(crate) struct BackgroundList {
    /// Each piece of work, with what the job list says of it if it is a
    /// run, and the daemon's read of it if it is a process.
    running: Vec<(BackgroundWork, Option<Run>, Option<SessionProcess>)>,
    /// Whether a process's row opens its output.
    output: Output,
    /// The work the user asked to stop, and how to ask; `None` where
    /// nothing can be stopped from here.
    stop: Option<(Vec<BackgroundId>, StopHandler)>,
    on_open: OpenHandler,
    /// The list's element ID, and the start of its rows'.
    ids: &'static str,
    nest: Nest,
}

impl BackgroundList {
    /// The running work in `work`, with what `sources` say of it, each row
    /// opening what it opens with `on_open`; `None` when nothing runs.
    /// Nothing offers to stop until [`Self::stoppable`].
    pub(crate) fn new(
        work: &[BackgroundWork],
        sources: &Sources<'_>,
        on_open: OpenHandler,
    ) -> Option<Self> {
        let running: Vec<_> = work
            .iter()
            .filter(|work| work.status() == BackgroundStatus::Running)
            .map(|work| {
                let run = run_of(sources.runs, work).cloned();
                (
                    work.clone(),
                    run,
                    process_of(sources.processes, work).cloned(),
                )
            })
            .collect();
        (!running.is_empty()).then_some(Self {
            running,
            output: sources.output,
            stop: None,
            on_open,
            ids: "background",
            nest: Nest::None,
        })
    }

    /// Names the list `ids` and its rows `ids-N`, in place of
    /// `background`, so a second list on screen has IDs of its own, and
    /// lists each row's runs under it.
    fn nested(mut self, ids: &'static str) -> Self {
        self.ids = ids;
        self.nest = Nest::Runs;
        self
    }

    /// Offers to stop each piece of work that can be, with `on_stop`.
    /// `stopping` holds the work the user asked to stop.
    pub(crate) fn stoppable(
        mut self,
        stopping: &[BackgroundId],
        on_stop: impl Fn(&BackgroundId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.stop = Some((stopping.to_vec(), Rc::new(on_stop)));
        self
    }
}

impl RenderOnce for BackgroundList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (stop, on_open, ids) = (self.stop, self.on_open, self.ids);
        let mut rows = Vec::new();
        for (ix, (work, run, process)) in self.running.iter().enumerate() {
            let offer = stop.as_ref().filter(|_| work.can_stop());
            let state = match offer {
                None => Stop::Never,
                Some((asked, _)) if asked.contains(work.id()) => Stop::Asked,
                Some(_) => Stop::Offered,
            };
            let trailing = match offer {
                Some((_, on_stop)) => {
                    let (on_stop, id) = (Rc::clone(on_stop), work.id().clone());
                    Button::new(SharedString::from(format!("{ids}-stop-{ix}")))
                        .ghost()
                        .xsmall()
                        .icon(Lucide::Square)
                        .tooltip("Stop")
                        .disabled(state == Stop::Asked)
                        .on_click(move |_, window, cx| {
                            // The row under it opens what it shows.
                            cx.stop_propagation();
                            on_stop(&id, window, cx);
                        })
                        .into_any_element()
                }
                // Keeps the status in the lane of the rows that can stop:
                // the size of an xsmall icon button.
                None => div().flex_shrink_0().size_5().into_any_element(),
            };
            let row = Row::new(work, run.as_ref(), process.as_ref(), self.output, state);
            rows.extend(row.render_tree(
                &format!("{ids}-{ix}"),
                0,
                Some(trailing),
                self.nest,
                &on_open,
                cx,
            ));
        }
        v_flex()
            .id(ids)
            .test_support()
            .w_full()
            .gap_1()
            .children(rows)
    }
}

/// A header button counting the session's ended background work, opening
/// the list of it, newest first, with how each piece ended.
#[derive(IntoElement)]
pub(crate) struct BackgroundHistory {
    rows: Rc<[Row]>,
    on_open: OpenHandler,
}

impl BackgroundHistory {
    /// `ended` with what `sources` say of it; each row opens what it opens
    /// with `on_open`. `None` when nothing ended yet.
    pub(crate) fn new(
        ended: &[BackgroundWork],
        sources: &Sources<'_>,
        on_open: OpenHandler,
    ) -> Option<Self> {
        if ended.is_empty() {
            return None;
        }
        Some(Self {
            rows: ended_rows(ended, sources),
            on_open,
        })
    }
}

/// The rows of `ended` work, newest first, with what `sources` say of it.
fn ended_rows(ended: &[BackgroundWork], sources: &Sources<'_>) -> Rc<[Row]> {
    ended
        .iter()
        .rev()
        .map(|work| sources.row(work, Stop::Never))
        .collect()
}

/// What the job list says of `work`, if it is one of `runs`.
fn run_of<'a>(runs: &'a [Run], work: &BackgroundWork) -> Option<&'a Run> {
    runs.iter().find(|run| run.id() == work.id())
}

/// What the daemon last read of `work`, if it is a process it read.
fn process_of<'a>(processes: &'a Processes, work: &BackgroundWork) -> Option<&'a SessionProcess> {
    match work.id().key() {
        Some((BackgroundKind::Process, id)) => processes.process(id),
        _ => None,
    }
}

impl RenderOnce for BackgroundHistory {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (rows, on_open) = (self.rows, self.on_open);
        Popover::new("background-history")
            .anchor(Anchor::TopRight)
            .trigger(
                Button::new("background-history-button")
                    .ghost()
                    .xsmall()
                    .icon(Lucide::ListChecks)
                    .label(rows.len().to_string())
                    .tooltip("Ended background work"),
            )
            .content(move |_, _, cx| {
                v_flex()
                    .w(rems(28.))
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .child("Ended background work"),
                    )
                    .child(
                        v_flex()
                            .id("background-history-list")
                            .max_h(rems(20.))
                            .overflow_y_scroll()
                            .gap_1()
                            .children(rows.iter().enumerate().flat_map(|(ix, row)| {
                                let id = format!("background-ended-{ix}");
                                row.render_tree(&id, 0, None, Nest::None, &on_open, cx)
                            })),
                    )
            })
    }
}

/// The session's background work as a surface of the right panel: what
/// runs, each with a button to stop it once [`Self::stoppable`], then what
/// ended, newest first, with how it ended. A process's output shows in its
/// place ([`ProcessOutput`]).
#[derive(IntoElement)]
pub(crate) struct BackgroundSurface {
    running: Option<BackgroundList>,
    ended: Rc<[Row]>,
    on_open: OpenHandler,
}

impl BackgroundSurface {
    /// `work` is the daemon's list, of which the running work shows, and
    /// `ended` the work seen to end, oldest first, each with what
    /// `sources` say of it. Each row opens what it opens with `on_open`.
    pub(crate) fn new(
        work: &[BackgroundWork],
        ended: &[BackgroundWork],
        sources: &Sources<'_>,
        on_open: OpenHandler,
    ) -> Self {
        Self {
            running: BackgroundList::new(work, sources, Rc::clone(&on_open))
                .map(|list| list.nested("panel-background")),
            ended: ended_rows(ended, sources),
            on_open,
        }
    }

    /// Offers to stop the running work; see [`BackgroundList::stoppable`].
    pub(crate) fn stoppable(
        mut self,
        stopping: &[BackgroundId],
        on_stop: impl Fn(&BackgroundId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.running = self.running.map(|list| list.stoppable(stopping, on_stop));
        self
    }
}

impl RenderOnce for BackgroundSurface {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.running.is_none() && self.ended.is_empty() {
            return Empty::new()
                .h_full()
                .header(EmptyHeader::new().description(
                    EmptyDescription::new().child("No background work in this session yet."),
                ))
                .into_any_element();
        }
        let muted = cx.theme().muted_foreground;
        let heading = move |text: &'static str| {
            div()
                .px_3()
                .text_xs()
                .font_semibold()
                .text_color(muted)
                .child(text)
        };
        let (ended, on_open) = (self.ended, self.on_open);
        v_flex()
            .id("panel-background-surface")
            .size_full()
            .overflow_y_scroll()
            .py_3()
            .gap_4()
            .when_some(self.running, |this, running| {
                this.child(v_flex().gap_2().child(heading("Running")).child(running))
            })
            .when(!ended.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_2()
                        .child(heading("Ended"))
                        .child(v_flex().gap_1().children(ended.iter().enumerate().flat_map(
                            |(ix, row)| {
                                let id = format!("panel-ended-{ix}");
                                row.render_tree(&id, 0, None, Nest::Runs, &on_open, cx)
                            },
                        ))),
                )
            })
            .into_any_element()
    }
}

/// How the work is doing, in words: "Running", "Exited 0", "Stopped".
/// A process that failed says how, as the daemon's read of it, `process`,
/// says.
fn status_label(work: &BackgroundWork, process: Option<&SessionProcess>, stop: Stop) -> String {
    let failed_as = process.and_then(|process| match process.state {
        ProcessState::TimedOut => Some("Timed out"),
        ProcessState::FailedToStart => Some("Failed to start"),
        _ => None,
    });
    if let (BackgroundStatus::Failed, Some(word)) = (work.status(), failed_as) {
        return word.to_owned();
    }
    match (work.status(), work.exit_code()) {
        (BackgroundStatus::Running, _) if stop == Stop::Asked => "Stopping…".to_owned(),
        (BackgroundStatus::Running, _) => "Running".to_owned(),
        (BackgroundStatus::Completed | BackgroundStatus::Failed, Some(code)) => {
            format!("Exited {code}")
        }
        (BackgroundStatus::Completed, None) => "Done".to_owned(),
        (BackgroundStatus::Failed, None) => "Failed".to_owned(),
        (BackgroundStatus::Cancelled, _) => "Stopped".to_owned(),
        (BackgroundStatus::Ended, _) => "Ended".to_owned(),
    }
}

/// How a run is doing, in words, from the job list.
fn run_label(status: &RunStatus) -> String {
    match status {
        RunStatus::Queued { position: Some(1) } => "Queued · next up".to_owned(),
        RunStatus::Queued {
            position: Some(position),
        } => format!("Queued · {} ahead", position.saturating_sub(1)),
        RunStatus::Queued { position: None } => "Queued".to_owned(),
        RunStatus::Running => "Running".to_owned(),
        RunStatus::Completed => "Done".to_owned(),
        RunStatus::Failed { .. } => "Failed".to_owned(),
        RunStatus::Cancelled => "Stopped".to_owned(),
    }
}

/// Why a run failed, if the job list says it did and why.
fn error(status: &RunStatus) -> Option<SharedString> {
    match status {
        RunStatus::Failed { error: Some(error) } => Some(error.clone().into()),
        _ => None,
    }
}

/// Whether the work ended badly: it failed, or its process exited nonzero.
fn failed(work: &BackgroundWork) -> bool {
    match work.status() {
        BackgroundStatus::Failed => true,
        BackgroundStatus::Completed => work.exit_code().is_some_and(|code| code != 0),
        BackgroundStatus::Running | BackgroundStatus::Cancelled | BackgroundStatus::Ended => false,
    }
}

fn kind_icon(kind: BackgroundKind) -> Icon {
    match kind {
        BackgroundKind::Process => Icon::new(Lucide::SquareTerminal),
        BackgroundKind::Workflow => Icon::new(Lucide::Workflow),
        BackgroundKind::Subagent => Icon::new(Lucide::Bot),
        BackgroundKind::Other => Icon::new(Lucide::Cog),
    }
}
