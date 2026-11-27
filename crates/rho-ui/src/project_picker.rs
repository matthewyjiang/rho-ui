//! The project a new session starts in, chosen in the middle of the draft:
//! a searchable list of the user's [`Projects`], the one used most recently
//! first, with the draft's project checked. With more than one machine, the
//! footer chooses the machine the session runs on, and the list shows each
//! project's checkout there: a project is one repository, whichever
//! machines hold a checkout of it and whatever branch each is on. A project
//! with no checkout on that machine offers to clone it there, from its
//! origin remote, into that machine's clone folder (see [`cloning`]); one
//! outside git shows dimmed, and choosing it looks for its folder there. Switching machines moves the draft to its project's
//! checkout on the new one, else to that machine's home folder, so the
//! footer always names where the session runs. Which checkouts are one
//! project is [`crate::workspaces::Workspace::project_key`]'s to say.
//!
//! A git URL typed in the search offers to clone that repository, unless
//! the machine holds it already. Typing an absolute or `~/` path browses
//! that machine's folders: the subfolders the path can go on to are
//! listed, and a folder that is there offers to add its project. Add
//! project… opens rho-ui's folder chooser on the chosen machine, this one
//! or a remote one, whose folders are read over ssh (see
//! [`folder_chooser`] and [`folders`]). Remove takes a project off
//! the list. In the footer, a git project offers a new worktree for the
//! session instead of its checkout; see [`Checkout`]. The choice holds for
//! new sessions after it, across runs too ([`LastCheckout`]).
//!
//! Above the projects, No project starts the session in a scratch folder of
//! its own instead (see [`crate::no_project`]), for work that belongs to no
//! repository. It is never the draft's default.
//!
//! A project's age and the check read [`Workspaces`]: the saved sessions
//! date each folder, and its repository says which project it is in, so a
//! session in a worktree counts for its project.

use std::{collections::HashSet, path::Path};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    group_box::{GroupBox, GroupBoxVariants as _},
    h_flex,
    kbd::Kbd,
    label::Label,
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    notification::Notification,
    separator::Separator,
    spinner::Spinner,
    tag::Tag,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _,
    WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    accesskit::Toggled, div, px, rems, Anchor, App, AppContext as _, BorrowAppContext as _,
    Context, Div, Entity, EventEmitter, Focusable as _, Global, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
};

use rho_ui_core::Checkout;

use crate::{
    badge::Badge,
    cloning::{self, Source},
    folder_chooser::{self, Chosen},
    folders::{self, PathStyle, Reader, Typed},
    machines::{MachineId, MachineStatus, Machines, Place},
    projects::Projects,
    repo,
    session_list::contains_ignore_case,
    store::{Key, StateDb},
    time,
    workspaces::{self, Workspace, Workspaces},
};

/// The search field's height: Kit's medium input is `h_8`, 2 rem, over a
/// 1 px border.
const SEARCH_REMS: f32 = 2.;
/// A row's height: measured at 51.5 px with a 16 px rem (3.219 rem) and
/// 64.5 px with a 20 px rem (3.225 rem), in the default font and Noto Sans.
/// The added directories' list of projects draws the same rows.
pub(crate) const ROW_REMS: f32 = 3.22;
/// Rows the list shows before it scrolls.
const MAX_ROWS: u8 = 5;
/// Rows the list keeps room for, so the empty state has space.
const MIN_ROWS: u8 = 2;
/// What the row that starts a session in no project reads.
const NO_PROJECT: &str = "No project";

/// Where a draft's session starts, as its picker checks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    /// In this directory: a project's folder, or one in it.
    Folder(Place),
    /// In no project: in a scratch folder of its own on this machine.
    NoProject(MachineId),
}

impl Pick {
    pub(crate) fn machine(&self) -> &MachineId {
        match self {
            Pick::Folder(place) => &place.machine,
            Pick::NoProject(machine) => machine,
        }
    }

    fn folder(&self) -> Option<&Place> {
        match self {
            Pick::Folder(place) => Some(place),
            Pick::NoProject(_) => None,
        }
    }
}

/// What the picker tells its draft.
pub(crate) enum PickerEvent {
    /// The user chose where the new session starts.
    Picked(Pick),
    /// The user left the search with Escape: back to the composer.
    Dismissed,
}

/// The checkout the user last chose in any draft, remembered across runs
/// under [`Key::Checkout`]. A global, as each draft has its own picker and
/// a new one starts with the last choice.
#[derive(Debug)]
pub(crate) struct LastCheckout {
    checkout: Checkout,
    db: StateDb,
}

impl Global for LastCheckout {}

impl LastCheckout {
    /// Loads the saved choice: Local unless New worktree was chosen.
    pub(crate) fn load(db: &StateDb) -> Self {
        let checkout = match db.get(Key::Checkout).as_deref() {
            Some("new-worktree") => Checkout::NewWorktree,
            _ => Checkout::Local,
        };
        Self {
            checkout,
            db: db.clone(),
        }
    }

    fn get(cx: &App) -> Checkout {
        cx.try_global::<Self>()
            .map_or(Checkout::Local, |this| this.checkout)
    }

    /// Chooses `checkout`, and saves it.
    fn set(checkout: Checkout, cx: &mut App) {
        cx.update_global(|this: &mut Self, _| {
            this.checkout = checkout;
            let value = match checkout {
                Checkout::Local => "local",
                Checkout::NewWorktree => "new-worktree",
            };
            this.db.set(Key::Checkout, value);
        });
    }
}

/// The project list, for one draft.
pub(crate) struct ProjectPicker {
    list: Entity<ListState<ProjectList>>,
    projects: Entity<Projects>,
    /// What the folder chooser that Add project… opened chooses.
    chooser: Option<Subscription>,
    _subscriptions: [Subscription; 5],
}

impl EventEmitter<PickerEvent> for ProjectPicker {}

impl ProjectPicker {
    /// A picker over `projects` on `machines` with `current` checked.
    pub(crate) fn new(
        workspaces: &Entity<Workspaces>,
        projects: &Entity<Projects>,
        machines: &Entity<Machines>,
        current: Pick,
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
                    ListEvent::Confirm(ix) => this.pick(*ix, window, cx),
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
            chooser: None,
            _subscriptions: subscriptions,
        }
    }

    /// Where the session works: the project's checkout, or a new worktree
    /// of it, which the daemon adds only as the session starts. The user's
    /// last choice ([`LastCheckout`]), but the checkout outside git and in
    /// no project, which leaves the choice for the next git project.
    pub(crate) fn checkout(&self, cx: &App) -> Checkout {
        if self.in_repository(cx) {
            LastCheckout::get(cx)
        } else {
            Checkout::Local
        }
    }

    fn set_checkout(checkout: Checkout, cx: &mut Context<'_, Self>) {
        LastCheckout::set(checkout, cx);
        cx.notify();
    }

    /// Whether the draft's directory is in a git repository, as far as its
    /// repository has been read: only one has worktrees to add.
    fn in_repository(&self, cx: &App) -> bool {
        let list = self.list.read(cx).delegate();
        let workspaces = list.workspaces.read(cx);
        list.current
            .folder()
            .and_then(|folder| workspaces.find(&[folder]))
            .is_some_and(|ix| workspaces.get(ix).branch.is_some())
    }

    /// Where the draft's session starts, as checked.
    pub(crate) fn current<'a>(&self, cx: &'a App) -> &'a Pick {
        &self.list.read(cx).delegate().current
    }

    /// The button naming where the session works, with a menu to choose:
    /// the project's checkout, or a new worktree.
    fn render_checkout(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let in_repository = self.in_repository(cx);
        let current = self.checkout(cx);
        let label = match current {
            Checkout::NewWorktree => "New worktree",
            Checkout::Local => "Local",
        };
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
                                let _ = view.update(cx, |_, cx| Self::set_checkout(checkout, cx));
                            }),
                    )
                })
            })
    }

    /// With more than one machine, the button naming the machine the
    /// session runs on, with a menu to choose another.
    fn render_machine(&self, cx: &mut Context<'_, Self>) -> Option<impl IntoElement> {
        let list = self.list.read(cx).delegate();
        let machines = list.machines.read(cx);
        if !machines.several() {
            return None;
        }
        let current = list.current.machine().clone();
        // A machine not connected can't open the draft's session.
        let choices: Vec<(MachineId, SharedString, bool)> = machines
            .iter()
            .map(|machine| {
                let ready = matches!(
                    machines.status(&machine.id),
                    Some(MachineStatus::Ready { .. })
                );
                (machine.id.clone(), machine.label.clone(), ready)
            })
            .collect();
        let view = cx.entity().downgrade();
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
                .tooltip("The machine the session runs on")
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, _| {
                    choices.iter().fold(menu, |menu, (machine, label, ready)| {
                        let (view, machine) = (view.clone(), machine.clone());
                        menu.item(
                            PopupMenuItem::new(label.clone())
                                .checked(machine == current)
                                .disabled(!ready && machine != current)
                                .on_click(move |_, _, cx| {
                                    let _ = view.update(cx, |picker, cx| {
                                        picker.choose_machine(machine.clone(), cx);
                                    });
                                }),
                        )
                    })
                }),
        )
    }

    /// Runs the session on `machine`: moves the draft to its project's
    /// checkout there, or with none, to the machine's home folder; in no
    /// project, it stays in none.
    fn choose_machine(&mut self, machine: MachineId, cx: &mut Context<'_, Self>) {
        let list = self.list.read(cx).delegate();
        // Already there, maybe in a worktree or subfolder: stay.
        if machine == *list.current.machine() {
            return;
        }
        if let Pick::NoProject(_) = list.current {
            cx.emit(PickerEvent::Picked(Pick::NoProject(machine)));
            return;
        }
        let target = list.checkout_on(&machine, cx).unwrap_or_else(|| {
            // A daemon that didn't say its home: the root is a folder too.
            let home = list.workspaces.read(cx).home(&machine);
            Place::new(machine, home.unwrap_or_else(|| "/".into()))
        });
        cx.emit(PickerEvent::Picked(Pick::Folder(target)));
    }

    /// Puts `query` in the search, with the keyboard there, as browsing
    /// folders does.
    fn search(&mut self, query: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.list.update(cx, |list, cx| {
            list.set_query(query, window, cx);
            list.focus(window, cx);
        });
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

    /// Checks `pick`, as the draft moved there, and clears the search. The
    /// list now shows its machine's checkouts.
    pub(crate) fn set_current(
        &mut self,
        pick: Pick,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.list.update(cx, |list, cx| {
            list.delegate_mut().current = pick;
            list.set_query("", window, cx);
            cx.notify();
        });
    }

    /// Puts the keyboard in the search field, the current project
    /// selected, else the first project: Enter never takes No project
    /// unless the draft is in no project already.
    pub(crate) fn focus(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.list.update(cx, |list, cx| {
            let delegate = list.delegate();
            let first = delegate
                .rows
                .iter()
                .position(|row| *row != Row::NoProject)
                .map(|row| IndexPath::default().row(row));
            let row = delegate
                .current_row()
                .or(first)
                .or_else(|| (!delegate.rows.is_empty()).then(IndexPath::default));
            list.set_selected_index(row, window, cx);
            list.focus(window, cx);
        });
    }

    /// Acts on the row at `ix`: starts the session in a project, adding a
    /// typed folder's project to the list first, or in no project; goes
    /// into a subfolder; or looks for a project elsewhere on the chosen
    /// machine, at its path there.
    fn pick(&mut self, ix: IndexPath, window: &mut Window, cx: &mut Context<'_, Self>) {
        let list = self.list.read(cx).delegate();
        let root = match list.rows.get(ix.row).cloned() {
            Some(Row::NoProject) => {
                let machine = list.current.machine().clone();
                cx.emit(PickerEvent::Picked(Pick::NoProject(machine)));
                return;
            }
            Some(Row::Project(ix)) => list.workspaces.read(cx).get(ix).place().clone(),
            Some(Row::Add(folder)) => {
                add(&self.projects, folder.root.clone(), cx);
                folder.root
            }
            Some(Row::Next(folder)) => {
                self.search(&format!("{}/", folder.shown), window, cx);
                return;
            }
            Some(Row::Elsewhere(ix)) => {
                let workspace = list.workspaces.read(cx).get(ix);
                if let Some(source) = workspace.origin().and_then(Source::new) {
                    self.clone_repo(source, window, cx);
                } else {
                    let path = workspace.path.clone();
                    self.search(&path, window, cx);
                }
                return;
            }
            Some(Row::Clone(source)) => {
                self.clone_repo(source, window, cx);
                return;
            }
            Some(Row::Unread(_)) | None => return,
        };
        cx.emit(PickerEvent::Picked(Pick::Folder(root)));
    }

    /// Clones `source` into the draft's machine's clone folder, then adds
    /// the clone to the projects and starts the session there. One clone
    /// at a time in this picker, though another draft's can clone the same
    /// repository into the same folder at once: the later one then takes
    /// the finished clone, or fails saying the folder is taken or the clone
    /// incomplete. A failure says git's or ssh's words.
    fn clone_repo(&mut self, source: Source, window: &mut Window, cx: &mut Context<'_, Self>) {
        let list = self.list.read(cx).delegate();
        if list.cloning.is_some() {
            return;
        }
        let machine = list.current.machine().clone();
        let reader = list.reader(cx);
        let folder = list.machines.read(cx).clone_folder(&machine).to_string();
        let executor = cx.background_executor().clone();
        self.set_cloning(Some(source.url.clone()), cx);
        cx.spawn_in(window, async move |this, cx| {
            let cloned = cloning::clone(reader, source.clone(), folder, executor).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.set_cloning(None, cx);
                match cloned {
                    Ok(path) => {
                        let root = Place::new(machine, path);
                        add(&this.projects, root.clone(), cx);
                        cx.emit(PickerEvent::Picked(Pick::Folder(root)));
                    }
                    Err(why) => window.push_notification(
                        Notification::error(why).title(format!("Couldn't clone {}", source.name)),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    fn set_cloning(&mut self, url: Option<String>, cx: &mut Context<'_, Self>) {
        self.list.update(cx, |list, cx| {
            list.delegate_mut().cloning = url;
            cx.notify();
        });
        cx.notify();
    }

    /// The repository a URL in the search names, if the list offers to
    /// clone it.
    fn typed_source(&self, cx: &App) -> Option<Source> {
        self.list
            .read(cx)
            .delegate()
            .found
            .iter()
            .find_map(|row| match row {
                Row::Clone(source) => Some(source.clone()),
                _ => None,
            })
    }

    /// The footer's button: Clone while the search holds a git URL to
    /// clone, else Add project….
    fn render_add(&self, cx: &mut Context<'_, Self>) -> Button {
        let muted = cx.theme().muted_foreground;
        let Some(source) = self.typed_source(cx) else {
            return Button::new("project-add")
                .ghost()
                .small()
                .icon(Icon::new(IconName::Plus).text_color(muted))
                .label("Add project…")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.add_project(window, cx);
                }));
        };
        let cloning = self.list.read(cx).delegate().cloning.is_some();
        let label = if cloning {
            format!("Cloning {}…", source.folder())
        } else {
            format!("Clone {}", source.folder())
        };
        Button::new("project-clone")
            .primary()
            .small()
            .icon(Icon::new(gpui_kit::assets::IconName::Download))
            .label(label)
            .loading(cloning)
            .disabled(cloning)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.clone_repo(source.clone(), window, cx);
            }))
    }

    /// Opens the folder chooser on the chosen machine, whose folder's
    /// project the picker adds and picks.
    fn add_project(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let list = self.list.read(cx).delegate();
        let machine = list.current.machine().clone();
        let workspaces = list.workspaces.read(cx);
        let mut order: Vec<usize> = list
            .known
            .iter()
            .copied()
            .filter(|&ix| *workspaces.get(ix).machine() == machine)
            .collect();
        workspaces.sort_by_use(&mut order);
        let projects = order
            .into_iter()
            .map(|ix| workspaces.get(ix).place().path.clone())
            .collect();
        let (workspaces, machines) = (list.workspaces.clone(), list.machines.clone());
        let Some(chooser) =
            folder_chooser::open(machine, projects, &workspaces, &machines, window, cx)
        else {
            return;
        };
        self.chooser = Some(cx.subscribe_in(
            &chooser,
            window,
            |this, _, Chosen(folder): &Chosen, window, cx| {
                this.add_folder(folder.clone(), window, cx);
            },
        ));
    }

    /// Adds the project of `folder`, as [`repo::inspect`] finds it, and
    /// picks it.
    fn add_folder(&mut self, folder: Place, window: &mut Window, cx: &mut Context<'_, Self>) {
        let list = self.list.read(cx).delegate();
        let reader = list.workspaces.read(cx).reader(&folder.machine);
        let label = list.machines.read(cx).label(&folder.machine);
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let found = folders::resolve(reader, folder.path.clone(), executor).await;
            let _ = this.update_in(cx, |this, window, cx| match found {
                Ok(Some(found)) => {
                    let root = Place::new(folder.machine, found.root);
                    add(&this.projects, root.clone(), cx);
                    cx.emit(PickerEvent::Picked(Pick::Folder(root)));
                }
                Ok(None) => window.push_notification(
                    Notification::error(format!("{} isn't a folder", folder.path.display()))
                        .title("Couldn't add the project"),
                    cx,
                ),
                Err(why) => window.push_notification(
                    Notification::error(format!("Couldn't read {label}'s folders: {why}"))
                        .title("Couldn't add the project"),
                    cx,
                ),
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
            Row::NoProject => NO_PROJECT_ID.to_owned(),
            Row::Project(ix) => {
                element_id(delegate.workspaces.read(cx).get(*ix).place()).to_string()
            }
            Row::Elsewhere(ix) => {
                elsewhere_id(delegate.workspaces.read(cx).get(*ix).place()).to_string()
            }
            Row::Add(_) => "project-typed".to_owned(),
            Row::Next(folder) => next_id(&folder.root).to_string(),
            Row::Unread(_) => "project-unread".to_owned(),
            Row::Clone(_) => "project-clone-url".to_owned(),
        })
    }
}

/// The No project row's element ID.
const NO_PROJECT_ID: &str = "project-none";

/// A project row's element ID: `project-<machine>:<path>`.
fn element_id(place: &Place) -> SharedString {
    format!("project-{}:{}", place.machine, place.path.display()).into()
}

/// The element ID of a project's row on a machine without it, named for its
/// checkout `place` elsewhere: `project-elsewhere-<machine>:<path>`.
fn elsewhere_id(place: &Place) -> SharedString {
    format!(
        "project-elsewhere-{}:{}",
        place.machine,
        place.path.display()
    )
    .into()
}

/// A subfolder row's element ID: `project-next:<path>`.
fn next_id(place: &Place) -> SharedString {
    format!("project-next:{}", place.path.display()).into()
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

/// The right-click menu of a project row: Remove, which takes `root` off
/// the user's projects, unless it is in its machine's clone folder, which
/// keeps it on the list.
fn with_remove(menu: PopupMenu, projects: &Entity<Projects>, root: &Place, cx: &App) -> PopupMenu {
    if projects.read(cx).is_found(root) {
        return menu;
    }
    let (projects, root) = (projects.clone(), root.clone());
    menu.item(
        PopupMenuItem::new("Remove from list")
            .icon(Icon::new(IconName::Close))
            .on_click(move |_, _, cx| remove(&projects, &root, cx)),
    )
}

impl Render for ProjectPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let rem = window.rem_size();
        // Sized by every project, not the matches, so it holds still while
        // the search narrows it; browsing folders can grow it.
        let list = self.list.read(cx).delegate();
        let total = list.listed.max(list.rows.len());
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
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .children(Kbd::binding_for_action(
                                &crate::NewSessionIn,
                                Some("RhoApp"),
                                window,
                            ))
                            .child("to search"),
                    ),
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
                            .child(self.render_add(cx)),
                    )
                    .child(self.render_checkout(cx)),
            )
    }
}

/// A folder the search names on the chosen machine.
#[derive(Clone, PartialEq)]
struct Folder {
    /// For a folder to add, its project's folder (see [`repo::Repo::root`]);
    /// for one to go into, the folder.
    root: Place,
    /// Its path, with the home directory as `~`.
    shown: SharedString,
}

/// One row of the list.
#[derive(Clone, PartialEq)]
enum Row {
    /// No project: a scratch folder of the session's own.
    NoProject,
    /// A project's checkout on the chosen machine: the index of its folder
    /// in the workspaces.
    Project(usize),
    /// A project with no checkout on the chosen machine: the index of its
    /// most recently used one, on another.
    Elsewhere(usize),
    /// A typed folder's project, to add.
    Add(Folder),
    /// A subfolder the typed path can go on to.
    Next(Folder),
    /// Why the draft's machine's folders couldn't be read.
    Unread(SharedString),
    /// A repository a git URL in the search names, to clone.
    Clone(Source),
}

/// The projects for a searchable [`List`]: what a typed path names, if
/// anything, then the projects that match the search, then those on other
/// machines only.
struct ProjectList {
    workspaces: Entity<Workspaces>,
    projects: Entity<Projects>,
    machines: Entity<Machines>,
    /// Each project's index in the workspaces, in the order added.
    known: Vec<usize>,
    /// Where the draft's session starts, which is checked: its directory's
    /// project, or No project. Its machine is the one the session runs on,
    /// whose checkouts the list shows and whose folders a typed path
    /// names.
    current: Pick,
    /// The draft's project, if it is on the list.
    checked: Option<usize>,
    /// The project under the mouse, which shows Remove instead of its age.
    hovered: Option<usize>,
    query: String,
    /// What the search names as a path, once looked up: the folder typed
    /// first, then the subfolders it can go on to.
    found: Vec<Row>,
    rows: Vec<Row>,
    /// How many rows an empty search lists, No project's too.
    listed: usize,
    /// The URL of the repository being cloned.
    cloning: Option<String>,
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
        current: Pick,
        cx: &mut App,
    ) -> Self {
        let mut list = Self {
            workspaces,
            projects,
            machines,
            known: Vec::new(),
            current,
            checked: None,
            hovered: None,
            query: String::new(),
            found: Vec::new(),
            rows: Vec::new(),
            listed: 0,
            cloning: None,
            last_used: Vec::new(),
            now: 0,
        };
        list.sync(cx);
        list
    }

    /// Follows the project list: gives each project a workspace, so its
    /// repository is read, then lists them again.
    fn sync(&mut self, cx: &mut App) {
        let roots: Vec<Place> = self.projects.read(cx).roots().cloned().collect();
        self.known = self.workspaces.update(cx, |workspaces, cx| {
            roots
                .iter()
                .map(|root| workspaces.index(root, cx))
                .collect()
        });
        // A searched project that was just removed is no longer one.
        let known = &self.known;
        self.found
            .retain(|row| !matches!(row, Row::Project(ix) if !known.contains(ix)));
        self.refilter(cx);
    }

    /// What a row that clones `url` shows on its right: Clone to the
    /// draft's machine, or while it clones, a spinner.
    fn clone_tag(&self, url: &str, cx: &App) -> Tag {
        let tag = Tag::secondary().small().flex_shrink_0();
        let content = if self.cloning.as_deref() == Some(url) {
            h_flex()
                .gap_1()
                .child(Spinner::new().xsmall())
                .child("Cloning…")
        } else {
            let machine = self.machines.read(cx).label(self.current.machine());
            h_flex()
                .gap_1()
                .child(Icon::new(gpui_kit::assets::IconName::Download).xsmall())
                .child(format!("Clone to {machine}"))
        };
        tag.child(content)
    }

    /// How the draft's machine's folders are read.
    fn reader(&self, cx: &App) -> Reader {
        self.workspaces.read(cx).reader(self.current.machine())
    }

    /// Unix seconds of the newest session in the project at `ix`; 0 with
    /// none, or before [`Self::refilter`] dates it.
    fn used(&self, ix: usize) -> i64 {
        self.last_used
            .iter()
            .find_map(|&(known, used)| (known == ix).then_some(used))
            .unwrap_or(0)
    }

    /// The checkout on `machine` of the draft's project, the most recently
    /// used if it has several.
    fn checkout_on(&self, machine: &MachineId, cx: &App) -> Option<Place> {
        let workspaces = self.workspaces.read(cx);
        let draft = workspaces.get(workspaces.find(&[self.current.folder()?])?);
        self.known
            .iter()
            .map(|&ix| workspaces.get(ix))
            .filter(|workspace| {
                workspace.machine() == machine && workspace.project_key() == draft.project_key()
            })
            .max_by_key(|workspace| workspaces.last_used_in(workspace.place()))
            .map(|workspace| workspace.place().clone())
    }

    /// Lists what the search names as a path, then the projects on the
    /// draft's machine whose name or path matches it, most recently used
    /// first, with No project if its name matches, then the projects only
    /// other machines hold.
    fn refilter(&mut self, cx: &App) {
        let workspaces = self.workspaces.read(cx);
        let machine = self.current.machine();
        let current = self.current.folder().map(|folder| {
            workspaces
                .find(&[folder])
                .map_or_else(|| folder.clone(), |ix| workspaces.get(ix).root())
        });
        self.last_used = self
            .known
            .iter()
            .map(|&ix| (ix, workspaces.last_used_in(workspaces.get(ix).place())))
            .collect();
        let mut order = self.known.clone();
        workspaces.sort_by_use(&mut order);
        // Every checkout here, then for each project with none here its
        // most recently used one elsewhere.
        let on_machine = |ix: usize| workspaces.get(ix).machine() == machine;
        let projects_here: HashSet<&str> = order
            .iter()
            .filter(|&&ix| on_machine(ix))
            .map(|&ix| workspaces.get(ix).project_key())
            .collect();
        let mut elsewhere_seen = HashSet::new();
        let (here, elsewhere): (Vec<usize>, Vec<usize>) = order
            .into_iter()
            .filter(|&ix| {
                let key = workspaces.get(ix).project_key();
                on_machine(ix) || (!projects_here.contains(key) && elsewhere_seen.insert(key))
            })
            .partition(|&ix| on_machine(ix));
        self.checked = here
            .iter()
            .copied()
            .find(|&ix| current.as_ref() == Some(workspaces.get(ix).place()));
        // No project's row, then the projects'.
        self.listed = 1 + here.len() + elsewhere.len();
        let query = self.query.as_str();
        let name_matches = |ix: &usize| contains_ignore_case(&workspaces.get(*ix).name, query);
        let matches =
            |ix: &usize| name_matches(ix) || contains_ignore_case(&workspaces.get(*ix).path, query);
        // First with no search; after the projects a search finds, so the
        // row Kit selects as a search starts, which Enter takes, is one of
        // theirs.
        let no_project = contains_ignore_case(NO_PROJECT, query).then_some(Row::NoProject);
        let (before, after) = if query.is_empty() {
            (no_project, None)
        } else {
            (None, no_project)
        };
        let found = &self.found;
        self.rows = found
            .iter()
            .cloned()
            .chain(before)
            .chain(
                here.into_iter()
                    .filter(|ix| matches(ix) && !found.contains(&Row::Project(*ix)))
                    .map(Row::Project),
            )
            .chain(after)
            // By name only: their paths are on another machine, which a
            // typed path doesn't name.
            .chain(
                elsewhere
                    .into_iter()
                    .filter(name_matches)
                    .map(Row::Elsewhere),
            )
            .collect();
        self.now = time::now_unix();
    }

    /// The row of the draft's project, or No project, if it matches the
    /// search.
    fn current_row(&self) -> Option<IndexPath> {
        let row = match self.current {
            Pick::NoProject(_) => self.rows.iter().position(|row| *row == Row::NoProject)?,
            Pick::Folder(_) => {
                let checked = self.checked?;
                self.rows
                    .iter()
                    .position(|row| matches!(row, Row::Project(ix) if *ix == checked))?
            }
        };
        Some(IndexPath::default().row(row))
    }

    /// The draft's machine's scratch root, with the home directory as `~`,
    /// once found; what No project's row says it works in.
    fn scratch_path(&self, cx: &App) -> Option<SharedString> {
        let workspaces = self.workspaces.read(cx);
        let machine = self.current.machine();
        let root = workspaces.scratch_root(machine)?;
        Some(workspaces::tilde(root, workspaces.home(machine).as_deref()).into())
    }

    /// No project's row: a tile without a badge, its scratch root, and the
    /// check while the draft is in no project.
    fn no_project_item(&self, cx: &App) -> ListItem {
        let toggled = if matches!(self.current, Pick::NoProject(_)) {
            Toggled::True
        } else {
            Toggled::False
        };
        ListItem::new(NO_PROJECT_ID)
            .accessibility_label(NO_PROJECT)
            .aria_toggled(toggled)
            .child(
                row(
                    folder_tile(Icon::new(gpui_kit::assets::IconName::SquareDashed), cx)
                        .into_any_element(),
                    NO_PROJECT.into(),
                    div().truncate().child(
                        self.scratch_path(cx)
                            .unwrap_or_else(|| "A folder of its own".into()),
                    ),
                    cx,
                )
                .child(check(toggled, cx)),
            )
    }

    fn project_item(&self, ix: usize, cx: &mut Context<'_, ListState<Self>>) -> ListItem {
        let workspace = self.workspaces.read(cx).get(ix);
        let toggled = if self.checked == Some(ix) {
            Toggled::True
        } else {
            Toggled::False
        };
        let root = workspace.place().clone();
        let removable = !self.projects.read(cx).is_found(&root);
        let trailing = if removable && self.hovered == Some(ix) {
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
            let last_used = self.used(ix);
            div()
                .when(last_used > 0, |this| {
                    this.child(time::relative_age(self.now - last_used))
                })
                .into_any_element()
        };
        let projects = self.projects.clone();
        ListItem::new(element_id(&root))
            .accessibility_label(workspace.name.clone())
            .aria_toggled(toggled)
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
                project_row(workspace, cx)
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(trailing)
                            .child(check(toggled, cx)),
                    )
                    .context_menu(move |menu, _, cx| with_remove(menu, &projects, &root, cx)),
            )
    }

    /// A project with no checkout on the draft's machine, under its name
    /// the machine and path of its checkout `ix`: with Clone to its machine
    /// for a repository with an origin, else dimmed.
    fn elsewhere_item(&self, ix: usize, cx: &App) -> ListItem {
        let workspace = self.workspaces.read(cx).get(ix);
        let machines = self.machines.read(cx);
        let here = machines.label(self.current.machine());
        let trailing = match workspace.origin() {
            Some(origin) => self.clone_tag(origin, cx).into_any_element(),
            None => div()
                .flex_shrink_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("Not on {here}"))
                .into_any_element(),
        };
        let (projects, root) = (self.projects.clone(), workspace.place().clone());
        let path = h_flex()
            .gap_1()
            .min_w_0()
            .child(
                Icon::new(gpui_kit::assets::IconName::Server)
                    .xsmall()
                    .flex_shrink_0(),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .child(machines.label(workspace.machine())),
            )
            .child(div().flex_shrink_0().child("·"))
            .child(div().truncate().child(workspace.path.clone()));
        ListItem::new(elsewhere_id(workspace.place()))
            .accessibility_label(workspace.name.clone())
            .child(
                row(
                    Badge::new(workspace.initials.clone(), rems(1.75)).into_any_element(),
                    workspace.name.clone(),
                    path,
                    cx,
                )
                .when(workspace.origin().is_none(), |this| this.opacity(0.6))
                .child(trailing)
                // Its machine may be gone from the settings, so it can be
                // removed from here.
                .context_menu(move |menu, _, cx| with_remove(menu, &projects, &root, cx)),
            )
    }
}

impl ListDelegate for ProjectList {
    type Item = ListItem;

    /// Matches the project name or its path. A query that is a path is
    /// looked up on the draft's machine in the background (see
    /// [`folders::look`]): a project on the list that it lies in comes
    /// first, any other folder offers its project to add, and the
    /// subfolders it can go on to follow. Until the lookup answers, the
    /// subfolders found for the last query that still match stay, so the
    /// list doesn't jump at each key.
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
        let machine = self.current.machine().clone();
        let workspaces = self.workspaces.read(cx);
        let home = workspaces.home(&machine);
        let typed = Typed::parse(query, home.as_deref(), PathStyle::of(&machine));
        self.found
            .retain(|row| matches!((row, &typed), (Row::Next(folder), Some(typed)) if typed.leads_to(&folder.root.path)));
        // A git URL: the machine's checkout of it, else a clone of it.
        if let Some(source) = Source::typed(query) {
            let held = self.known.iter().copied().find(|&ix| {
                let workspace = workspaces.get(ix);
                *workspace.machine() == machine && workspace.project_key() == source.folder()
            });
            self.found = vec![held.map_or(Row::Clone(source), Row::Project)];
        }
        let reader = self.reader(cx);
        self.refilter(cx);
        let label = self.machines.read(cx).label(&machine);
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |list, cx| {
            let lookup = match typed {
                Some(typed) => Some(folders::look(reader, typed, executor).await),
                None => None,
            };
            let _ = list.update_in(cx, |list, window, cx| {
                let this = list.delegate_mut();
                let workspaces = this.workspaces.read(cx);
                let shown = |path: &Path| -> SharedString {
                    workspaces::tilde(path, home.as_deref()).into()
                };
                let folder = |path: &Path| Folder {
                    shown: shown(path),
                    root: Place::new(machine.clone(), path),
                };
                match lookup {
                    // Not a path: the rows found for it stay.
                    None => {}
                    Some(Ok(lookup)) => {
                        let typed_row = lookup.found.map(|found| {
                            let known = this.known.iter().copied().find(|&ix| {
                                let at = workspaces.get(ix).place();
                                at.machine == machine
                                    && found.spellings().any(|path| at.path == path)
                            });
                            known.map_or_else(|| Row::Add(folder(&found.root)), Row::Project)
                        });
                        let next = lookup.next.iter().map(|path| Row::Next(folder(path)));
                        this.found = typed_row.into_iter().chain(next).collect();
                    }
                    Some(Err(why)) => {
                        let why = format!("Couldn't read {label}'s folders: {why}");
                        this.found = vec![Row::Unread(why.into())];
                    }
                }
                this.refilter(cx);
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
            Row::NoProject => self.no_project_item(cx),
            Row::Add(folder) => ListItem::new("project-typed")
                .accessibility_label(folder.shown.clone())
                .child(row(
                    folder_tile(Icon::new(IconName::Plus), cx).into_any_element(),
                    format!("Add {}", repo::folder_name(&folder.root.path)).into(),
                    div().child(folder.shown.clone()),
                    cx,
                )),
            Row::Next(folder) => ListItem::new(next_id(&folder.root))
                .accessibility_label(folder.shown.clone())
                .child(row(
                    folder_tile(Icon::new(IconName::Folder), cx).into_any_element(),
                    format!("{}/", repo::folder_name(&folder.root.path)).into(),
                    div().child(folder.shown.clone()),
                    cx,
                )),
            Row::Project(project) => self.project_item(*project, cx),
            Row::Elsewhere(project) => self.elsewhere_item(*project, cx),
            Row::Clone(source) => {
                let tag = self.clone_tag(&source.url, cx);
                ListItem::new("project-clone-url")
                    .accessibility_label(format!("Clone {}", source.name))
                    .child(
                        row(
                            folder_tile(Icon::new(gpui_kit::assets::IconName::Download), cx)
                                .into_any_element(),
                            format!("Clone {}", source.name).into(),
                            div().truncate().child(source.url.clone()),
                            cx,
                        )
                        .child(tag),
                    )
            }
            Row::Unread(why) => ListItem::new("project-unread")
                .disabled(true)
                .accessibility_label(why.clone())
                .child(
                    div()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(why.clone()),
                ),
        };
        Some(item.rounded(radius))
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<'_, ListState<Self>>,
    ) -> impl IntoElement {
        // No project lists for an empty search, so only a search empties
        // the list.
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
                    .child("Type a path such as ~/code/app to add it"),
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

/// A project's row: its badge, its name, and under it its path, with the
/// home directory as `~`. Callers add what goes on the right. The added
/// directories' list of projects draws the same.
pub(crate) fn project_row(workspace: &Workspace, cx: &App) -> Div {
    row(
        Badge::new(workspace.initials.clone(), rems(1.75)).into_any_element(),
        workspace.name.clone(),
        div().truncate().child(workspace.path.clone()),
        cx,
    )
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

/// The check of the row the draft starts in, or the space it takes. Kit's
/// `confirmed` check turns off the row's hover, so it is drawn here.
fn check(toggled: Toggled, cx: &App) -> impl IntoElement {
    div()
        .w_4()
        .flex_shrink_0()
        .when(toggled == Toggled::True, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
        })
}

/// `icon` in a tile the size of a project badge.
fn folder_tile(icon: Icon, cx: &App) -> impl IntoElement {
    h_flex()
        .flex_shrink_0()
        .size(rems(1.75))
        .justify_center()
        .rounded(rems(0.25))
        .bg(cx.theme().secondary)
        .child(icon.small())
}
