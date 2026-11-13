//! The bar across the top of the main pane: the project, then the path to
//! what the pane shows, each step nested under the one before it, as
//! `rho-ui / Fix the parser / Read the parser` for a subagent's session
//! under the session that started it. Every step but the last links back to
//! it; controls for what the pane shows sit at the right.
//!
//! Kit's `Breadcrumb` separates its items with chevrons and gives UI tests
//! no way to find them; this bar uses slashes, and tests find each step.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{h_flex, ActiveTheme as _, StyledExt as _};
use gpui_kit::{
    div, px, rems, AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
};

use crate::{sidebar::badge, workspaces::Workspace};

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
                .text_color(muted.opacity(0.6))
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
                    .max_w(rems(16.))
                    .truncate()
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(move |style| style.text_color(foreground))
                    .on_click(move |_, window, cx| go(window, cx))
                    .child(label);
                [step.into_any_element(), slash().into_any_element()]
            });
        h_flex()
            .id("title-bar")
            .flex_shrink_0()
            .min_h(px(44.))
            .gap_2()
            .px_4()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(badge(self.initials))
                    .child(div().flex_shrink_0().text_color(muted).child(self.project))
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
    }
}
