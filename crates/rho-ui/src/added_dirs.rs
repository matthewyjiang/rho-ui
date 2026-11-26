//! The directories a session's tools reach besides its workspace, behind a
//! button in the title bar. The button counts them and opens their list,
//! with the user's projects not reached yet (see [`Projects`]) and a box
//! for any other: an absolute or `~/` path, by the rules of the machine.
//!
//! It works in one of two modes ([`Target`]):
//!
//! - A session's: each add goes to the daemon, which saves it with the
//!   session and sends every client that follows it the session's options,
//!   which hold the list ([`rho_ui_core::Session::added_dirs`]), so this
//!   shows what the daemon says. An add sent while a turn runs waits for
//!   the turn to end, as a settings change does. rho has no call that
//!   takes one out. A view not given the user's projects, as a subagent's
//!   or a side chat's, only lists them.
//! - A lead's draft's: the draft's own session is not the lead's, so the
//!   list stays here, where a pick can be taken out again, until the first
//!   message starts the lead with it ([`AddedDirs::dirs`]). It is kept
//!   folded as rho keeps a session's ([`with_added`]).
//!
//! A lead works in other folders than its own, the repositories it starts
//! sessions in, so a lead's are worded as projects ([`Kind`]).

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::ListItem,
    popover::Popover,
    v_flex, ActiveTheme as _, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, Anchor, AnyElement, App, AppContext as _, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window,
};
use rho_ui_core::{Daemon, Session, SessionId};

use crate::{
    folders::{PathStyle, Typed},
    leads::Kind,
    machines::{MachineId, Place},
    project_picker::{project_row, ROW_REMS},
    project_stack::session_projects,
    projects::Projects,
    workspaces::{tilde, Workspace, WorkspaceRef, Workspaces},
};

/// Rows the list of projects shows before it scrolls: as many as the
/// project picker, so the popover stays about its height.
const MAX_ROWS: f32 = 5.;

/// Whether the user can add a directory to a session.
#[derive(Clone)]
pub(crate) enum DirsAccess {
    /// A view given the user's projects, these, which it offers to add.
    Add(Entity<Projects>),
    /// Any other view, as a subagent's, which only its run changes, or a
    /// side chat's: it only lists them.
    ListOnly,
}

/// Where an add goes.
enum Target {
    /// To the daemon, which sends back the session's list. The session
    /// works in `workspace`.
    Session {
        daemon: Daemon,
        session: SessionId,
        workspace: WorkspaceRef,
    },
    /// Nowhere yet: a lead's draft keeps them here until the lead starts,
    /// because the draft's own session is not the lead's.
    Pending,
}

/// Where an add came from: the box, which empties once the add is taken,
/// or the list of projects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Typed,
    Project,
}

/// One session's added directories, or a lead's draft's, and the box that
/// adds one.
pub(crate) struct AddedDirs {
    target: Target,
    /// The machine the directories are on.
    machine: MachineId,
    /// Where the machine's home is, which `~` stands for, and what each
    /// project is called.
    workspaces: Entity<Workspaces>,
    /// As the session last listed them, or as picked in a lead's draft.
    dirs: Vec<PathBuf>,
    /// Each of `dirs`' index into `workspaces`, which names its project.
    indices: Vec<usize>,
    /// How they are worded.
    kind: Kind,
    /// `None` in a view that only lists them; see [`DirsAccess`].
    form: Option<AddForm>,
}

/// The projects and the box that add a directory.
struct AddForm {
    input: Entity<InputState>,
    /// Why the last add failed.
    error: Option<SharedString>,
    /// An add is on its way to the daemon.
    adding: bool,
    /// The user's projects, the ones offered among them.
    projects: Entity<Projects>,
    /// The projects offered, by index into the workspaces, in the project
    /// picker's order; see [`to_offer`].
    offered: Vec<usize>,
    _subscriptions: [Subscription; 3],
}

impl AddForm {
    fn new(
        projects: Entity<Projects>,
        workspaces: &Entity<Workspaces>,
        window: &mut Window,
        cx: &mut Context<'_, AddedDirs>,
    ) -> Self {
        let input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Add a directory: /path or ~/path"));
        let subscriptions = [
            cx.subscribe_in(
                &input,
                window,
                |this: &mut AddedDirs, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.add_typed(window, cx),
                    InputEvent::Change => {
                        if let Some(form) = &mut this.form {
                            form.error = None;
                        }
                        cx.notify();
                    }
                    _ => {}
                },
            ),
            // A project added, removed, or found in a clone folder.
            cx.observe(&projects, |this, _, cx| this.refresh_offer(cx)),
            // A project's name, read from its repository, or its last use;
            // or the repository the session's workspace is in.
            cx.observe(workspaces, |this, _, cx| {
                this.refresh_offer(cx);
                cx.notify();
            }),
        ];
        Self {
            input,
            error: None,
            adding: false,
            projects,
            offered: Vec::new(),
            _subscriptions: subscriptions,
        }
    }
}

impl AddedDirs {
    /// The added directories of `session`, which works in `workspace`,
    /// worded for `kind` until [`Self::set_kind`].
    pub(crate) fn of_session(
        daemon: &Daemon,
        session: &Session,
        workspace: &WorkspaceRef,
        access: DirsAccess,
        kind: Kind,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let machine = workspace.read(cx).machine().clone();
        let workspaces = workspace.table().clone();
        let target = Target::Session {
            daemon: daemon.clone(),
            session: session.id().clone(),
            workspace: workspace.clone(),
        };
        let dirs = session.added_dirs().to_vec();
        cx.new(|cx| {
            let form = match access {
                DirsAccess::Add(projects) => Some(AddForm::new(projects, &workspaces, window, cx)),
                DirsAccess::ListOnly => None,
            };
            Self::with_form(target, machine, workspaces, dirs, kind, form, cx)
        })
    }

    /// The directories a lead's draft on `machine` picks for its lead, from
    /// the user's `projects` or typed, none at first. The lead's folder is
    /// made only as it starts, so no workspace holds any of them yet.
    pub(crate) fn pending(
        machine: MachineId,
        workspaces: &Entity<Workspaces>,
        projects: Entity<Projects>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let workspaces = workspaces.clone();
        cx.new(|cx| {
            let form = Some(AddForm::new(projects, &workspaces, window, cx));
            let dirs = Vec::new();
            Self::with_form(
                Target::Pending,
                machine,
                workspaces,
                dirs,
                Kind::Lead,
                form,
                cx,
            )
        })
    }

    /// The control, with the projects `form` offers listed.
    fn with_form(
        target: Target,
        machine: MachineId,
        workspaces: Entity<Workspaces>,
        dirs: Vec<PathBuf>,
        kind: Kind,
        form: Option<AddForm>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut this = Self {
            target,
            machine,
            workspaces,
            dirs,
            indices: Vec::new(),
            kind,
            form,
        };
        this.reindex(cx);
        this.refresh_offer(cx);
        this
    }

    /// The projects the session works in, by [`session_projects`], with
    /// `own`, the session's workspace, by its index into the workspaces;
    /// `None` for a lead's draft, which has none yet.
    pub(crate) fn projects<'a>(&self, own: Option<usize>, cx: &'a App) -> Vec<&'a Workspace> {
        let workspaces = self.workspaces.read(cx);
        let projects = self.form.as_ref().map(|form| form.projects.read(cx));
        session_projects(
            self.kind,
            own.map(|ix| workspaces.get(ix)),
            self.indices.iter().map(|&ix| workspaces.get(ix)),
            projects,
        )
    }

    /// The directories' indices into the workspaces, for whoever names
    /// their projects elsewhere, as the sidebar's row of a lead's draft.
    pub(crate) fn indices(&self) -> &[usize] {
        &self.indices
    }

    /// Finds each directory's workspace, adding those not in the table, so
    /// its project can be named.
    fn reindex(&mut self, cx: &mut Context<'_, Self>) {
        let (machine, dirs) = (&self.machine, &self.dirs);
        self.indices = self.workspaces.update(cx, |workspaces, cx| {
            dirs.iter()
                .map(|dir| workspaces.index(&Place::new(machine.clone(), dir.clone()), cx))
                .collect()
        });
    }

    /// The directories listed: the session's, or those a lead's draft
    /// picked for its lead.
    pub(crate) fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// Sends a session's adds to `new` from now on, as once its machine
    /// connected again.
    pub(crate) fn set_daemon(&mut self, new: Daemon) {
        if let Target::Session { daemon, .. } = &mut self.target {
            *daemon = new;
        }
    }

    /// Shows `dirs`, the session's list as the daemon last sent it.
    pub(crate) fn sync(&mut self, dirs: &[PathBuf], cx: &mut Context<'_, Self>) {
        if self.dirs != dirs {
            self.dirs = dirs.to_vec();
            self.reindex(cx);
            self.refresh_offer(cx);
            cx.notify();
        }
    }

    /// Words them for `kind`, as the session's kind becomes known.
    pub(crate) fn set_kind(&mut self, kind: Kind, cx: &mut Context<'_, Self>) {
        if self.kind != kind {
            self.kind = kind;
            cx.notify();
        }
    }

    /// Lists again the projects to offer, as the user's projects, the
    /// directories, or the session's workspace change.
    fn refresh_offer(&mut self, cx: &mut Context<'_, Self>) {
        let Some(form) = &mut self.form else {
            return;
        };
        let mut reached = self.dirs.clone();
        if let Target::Session { workspace, .. } = &self.target {
            // Its directory, and the project it is in, as a worktree is.
            let workspace = workspace.read(cx);
            reached.push(workspace.cwd().to_path_buf());
            reached.push(workspace.root_path().to_path_buf());
        }
        let roots = to_offer(form.projects.read(cx).roots(), &self.machine, &reached);
        let offered = self.workspaces.update(cx, |workspaces, cx| {
            let mut offered: Vec<usize> = roots
                .iter()
                .map(|root| workspaces.index(root, cx))
                .collect();
            workspaces.sort_by_use(&mut offered);
            offered
        });
        if form.offered != offered {
            form.offered = offered;
            cx.notify();
        }
    }

    /// Adds the typed path, or says why it isn't one.
    fn add_typed(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(form) = &mut self.form else {
            return;
        };
        let text = form.input.read(cx).value().trim().to_owned();
        if form.adding || text.is_empty() {
            return;
        }
        let home = self.workspaces.read(cx).home(&self.machine);
        let Some(typed) = Typed::parse(&text, home.as_deref(), PathStyle::of(&self.machine)) else {
            form.error = Some("Type an absolute path, or one under ~/".into());
            cx.notify();
            return;
        };
        self.add(typed.path, Source::Typed, window, cx);
    }

    /// Adds `path`: sends it to the daemon, unless an add is on its way
    /// already, or for a lead's draft, lists it.
    fn add(
        &mut self,
        path: PathBuf,
        source: Source,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(form) = &mut self.form else {
            return;
        };
        let (daemon, session) = match &self.target {
            Target::Session {
                daemon, session, ..
            } => (daemon.clone(), session.clone()),
            Target::Pending => {
                if let Some(dirs) = with_added(&self.dirs, path) {
                    self.dirs = dirs;
                    form.error = None;
                    if source == Source::Typed {
                        form.input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
                } else {
                    form.error = Some("It's listed already, or in one listed.".into());
                }
                self.reindex(cx);
                self.refresh_offer(cx);
                cx.notify();
                return;
            }
        };
        if form.adding {
            return;
        }
        form.adding = true;
        form.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = daemon.add_directory(&session, &path).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(form) = &mut this.form else {
                    return;
                };
                form.adding = false;
                match result {
                    // The list itself comes with the session's options.
                    Ok(()) if source == Source::Typed => form
                        .input
                        .update(cx, |input, cx| input.set_value("", window, cx)),
                    Ok(()) => {}
                    Err(error) => form.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Takes `dir` out of a lead's draft's list, which offers it again if
    /// it is a project.
    fn remove(&mut self, dir: &Path, cx: &mut Context<'_, Self>) {
        self.dirs.retain(|listed| listed != dir);
        self.reindex(cx);
        self.refresh_offer(cx);
        cx.notify();
    }

    /// The popover's list, projects and box, read from `this` as it is now.
    fn render_content(this: &WeakEntity<Self>, cx: &App) -> AnyElement {
        let Some(this_entity) = this.upgrade() else {
            return div().into_any_element();
        };
        let dirs = this_entity.read(cx);
        v_flex()
            .w(rems(26.))
            .gap_2()
            .child(section_heading(heading(dirs.kind), cx))
            .child(dirs.render_listed(this, cx))
            .when_some(dirs.form.as_ref(), |content, form| {
                content
                    .children(dirs.render_offer(this, form, cx))
                    .child(render_box(this, form))
                    .when_some(form.error.clone(), |content, why| {
                        content.child(
                            div()
                                .id("added-dirs-error")
                                .test_support()
                                .text_xs()
                                .text_color(cx.theme().red)
                                .child(why),
                        )
                    })
            })
            .into_any_element()
    }

    /// The directories listed, each with a button that takes it out in a
    /// lead's draft: rho can't take one out of a session.
    fn render_listed(&self, this: &WeakEntity<Self>, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let home = self.workspaces.read(cx).home(&self.machine);
        let removable = matches!(self.target, Target::Pending);
        v_flex()
            .id("added-dirs-list")
            .max_h(rems(16.))
            .overflow_y_scroll()
            .gap_1()
            .when(self.dirs.is_empty(), |list| {
                list.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(empty_text(self.kind)),
                )
            })
            .children(self.dirs.iter().enumerate().map(|(ix, dir)| {
                let (this, dir_path) = (this.clone(), dir.clone());
                h_flex()
                    .id(SharedString::from(format!("added-dir-{ix}")))
                    .test_support()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(tilde(dir, home.as_deref())),
                    )
                    .when(removable, |row| {
                        row.child(
                            Button::new(SharedString::from(format!("added-dir-remove-{ix}")))
                                .ghost()
                                .xsmall()
                                .icon(Lucide::X)
                                .tooltip("Remove")
                                .on_click(move |_, _, cx| {
                                    let _ = this.update(cx, |this, cx| this.remove(&dir_path, cx));
                                }),
                        )
                    })
            }))
    }

    /// The projects `form` offers, each added with a click, as the project
    /// picker draws them; nothing when none is left to offer.
    fn render_offer(
        &self,
        this: &WeakEntity<Self>,
        form: &AddForm,
        cx: &App,
    ) -> Option<impl IntoElement> {
        if form.offered.is_empty() {
            return None;
        }
        let workspaces = self.workspaces.read(cx);
        let rows = form.offered.iter().enumerate().map(|(n, &ix)| {
            let workspace = workspaces.get(ix);
            let (this, path) = (this.clone(), workspace.cwd().to_path_buf());
            ListItem::new(SharedString::from(format!("add-project-{n}")))
                .accessibility_label(format!("Add {}", workspace.name))
                .rounded(cx.theme().radius)
                .px_2()
                .disabled(form.adding)
                .on_click(move |_, window, cx| {
                    let path = path.clone();
                    let _ = this.update(cx, |this, cx| {
                        this.add(path, Source::Project, window, cx);
                    });
                })
                .child(project_row(workspace, cx))
        });
        Some(
            v_flex()
                .gap_1()
                .child(section_heading("Your projects", cx))
                .child(
                    v_flex()
                        .id("add-projects-list")
                        .max_h(rems(ROW_REMS * MAX_ROWS))
                        .overflow_y_scroll()
                        .children(rows),
                ),
        )
    }
}

/// The heading over the directories listed.
fn heading(kind: Kind) -> &'static str {
    match kind {
        Kind::Session => "Directories beside the workspace",
        Kind::Lead => "Projects this lead works in",
    }
}

/// What the list says with none.
fn empty_text(kind: Kind) -> &'static str {
    match kind {
        Kind::Session => "Its tools reach only its workspace.",
        Kind::Lead => "None yet.",
    }
}

/// What the button reads with `count` listed, by its folder icon: for a
/// session, their count, and nothing with none; for a lead, its projects.
fn button_label(kind: Kind, count: usize) -> Option<String> {
    match (kind, count) {
        (Kind::Session, 0) => None,
        (Kind::Session, n) => Some(n.to_string()),
        (Kind::Lead, 0) => Some("Add projects".to_owned()),
        (Kind::Lead, 1) => Some("1 project".to_owned()),
        (Kind::Lead, n) => Some(format!("{n} projects")),
    }
}

/// A small heading over a part of the popover.
fn section_heading(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_semibold()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// The box that takes a path, and its Add button.
fn render_box(this: &WeakEntity<AddedDirs>, form: &AddForm) -> impl IntoElement {
    let add = this.clone();
    h_flex()
        .gap_2()
        .child(
            div()
                .id("added-dirs-input")
                .test_support()
                .flex_1()
                .min_w_0()
                .child(Input::new(&form.input).small()),
        )
        .child(
            Button::new("added-dirs-add")
                .primary()
                .small()
                .label("Add")
                .loading(form.adding)
                .on_click(move |_, window, cx| {
                    let _ = add.update(cx, |this, cx| this.add_typed(window, cx));
                }),
        )
}

/// Whether `dir` holds `path`, folder by folder: `path` is `dir` or in
/// it, not merely named like it.
fn holds(dir: &Path, path: &Path) -> bool {
    path.starts_with(dir)
}

/// The projects of `known` on `machine` that no directory in `reached`
/// holds: those a session that reaches those could add.
fn to_offer<'a>(
    known: impl Iterator<Item = &'a Place>,
    machine: &MachineId,
    reached: &[PathBuf],
) -> Vec<Place> {
    known
        .filter(|root| {
            root.machine == *machine && !reached.iter().any(|dir| holds(dir, &root.path))
        })
        .cloned()
        .collect()
}

/// `dirs` with `path` added, kept folded as rho keeps a session's: `None`
/// when one of them holds `path` already; else without those `path` holds,
/// and with it last.
fn with_added(dirs: &[PathBuf], path: PathBuf) -> Option<Vec<PathBuf>> {
    if dirs.iter().any(|dir| holds(dir, &path)) {
        return None;
    }
    let mut folded: Vec<PathBuf> = dirs
        .iter()
        .filter(|dir| !holds(&path, dir))
        .cloned()
        .collect();
    folded.push(path);
    Some(folded)
}

impl Render for AddedDirs {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        // A view that only lists them, with none, has nothing to show.
        if self.form.is_none() && self.dirs.is_empty() {
            return div().into_any_element();
        }
        let this = cx.entity().downgrade();
        let focus = self.form.as_ref().map(|form| form.input.focus_handle(cx));
        let button = Button::new("added-dirs-button")
            .ghost()
            .xsmall()
            .icon(Lucide::FolderPlus)
            .when_some(button_label(self.kind, self.dirs.len()), |button, label| {
                button.label(label)
            })
            // A lead's label says what it holds.
            .when(self.kind == Kind::Session, |button| {
                button.tooltip(heading(Kind::Session))
            });
        Popover::new("added-dirs")
            .anchor(Anchor::TopRight)
            .when_some(focus, |popover, focus| popover.track_focus(&focus))
            .trigger(button)
            .content(move |_, _, cx| Self::render_content(&this, cx))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session is offered its machine's projects that nothing it reaches
    /// holds: not its workspace, the project a worktree belongs to, an
    /// added directory, or a folder in one.
    #[test]
    fn projects_on_offer_are_those_the_session_does_not_reach() {
        let known = [
            Place::local("/w/app"),
            Place::local("/w/api"),
            Place::local("/w/lib"),
            Place::local("/w/lib/vendor"),
            Place::new(MachineId::remote("workstation"), "/w/web"),
            // Named like the workspace, but beside it.
            Place::local("/w/application"),
        ];
        // What each session reaches: its workspace, its project's root, and
        // its added directories, as `refresh_offer` gathers them.
        let table: [(&str, &[&str], &[&str]); 2] = [
            (
                "in its project",
                &["/w/app", "/w/app", "/w/lib"],
                &["/w/api", "/w/application"],
            ),
            (
                "in a worktree of /w/api",
                &["/rho/worktrees/api-1", "/w/api"],
                &["/w/app", "/w/lib", "/w/lib/vendor", "/w/application"],
            ),
        ];
        for (session, reached, expected) in table {
            let reached: Vec<PathBuf> = reached.iter().map(PathBuf::from).collect();
            let expected: Vec<Place> = expected.iter().copied().map(Place::local).collect();
            assert_eq!(
                to_offer(known.iter(), &MachineId::local(), &reached),
                expected,
                "{session}"
            );
        }
    }

    /// A lead's draft lists a pick unless a listed one holds it, and a
    /// pick that holds listed ones takes their place.
    #[test]
    fn a_pick_is_listed_folded() {
        /// What is listed, the pick, and the list after it, if it is taken.
        type Case = (
            &'static [&'static str],
            &'static str,
            Option<&'static [&'static str]>,
        );
        let paths = |list: &[&str]| -> Vec<PathBuf> { list.iter().map(PathBuf::from).collect() };
        let table: [Case; 6] = [
            (&[], "/w/app", Some(&["/w/app"])),
            (&["/w/app"], "/w/lib", Some(&["/w/app", "/w/lib"])),
            // Named like a listed one, but beside it.
            (
                &["/w/app"],
                "/w/application",
                Some(&["/w/app", "/w/application"]),
            ),
            (&["/w/app"], "/w/app", None),
            (&["/w/app"], "/w/app/src", None),
            (&["/w/app", "/x", "/w/lib"], "/w", Some(&["/x", "/w"])),
        ];
        for (listed, pick, expected) in table {
            assert_eq!(
                with_added(&paths(listed), PathBuf::from(pick)),
                expected.map(paths),
                "{pick} onto {listed:?}"
            );
        }
    }
}
