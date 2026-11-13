//! The session's background work, listed above its composer: the processes,
//! workflow runs and subagents its turns left running, each with how it is
//! doing and a button to stop it. Work that ended shows how it ended until
//! the daemon has given the model its result and drops it from the list.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Icon, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, App, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window,
};
use rho_ui_core::{BackgroundId, BackgroundKind, BackgroundStatus, BackgroundWork};

/// Asks the daemon to stop one piece of work.
type StopHandler = Rc<dyn Fn(&BackgroundId, &mut Window, &mut App)>;

/// One row's snapshot.
struct Item {
    id: BackgroundId,
    kind: BackgroundKind,
    title: SharedString,
    status: SharedString,
    /// Whether it ended badly, which the status also says in words.
    failed: bool,
    stop: Stop,
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

/// The session's background work, oldest first within each kind, as the
/// daemon lists it.
#[derive(IntoElement)]
pub(crate) struct BackgroundList {
    items: Vec<Item>,
    on_stop: StopHandler,
}

impl BackgroundList {
    /// `stopping` holds the work the user asked to stop. `None` when there
    /// is no work to show.
    pub(crate) fn new(
        work: &[BackgroundWork],
        stopping: &[BackgroundId],
        on_stop: impl Fn(&BackgroundId, &mut Window, &mut App) + 'static,
    ) -> Option<Self> {
        if work.is_empty() {
            return None;
        }
        let items = work
            .iter()
            .map(|work| {
                let asked = stopping.contains(work.id());
                let stop = match (work.can_stop(), asked) {
                    (false, _) => Stop::Never,
                    (true, false) => Stop::Offered,
                    (true, true) => Stop::Asked,
                };
                Item {
                    id: work.id().clone(),
                    kind: work.kind(),
                    title: work.title().to_owned().into(),
                    status: status_label(work, stop).into(),
                    failed: failed(work),
                    stop,
                }
            })
            .collect();
        Some(Self {
            items,
            on_stop: Rc::new(on_stop),
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

impl RenderOnce for BackgroundList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        v_flex()
            .id("background")
            .test_support()
            .w_full()
            .gap_1()
            .children(self.items.into_iter().enumerate().map(|(ix, item)| {
                let status_color: Hsla = if item.failed { danger } else { muted };
                let on_stop = Rc::clone(&self.on_stop);
                let id = item.id.clone();
                h_flex()
                    .id(SharedString::from(format!("background-{ix}")))
                    .test_support()
                    .aria_label(format!("{}: {}", item.title, item.status))
                    .gap_2()
                    .px_3()
                    .text_sm()
                    .text_color(muted)
                    .child(kind_icon(item.kind).xsmall())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .child(item.title),
                    )
                    .child(div().text_xs().text_color(status_color).child(item.status))
                    .map(|this| match item.stop {
                        Stop::Offered | Stop::Asked => this.child(
                            Button::new(SharedString::from(format!("background-stop-{ix}")))
                                .ghost()
                                .xsmall()
                                .icon(Lucide::Square)
                                .tooltip("Stop")
                                .disabled(item.stop == Stop::Asked)
                                .on_click(move |_, window, cx| on_stop(&id, window, cx)),
                        ),
                        // Keeps the status in the lane of the rows that can
                        // stop: the size of an xsmall icon button.
                        Stop::Never => this.child(div().flex_shrink_0().size_5()),
                    })
            }))
    }
}
