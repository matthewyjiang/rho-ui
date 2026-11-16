//! The session's background work. What still runs is listed above the
//! composer: the processes, workflow runs and subagents its turns left
//! running, each with a button to stop it. Work leaves that list as it ends;
//! a button in the header opens what ended and how, as the daemon's turn
//! that tells the model can be missed or scroll away. The right panel's
//! Background surface ([`BackgroundSurface`]) shows both lists at once. A
//! subagent's row, in any of them, opens the subagent's own session once it
//! has one.
//!
//! The session's background list says only whether a subagent or workflow
//! run is running or how it ended. The daemon's job list ([`Run`]) adds
//! where a queued run waits and why a failed one failed, which every row
//! shows, and the runs a run started, such as a workflow's agent nodes,
//! which the surface nests under it.

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
    BackgroundId, BackgroundKind, BackgroundStatus, BackgroundWork, Run, RunStatus, SessionId,
};

/// Asks the daemon to stop one piece of work.
type StopHandler = Rc<dyn Fn(&BackgroundId, &mut Window, &mut App)>;

/// Opens a subagent's session, given its ID and the subagent's task.
pub(crate) type OpenHandler = Rc<dyn Fn(&SessionId, &SharedString, &mut Window, &mut App)>;

/// What a row shows of one piece of work.
struct Row {
    kind: BackgroundKind,
    title: SharedString,
    status: SharedString,
    /// Whether it ended badly, which the status also says in words.
    failed: bool,
    /// Why it failed, when the daemon's job list says.
    error: Option<SharedString>,
    /// A subagent's session, which a click on the row opens.
    session: Option<SessionId>,
    /// The runs it started, which only the surface shows.
    runs: Vec<Row>,
}

impl Row {
    /// `work` as its session lists it, with what the job list says of it,
    /// `run`, if it is a run.
    fn new(work: &BackgroundWork, run: Option<&Run>, stop: Stop) -> Self {
        let outcome = run.map(Run::status);
        let failed = match work.status() {
            // The job list keeps how a run ended after the session forgot.
            BackgroundStatus::Ended => matches!(outcome, Some(RunStatus::Failed { .. })),
            _ => failed(work),
        };
        Self {
            kind: work.kind(),
            title: work.title().to_owned().into(),
            status: status_label(work, outcome, stop).into(),
            failed,
            error: failed.then(|| error(outcome)).flatten(),
            session: work.session(),
            runs: run.map(Self::runs).unwrap_or_default(),
        }
    }

    /// A run only the job list names, as a workflow's agent node.
    fn of_run(run: &Run) -> Self {
        let status = run.status();
        let failed = matches!(status, RunStatus::Failed { .. });
        let title = run.title().unwrap_or(match run.kind() {
            BackgroundKind::Workflow => "Workflow",
            _ => "Subagent",
        });
        Self {
            kind: run.kind(),
            title: title.to_owned().into(),
            status: run_label(status).into(),
            failed,
            error: error(Some(status)),
            session: run.session(),
            runs: Self::runs(run),
        }
    }

    /// The rows of the runs `run` started.
    fn runs(run: &Run) -> Vec<Self> {
        run.runs().iter().map(Self::of_run).collect()
    }

    /// Icon, title and status, labelled "title: status" for UI tests. A
    /// subagent's row opens its session with `open`.
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
            .when_some(self.session.clone(), |this, session| {
                let (open, title) = (Rc::clone(open), self.title.clone());
                this.cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(move |_, window, cx| open(&session, &title, window, cx))
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

impl Row {
    /// Why the row's work failed, under its title, if the daemon said.
    fn render_error(&self, id: &str, cx: &App) -> Option<ObservedElement<Stateful<Div>>> {
        let error = self.error.clone()?;
        Some(
            div()
                .id(SharedString::from(format!("{id}-error")))
                .test_support()
                .aria_label(error.clone())
                // In line with the title, past the icon and the gap.
                .pl(rems(1.25))
                .text_xs()
                .text_color(cx.theme().danger)
                .child(error),
        )
    }

    /// The rows of the runs this one started, each indented a step further
    /// than the one that started it, named `id-run-N`.
    fn render_runs(&self, id: &str, depth: f32, open: &OpenHandler, cx: &App) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        for (ix, run) in self.runs.iter().enumerate() {
            let id = format!("{id}-run-{ix}");
            let indent = rems(0.75 + 1.25 * depth);
            rows.push(
                run.render(id.clone().into(), open, cx)
                    .pl(indent)
                    .pr_3()
                    .into_any_element(),
            );
            if let Some(error) = run.render_error(&id, cx) {
                rows.push(error.ml(indent).pr_3().into_any_element());
            }
            rows.extend(run.render_runs(&id, depth + 1., open, cx));
        }
        rows
    }
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

/// The session's running background work, oldest first within each kind,
/// as the daemon lists it.
#[derive(IntoElement)]
pub(crate) struct BackgroundList {
    /// Each piece of work, with what the job list says of it if it is a run.
    running: Vec<(BackgroundWork, Option<Run>)>,
    /// The work the user asked to stop, and how to ask; `None` where
    /// nothing can be stopped from here.
    stop: Option<(Vec<BackgroundId>, StopHandler)>,
    on_open: OpenHandler,
    /// The list's element ID, and the start of its rows'.
    ids: &'static str,
    /// Whether each row lists the runs it started under it.
    nested: bool,
}

impl BackgroundList {
    /// The running work in `work`, with what `runs`, the session's runs as
    /// the job list says, adds, each subagent opening its session with
    /// `on_open`; `None` when nothing runs. Nothing offers to stop until
    /// [`Self::stoppable`].
    pub(crate) fn new(work: &[BackgroundWork], runs: &[Run], on_open: OpenHandler) -> Option<Self> {
        let running: Vec<_> = work
            .iter()
            .filter(|work| work.status() == BackgroundStatus::Running)
            .map(|work| (work.clone(), run_of(runs, work).cloned()))
            .collect();
        (!running.is_empty()).then_some(Self {
            running,
            stop: None,
            on_open,
            ids: "background",
            nested: false,
        })
    }

    /// Names the list `ids` and its rows `ids-N`, in place of
    /// `background`, so a second list on screen has IDs of its own, and
    /// lists each row's runs under it.
    fn nested(mut self, ids: &'static str) -> Self {
        self.ids = ids;
        self.nested = true;
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
        for (ix, (work, run)) in self.running.iter().enumerate() {
            let offer = stop.as_ref().filter(|_| work.can_stop());
            let state = match offer {
                None => Stop::Never,
                Some((asked, _)) if asked.contains(work.id()) => Stop::Asked,
                Some(_) => Stop::Offered,
            };
            let row = Row::new(work, run.as_ref(), state);
            let id = format!("{ids}-{ix}");
            rows.push(
                row.render(id.clone().into(), &on_open, cx)
                    .px_3()
                    .map(|this| match offer {
                        Some((_, on_stop)) => {
                            let (on_stop, id) = (Rc::clone(on_stop), work.id().clone());
                            this.child(
                                Button::new(SharedString::from(format!("{ids}-stop-{ix}")))
                                    .ghost()
                                    .xsmall()
                                    .icon(Lucide::Square)
                                    .tooltip("Stop")
                                    .disabled(state == Stop::Asked)
                                    .on_click(move |_, window, cx| on_stop(&id, window, cx)),
                            )
                        }
                        // Keeps the status in the lane of the rows that can
                        // stop: the size of an xsmall icon button.
                        None => this.child(div().flex_shrink_0().size_5()),
                    })
                    .into_any_element(),
            );
            if self.nested {
                rows.extend(row.render_runs(&id, 1., &on_open, cx));
            }
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
    /// `runs` is the session's runs as the job list says. Each subagent
    /// opens its session with `on_open`. `None` when nothing ended yet.
    pub(crate) fn new(
        ended: &[BackgroundWork],
        runs: &[Run],
        on_open: OpenHandler,
    ) -> Option<Self> {
        if ended.is_empty() {
            return None;
        }
        Some(Self {
            rows: ended_rows(ended, runs),
            on_open,
        })
    }
}

/// The rows of `ended` work, newest first, with what `runs` says of it.
fn ended_rows(ended: &[BackgroundWork], runs: &[Run]) -> Rc<[Row]> {
    ended
        .iter()
        .rev()
        .map(|work| Row::new(work, run_of(runs, work), Stop::Never))
        .collect()
}

/// What the job list says of `work`, if it is one of `runs`. A workflow
/// run that was driven again has a job for each drive; the last is the
/// one under way.
fn run_of<'a>(runs: &'a [Run], work: &BackgroundWork) -> Option<&'a Run> {
    runs.iter().rev().find(|run| run.id() == work.id())
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
                                let error = row.render_error(&id, cx);
                                [
                                    Some(row.render(id.into(), &on_open, cx).into_any_element()),
                                    error.map(IntoElement::into_any_element),
                                ]
                                .into_iter()
                                .flatten()
                            })),
                    )
            })
    }
}

/// The session's background work as a surface of the right panel: what
/// runs, each with a button to stop it once [`Self::stoppable`], then what
/// ended, newest first, with how it ended.
#[derive(IntoElement)]
pub(crate) struct BackgroundSurface {
    running: Option<BackgroundList>,
    ended: Rc<[Row]>,
    on_open: OpenHandler,
}

impl BackgroundSurface {
    /// `work` is the daemon's list, of which the running work shows,
    /// `ended` the work seen to end, oldest first, and `runs` the session's
    /// runs as the job list says. Each subagent opens its session with
    /// `on_open`.
    pub(crate) fn new(
        work: &[BackgroundWork],
        ended: &[BackgroundWork],
        runs: &[Run],
        on_open: OpenHandler,
    ) -> Self {
        Self {
            running: BackgroundList::new(work, runs, Rc::clone(&on_open))
                .map(|list| list.nested("panel-background")),
            ended: ended_rows(ended, runs),
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
                                let mut rows = vec![row
                                    .render(id.clone().into(), &on_open, cx)
                                    .px_3()
                                    .into_any_element()];
                                rows.extend(
                                    row.render_error(&id, cx)
                                        .map(|error| error.ml_3().pr_3().into_any_element()),
                                );
                                rows.extend(row.render_runs(&id, 1., &on_open, cx));
                                rows
                            },
                        ))),
                )
            })
            .into_any_element()
    }
}

/// How the work is doing, in words: "Running", "Exited 0", "Stopped".
/// `outcome` is what the job list says of a run, which tells where a
/// queued one waits, and how one ended that the session didn't see end.
fn status_label(work: &BackgroundWork, outcome: Option<&RunStatus>, stop: Stop) -> String {
    match (work.status(), work.exit_code(), outcome) {
        (BackgroundStatus::Running, _, _) if stop == Stop::Asked => "Stopping…".to_owned(),
        (BackgroundStatus::Running, _, Some(queued @ RunStatus::Queued { .. })) => {
            run_label(queued)
        }
        (BackgroundStatus::Running, _, _) => "Running".to_owned(),
        (BackgroundStatus::Completed | BackgroundStatus::Failed, Some(code), _) => {
            format!("Exited {code}")
        }
        (BackgroundStatus::Completed, None, _) => "Done".to_owned(),
        (BackgroundStatus::Failed, None, _) => "Failed".to_owned(),
        (BackgroundStatus::Cancelled, _, _) => "Stopped".to_owned(),
        (BackgroundStatus::Ended, _, Some(outcome)) => run_label(outcome),
        (BackgroundStatus::Ended, _, None) => "Ended".to_owned(),
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
fn error(outcome: Option<&RunStatus>) -> Option<SharedString> {
    match outcome {
        Some(RunStatus::Failed { error: Some(error) }) => Some(error.clone().into()),
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
