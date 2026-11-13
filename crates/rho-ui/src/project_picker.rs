//! The project a new session starts in, chosen in the middle of the draft:
//! a searchable list of every project rho-ui knows, the one used most
//! recently first, with the draft's project checked. Typing an absolute or
//! `~/` path offers that folder too, and Open folder… asks the desktop for
//! one.
//!
//! The list reads [`Workspaces`], which the saved sessions fill and order,
//! so it follows the sidebar without a copy of its own.

use std::path::{Path, PathBuf};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    group_box::{GroupBox, GroupBoxVariants as _},
    h_flex,
    label::Label,
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    notification::Notification,
    separator::Separator,
    v_flex, ActiveTheme as _, Icon, IconName, IndexPath, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, App, AppContext as _, Context, Entity, EventEmitter, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription, Task,
    Window,
};

use crate::{
    sidebar::badge,
    time,
    workspaces::{Workspace, Workspaces},
};

/// The search field's height, measured at 2.1 rem with the default scale.
const SEARCH_REMS: f32 = 2.25;
/// A row's height: measured at 51.5 px with a 16 px rem, 3.22 rem.
const ROW_REMS: f32 = 3.25;
/// Rows the list shows before it scrolls.
const MAX_ROWS: u8 = 5;
/// Rows the list keeps room for, so the empty state has space.
const MIN_ROWS: u8 = 2;

/// What the picker tells its draft.
pub(crate) enum PickerEvent {
    /// The user chose this directory for the new session.
    Picked(PathBuf),
    /// The user left the search with Escape: back to the composer.
    Dismissed,
}

/// The project list, for one draft.
pub(crate) struct ProjectPicker {
    list: Entity<ListState<ProjectList>>,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<PickerEvent> for ProjectPicker {}

impl ProjectPicker {
    /// A picker over `workspaces` with `current` checked.
    pub(crate) fn new(
        workspaces: &Entity<Workspaces>,
        current: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let delegate = ProjectList::new(workspaces.clone(), current, cx);
        let list = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
        let subscriptions = [
            cx.subscribe_in(
                &list,
                window,
                |_, list, event: &ListEvent, _, cx| match event {
                    ListEvent::Confirm(ix) => {
                        if let Some(cwd) = list.read(cx).delegate().cwd_at(*ix, cx) {
                            cx.emit(PickerEvent::Picked(cwd));
                        }
                    }
                    ListEvent::Cancel => cx.emit(PickerEvent::Dismissed),
                    ListEvent::Select(_) => {}
                },
            ),
            // New sessions elsewhere reorder the projects; a fresh read can
            // rename one.
            cx.observe(workspaces, |this, _, cx| {
                this.list.update(cx, |list, cx| {
                    list.delegate_mut().refilter(cx);
                    cx.notify();
                });
            }),
        ];
        Self {
            list,
            _subscriptions: subscriptions,
        }
    }

    /// Checks `cwd` in the list, as the draft moved there, and clears the
    /// search.
    pub(crate) fn set_current(
        &mut self,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.list.update(cx, |list, cx| {
            list.delegate_mut().current = cwd;
            list.set_query("", window, cx);
            cx.notify();
        });
    }

    /// Puts the keyboard in the search field, the current project selected.
    pub(crate) fn focus(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.list.update(cx, |list, cx| {
            let current = list.delegate().current_row(cx);
            list.set_selected_index(current, window, cx);
            list.focus(window, cx);
        });
    }

    /// Asks the desktop's folder chooser for a directory.
    fn browse(window: &mut Window, cx: &mut Context<'_, Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let chosen = chosen.await;
            let _ = this.update_in(cx, |_, window, cx| match chosen {
                Ok(Ok(Some(paths))) => {
                    if let Some(cwd) = paths.into_iter().next() {
                        cx.emit(PickerEvent::Picked(cwd));
                    }
                }
                // Cancelled, or the chooser went away.
                Ok(Ok(None)) | Err(_) => {}
                Ok(Err(error)) => window.push_notification(
                    Notification::error(error.to_string()).title("Couldn't open a folder chooser"),
                    cx,
                ),
            });
        })
        .detach();
    }
}

impl Render for ProjectPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        // Sized by every project, not the matches, so it holds still while
        // the search narrows it.
        let total = self.list.read(cx).delegate().total;
        let rows = f32::from(
            u8::try_from(total)
                .unwrap_or(u8::MAX)
                .clamp(MIN_ROWS, MAX_ROWS),
        );
        GroupBox::new()
            .id("project-picker")
            .outline()
            .title(
                h_flex()
                    .justify_between()
                    .text_sm()
                    .child("Start a session in")
                    .child(div().text_xs().child("Ctrl+Shift+N to search")),
            )
            .content_style(gpui_kit::StyleRefinement::default().p_1().gap_1())
            .child(
                // Kit's list scrolls in a fixed height; it can give some up
                // to a short window.
                v_flex()
                    .h(rems(SEARCH_REMS + rows * ROW_REMS))
                    .min_h(rems(SEARCH_REMS + ROW_REMS))
                    .flex_shrink(1.)
                    .child(
                        List::new(&self.list).search_placeholder("Search projects or type a path"),
                    ),
            )
            .child(Separator::horizontal())
            .child(
                Button::new("project-browse")
                    .ghost()
                    .small()
                    .w_full()
                    .justify_start()
                    .icon(Icon::new(IconName::FolderOpen).text_color(muted))
                    .label("Open folder…")
                    .on_click(cx.listener(|_, _, window, cx| Self::browse(window, cx))),
            )
    }
}

/// A folder typed into the search that exists and isn't listed.
struct Typed {
    cwd: PathBuf,
    /// The canonical path, for its row.
    shown: SharedString,
}

/// The projects for a searchable [`List`]: a typed folder, if any, then the
/// workspaces that match the search.
struct ProjectList {
    workspaces: Entity<Workspaces>,
    /// The draft's directory, checked in the list.
    current: PathBuf,
    query: String,
    /// Indices into `workspaces` that match `query`, most recent first.
    matches: Vec<usize>,
    typed: Option<Typed>,
    /// How many projects there are, matching or not.
    total: usize,
    /// Unix seconds when the list was last filtered, for ages.
    now: i64,
}

/// What a row of the list is.
enum Choice<'a> {
    Typed(&'a Typed),
    Workspace(&'a Workspace),
}

impl ProjectList {
    fn new(workspaces: Entity<Workspaces>, current: PathBuf, cx: &App) -> Self {
        let mut list = Self {
            workspaces,
            current,
            query: String::new(),
            matches: Vec::new(),
            typed: None,
            total: 0,
            now: 0,
        };
        list.refilter(cx);
        list
    }

    /// Filters the workspaces by the search again, keeping their order.
    fn refilter(&mut self, cx: &App) {
        let workspaces = self.workspaces.read(cx);
        let query = self.query.to_lowercase();
        let recent = workspaces.recent();
        self.total = recent.len();
        self.matches = recent
            .into_iter()
            .filter(|&ix| {
                let workspace = workspaces.get(ix);
                workspace.name.to_lowercase().contains(&query)
                    || workspace.path.to_lowercase().contains(&query)
            })
            .collect();
        self.now = time::now_unix();
        if let Some(typed) = &self.typed {
            if self.listed(&typed.cwd, cx) {
                self.typed = None;
            }
        }
    }

    /// Whether `cwd` already has a row among the matches.
    fn listed(&self, cwd: &Path, cx: &App) -> bool {
        let workspaces = self.workspaces.read(cx);
        self.matches
            .iter()
            .any(|&ix| workspaces.get(ix).cwd() == cwd)
    }

    fn choice_at<'a>(&'a self, ix: IndexPath, cx: &'a App) -> Option<Choice<'a>> {
        let row = match &self.typed {
            Some(typed) if ix.row == 0 => return Some(Choice::Typed(typed)),
            Some(_) => ix.row - 1,
            None => ix.row,
        };
        let workspace = *self.matches.get(row)?;
        Some(Choice::Workspace(self.workspaces.read(cx).get(workspace)))
    }

    fn cwd_at(&self, ix: IndexPath, cx: &App) -> Option<PathBuf> {
        Some(match self.choice_at(ix, cx)? {
            Choice::Typed(typed) => typed.cwd.clone(),
            Choice::Workspace(workspace) => workspace.cwd().to_path_buf(),
        })
    }

    /// The row of the draft's project, if it matches the search.
    fn current_row(&self, cx: &App) -> Option<IndexPath> {
        let workspaces = self.workspaces.read(cx);
        let row = self
            .matches
            .iter()
            .position(|&ix| workspaces.get(ix).cwd() == self.current)?;
        Some(IndexPath::default().row(row + usize::from(self.typed.is_some())))
    }
}

impl ListDelegate for ProjectList {
    type Item = ListItem;

    /// Matches the project name or its path. A query that is a path is
    /// checked in the background, and offered when it names a directory.
    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        cx: &mut Context<'_, ListState<Self>>,
    ) -> Task<()> {
        query.clone_into(&mut self.query);
        self.typed = None;
        self.refilter(cx);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let Some(path) = typed_path(query, home.as_deref()) else {
            return Task::ready(());
        };
        let query = query.to_owned();
        cx.spawn(async move |list, cx| {
            let dir = cx
                .background_executor()
                .spawn(async move { path.canonicalize().ok().filter(|path| path.is_dir()) })
                .await;
            let _ = list.update(cx, |list, cx| {
                let this = list.delegate_mut();
                let Some(cwd) = dir.filter(|dir| this.query == query && !this.listed(dir, cx))
                else {
                    return;
                };
                this.typed = Some(Typed {
                    shown: cwd.display().to_string().into(),
                    cwd,
                });
                cx.notify();
            });
        })
    }

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.matches.len() + usize::from(self.typed.is_some())
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<'_, ListState<Self>>,
    ) -> Option<ListItem> {
        let radius = cx.theme().radius;
        let item = match self.choice_at(ix, cx)? {
            Choice::Typed(typed) => ListItem::new("project-typed")
                .accessibility_label(typed.shown.clone())
                .child(row(
                    folder_tile(cx).into_any_element(),
                    format!("Open {}", folder_name(&typed.cwd)).into(),
                    div().child(typed.shown.clone()),
                    None,
                    cx,
                )),
            Choice::Workspace(workspace) => {
                let path = h_flex()
                    .gap_1()
                    .min_w_0()
                    .child(div().truncate().child(workspace.path.clone()))
                    .when_some(workspace.branch.clone(), |this, branch| {
                        this.child(
                            Icon::new(gpui_kit::assets::IconName::GitBranch)
                                .xsmall()
                                .ml_1()
                                .flex_shrink_0(),
                        )
                        .child(div().truncate().child(branch))
                    });
                let age = (workspace.last_used > 0).then(|| {
                    SharedString::from(time::relative_age(self.now - workspace.last_used))
                });
                ListItem::new(SharedString::from(format!(
                    "project-{}",
                    workspace.cwd().display()
                )))
                .check_icon(IconName::Check)
                .confirmed(workspace.cwd() == self.current)
                .accessibility_label(workspace.name.clone())
                .child(row(
                    badge(workspace.initials.clone())
                        .size(rems(1.75))
                        .into_any_element(),
                    workspace.name.clone(),
                    path,
                    age,
                    cx,
                ))
            }
        };
        Some(item.rounded(radius))
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<'_, ListState<Self>>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("No matching projects")
            .child(
                div()
                    .text_xs()
                    .child("Type a path such as ~/code/app, or open a folder"),
            )
    }

    fn set_selected_index(
        &mut self,
        _: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<'_, ListState<Self>>,
    ) {
        // ListState keeps the selection; rows read it when it renders them.
    }
}

/// One two-line row: a tile, the name, and under it the path; the age of
/// its newest session on the right.
fn row(
    tile: gpui_kit::AnyElement,
    name: SharedString,
    path: impl IntoElement,
    age: Option<SharedString>,
    cx: &App,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .w_full()
        .gap_2p5()
        .py_0p5()
        .child(tile)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(Label::new(name).text_sm().truncate())
                .child(div().text_xs().text_color(muted).child(path)),
        )
        .when_some(age, |this, age| {
            this.child(div().flex_shrink_0().text_xs().text_color(muted).child(age))
        })
}

/// A folder icon in a tile the size of a project badge.
fn folder_tile(cx: &App) -> impl IntoElement {
    h_flex()
        .flex_shrink_0()
        .size(rems(1.75))
        .justify_center()
        .rounded(rems(0.25))
        .bg(cx.theme().secondary)
        .child(Icon::new(IconName::Folder).small())
}

fn folder_name(cwd: &Path) -> String {
    cwd.file_name().map_or_else(
        || cwd.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The directory a search names, if it is a path: absolute, `~`, or under
/// `~/`. Whether it exists is checked later, off the UI thread.
fn typed_path(query: &str, home: Option<&Path>) -> Option<PathBuf> {
    if query == "~" {
        return home.map(Path::to_path_buf);
    }
    if let Some(rest) = query.strip_prefix("~/") {
        return home.map(|home| home.join(rest));
    }
    let path = Path::new(query);
    path.is_absolute().then(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_absolute_or_from_home() {
        let home = Some(Path::new("/home/me"));
        assert_eq!(typed_path("~", home), Some(PathBuf::from("/home/me")));
        assert_eq!(
            typed_path("~/code/rho", home),
            Some(PathBuf::from("/home/me/code/rho"))
        );
        assert_eq!(typed_path("/srv/x", home), Some(PathBuf::from("/srv/x")));
        assert_eq!(typed_path("rho-ui", home), None, "a name is a search");
        assert_eq!(typed_path("~/code", None), None, "no home, no ~");
    }
}
