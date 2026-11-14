//! The project a new session starts in, chosen in the middle of the draft:
//! a searchable list of the user's [`Projects`] on every machine, the one
//! used most recently first, with the draft's project checked. Typing an
//! absolute or `~/` path offers to add that folder's project on the machine
//! the footer names, and on this machine Add project… asks the desktop for
//! one. A remote machine's folders can't be read from here, so a typed
//! remote path is added as typed, and the daemon says if it isn't there
//! when a session opens in it. Remove takes a project off the list. In the
//! footer, a git project offers a new worktree for the session instead of
//! its checkout; see [`Checkout`].
//!
//! A project's age and the check read [`Workspaces`]: the saved sessions
//! date each folder, and its repository says which project it is in, so a
//! session in a worktree counts for its project.

use std::path::Path;

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    group_box::{GroupBox, GroupBoxVariants as _},
    h_flex,
    label::Label,
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    separator::Separator,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _,
    WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    accesskit::Toggled, div, px, rems, Anchor, App, AppContext as _, Context, Div, Entity,
    EventEmitter, Focusable as _, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window,
};

use rho_ui_core::Checkout;

use crate::{
    badge::Badge,
    machines::{MachineId, Machines, Place},
    projects::Projects,
    repo, time,
    workspaces::{self, Workspace, Workspaces},
};

/// The search field's height: Kit's medium input is `h_8`, 2 rem, over a
/// 1 px border.
const SEARCH_REMS: f32 = 2.;
/// A row's height: measured at 51.5 px with a 16 px rem (3.219 rem) and
/// 64.5 px with a 20 px rem (3.225 rem), in the default font and Noto Sans.
const ROW_REMS: f32 = 3.22;
/// Rows the list shows before it scrolls.
const MAX_ROWS: u8 = 5;
/// Rows the list keeps room for, so the empty state has space.
const MIN_ROWS: u8 = 2;

/// What the picker tells its draft.
pub(crate) enum PickerEvent {
    /// The user chose this directory for the new session.
    Picked(Place),
    /// The user left the search with Escape: back to the composer.
    Dismissed,
}

/// The project list, for one draft.
pub(crate) struct ProjectPicker {
    list: Entity<ListState<ProjectList>>,
    projects: Entity<Projects>,
    /// Whether the session works in the project's checkout or in a new
    /// worktree of it, which the daemon adds only as the session starts.
    checkout: Checkout,
    _subscriptions: [Subscription; 5],
}

impl EventEmitter<PickerEvent> for ProjectPicker {}

impl ProjectPicker {
    /// A picker over `projects` on `machines` with the project of
    /// `current` checked.
    pub(crate) fn new(
        workspaces: &Entity<Workspaces>,
        projects: &Entity<Projects>,
        machines: &Entity<Machines>,
        current: Place,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let delegate = ProjectList::new(
            workspaces.clone(),
            projects.clone(),
            machines.clone(),
            current,
            cx,
        );
        let list = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
        let search = list.read(cx).focus_handle(cx);
        let subscriptions = [
            cx.subscribe_in(
                &list,
                window,
                |this, list, event: &ListEvent, window, cx| match event {
                    ListEvent::Confirm(ix) => this.pick(*ix, cx),
                    ListEvent::Cancel => {
                        list.update(cx, |list, cx| list.set_selected_index(None, window, cx));
                        cx.emit(PickerEvent::Dismissed);
                    }
                    ListEvent::Select(_) => {}
                },
            ),
            // New sessions date the projects; a fresh read can rename one.
            cx.observe_in(workspaces, window, |this, _, window, cx| {
                this.relist(window, cx, |list, cx| list.refilter(cx));
            }),
            // Another draft, or this one, added or removed a project.
            cx.observe_in(projects, window, |this, _, window, cx| {
                this.relist(window, cx, |list, cx| list.sync(cx));
            }),
            // A machine came, went or was renamed.
            cx.observe_in(machines, window, |this, _, window, cx| {
                this.relist(window, cx, |list, cx| list.refilter(cx));
            }),
            // The keyboard's row shows only while the keyboard is here, so
            // it never looks like a second choice next to the check: Escape
            // and a search without the keyboard clear it too, and so does a
            // click elsewhere. Focus events need an active window, which
            // headless UI tests lack.
            cx.on_blur(&search, window, |this, window, cx| {
                this.list.update(cx, |list, cx| {
                    list.set_selected_index(None, window, cx);
                    cx.notify();
                });
            }),
        ];
        Self {
            list,
            projects: projects.clone(),
            checkout: Checkout::Local,
            _subscriptions: subscriptions,
        }
    }

    /// Where the session works: the project's checkout or a new worktree.
    pub(crate) fn checkout(&self) -> Checkout {
        self.checkout
    }

    fn set_checkout(&mut self, checkout: Checkout, cx: &mut Context<'_, Self>) {
        self.checkout = checkout;
        cx.notify();
    }

    /// Whether the draft's directory is in a git repository, as far as its
    /// repository has been read: only one has worktrees to add.
    fn in_repository(&self, cx: &App) -> bool {
        let list = self.list.read(cx).delegate();
        let workspaces = list.workspaces.read(cx);
        workspaces
            .find(&[&list.current])
            .is_some_and(|ix| workspaces.get(ix).branch.is_some())
    }

    /// The button naming where the session works, with a menu to choose:
    /// the project's checkout, or a new worktree.
    fn render_checkout(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let in_repository = self.in_repository(cx);
        let label = match self.checkout {
            Checkout::NewWorktree => "New worktree",
            Checkout::Local => "Local",
        };
        let current = self.checkout;
        let view = cx.entity().downgrade();
        Button::new("project-checkout")
            .ghost()
            .small()
            .flex_shrink_0()
            .icon(
                Icon::new(gpui_kit::assets::IconName::GitBranch)
                    .text_color(cx.theme().muted_foreground),
            )
            .label(label)
            .accessibility_label("Checkout")
            .dropdown_caret(true)
            .disabled(!in_repository)
            .when(!in_repository, |this| this.tooltip("Not a git repository"))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                [
                    (Checkout::Local, "Local"),
                    (Checkout::NewWorktree, "New worktree"),
                ]
                .into_iter()
                .fold(menu, |menu, (checkout, name)| {
                    let view = view.clone();
                    menu.item(
                        PopupMenuItem::new(name)
                            .checked(checkout == current)
                            .on_click(move |_, _, cx| {
                                let _ =
                                    view.update(cx, |picker, cx| picker.set_checkout(checkout, cx));
                            }),
                    )
                })
            })
    }

    /// With more than one machine, the button naming the machine a typed
    /// path is added on, with a menu to choose another.
    fn render_machine(&self, cx: &mut Context<'_, Self>) -> Option<impl IntoElement> {
        let list = self.list.read(cx).delegate();
        let machines = list.machines.read(cx);
        if !machines.several() {
            return None;
        }
        let current = list.adding_on.clone();
        let choices: Vec<(MachineId, SharedString)> = machines
            .iter()
            .map(|machine| (machine.id.clone(), machine.label.clone()))
            .collect();
        let view = self.list.downgrade();
        Some(
            Button::new("project-machine")
                .ghost()
                .small()
                .flex_shrink_0()
                .icon(
                    Icon::new(gpui_kit::assets::IconName::Server)
                        .text_color(cx.theme().muted_foreground),
                )
                .label(machines.label(&current))
                .accessibility_label("Machine")
                .tooltip("The machine a typed path is on")
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, _| {
                    choices.iter().fold(menu, |menu, (machine, label)| {
                        let (view, machine) = (view.clone(), machine.clone());
                        menu.item(
                            PopupMenuItem::new(label.clone())
                                .checked(machine == current)
                                .on_click(move |_, window, cx| {
                                    let _ = view.update(cx, |list, cx| {
                                        list.delegate_mut().adding_on = machine.clone();
                                        let query = list.delegate().query.clone();
                                        // Look the typed path up again there.
                                        list.set_query(&query, window, cx);
                                        cx.notify();
                                    });
                                }),
                        )
                    })
                }),
        )
    }

    /// Lists the projects again with `change`, keeping the keyboard on the
    /// project it was on: `ListState` keeps the selection as a row number,
    /// which a new order would hand to another project.
    fn relist(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        change: impl FnOnce(&mut ProjectList, &mut Context<'_, ListState<ProjectList>>),
    ) {
        self.list.update(cx, |list, cx| {
            let before = list
                .selected_index()
                .and_then(|ix| list.delegate().rows.get(ix.row).cloned());
            change(list.delegate_mut(), cx);
            if let Some(before) = before {
                let row = list.delegate().rows.iter().position(|row| *row == before);
                list.set_selected_index(row.map(|row| IndexPath::default().row(row)), window, cx);
            }
            cx.notify();
        });
    }

    /// Checks the project of `cwd`, as the draft moved there, and clears
    /// the search. Typed paths now go to its machine.
    pub(crate) fn set_current(
        &mut self,
        cwd: Place,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.list.update(cx, |list, cx| {
            let delegate = list.delegate_mut();
            delegate.adding_on = cwd.machine.clone();
            delegate.current = cwd;
            list.set_query("", window, cx);
            cx.notify();
        });
        // Only a git repository has worktrees to add.
        if !self.in_repository(cx) {
            self.checkout = Checkout::Local;
        }
    }

    /// Puts the keyboard in the search field, the current project
    /// selected, else the first.
    pub(crate) fn focus(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.list.update(cx, |list, cx| {
            let delegate = list.delegate();
            let row = delegate
                .current_row()
                .or_else(|| (!delegate.rows.is_empty()).then(IndexPath::default));
            list.set_selected_index(row, window, cx);
            list.focus(window, cx);
        });
    }

    /// Starts the session in the project at row `ix`, adding a typed
    /// folder's project to the list first.
    fn pick(&mut self, ix: IndexPath, cx: &mut Context<'_, Self>) {
        let list = self.list.read(cx).delegate();
        let root = match list.rows.get(ix.row) {
            Some(Row::Project(ix)) => list.workspaces.read(cx).get(*ix).place().clone(),
            Some(Row::Add(folder)) => {
                let root = folder.root.clone();
                add(&self.projects, root.clone(), cx);
                root
            }
            None => return,
        };
        cx.emit(PickerEvent::Picked(root));
    }

    /// Asks the desktop's folder chooser for a folder, and adds its project.
    fn browse(window: &mut Window, cx: &mut Context<'_, Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let folder = match chosen.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                // Cancelled, or the chooser went away.
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(error)) => {
                    let _ = this.update_in(cx, |_, window, cx| {
                        window.push_notification(
                            Notification::error(error.to_string())
                                .title("Couldn't open a folder chooser"),
                            cx,
                        );
                    });
                    None
                }
            };
            let Some(folder) = folder else { return };
            let root = cx
                .background_executor()
                .spawn(async move { Place::new(MachineId::local(), repo::inspect(&folder).root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                add(&this.projects, root.clone(), cx);
                cx.emit(PickerEvent::Picked(root));
            });
        })
        .detach();
    }
}

#[cfg(test)]
impl ProjectPicker {
    /// The row the keyboard is on, as its row's element ID, for UI tests:
    /// Kit's selection reaches no element a test can see.
    pub(crate) fn selected_row(&self, cx: &App) -> Option<String> {
        let list = self.list.read(cx);
        let delegate = list.delegate();
        Some(match delegate.rows.get(list.selected_index()?.row)? {
            Row::Project(ix) => {
                element_id(delegate.workspaces.read(cx).get(*ix).place()).to_string()
            }
            Row::Add(_) => "project-typed".to_owned(),
        })
    }
}

/// A project row's element ID: `project-<machine>:<path>`.
fn element_id(place: &Place) -> SharedString {
    format!("project-{}:{}", place.machine, place.path.display()).into()
}

/// Adds `root` to the user's projects.
fn add(projects: &Entity<Projects>, root: Place, cx: &mut App) {
    projects.update(cx, |projects, cx| {
        if projects.add(root) {
            cx.notify();
        }
    });
}

/// Takes `root` off the user's projects.
fn remove(projects: &Entity<Projects>, root: &Place, cx: &mut App) {
    projects.update(cx, |projects, cx| {
        if projects.remove(root) {
            cx.notify();
        }
    });
}

impl Render for ProjectPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let rem = window.rem_size();
        // Sized by every project, not the matches, so it holds still while
        // the search narrows it.
        let total = self.list.read(cx).delegate().known.len();
        // The desktop's folder chooser shows this machine's folders.
        let adding_locally = self.list.read(cx).delegate().adding_on.is_local();
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
                // Kit's list scrolls in a fixed height, which holds whole
                // rows so none shows cut off; it can give some up to a
                // short window.
                v_flex()
                    .h(rem * (SEARCH_REMS + rows * ROW_REMS) + px(1.))
                    .min_h(rem * (SEARCH_REMS + ROW_REMS) + px(1.))
                    .flex_shrink(1.)
                    .child(
                        List::new(&self.list).search_placeholder("Search projects or type a path"),
                    ),
            )
            .child(Separator::horizontal())
            .child(
                h_flex()
                    .justify_between()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_1()
                            .min_w_0()
                            .children(self.render_machine(cx))
                            .child(
                                Button::new("project-add")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(IconName::Plus).text_color(muted))
                                    .label("Add project…")
                                    .disabled(!adding_locally)
                                    .when(!adding_locally, |this| {
                                        this.tooltip("Type the folder's path on the remote machine")
                                    })
                                    .on_click(
                                        cx.listener(|_, _, window, cx| Self::browse(window, cx)),
                                    ),
                            ),
                    )
                    .child(self.render_checkout(cx)),
            )
    }
}

/// A folder the search names whose project is not on the list.
#[derive(Clone, PartialEq)]
struct Folder {
    /// Its project's folder; see [`repo::Repo::root`].
    root: Place,
    /// The project's path, with the home directory as `~`.
    shown: SharedString,
}

/// One row of the list.
#[derive(Clone, PartialEq)]
enum Row {
    /// A project: the index of its folder in the workspaces.
    Project(usize),
    /// A typed folder's project, to add.
    Add(Folder),
}

/// The projects for a searchable [`List`]: what a typed path names, if
/// anything, then the projects that match the search.
struct ProjectList {
    workspaces: Entity<Workspaces>,
    projects: Entity<Projects>,
    machines: Entity<Machines>,
    /// Each project's index in the workspaces, in the order added.
    known: Vec<usize>,
    /// The draft's directory, whose project is checked.
    current: Place,
    /// The machine a typed path names a folder on.
    adding_on: MachineId,
    /// The draft's project, if it is on the list.
    checked: Option<usize>,
    /// The project under the mouse, which shows Remove instead of its age.
    hovered: Option<usize>,
    query: String,
    /// What the search names as a path, once checked.
    found: Option<Row>,
    rows: Vec<Row>,
    /// Unix seconds of each listed project's newest session, by workspace
    /// index; 0 with none.
    last_used: Vec<(usize, i64)>,
    /// Unix seconds when the list was last filtered, for ages.
    now: i64,
}

impl ProjectList {
    fn new(
        workspaces: Entity<Workspaces>,
        projects: Entity<Projects>,
        machines: Entity<Machines>,
        current: Place,
        cx: &mut App,
    ) -> Self {
        let mut list = Self {
            workspaces,
            projects,
            machines,
            known: Vec::new(),
            adding_on: current.machine.clone(),
            current,
            checked: None,
            hovered: None,
            query: String::new(),
            found: None,
            rows: Vec::new(),
            last_used: Vec::new(),
            now: 0,
        };
        list.sync(cx);
        list
    }

    /// Follows the project list: gives each project a workspace, so its
    /// repository is read, then lists them again.
    fn sync(&mut self, cx: &mut App) {
        let roots = self.projects.read(cx).roots().to_vec();
        self.known = self.workspaces.update(cx, |workspaces, cx| {
            roots
                .iter()
                .map(|root| workspaces.index(root, cx))
                .collect()
        });
        // A searched project that was just removed is no longer one.
        if let Some(Row::Project(ix)) = self.found {
            if !self.known.contains(&ix) {
                self.found = None;
            }
        }
        self.refilter(cx);
    }

    /// Lists what the search names as a path, then the projects whose name
    /// or path matches it, most recently used first.
    fn refilter(&mut self, cx: &App) {
        let workspaces = self.workspaces.read(cx);
        let current = workspaces
            .find(&[&self.current])
            .map_or_else(|| self.current.clone(), |ix| workspaces.get(ix).root());
        self.checked = self
            .known
            .iter()
            .copied()
            .find(|&ix| *workspaces.get(ix).place() == current);
        self.last_used = self
            .known
            .iter()
            .map(|&ix| (ix, workspaces.last_used_in(workspaces.get(ix).place())))
            .collect();
        let machines = self.machines.read(cx);
        let mut order = self.last_used.clone();
        order.sort_by(|&(a, a_used), &(b, b_used)| {
            let (a, b) = (workspaces.get(a), workspaces.get(b));
            b_used
                .cmp(&a_used)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.cwd().cmp(b.cwd()))
        });
        let query = self.query.to_lowercase();
        let found_ix = match self.found {
            Some(Row::Project(ix)) => Some(ix),
            Some(Row::Add(_)) | None => None,
        };
        self.rows = self
            .found
            .clone()
            .into_iter()
            .chain(
                order
                    .into_iter()
                    .map(|(ix, _)| ix)
                    .filter(|&ix| {
                        let workspace = workspaces.get(ix);
                        Some(ix) != found_ix
                            && (workspace.name.to_lowercase().contains(&query)
                                || workspace.path.to_lowercase().contains(&query)
                                || machines
                                    .tag(workspace.machine())
                                    .is_some_and(|tag| tag.to_lowercase().contains(&query)))
                    })
                    .map(Row::Project),
            )
            .collect();
        self.now = time::now_unix();
    }

    /// The row of the draft's project, if it matches the search.
    fn current_row(&self) -> Option<IndexPath> {
        let checked = self.checked?;
        let row = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Project(ix) if *ix == checked))?;
        Some(IndexPath::default().row(row))
    }

    fn project_item(&self, ix: usize, cx: &mut Context<'_, ListState<Self>>) -> ListItem {
        let workspace = self.workspaces.read(cx).get(ix);
        let checked = self.checked == Some(ix);
        let machines = self.machines.read(cx);
        let path = path_line(workspace, machines.tag(workspace.machine()));
        let root = workspace.place().clone();
        let trailing = if self.hovered == Some(ix) {
            let (projects, root) = (self.projects.clone(), root.clone());
            div()
                // The button acts on the row; it must not also pick it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Button::new("project-remove")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Remove from list")
                        .on_click(move |_, _, cx| remove(&projects, &root, cx)),
                )
                .into_any_element()
        } else {
            let last_used = self
                .last_used
                .iter()
                .find_map(|&(known, used)| (known == ix).then_some(used))
                .unwrap_or(0);
            div()
                .when(last_used > 0, |this| {
                    this.child(time::relative_age(self.now - last_used))
                })
                .into_any_element()
        };
        let projects = self.projects.clone();
        // Kit's `confirmed` check turns off the row's hover, so the check
        // is drawn here.
        let check = div().w_4().flex_shrink_0().when(checked, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
        });
        ListItem::new(element_id(&root))
            .accessibility_label(workspace.name.clone())
            .aria_toggled(if checked {
                Toggled::True
            } else {
                Toggled::False
            })
            .on_hover(cx.listener(move |list, hovered: &bool, _, cx| {
                let this = list.delegate_mut();
                if *hovered {
                    this.hovered = Some(ix);
                } else if this.hovered == Some(ix) {
                    this.hovered = None;
                }
                cx.notify();
            }))
            .child(
                row(
                    Badge::new(workspace.initials.clone(), rems(1.75)).into_any_element(),
                    workspace.name.clone(),
                    path,
                    cx,
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(trailing)
                        .child(check),
                )
                .context_menu(move |menu, _, _| {
                    let (projects, root) = (projects.clone(), root.clone());
                    menu.item(
                        PopupMenuItem::new("Remove from list")
                            .icon(Icon::new(IconName::Close))
                            .on_click(move |_, _, cx| remove(&projects, &root, cx)),
                    )
                }),
            )
    }
}

impl ListDelegate for ProjectList {
    type Item = ListItem;

    /// Matches the project name or its path. A query that is a path is
    /// checked in the background: a project on the list that it lies in
    /// comes first, and any other folder offers its project to add.
    ///
    /// Kit picks the first row when a search starts only if the last frame
    /// drew any, so a search that follows an empty one, as typing a new
    /// folder's path does, selects it here once its rows are known; Enter
    /// then takes it. Kit selects rows whether or not the search has the
    /// keyboard, so without it the selection is cleared here.
    fn perform_search(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<'_, ListState<Self>>,
    ) -> Task<()> {
        query.clone_into(&mut self.query);
        self.found = None;
        self.refilter(cx);
        let machine = self.adding_on.clone();
        let home = self.workspaces.read(cx).home(&machine);
        let path = workspaces::typed_path(query, home.as_deref());
        cx.spawn_in(window, async move |list, cx| {
            let checked = match path {
                // This machine's folders are read: the folder must exist,
                // and a git repository's project is its main checkout.
                Some(path) if machine.is_local() => {
                    cx.background_executor()
                        .spawn(async move {
                            // The project file may hold the typed or the
                            // resolved spelling.
                            let dir = path.canonicalize().ok().filter(|dir| dir.is_dir())?;
                            Some((repo::inspect(&path).root, repo::inspect(&dir).root))
                        })
                        .await
                }
                // Another machine's can't be: the path is taken as typed.
                Some(path) => Some((path.clone(), path)),
                None => None,
            };
            let _ = list.update_in(cx, |list, window, cx| {
                if let Some((typed, resolved)) = checked {
                    let this = list.delegate_mut();
                    let workspaces = this.workspaces.read(cx);
                    let typed = Place::new(machine.clone(), typed);
                    let resolved = Place::new(machine.clone(), resolved);
                    let known = this.known.iter().copied().find(|&ix| {
                        let place = workspaces.get(ix).place();
                        *place == typed || *place == resolved
                    });
                    this.found = Some(match known {
                        Some(ix) => Row::Project(ix),
                        None => Row::Add(Folder {
                            shown: workspaces::tilde(&resolved.path, home.as_deref()).into(),
                            root: resolved,
                        }),
                    });
                    this.refilter(cx);
                }
                let focused = list.focus_handle(cx).is_focused(window);
                if !focused {
                    list.set_selected_index(None, window, cx);
                } else if list.selected_index().is_none() && !list.delegate().rows.is_empty() {
                    list.set_selected_index(Some(IndexPath::default()), window, cx);
                }
                cx.notify();
            });
        })
    }

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.rows.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<'_, ListState<Self>>,
    ) -> Option<ListItem> {
        let radius = cx.theme().radius;
        let item = match self.rows.get(ix.row)? {
            Row::Add(folder) => ListItem::new("project-typed")
                .accessibility_label(folder.shown.clone())
                .child(row(
                    folder_tile(cx).into_any_element(),
                    format!("Add {}", folder_name(&folder.root.path)).into(),
                    div().child(folder.shown.clone()),
                    cx,
                )),
            Row::Project(project) => self.project_item(*project, cx),
        };
        Some(item.rounded(radius))
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<'_, ListState<Self>>,
    ) -> impl IntoElement {
        let (title, hint) = if self.known.is_empty() && self.query.is_empty() {
            (
                "No projects yet",
                "Add one with Add project…, or type its path",
            )
        } else {
            (
                "No matching projects",
                "Type a path such as ~/code/app to add it",
            )
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(title)
            .child(div().text_xs().child(hint))
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

/// Under a project's name: its machine, with more than one, its path, and
/// its branch.
fn path_line(workspace: &Workspace, machine: Option<SharedString>) -> Div {
    let server = || {
        Icon::new(gpui_kit::assets::IconName::Server)
            .xsmall()
            .flex_shrink_0()
    };
    h_flex()
        .gap_1()
        .min_w_0()
        .when_some(machine, |this, machine| {
            this.child(server())
                .child(div().flex_shrink_0().child(machine))
                .child(div().flex_shrink_0().child("·"))
        })
        .child(div().truncate().child(workspace.path.clone()))
        .when_some(workspace.branch.clone(), |this, branch| {
            this.child(
                Icon::new(gpui_kit::assets::IconName::GitBranch)
                    .xsmall()
                    .ml_1()
                    .flex_shrink_0(),
            )
            .child(div().truncate().child(branch))
        })
}

/// One two-line row: a tile, the name, and under it the path. Callers add
/// what goes on the right.
fn row(tile: gpui_kit::AnyElement, name: SharedString, path: impl IntoElement, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    h_flex().w_full().gap_2p5().py_0p5().child(tile).child(
        v_flex()
            .flex_1()
            .min_w_0()
            .child(Label::new(name).text_sm().truncate())
            .child(div().text_xs().text_color(muted).child(path)),
    )
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
