//! The session's background work. What still runs is listed above the
//! composer: the processes, workflow runs and subagents its turns left
//! running, each with a button to stop it. Work leaves that list as it ends;
//! a button in the header opens what ended and how, as the daemon's turn
//! that tells the model can be missed or scroll away. A subagent's row, in
//! either list, opens the subagent's own session once it has one.

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
use rho_ui_core::{BackgroundId, BackgroundKind, BackgroundStatus, BackgroundWork, SessionId};

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
    /// A subagent's session, which a click on the row opens.
    session: Option<SessionId>,
}

impl Row {
    fn new(work: &BackgroundWork, stop: Stop) -> Self {
        Self {
            kind: work.kind(),
            title: work.title().to_owned().into(),
            status: status_label(work, stop).into(),
            failed: failed(work),
            session: work.session(),
        }
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
    running: Vec<BackgroundWork>,
    /// The work the user asked to stop, and how to ask; `None` where
    /// nothing can be stopped from here.
    stop: Option<(Vec<BackgroundId>, StopHandler)>,
    on_open: OpenHandler,
}

impl BackgroundList {
    /// The running work in `work`, each subagent opening its session with
    /// `on_open`; `None` when nothing runs. Nothing offers to stop until
    /// [`Self::stoppable`].
    pub(crate) fn new(work: &[BackgroundWork], on_open: OpenHandler) -> Option<Self> {
        let running: Vec<_> = work
            .iter()
            .filter(|work| work.status() == BackgroundStatus::Running)
            .cloned()
            .collect();
        (!running.is_empty()).then_some(Self {
            running,
            stop: None,
            on_open,
        })
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
        let (stop, on_open) = (self.stop, self.on_open);
        v_flex()
            .id("background")
            .test_support()
            .w_full()
            .gap_1()
            .children(self.running.iter().enumerate().map(|(ix, work)| {
                let offer = stop.as_ref().filter(|_| work.can_stop());
                let state = match offer {
                    None => Stop::Never,
                    Some((asked, _)) if asked.contains(work.id()) => Stop::Asked,
                    Some(_) => Stop::Offered,
                };
                let row = Row::new(work, state);
                row.render(format!("background-{ix}").into(), &on_open, cx)
                    .px_3()
                    .map(|this| match offer {
                        Some((_, on_stop)) => {
                            let (on_stop, id) = (Rc::clone(on_stop), work.id().clone());
                            this.child(
                                Button::new(SharedString::from(format!("background-stop-{ix}")))
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
            }))
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
    /// Each subagent opens its session with `on_open`. `None` when
    /// nothing ended yet.
    pub(crate) fn new(ended: &[BackgroundWork], on_open: OpenHandler) -> Option<Self> {
        if ended.is_empty() {
            return None;
        }
        let rows = ended
            .iter()
            .rev()
            .map(|work| Row::new(work, Stop::Never))
            .collect();
        Some(Self { rows, on_open })
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
                            .children(rows.iter().enumerate().map(|(ix, row)| {
                                row.render(format!("background-ended-{ix}").into(), &on_open, cx)
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
        (BackgroundStatus::Ended, _) => "Ended".to_owned(),
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
