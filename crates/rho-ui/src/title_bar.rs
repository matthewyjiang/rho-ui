//! The bar across the top of the main pane: the project, then the path to
//! what the pane shows, each step nested under the one before it, as
//! `rho-ui / Fix the parser / Read the parser` for a subagent's session
//! under the session that started it. Every step but the last links back to
//! it; controls for what the pane shows sit at the right, and at the right
//! edge, while the right panel is hidden, the toggle that shows it.
//!
//! Kit's `Breadcrumb` separates its items with chevrons and gives UI tests
//! no way to find them; this bar uses slashes, and tests find each step.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{h_flex, ActiveTheme as _, StyledExt as _};
use gpui_kit::{
    div, rems, AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
};

use crate::{
    badge::Badge,
    right_panel::{self, PanelLayout},
    workspaces::Workspace,
};

/// The height of the window's top row, in rems: this bar, and the
/// sidebar's top row beside it, so the two line up and grow with the UI
/// scale. 44px at the default scale.
pub(crate) const TOP_ROW_REMS: f32 = 2.75;

/// The project badge's side, in rems: the height of a line of the bar's
/// text, so the badge reads as part of the line, where the sidebar's is
/// sized to its smaller text.
const BADGE_REMS: f32 = 1.375;

/// The longest a step above the title runs, in characters, before it is cut
/// with an ellipsis: four or five words of a title, which rho keeps to 80
/// characters, leaving the bar's width to the title on screen. Cut here
/// rather than by a box's width, as GPUI's ellipsis leaves the box's slack
/// as a gap before the next slash.
const PARENT_CHARS: usize = 32;

/// Shows what a step links to.
type Go = Box<dyn Fn(&mut Window, &mut App)>;

/// The title bar of one pane. Build it with [`TitleBar::new`], add the
/// steps above the title with [`TitleBar::parent`], outermost first.
#[derive(IntoElement)]
pub(crate) struct TitleBar {
    initials: SharedString,
    project: SharedString,
    /// The steps between the project and the title, each with what a click
    /// shows.
    parents: Vec<(SharedString, Go)>,
    title: SharedString,
    trailing: Option<AnyElement>,
}

impl TitleBar {
    /// A bar reading `project / title`.
    pub(crate) fn new(workspace: &Workspace, title: impl Into<SharedString>) -> Self {
        Self {
            initials: workspace.initials.clone(),
            project: workspace.short_name.clone(),
            parents: Vec::new(),
            title: title.into(),
            trailing: None,
        }
    }

    /// Adds a step above the title, after those added before it; a click on
    /// it calls `go`.
    pub(crate) fn parent(
        mut self,
        label: impl Into<SharedString>,
        go: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.parents.push((label.into(), Box::new(go)));
        self
    }

    /// Controls at the right end of the bar.
    pub(crate) fn trailing(mut self, trailing: Option<impl IntoElement>) -> Self {
        self.trailing = trailing.map(IntoElement::into_any_element);
        self
    }
}

impl RenderOnce for TitleBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, foreground) = (theme.muted_foreground, theme.foreground);
        let slash = || {
            div()
                .flex_shrink_0()
                .text_color(muted.opacity(0.5))
                .child("/")
        };
        let parents = self
            .parents
            .into_iter()
            .enumerate()
            .flat_map(|(ix, (label, go))| {
                let step = div()
                    .id(SharedString::from(format!("title-parent-{ix}")))
                    .test_support()
                    .aria_label(label.clone())
                    .min_w_0()
                    .truncate()
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(move |style| style.text_color(foreground))
                    .on_click(move |_, window, cx| go(window, cx))
                    .child(shorten(&label));
                [step.into_any_element(), slash().into_any_element()]
            });
        h_flex()
            .id("title-bar")
            .flex_shrink_0()
            .h(rems(TOP_ROW_REMS))
            .gap_2()
            .px_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_2()
                            .child(Badge::new(self.initials, rems(BADGE_REMS)))
                            .child(div().text_color(muted).child(self.project)),
                    )
                    .child(slash())
                    .children(parents)
                    .child(
                        div()
                            .id("session-title")
                            .test_support()
                            .aria_label(self.title.clone())
                            .min_w_0()
                            .truncate()
                            .font_medium()
                            .child(self.title),
                    ),
            )
            .children(self.trailing)
            .children((!PanelLayout::get(cx)).then(|| right_panel::toggle_button(cx)))
    }
}

/// `label` cut to [`PARENT_CHARS`] characters, the last an ellipsis, at a
/// word's end when one is near.
fn shorten(label: &SharedString) -> SharedString {
    if label.chars().count() <= PARENT_CHARS {
        return label.clone();
    }
    let cut: String = label.chars().take(PARENT_CHARS - 1).collect();
    let cut = match cut.rfind(' ') {
        Some(space) if space > cut.len() * 2 / 3 => &cut[..space],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end()).into()
}

#[cfg(test)]
mod tests {
    use super::{shorten, PARENT_CHARS};

    #[test]
    fn long_steps_are_cut_at_a_word() {
        let short = "Fix the parser".into();
        assert_eq!(shorten(&short), short);
        let cut = shorten(&"Add SSH Remote Daemon Support for Many Machines".into());
        assert_eq!(cut.as_ref(), "Add SSH Remote Daemon Support…");
        assert!(cut.chars().count() <= PARENT_CHARS);
        let word = shorten(&"Supercalifragilisticexpialidocious-and-then-some".into());
        assert_eq!(word.chars().count(), PARENT_CHARS);
    }
}
