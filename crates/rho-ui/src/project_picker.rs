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
//! that machine's folders: the
//! subfolders the path can go on to are listed, and a folder that is there
//! offers to add its project. Add project… asks the desktop for a folder on
//! this machine, and starts the search at `~/` on a remote one, whose
//! folders are read over ssh (see [`folders`]). Remove takes a project off
//! the list. In the footer, a git project offers a new worktree for the
//! session instead of its checkout; see [`Checkout`].
//!
//! A project's age and the check read [`Workspaces`]: the saved sessions
//! date each folder, and its repository says which project it is in, so a
//! session in a worktree counts for its project.

use std::{collections::HashSet, path::Path};

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    group_box::{GroupBox, GroupBoxVariants as _},
    h_flex,
    label::Label,
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    separator::Separator,
    spinner::Spinner,
    tag::Tag,
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
    cloning::{self, Source},
    folders::{self, Reader, Typed},
    machines::{MachineId, MachineStatus, Machines, Place},
    projects::Projects,
    repo, time,
    workspaces::{self, Workspaces},
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

    /// With more than one machine, the button naming the machine the
    /// session runs on, with a menu to choose another.
    fn render_machine(&self, cx: &mut Context<'_, Self>) -> Option<impl IntoElement> {
        let list = self.list.read(cx).delegate();
        let machines = list.machines.read(cx);
        if !machines.several() {
            return None;
        }
        let current = list.current.machine.clone();
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
    /// checkout there, or with none, to the machine's home folder.
    fn choose_machine(&mut self, machine: MachineId, cx: &mut Context<'_, Self>) {
        let list = self.list.read(cx).delegate();
        // Already there, maybe in a worktree or subfolder: stay.
        if machine == list.current.machine {
            return;
        }
        let target = list.checkout_on(&machine, cx).unwrap_or_else(|| {
            // A daemon that didn't say its home: the root is a folder too.
            let home = list.workspaces.read(cx).home(&machine);
            Place::new(machine, home.unwrap_or_else(|| "/".into()))
        });
        cx.emit(PickerEvent::Picked(target));
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

    /// Checks the project of `cwd`, as the draft moved there, and clears
    /// the search. The list now shows its machine's checkouts.
    pub(crate) fn set_current(
        &mut self,
        cwd: Place,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.list.update(cx, |list, cx| {
            list.delegate_mut().current = cwd;
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

    /// Acts on the row at `ix`: starts the session in a project, adding a
    /// typed folder's project to the list first; goes into a subfolder; or
    /// looks for a project elsewhere on the chosen machine, at its path
    /// there.
    fn pick(&mut self, ix: IndexPath, window: &mut Window, cx: &mut Context<'_, Self>) {
        let list = self.list.read(cx).delegate();
        let root = match list.rows.get(ix.row).cloned() {
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
        cx.emit(PickerEvent::Picked(root));
    }

    /// Clones `source` into the draft's machine's clone folder, then adds
    /// the clone to the projects and starts the session there. One clone
    /// at a time; a failure says git's or ssh's words.
    fn clone_repo(&mut self, source: Source, window: &mut Window, cx: &mut Context<'_, Self>) {
        let list = self.list.read(cx).delegate();
        if list.cloning.is_some() {
            return;
        }
        let machine = list.current.machine.clone();
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
                        cx.emit(PickerEvent::Picked(root));
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

    /// Adds a project on the chosen machine: from the desktop's folder
    /// chooser on this one, else by browsing from `~/` in the search.
    fn add_project(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.list.read(cx).delegate().current.machine.is_local() {
            Self::browse(window, cx);
        } else {
            self.search("~/", window, cx);
        }
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
    /// The draft's directory, whose project is checked. Its machine is the
    /// one the session runs on, whose checkouts the list shows and whose
    /// folders a typed path names.
    current: Place,
    /// The draft's project, if it is on the list.
    checked: Option<usize>,
    /// The project under the mouse, which shows Remove instead of its age.
    hovered: Option<usize>,
    query: String,
    /// What the search names as a path, once looked up: the folder typed
    /// first, then the subfolders it can go on to.
    found: Vec<Row>,
    rows: Vec<Row>,
    /// How many rows an empty search lists.
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
        current: Place,
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
        let roots = self.projects.read(cx).roots().to_vec();
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
            let machine = self.machines.read(cx).label(&self.current.machine);
            h_flex()
                .gap_1()
                .child(Icon::new(gpui_kit::assets::IconName::Download).xsmall())
                .child(format!("Clone to {machine}"))
        };
        tag.child(content)
    }

    /// How the draft's machine's folders are read.
    fn reader(&self, cx: &App) -> Reader {
        self.workspaces.read(cx).reader(&self.current.machine)
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
        let draft = workspaces.get(workspaces.find(&[&self.current])?);
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
    /// first, then the projects only other machines hold.
    fn refilter(&mut self, cx: &App) {
        let workspaces = self.workspaces.read(cx);
        let machine = &self.current.machine;
        let current = workspaces
            .find(&[&self.current])
            .map_or_else(|| self.current.clone(), |ix| workspaces.get(ix).root());
        self.last_used = self
            .known
            .iter()
            .map(|&ix| (ix, workspaces.last_used_in(workspaces.get(ix).place())))
            .collect();
        let mut order = self.last_used.clone();
        order.sort_by(|&(a, a_used), &(b, b_used)| {
            let (a, b) = (workspaces.get(a), workspaces.get(b));
            b_used
                .cmp(&a_used)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.cwd().cmp(b.cwd()))
        });
        // Every checkout here, then for each project with none here its
        // most recently used one elsewhere.
        let on_machine = |ix: usize| workspaces.get(ix).machine() == machine;
        let projects_here: HashSet<&str> = order
            .iter()
            .filter(|&&(ix, _)| on_machine(ix))
            .map(|&(ix, _)| workspaces.get(ix).project_key())
            .collect();
        let mut elsewhere_seen = HashSet::new();
        let (here, elsewhere): (Vec<usize>, Vec<usize>) = order
            .iter()
            .map(|&(ix, _)| ix)
            .filter(|&ix| {
                let key = workspaces.get(ix).project_key();
                on_machine(ix) || (!projects_here.contains(key) && elsewhere_seen.insert(key))
            })
            .partition(|&ix| on_machine(ix));
        self.checked = here
            .iter()
            .copied()
            .find(|&ix| *workspaces.get(ix).place() == current);
        self.listed = here.len() + elsewhere.len();
        let query = self.query.to_lowercase();
        let name_matches = |ix: &usize| workspaces.get(*ix).name.to_lowercase().contains(&query);
        let matches = |ix: &usize| {
            name_matches(ix) || workspaces.get(*ix).path.to_lowercase().contains(&query)
        };
        let found = &self.found;
        self.rows = found
            .iter()
            .cloned()
            .chain(
                here.into_iter()
                    .filter(|ix| matches(ix) && !found.contains(&Row::Project(*ix)))
                    .map(Row::Project),
            )
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
            let last_used = self.used(ix);
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
                    div().truncate().child(workspace.path.clone()),
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

    /// A project with no checkout on the draft's machine, under its name
    /// the machine and path of its checkout `ix`: with Clone to its machine
    /// for a repository with an origin, else dimmed.
    fn elsewhere_item(&self, ix: usize, cx: &App) -> ListItem {
        let workspace = self.workspaces.read(cx).get(ix);
        let machines = self.machines.read(cx);
        let here = machines.label(&self.current.machine);
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
        let machine = self.current.machine.clone();
        let workspaces = self.workspaces.read(cx);
        let home = workspaces.home(&machine);
        let typed = Typed::parse(query, home.as_deref());
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
            Row::Add(folder) => ListItem::new("project-typed")
                .accessibility_label(folder.shown.clone())
                .child(row(
                    folder_tile(Icon::new(IconName::Plus), cx).into_any_element(),
                    format!("Add {}", folder_name(&folder.root.path)).into(),
                    div().child(folder.shown.clone()),
                    cx,
                )),
            Row::Next(folder) => ListItem::new(next_id(&folder.root))
                .accessibility_label(folder.shown.clone())
                .child(row(
                    folder_tile(Icon::new(IconName::Folder), cx).into_any_element(),
                    format!("{}/", folder_name(&folder.root.path)).into(),
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
        let (title, hint) = if self.listed == 0 && self.query.is_empty() {
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

fn folder_name(cwd: &Path) -> String {
    cwd.file_name().map_or_else(
        || cwd.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}
