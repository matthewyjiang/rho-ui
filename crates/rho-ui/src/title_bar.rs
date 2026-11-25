//! The bar across the top of the main pane: the project, then the path to
//! what the pane shows, each step nested under the one before it, as
//! `rho-ui / Fix the parser / Read the parser` for a subagent's session
//! under the session that started it. The project can start a new session
//! in it, a button beside it opens the repository's web page, and every
//! step but the last links back to it; controls for what the pane shows sit
//! at the right, and at the right edge, while the right panel is hidden, the
//! toggle that shows it.
//!
//! Kit's `Breadcrumb` separates its items with chevrons and gives UI tests
//! no way to find them; this bar uses slashes, and tests find each step.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex, ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
};

use crate::{
    badge::Badge,
    repo,
    right_panel::{self, PanelLayout},
    window_chrome::{self, DragArea, MovesWindow as _},
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
    /// The project's initials and name; `None` while the pane has no
    /// folder yet, as a lead's draft until its session opens.
    project: Option<(SharedString, SharedString)>,
    /// The repository's web page, from its origin remote.
    repo_page: Option<String>,
    /// What a click on the project does; without it, the project is text.
    on_project: Option<Go>,
    /// The steps between the project and the title, each with what a click
    /// shows.
    parents: Vec<(SharedString, Go)>,
    title: SharedString,
    trailing: Option<AnyElement>,
}

impl TitleBar {
    /// A bar reading `project / title`, or `title` alone with no
    /// `workspace`.
    pub(crate) fn new(workspace: Option<&Workspace>, title: impl Into<SharedString>) -> Self {
        Self {
            project: workspace
                .map(|workspace| (workspace.initials.clone(), workspace.short_name.clone())),
            repo_page: workspace
                .and_then(Workspace::origin)
                .and_then(repo::web_url),
            on_project: None,
            parents: Vec::new(),
            title: title.into(),
            trailing: None,
        }
    }

    /// Makes the project a link that starts a new session in it, which `go`
    /// does.
    pub(crate) fn on_project(mut self, go: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_project = Some(Box::new(go));
        self
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
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
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
        let on_project = self.on_project;
        let project = self
            .project
            .map(|(initials, name)| project_step(initials, &name, on_project, cx));
        let has_project = project.is_some();
        let repo_page = self.repo_page.map(|url| {
            Button::new("title-repo")
                .ghost()
                .xsmall()
                .icon(IconName::ExternalLink)
                .tooltip(format!("Open {url}"))
                .on_click(move |_, _, cx| cx.open_url(&url))
        });
        h_flex()
            .id("title-bar")
            .flex_shrink_0()
            .h(rems(TOP_ROW_REMS))
            .gap_2()
            .pl_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .gap_3()
                    .when(has_project, |bar| {
                        bar.child(
                            h_flex()
                                .flex_shrink_0()
                                .gap_1()
                                .children(project)
                                .children(repo_page),
                        )
                        .child(slash())
                    })
                    .children(parents)
                    // Takes the bar's room up to the controls, so all of
                    // it moves the window where the app draws the title
                    // bar.
                    .child(
                        div()
                            .id("session-title")
                            .test_support()
                            .aria_label(self.title.clone())
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .items_center()
                            .font_medium()
                            .moves_window(DragArea::SessionTitle, window, cx)
                            .child(div().min_w_0().truncate().child(self.title)),
                    ),
            )
            .children(self.trailing)
            // At the window's right edge while the panel is hidden, where
            // the window buttons' room takes the place of the inset.
            .when(!PanelLayout::get(cx), |bar| {
                bar.child(right_panel::toggle_button(cx))
            })
            .map(|bar| match window_chrome::caption_room(window, cx) {
                Some(room) if !PanelLayout::get(cx) => bar.child(room),
                _ => bar.pr_4(),
            })
    }
}

/// The project at the head of the bar: the same text with or without a
/// click, so the bar reads alike in every pane; with `go`, a link, as the
/// steps after it are.
fn project_step(
    initials: SharedString,
    name: &SharedString,
    go: Option<Go>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, foreground) = (theme.muted_foreground, theme.foreground);
    let project = h_flex()
        .flex_shrink_0()
        .gap_2()
        .text_color(muted)
        .child(Badge::new(initials, rems(BADGE_REMS)))
        .child(name.clone());
    match go {
        Some(go) => project
            .id("title-project")
            .test_support()
            .aria_label(format!("New session in {name}"))
            .cursor_pointer()
            .hover(move |style| style.text_color(foreground))
            .on_click(move |_, window, cx| go(window, cx))
            .into_any_element(),
        None => project.into_any_element(),
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
