//! The session's background work. What still runs is listed above the
//! composer: the processes, workflow runs and subagents its turns left
//! running, each with a button to stop it. Work leaves that list as it ends;
//! a button in the header opens what ended and how, as the daemon's turn
//! that tells the model can be missed or scroll away.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::{ObservedElement, TestSupportExt as _};
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    popover::Popover,
    v_flex, ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, Anchor, App, Div, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Window,
};
use rho_ui_core::{BackgroundId, BackgroundKind, BackgroundStatus, BackgroundWork};

/// Asks the daemon to stop one piece of work.
type StopHandler = Rc<dyn Fn(&BackgroundId, &mut Window, &mut App)>;

/// What a row shows of one piece of work.
struct Row {
    kind: BackgroundKind,
    title: SharedString,
    status: SharedString,
    /// Whether it ended badly, which the status also says in words.
    failed: bool,
}

impl Row {
    fn new(work: &BackgroundWork, stop: Stop) -> Self {
        Self {
            kind: work.kind(),
            title: work.title().to_owned().into(),
            status: status_label(work, stop).into(),
            failed: failed(work),
        }
    }

    /// Icon, title and status, labelled "title: status" for UI tests.
    fn render(&self, id: SharedString, cx: &App) -> ObservedElement<Stateful<Div>> {
        let muted = cx.theme().muted_foreground;
        let status_color: Hsla = if self.failed {
            cx.theme().danger
        } else {
            muted
        };
        h_flex()
            .id(id)
            .test_support()
            .aria_label(format!("{}: {}", self.title, self.status))
            .gap_2()
            .text_sm()
            .text_color(muted)
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
    items: Vec<(BackgroundId, Row, Stop)>,
    on_stop: StopHandler,
}

impl BackgroundList {
    /// `stopping` holds the work the user asked to stop. `None` when
    /// nothing runs.
    pub(crate) fn new(
        work: &[BackgroundWork],
        stopping: &[BackgroundId],
        on_stop: impl Fn(&BackgroundId, &mut Window, &mut App) + 'static,
    ) -> Option<Self> {
        let items: Vec<_> = work
            .iter()
            .filter(|work| work.status() == BackgroundStatus::Running)
            .map(|work| {
                let stop = match (work.can_stop(), stopping.contains(work.id())) {
                    (false, _) => Stop::Never,
                    (true, false) => Stop::Offered,
                    (true, true) => Stop::Asked,
                };
                (work.id().clone(), Row::new(work, stop), stop)
            })
            .collect();
        (!items.is_empty()).then(|| Self {
            items,
            on_stop: Rc::new(on_stop),
        })
    }
}

impl RenderOnce for BackgroundList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .id("background")
            .test_support()
            .w_full()
            .gap_1()
            .children(
                self.items
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (id, row, stop))| {
                        let on_stop = Rc::clone(&self.on_stop);
                        row.render(format!("background-{ix}").into(), cx)
                            .px_3()
                            .map(|this| match stop {
                                Stop::Offered | Stop::Asked => this.child(
                                    Button::new(SharedString::from(format!(
                                        "background-stop-{ix}"
                                    )))
                                    .ghost()
                                    .xsmall()
                                    .icon(Lucide::Square)
                                    .tooltip("Stop")
                                    .disabled(stop == Stop::Asked)
                                    .on_click(move |_, window, cx| on_stop(&id, window, cx)),
                                ),
                                // Keeps the status in the lane of the rows that can
                                // stop: the size of an xsmall icon button.
                                Stop::Never => this.child(div().flex_shrink_0().size_5()),
                            })
                    }),
            )
    }
}

/// A header button counting the session's ended background work, opening
/// the list of it, newest first, with how each piece ended.
#[derive(IntoElement)]
pub(crate) struct BackgroundHistory {
    rows: Rc<[Row]>,
}

impl BackgroundHistory {
    /// `None` when nothing ended yet.
    pub(crate) fn new(ended: &[BackgroundWork]) -> Option<Self> {
        if ended.is_empty() {
            return None;
        }
        let rows = ended
            .iter()
            .rev()
            .map(|work| Row::new(work, Stop::Never))
            .collect();
        Some(Self { rows })
    }
}

impl RenderOnce for BackgroundHistory {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let rows = self.rows;
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
                            .children(rows.iter().enumerate().map(|(ix, row)| {
                                row.render(format!("background-ended-{ix}").into(), cx)
                            })),
                    )
            })
    }
}

/// How the work is doing, in words: "Running", "Exited 0", "Stopped".
fn status_label(work: &BackgroundWork, stop: Stop) -> String {
    match (work.status(), work.exit_code()) {
        (BackgroundStatus::Running, _) if stop == Stop::Asked => "Stopping…".to_owned(),
        (BackgroundStatus::Running, _) => "Running".to_owned(),
        (BackgroundStatus::Completed | BackgroundStatus::Failed, Some(code)) => {
            format!("Exited {code}")
        }
        (BackgroundStatus::Completed, None) => "Done".to_owned(),
        (BackgroundStatus::Failed, None) => "Failed".to_owned(),
        (BackgroundStatus::Cancelled, _) => "Stopped".to_owned(),
    }
}

/// Whether the work ended badly: it failed, or its process exited nonzero.
fn failed(work: &BackgroundWork) -> bool {
    match work.status() {
        BackgroundStatus::Failed => true,
        BackgroundStatus::Completed => work.exit_code().is_some_and(|code| code != 0),
        BackgroundStatus::Running | BackgroundStatus::Cancelled => false,
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
