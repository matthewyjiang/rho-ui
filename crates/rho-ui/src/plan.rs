//! The agent's checklist, above the composer while any step of it is left:
//! "Plan 2 of 5", then each step, done ones struck through. The daemon
//! sends the whole list each time it changes.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    h_flex, scroll::ScrollableElement as _, v_flex, ActiveTheme as _, Icon, Sizable as _,
    StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window,
};
use rho_ui_core::{PlanItem, PlanStatus};

/// The most of the list that shows before it scrolls, in rems: about
/// seven steps, so a long plan leaves the transcript most of the window.
const MAX_HEIGHT_REMS: f32 = 10.;

/// The checklist, drawn from the session's [`PlanItem`]s.
#[derive(IntoElement)]
pub(crate) struct PlanList {
    steps: Vec<(SharedString, PlanStatus)>,
}

impl PlanList {
    /// `None` once every step is done, or before there is a plan.
    pub(crate) fn new(plan: &[PlanItem]) -> Option<Self> {
        plan.iter()
            .any(|step| step.status() != PlanStatus::Done)
            .then(|| Self {
                steps: plan
                    .iter()
                    .map(|step| (step.text().to_owned().into(), step.status()))
                    .collect(),
            })
    }
}

impl RenderOnce for PlanList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, foreground) = (theme.muted_foreground, theme.foreground);
        let done = self
            .steps
            .iter()
            .filter(|(_, status)| *status == PlanStatus::Done)
            .count();
        let heading = format!("Plan {done} of {}", self.steps.len());
        v_flex()
            .id("plan")
            .test_support()
            .aria_label(heading.clone())
            .w_full()
            .gap_1()
            .px_3()
            .text_sm()
            .text_color(muted)
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(Lucide::ListChecks).xsmall())
                    .child(heading),
            )
            .child(
                v_flex()
                    .id("plan-steps")
                    .gap_0p5()
                    .max_h(rems(MAX_HEIGHT_REMS))
                    .overflow_y_scrollbar()
                    .children(
                        self.steps
                            .into_iter()
                            .enumerate()
                            .map(|(ix, (text, status))| {
                                let icon = match status {
                                    PlanStatus::Done => Lucide::CircleCheck,
                                    PlanStatus::InProgress => Lucide::CircleDot,
                                    PlanStatus::Pending => Lucide::Circle,
                                };
                                h_flex()
                                    .id(SharedString::from(format!("plan-{ix}")))
                                    .test_support()
                                    .aria_label(text.clone())
                                    .gap_2()
                                    .child(Icon::new(icon).xsmall().flex_shrink_0())
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .map(|this| match status {
                                                PlanStatus::Done => this.line_through(),
                                                PlanStatus::InProgress => {
                                                    this.text_color(foreground).font_medium()
                                                }
                                                PlanStatus::Pending => this,
                                            })
                                            .child(text),
                                    )
                            }),
                    ),
            )
    }
}
