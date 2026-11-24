//! A session's pull request in one line, on its sidebar row and in its
//! title bar: its state's icon and its number, a dot for how its checks
//! stand, and how many more the session links.

use gpui_kit::component::{h_flex, Icon, Sizable as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{div, Div, ParentElement as _, Styled as _};
use rho_ui_core::{PullRequestRollup, PullRequestState, PullRequestSummary};

use crate::{pull_requests::Mark, right_panel::state_look};

/// How a PR's checks stand, where one line marks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Checks {
    Running,
    Passing,
    Failing,
}

impl Checks {
    /// `pr`'s, while they say something: a merged or closed PR's are
    /// history, and one with no checks has none to show.
    fn of(pr: &PullRequestSummary) -> Option<Self> {
        if !matches!(pr.state, PullRequestState::Open | PullRequestState::Draft) {
            return None;
        }
        match pr.rollup {
            PullRequestRollup::Pending => Some(Self::Running),
            PullRequestRollup::Passing => Some(Self::Passing),
            PullRequestRollup::Failing => Some(Self::Failing),
            // No checks, or a value a newer rho sends.
            _ => None,
        }
    }

    fn words(self) -> &'static str {
        match self {
            Self::Running => "checks running",
            Self::Passing => "checks passing",
            Self::Failing => "checks failing",
        }
    }

    /// Its dot: a ring while they run, filled once they end, green or
    /// red. A dot and no icon, as a check or a cross beside a number reads
    /// as done or as remove, and it stays apart from the state's icon.
    fn dot(self, theme: &Theme) -> Div {
        let dot = div().flex_shrink_0().size_1p5().rounded_full();
        match self {
            Self::Running => dot.border_1().border_color(theme.yellow),
            Self::Passing => dot.bg(theme.green),
            Self::Failing => dot.bg(theme.red),
        }
    }
}

/// What [`pr_mark`] shows of `mark`, in words: "#12, checks failing, 1
/// more".
pub(crate) fn mark_label(mark: &Mark<'_>) -> String {
    let checks = Checks::of(mark.pr)
        .map(|checks| format!(", {}", checks.words()))
        .unwrap_or_default();
    let more = match mark.more {
        0 => String::new(),
        more => format!(", {more} more"),
    };
    format!("#{}{checks}{more}", mark.pr.id.number)
}

/// `mark` in a line: its PR's state as an icon, its number, a dot for its
/// checks while it is open, and "+2" when the session links two more.
pub(crate) fn pr_mark(mark: &Mark<'_>, theme: &Theme) -> Div {
    let (_, icon, color) = state_look(mark.pr.state);
    // As the surface's tag colors its text.
    let color = color.scale(if theme.is_dark() { 300 } else { 600 });
    h_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_0p5()
                .child(Icon::new(icon).xsmall().text_color(color))
                .child(format!("#{}", mark.pr.id.number)),
        )
        .children(Checks::of(mark.pr).map(|checks| checks.dot(theme)))
        .when(mark.more > 0, |this| this.child(format!("+{}", mark.more)))
}
