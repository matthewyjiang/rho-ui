//! rho-ui's own folder chooser, the same for this machine and a remote
//! one: a dialog that browses one machine's folders and tells its caller
//! the folder the user chose ([`Chosen`]). The path bar goes back, forward
//! and up, shows the folder as clickable segments, and takes a typed path
//! by its machine's rules ([`PathStyle`]); what follows the last separator
//! filters the folders, and Tab completes it. Places lead to home, the
//! folders the machine's projects are in, and its recent projects. Folders
//! are read with [`folders::list`], one ssh script a folder on a remote
//! machine, which also tells its git checkouts and their branches.
//!
//! Add project… in the project picker opens it ([`open`]); what choosing a
//! folder does is the caller's.

use std::path::{Path, PathBuf};

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    breadcrumb::{Breadcrumb, BreadcrumbItem},
    button::{Button, ButtonVariants as _},
    dialog::{DialogFooter, DialogTitle},
    h_flex,
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    separator::Separator,
    spinner::Spinner,
    tag::Tag,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _,
    WindowExt as _,
};
use gpui_kit::{
    actions, div, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, Focusable as _,
    Hsla, InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity,
    Window,
};

use crate::{
    folders::{self, PathStyle, Subfolder},
    machines::{MachineId, MachineStatus, Machines, Place},
    repo,
    workspaces::{self, Workspaces},
};

actions!(
    folder_chooser,
    [
        /// Go into the selected folder.
        Open,
        /// Choose the selected folder, or the one shown.
        Choose,
        /// Complete the name typed in the path bar.
        Complete,
        /// Select the folder above.
        SelectPrev,
        /// Select the folder below.
        SelectNext,
        /// Go to the folder the one shown is in.
        Up,
        /// Go back to the folder shown before.
        Back,
        /// Go forward again.
        Forward
    ]
);

/// The chooser's key context.
const CONTEXT: &str = "FolderChooser";

/// The dialog's width: the mockup's 760 px at a 16 px rem, which leaves
/// room at the smallest window (720 px wide) only once clamped to it.
const WIDTH_REMS: f32 = 47.5;
/// The height of the places and the folders: the mockup's 300 px.
const BODY_REMS: f32 = 18.75;
/// The width of the places: the mockup's 170 px.
const PLACES_REMS: f32 = 10.6;
/// The recent projects the places list: as many as the project list shows
/// before it scrolls (`project_picker::MAX_ROWS`); the rest are a search
/// away there.
const RECENT: usize = 5;
/// Path segments the path bar shows before it leaves out the middle ones:
/// the top, then the last three. Each is a click target, so deep paths
/// keep the ones near the folder shown, and the bar keeps room to type.
const CRUMBS: usize = 4;

/// Binds the chooser's keys. Its path bar holds the keyboard, so Enter,
/// Tab and the arrows reach these through Kit's single-line input, which
/// leaves them alone. Call it after `gpui_kit::init`, whose bindings it
/// overrides.
pub(crate) fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    let mac = cx.platform_info().is_mac();
    // Kit's input takes Cmd+Up to its start on macOS, single-line too; a
    // binding as deep as its own, bound after it, wins.
    let up_context = Some(if mac {
        "FolderChooser > Input"
    } else {
        CONTEXT
    });
    cx.bind_keys([
        KeyBinding::new("enter", Open, context),
        KeyBinding::new(
            if mac { "cmd-enter" } else { "ctrl-enter" },
            Choose,
            context,
        ),
        KeyBinding::new("tab", Complete, context),
        KeyBinding::new("up", SelectPrev, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new(if mac { "cmd-up" } else { "alt-up" }, Up, up_context),
        KeyBinding::new(if mac { "cmd-[" } else { "alt-left" }, Back, context),
        KeyBinding::new(if mac { "cmd-]" } else { "alt-right" }, Forward, context),
    ]);
}

/// The folder the user chose, on the chooser's machine.
pub(crate) struct Chosen(pub(crate) Place);

/// Opens the chooser on `machine`, in the folder its most recently used
/// project is in, else its home folder. `projects` are its
/// projects' folders there, the most recently used first: the places list
/// the folders they are in and the first few, and the folders they are
/// show as added. `None` while another dialog is open.
pub(crate) fn open(
    machine: MachineId,
    projects: Vec<PathBuf>,
    workspaces: &Entity<Workspaces>,
    machines: &Entity<Machines>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<FolderChooser>> {
    if window.has_active_dialog(cx) {
        return None;
    }
    let view = cx.new(|cx| FolderChooser::new(machine, projects, workspaces, machines, window, cx));
    let first = std::cell::Cell::new(true);
    let chooser = view.clone();
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            let path = view.read(cx).path.clone();
            path.update(cx, |path, cx| path.focus(window, cx));
        }
        let view = view.clone();
        let width = (window.rem_size() * WIDTH_REMS).min(window.viewport_size().width * 0.92);
        dialog
            .w(width)
            .content(move |content, _, _| content.child(view.clone()))
    });
    Some(chooser)
}

/// The chooser: one machine's folders, one at a time.
pub(crate) struct FolderChooser {
    machine: MachineId,
    style: PathStyle,
    home: Option<PathBuf>,
    places: Places,
    workspaces: Entity<Workspaces>,
    machines: Entity<Machines>,
    /// The folders shown before the one shown, and after it, once back.
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// The path bar: what is typed after the folder shown.
    path: Entity<InputState>,
    list: Entity<ListState<FolderList>>,
    /// The read of the folder shown; dropped with the next.
    reading: Task<()>,
    _subscriptions: [Subscription; 4],
}

impl EventEmitter<Chosen> for FolderChooser {}

/// The places beside the folders.
struct Places {
    /// The folders the machine's projects are in, by their most recent
    /// project, home left out, as it has a place of its own.
    parents: Vec<PathBuf>,
    /// The most recently used projects.
    recent: Vec<PathBuf>,
}

impl Places {
    fn new(projects: &[PathBuf], home: Option<&Path>, style: PathStyle) -> Self {
        let mut parents: Vec<PathBuf> = Vec::new();
        for parent in projects.iter().filter_map(|project| style.parent(project)) {
            if Some(parent.as_path()) != home && !parents.contains(&parent) {
                parents.push(parent);
            }
        }
        Self {
            parents,
            recent: projects.iter().take(RECENT).cloned().collect(),
        }
    }
}

impl FolderChooser {
    fn new(
        machine: MachineId,
        projects: Vec<PathBuf>,
        workspaces: &Entity<Workspaces>,
        machines: &Entity<Machines>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let style = PathStyle::of(&machine);
        let home = workspaces.read(cx).home(&machine);
        // Where its projects live, as the most recent one, else home.
        let start = projects
            .first()
            .and_then(|project| style.parent(project))
            .or_else(|| home.clone())
            .unwrap_or_else(|| PathBuf::from("/"));
        let path = cx.new(|cx| InputState::new(window, cx).placeholder("Type a path"));
        let places = Places::new(&projects, home.as_deref(), style);
        let delegate = FolderList {
            chooser: cx.weak_entity(),
            style,
            folder: start,
            contents: Contents::Reading,
            filter: String::new(),
            rows: Vec::new(),
            projects,
        };
        let list = cx.new(|cx| ListState::new(delegate, window, cx));
        let list_focus = list.read(cx).focus_handle(cx);
        let subscriptions = [
            cx.subscribe_in(&path, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.typed(window, cx);
                }
            }),
            cx.subscribe_in(&list, window, |this, _, event: &ListEvent, window, cx| {
                if let ListEvent::Confirm(_) = event {
                    this.open_selected(window, cx);
                }
            }),
            // The keyboard stays in the path bar, which a click in the list
            // would take it from. Focus events need an active window, which
            // headless UI tests lack.
            cx.on_focus(&list_focus, window, |this, window, cx| {
                this.path.update(cx, |path, cx| path.focus(window, cx));
            }),
            // The title's dot follows the machine's connection.
            cx.observe(machines, |_, _, cx| cx.notify()),
        ];
        let mut chooser = Self {
            machine,
            style,
            home,
            places,
            workspaces: workspaces.clone(),
            machines: machines.clone(),
            back: Vec::new(),
            forward: Vec::new(),
            path,
            list,
            reading: Task::ready(()),
            _subscriptions: subscriptions,
        };
        chooser.read(window, cx);
        chooser
    }

    fn folder<'a>(&self, cx: &'a App) -> &'a Path {
        &self.list.read(cx).delegate().folder
    }

    /// Goes to `folder`, from which Back returns, with `filter` typed.
    fn visit(
        &mut self,
        folder: PathBuf,
        filter: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let from = self.folder(cx).to_path_buf();
        if folder != from {
            self.back.push(from);
            self.forward.clear();
        }
        self.show(folder, filter, window, cx);
    }

    /// Shows `folder` with `filter` typed after it, and reads it.
    fn show(
        &mut self,
        folder: PathBuf,
        filter: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // Setting the text doesn't report a change, so this isn't typed.
        self.path
            .update(cx, |path, cx| path.set_value(filter.to_owned(), window, cx));
        self.list.update(cx, |list, _| {
            let delegate = list.delegate_mut();
            delegate.folder = folder;
            filter.clone_into(&mut delegate.filter);
        });
        self.read(window, cx);
    }

    /// Reads the folder shown, in place of any read under way.
    fn read(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let folder = self.folder(cx).to_path_buf();
        self.list.update(cx, |list, cx| {
            let delegate = list.delegate_mut();
            delegate.contents = Contents::Reading;
            delegate.refilter();
            list.set_selected_index(None, window, cx);
            cx.notify();
        });
        let reader = self.workspaces.read(cx).reader(&self.machine);
        let label = self.machines.read(cx).label(&self.machine);
        let name = self
            .style
            .crumbs(&folder, self.home.as_deref())
            .pop()
            .map_or_else(String::new, |(name, _)| name);
        let executor = cx.background_executor().clone();
        let list = self.list.downgrade();
        self.reading = cx.spawn_in(window, async move |_, cx| {
            let listing = folders::list(reader, folder, executor).await;
            let _ = list.update_in(cx, |list, window, cx| {
                let delegate = list.delegate_mut();
                delegate.contents = match listing {
                    Ok(Ok(listing)) => Contents::Listed(listing),
                    Ok(Err(why)) => Contents::Unread(format!("Couldn't read {name}: {why}").into()),
                    Err(why) => {
                        Contents::Unread(format!("Couldn't read {label}'s folders: {why}").into())
                    }
                };
                delegate.refilter();
                let first = delegate.first_match();
                list.set_selected_index(first, window, cx);
                cx.notify();
            });
        });
        cx.notify();
    }

    /// Takes what the path bar holds: folders typed before its last
    /// separator are gone to, and the rest filters the folders.
    fn typed(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let text = self.path.read(cx).value().to_string();
        let to = self.style.split_typed(&text).and_then(|(head, tail)| {
            let folder = self
                .style
                .resolve(self.folder(cx), self.home.as_deref(), head)?;
            Some((folder, tail.to_owned()))
        });
        if let Some((folder, tail)) = to {
            self.visit(folder, &tail, window, cx);
            return;
        }
        self.filter(text, window, cx);
    }

    fn filter(&mut self, filter: String, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.list.update(cx, |list, cx| {
            let delegate = list.delegate_mut();
            delegate.filter = filter;
            delegate.refilter();
            let first = delegate.first_match();
            list.set_selected_index(first, window, cx);
            list.scroll_to_selected_item(window, cx);
            cx.notify();
        });
    }

    /// The selected folder's path.
    fn selected(&self, cx: &App) -> Option<PathBuf> {
        let list = self.list.read(cx);
        list.delegate().path_at(list.selected_index()?.row)
    }

    /// What Add adds: the selected folder, else the one shown, unless it
    /// couldn't be read.
    fn target(&self, cx: &App) -> Option<PathBuf> {
        self.selected(cx).or_else(|| {
            let delegate = self.list.read(cx).delegate();
            (!matches!(delegate.contents, Contents::Unread(_))).then(|| delegate.folder.clone())
        })
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(folder) = self.selected(cx) {
            self.visit(folder, "", window, cx);
        }
    }

    fn choose(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(folder) = self.target(cx) {
            cx.emit(Chosen(Place::new(self.machine.clone(), folder)));
            window.close_dialog(cx);
        }
    }

    /// Completes the name typed: goes into the one folder it matches, or
    /// types on to what all its matches start with. With nothing to
    /// complete, Tab moves the focus on, as elsewhere.
    fn complete(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let delegate = self.list.read(cx).delegate();
        let matches: Vec<&Subfolder> = delegate.matches().collect();
        match matches.as_slice() {
            [] => cx.propagate(),
            [only] => {
                let folder = self.style.join(&delegate.folder, &only.name);
                self.visit(folder, "", window, cx);
            }
            several => {
                let names: Vec<String> = several
                    .iter()
                    .map(|subfolder| subfolder.name.to_string_lossy().into_owned())
                    .collect();
                let common = common_prefix(&names);
                if common.chars().count() > delegate.filter.chars().count() {
                    self.path
                        .update(cx, |path, cx| path.set_value(common.clone(), window, cx));
                    self.filter(common, window, cx);
                } else {
                    cx.propagate();
                }
            }
        }
    }

    /// Moves the selection `step` folders, round the ends.
    fn step(&mut self, step: isize, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.list.update(cx, |list, cx| {
            let count = list.delegate().folder_rows();
            if count == 0 {
                return;
            }
            let row = match list.selected_index() {
                Some(at) => at
                    .row
                    .cast_signed()
                    .saturating_add(step)
                    .rem_euclid(count.cast_signed()),
                None if step < 0 => count.cast_signed() - 1,
                None => 0,
            };
            list.set_selected_index(Some(IndexPath::new(row.cast_unsigned())), window, cx);
            list.scroll_to_selected_item(window, cx);
            cx.notify();
        });
    }

    fn up(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(parent) = self.style.parent(self.folder(cx)) {
            self.visit(parent, "", window, cx);
        }
    }

    fn go_back(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(folder) = self.back.pop() {
            self.forward.push(self.folder(cx).to_path_buf());
            self.show(folder, "", window, cx);
        }
    }

    fn go_forward(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(folder) = self.forward.pop() {
            self.back.push(self.folder(cx).to_path_buf());
            self.show(folder, "", window, cx);
        }
    }

    /// A folder's path as the chooser shows it: home as `~`.
    fn shown(&self, path: &Path) -> String {
        workspaces::tilde(path, self.home.as_deref())
    }

    fn render_title(&self, cx: &App) -> impl IntoElement {
        let machines = self.machines.read(cx);
        let theme = cx.theme();
        let color: Hsla = match machines.status(&self.machine) {
            Some(MachineStatus::Ready { .. }) => theme.success,
            Some(MachineStatus::Connecting) => theme.warning,
            Some(_) => theme.danger,
            None => theme.muted_foreground,
        };
        DialogTitle::new().child(
            h_flex().gap_2().child("Add a project on").child(
                // Kit's Tag holds a label; its dot is a plain circle,
                // as Kit's dot Badge only marks another element.
                Tag::secondary().small().child(
                    h_flex()
                        .gap_1p5()
                        .child(div().size_2().rounded_full().bg(color))
                        .child(machines.label(&self.machine)),
                ),
            ),
        )
    }

    fn render_path_bar(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let folder = self.folder(cx).to_path_buf();
        let mut crumbs = self.style.crumbs(&folder, self.home.as_deref());
        let last = crumbs.pop();
        let view = cx.weak_entity();
        let crumb = |(name, path): (String, PathBuf)| {
            let view = view.clone();
            BreadcrumbItem::new(name).on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| this.visit(path.clone(), "", window, cx));
            })
        };
        let mut items: Vec<BreadcrumbItem> = Vec::new();
        if crumbs.len() > CRUMBS {
            let tail = crumbs.split_off(crumbs.len() - (CRUMBS - 2));
            items.extend(crumbs.into_iter().next().map(crumb));
            items.push(BreadcrumbItem::new("…").disabled(true));
            items.extend(tail.into_iter().map(crumb));
        } else {
            items.extend(crumbs.into_iter().map(crumb));
        }
        items.extend(last.map(|(name, _)| BreadcrumbItem::new(name)));
        let nav = |id: &'static str, icon: IconName, label: &'static str, enabled: bool| {
            Button::new(id)
                .ghost()
                .small()
                .icon(icon)
                .accessibility_label(label)
                .tooltip(label)
                .disabled(!enabled)
        };
        h_flex()
            .gap_1()
            .child(
                nav(
                    "folder-back",
                    IconName::ArrowLeft,
                    "Back",
                    !self.back.is_empty(),
                )
                .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
            )
            .child(
                nav(
                    "folder-forward",
                    IconName::ArrowRight,
                    "Forward",
                    !self.forward.is_empty(),
                )
                .on_click(cx.listener(|this, _, window, cx| this.go_forward(window, cx))),
            )
            .child(
                nav(
                    "folder-up",
                    IconName::ArrowUp,
                    "Up",
                    self.style.parent(&folder).is_some(),
                )
                .on_click(cx.listener(|this, _, window, cx| this.up(window, cx))),
            )
            .child(
                div()
                    .id("folder-path")
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .child(
                        Input::new(&self.path)
                            .prefix(Breadcrumb::new().children(items).flex_shrink_0())
                            .cleanable(false),
                    ),
            )
    }

    fn render_places(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let folder = self.folder(cx).to_path_buf();
        let place =
            |id: String, icon: Icon, label: String, path: &Path, cx: &mut Context<'_, Self>| {
                let to = path.to_path_buf();
                ListItem::new(SharedString::from(id))
                    .selected(folder == path)
                    .accessibility_label(label.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.visit(to.clone(), "", window, cx);
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .min_w_0()
                            .child(icon.small().text_color(cx.theme().muted_foreground))
                            .child(div().truncate().child(label)),
                    )
            };
        let heading = |text: &'static str, cx: &App| {
            div()
                .px_2()
                .pt_2()
                .pb_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(text)
        };
        let mut column = v_flex().gap_0p5().child(heading("Places", cx));
        if let Some(home) = self.home.clone() {
            column = column.child(place(
                format!("folder-place:{}", home.display()),
                Icon::new(gpui_kit::assets::IconName::House),
                "Home".to_owned(),
                &home,
                cx,
            ));
        }
        for parent in self.places.parents.clone() {
            column = column.child(place(
                format!("folder-place:{}", parent.display()),
                Icon::new(IconName::Folder),
                self.shown(&parent),
                &parent,
                cx,
            ));
        }
        if !self.places.recent.is_empty() {
            column = column.child(heading("Recent projects", cx));
        }
        for project in self.places.recent.clone() {
            column = column.child(place(
                format!("folder-recent:{}", project.display()),
                Icon::new(gpui_kit::assets::IconName::FolderGit2),
                repo::folder_name(&project),
                &project,
                cx,
            ));
        }
        div()
            .id("folder-places")
            .w(gpui_kit::rems(PLACES_REMS))
            .flex_shrink_0()
            .h_full()
            .overflow_y_scroll()
            .pr_1()
            .child(column)
    }

    fn render_footer(&self, window: &Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let target = self.target(cx);
        let adds = target.as_ref().map(|path| self.shown(path));
        let hint = |action: &dyn gpui_kit::Action, what: &'static str| {
            h_flex()
                .gap_1()
                .children(Kbd::binding_for_action(action, Some(CONTEXT), window))
                .child(what)
        };
        h_flex()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .id("folder-adds")
                    .test_support()
                    .aria_label(SharedString::from(
                        target
                            .as_ref()
                            .map_or_else(String::new, |path| path.display().to_string()),
                    ))
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(match &adds {
                        Some(path) => format!("Adds {path}"),
                        None => "No folder to add".to_owned(),
                    }),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_shrink_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(hint(&Open, "open"))
                            .child(hint(&Choose, "add")),
                    )
                    .child(
                        DialogFooter::new()
                            .child(
                                Button::new("folder-cancel")
                                    .outline()
                                    .label("Cancel")
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("folder-add")
                                    .primary()
                                    .label("Add")
                                    .disabled(adds.is_none())
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.choose(window, cx);
                                    })),
                            ),
                    ),
            )
    }
}

impl Render for FolderChooser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let body = (window.rem_size() * BODY_REMS).min(window.viewport_size().height * 0.5);
        v_flex()
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &Open, window, cx| this.open_selected(window, cx)))
            .on_action(cx.listener(|this, _: &Choose, window, cx| this.choose(window, cx)))
            .on_action(cx.listener(|this, _: &Complete, window, cx| this.complete(window, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, window, cx| this.step(-1, window, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| this.step(1, window, cx)))
            .on_action(cx.listener(|this, _: &Up, window, cx| this.up(window, cx)))
            .on_action(cx.listener(|this, _: &Back, window, cx| this.go_back(window, cx)))
            .on_action(cx.listener(|this, _: &Forward, window, cx| this.go_forward(window, cx)))
            .gap_3()
            .child(self.render_title(cx))
            .child(self.render_path_bar(cx))
            .child(
                h_flex()
                    .h(body)
                    .gap_2()
                    .child(self.render_places(cx))
                    .child(Separator::vertical())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(List::new(&self.list)),
                    ),
            )
            .child(self.render_footer(window, cx))
    }
}

/// What all of `names` start with, ignoring case, cased as the first.
fn common_prefix(names: &[String]) -> String {
    let Some((first, rest)) = names.split_first() else {
        return String::new();
    };
    // A byte offset in `first`, after the last character all share: two
    // names' characters can differ in width where they match in case.
    let mut end = first.len();
    for name in rest {
        let mut name = name.chars();
        let same = first
            .char_indices()
            .find(|&(_, a)| {
                name.next()
                    .is_none_or(|b| !a.to_lowercase().eq(b.to_lowercase()))
            })
            .map_or(first.len(), |(at, _)| at);
        end = end.min(same);
    }
    first[..end].to_owned()
}

/// What the folder shown holds, as far as it is read.
enum Contents {
    Reading,
    Listed(folders::Listing),
    /// Why it, or its machine, couldn't be read.
    Unread(SharedString),
}

/// One row of the folder list.
enum Row {
    /// The subfolder at this index in [`Contents::Listed`].
    Folder(usize),
    Unread(SharedString),
}

/// The folder shown's subfolders that match what is typed, for Kit's list.
struct FolderList {
    chooser: WeakEntity<FolderChooser>,
    style: PathStyle,
    folder: PathBuf,
    contents: Contents,
    /// The name typed after the folder in the path bar.
    filter: String,
    rows: Vec<Row>,
    /// The machine's projects' folders, which show as added.
    projects: Vec<PathBuf>,
}

impl FolderList {
    fn refilter(&mut self) {
        self.rows = match &self.contents {
            Contents::Reading => Vec::new(),
            Contents::Unread(why) => vec![Row::Unread(why.clone())],
            Contents::Listed(listing) => listing
                .subfolders
                .iter()
                .enumerate()
                .filter(|(_, subfolder)| folders::matches_prefix(&subfolder.name, &self.filter))
                .map(|(ix, _)| Row::Folder(ix))
                .collect(),
        };
    }

    /// The subfolders that match what is typed.
    fn matches(&self) -> impl Iterator<Item = &Subfolder> {
        self.rows.iter().filter_map(|row| self.subfolder(row))
    }

    fn subfolder(&self, row: &Row) -> Option<&Subfolder> {
        match (row, &self.contents) {
            (Row::Folder(ix), Contents::Listed(listing)) => listing.subfolders.get(*ix),
            _ => None,
        }
    }

    /// How many rows are folders, which can be selected.
    fn folder_rows(&self) -> usize {
        self.matches().count()
    }

    /// The first folder, which a name typed selects so Enter opens it.
    fn first_match(&self) -> Option<IndexPath> {
        (!self.filter.is_empty() && self.folder_rows() > 0).then(IndexPath::default)
    }

    fn path_at(&self, row: usize) -> Option<PathBuf> {
        let subfolder = self.subfolder(self.rows.get(row)?)?;
        Some(self.style.join(&self.folder, &subfolder.name))
    }

    /// Whether the subfolder `name` is a project already, by its path or
    /// its resolved one, as projects are kept.
    fn added(&self, path: &Path, name: &std::ffi::OsStr) -> bool {
        let real = match &self.contents {
            Contents::Listed(listing) => listing
                .real
                .as_ref()
                .map(|real| self.style.join(real, name)),
            Contents::Reading | Contents::Unread(_) => None,
        };
        self.projects
            .iter()
            .any(|project| project == path || Some(project) == real.as_ref())
    }
}

impl ListDelegate for FolderList {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.rows.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<'_, ListState<Self>>,
    ) -> Option<ListItem> {
        let theme = cx.theme();
        let row = self.rows.get(ix.row)?;
        let Some(subfolder) = self.subfolder(row) else {
            let Row::Unread(why) = row else { return None };
            return Some(
                ListItem::new("folder-unread")
                    .disabled(true)
                    .accessibility_label(why.clone())
                    .child(
                        div()
                            .py_1()
                            .text_xs()
                            .text_color(theme.danger)
                            .child(why.clone()),
                    ),
            );
        };
        let path = self.style.join(&self.folder, &subfolder.name);
        let name = subfolder.name.to_string_lossy().into_owned();
        let git = subfolder.git.as_ref().map(|git| match &git.head {
            Some(repo::Head::Branch(branch)) => format!("git · {branch}"),
            Some(repo::Head::Detached(commit)) => format!("git · {commit}"),
            None => "git".to_owned(),
        });
        let added = self.added(&path, &subfolder.name);
        let icon = if subfolder.git.is_some() {
            Icon::new(gpui_kit::assets::IconName::FolderGit2)
        } else {
            Icon::new(IconName::Folder)
        };
        // The row read out: its name, then its badges.
        let label = [
            Some(name.as_str()),
            added.then_some("added"),
            git.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        let chooser = self.chooser.clone();
        Some(
            ListItem::new(SharedString::from(format!("folder-row:{}", path.display())))
                .accessibility_label(label)
                // One click selects, as the keyboard does; two go in. Kit's
                // list would take one as Enter. It gives the keyboard back to
                // the path bar here as well as on the list's focus, which
                // headless UI tests don't get.
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    let _ = chooser.update(cx, |this, cx| {
                        if event.click_count() > 1 {
                            this.visit(path.clone(), "", window, cx);
                            return;
                        }
                        this.list.update(cx, |list, cx| {
                            list.set_selected_index(Some(ix), window, cx);
                            cx.notify();
                        });
                        this.path.update(cx, |path, cx| path.focus(window, cx));
                        cx.notify();
                    });
                })
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .min_w_0()
                        .child(icon.small().text_color(theme.muted_foreground))
                        .child(div().flex_1().min_w_0().truncate().child(name))
                        .children(added.then(|| Tag::secondary().small().child("added")))
                        .children(git.map(|git| Tag::success().small().outline().child(git))),
                ),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<'_, ListState<Self>>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let shown = v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .text_sm()
            .text_color(muted);
        match self.contents {
            Contents::Reading => shown.child(Spinner::new().small()),
            _ if self.filter.is_empty() => shown.child("No folders here"),
            _ => shown.child(format!("No folder starts with {}", self.filter)),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Tab types on to what every match starts with, ignoring case, in the
    /// first match's casing, and stops at a name that ends sooner.
    #[test]
    fn completion_types_the_prefix_every_match_shares() {
        let names = |list: &[&str]| -> Vec<String> { list.iter().map(|&n| n.to_owned()).collect() };
        assert_eq!(
            common_prefix(&names(&["Projects", "proj-old", "PROJx"])),
            "Proj"
        );
        assert_eq!(common_prefix(&names(&["rust-viz", "rust"])), "rust");
        assert_eq!(common_prefix(&names(&["a", "b"])), "");
        assert_eq!(common_prefix(&names(&["éa", "Éb"])), "é");
        // Characters that match in case but not in width.
        assert_eq!(common_prefix(&names(&["ẞx", "ß"])), "ẞ");
        assert_eq!(common_prefix(&names(&["\u{212A}x", "k"])), "\u{212A}");
    }
}
