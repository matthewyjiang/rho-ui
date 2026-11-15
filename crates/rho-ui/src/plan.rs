//! The agent's checklist, above the composer while any step of it is left:
//! "Todo 2 of 5", which a click opens to each step, done ones struck
//! through. It starts folded. The daemon sends the whole list each time it
//! changes.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    collapsible::Collapsible, h_flex, scroll::ScrollableElement as _, v_flex, ActiveTheme as _,
    Icon, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, App, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window,
};
use rho_ui_core::{PlanItem, PlanStatus};

use crate::work::Disclosure;

/// The most of the list that shows before it scrolls, in rems: about
/// seven steps, so a long plan leaves the transcript most of the window.
const MAX_HEIGHT_REMS: f32 = 10.;

/// What a click on the heading does: open or fold the list.
type Toggle = Box<dyn Fn(&mut Window, &mut App)>;

/// The checklist, drawn from the session's [`PlanItem`]s.
#[derive(IntoElement)]
pub(crate) struct PlanList {
    steps: Vec<(SharedString, PlanStatus)>,
    disclosure: Disclosure,
    on_toggle: Toggle,
}

impl PlanList {
    /// `None` once every step is done, or before there is a plan. A click
    /// on the heading calls `on_toggle`, which flips `disclosure`.
    pub(crate) fn new(
        plan: &[PlanItem],
        disclosure: Disclosure,
        on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Option<Self> {
        plan.iter()
            .any(|step| step.status() != PlanStatus::Done)
            .then(|| Self {
                steps: plan
                    .iter()
                    .map(|step| (step.text().to_owned().into(), step.status()))
                    .collect(),
                disclosure,
                on_toggle: Box::new(on_toggle),
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
        let heading = format!("Todo {done} of {}", self.steps.len());
        let open = self.disclosure.is_open();
        let on_toggle = self.on_toggle;
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
                    .id("plan-toggle")
                    .test_support()
                    .gap_2()
                    .cursor_pointer()
                    .hover(move |style| style.text_color(foreground))
                    .on_click(move |_, window, cx| on_toggle(window, cx))
                    .child(Icon::new(Lucide::ListChecks).xsmall())
                    .child(heading)
                    .child(
                        Icon::new(if open {
                            Lucide::ChevronDown
                        } else {
                            Lucide::ChevronRight
                        })
                        .xsmall()
                        .opacity(0.7),
                    ),
            )
            .child(
                Collapsible::new()
                    .open(open)
                    // Folded steps aren't built at all.
                    .when(open, |this| this.content(steps(self.steps, foreground))),
            )
    }
}

/// The open list: one line per step, scrolling past [`MAX_HEIGHT_REMS`].
fn steps(steps: Vec<(SharedString, PlanStatus)>, foreground: Hsla) -> impl IntoElement {
    v_flex()
        .id("plan-steps")
        .gap_0p5()
        .max_h(rems(MAX_HEIGHT_REMS))
        .overflow_y_scrollbar()
        .children(steps.into_iter().enumerate().map(|(ix, (text, status))| {
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
                            PlanStatus::InProgress => this.text_color(foreground).font_medium(),
                            PlanStatus::Pending => this,
                        })
                        .child(text),
                )
        }))
}
