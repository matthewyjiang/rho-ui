//! The filter beside the sidebar's search: a button that opens a popover of
//! the projects to keep to and, with more than one machine, the machines to
//! show ([`menu`]), and while it hides anything, a row of chips under the
//! search, one for each part of it ([`chips`]). It is saved in the state
//! database so it holds across runs. [`Filter`] decides which rows it lets
//! through.

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    badge::Badge,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    input::InputState,
    popover::Popover,
    ActiveTheme as _, Sizable as _, Theme,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, Anchor, App, ClickEvent, Context, Focusable as _, Hsla, IntoElement, ParentElement as _,
    ScrollStrategy, SharedString, Styled as _, WeakEntity, Window,
};

use rho_ui_core::platform;

use super::SidebarView;
use crate::{
    machines::{MachineId, MachineStatus, Machines, Place},
    project_stack::session_workspaces,
    repo,
    session_list::Filter,
    store::StateDb,
    workspaces::Workspaces,
};

mod chips;
mod menu;

pub(super) use chips::chips;

/// The popover offers a search once it lists more projects than this, as
/// the design asks: past it, finding one by eye gets slow.
const SEARCH_FROM: usize = 8;

/// The sidebar's [`Filter`], kept in the state database's
/// `filter_machines` and `filter_projects` tables.
pub(crate) struct SavedFilter {
    filter: Filter,
    db: StateDb,
}

impl SavedFilter {
    pub(crate) fn load(db: &StateDb) -> Self {
        let hidden = db.query("SELECT machine FROM filter_machines", [], |row| {
            Ok(MachineId::from(row.get::<_, String>(0)?.as_str()))
        });
        let projects = db.query("SELECT machine, path FROM filter_projects", [], |row| {
            let machine = MachineId::from(row.get::<_, String>(0)?.as_str());
            let path = platform::path_from_bytes(&row.get::<_, Vec<u8>>(1)?);
            Ok(Place::new(machine, path))
        });
        Self {
            filter: Filter::new(hidden.into_iter().collect(), projects.into_iter().collect()),
            db: db.clone(),
        }
    }

    pub(crate) fn filter(&self) -> &Filter {
        &self.filter
    }

    /// Makes `filter` the saved one, writing a row for each machine or
    /// project that came or went.
    fn save(&mut self, filter: Filter) {
        let (old, new) = (&self.filter, &filter);
        for machine in old.hidden().difference(new.hidden()) {
            self.db.execute(
                "DELETE FROM filter_machines WHERE machine = ?1",
                [machine.as_str()],
            );
        }
        for machine in new.hidden().difference(old.hidden()) {
            self.db.execute(
                "INSERT OR IGNORE INTO filter_machines (machine) VALUES (?1)",
                [machine.as_str()],
            );
        }
        let row = |project: &Place| {
            let path = platform::path_to_bytes(project.path.as_os_str());
            (project.machine.as_str().to_owned(), path)
        };
        for project in old.projects().difference(new.projects()) {
            self.db.execute(
                "DELETE FROM filter_projects WHERE machine = ?1 AND path = ?2",
                row(project),
            );
        }
        for project in new.projects().difference(old.projects()) {
            self.db.execute(
                "INSERT OR IGNORE INTO filter_projects (machine, path) VALUES (?1, ?2)",
                row(project),
            );
        }
        self.filter = filter;
    }
}

impl SidebarView {
    /// Changes the filter with `change`, saves it, and lists the sessions
    /// it lets through from the top.
    fn change_filter(&mut self, change: impl FnOnce(&mut Filter), cx: &mut Context<'_, Self>) {
        let mut filter = self.saved_filter.filter().clone();
        change(&mut filter);
        self.list.set_filter(filter.clone());
        self.saved_filter.save(filter);
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.rebuild(cx);
    }

    /// Drops from the filter what the menu no longer offers, with `prune`,
    /// so it can't keep hiding sessions with no item left to undo it.
    pub(super) fn prune_filter(&mut self, prune: impl FnOnce(&mut Filter) -> bool) {
        let mut filter = self.saved_filter.filter().clone();
        if prune(&mut filter) {
            self.list.set_filter(filter.clone());
            self.saved_filter.save(filter);
        }
    }
}

/// A click that applies `change` to the filter of `view`: a menu row's,
/// an Only's, Clear's or a chip's.
fn on_click(
    view: &WeakEntity<SidebarView>,
    change: impl Fn(&mut Filter) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |_, _, cx| {
        let _ = view.update(cx, |this, cx| this.change_filter(&change, cx));
    }
}

/// A project the filter offers, named as the sidebar's rows name it.
struct ProjectChoice {
    place: Place,
    name: SharedString,
    initials: SharedString,
    /// Its machine's label, when that isn't this machine.
    machine: Option<SharedString>,
    /// The families of sessions that work in it; see [`Counts`].
    families: usize,
}

impl ProjectChoice {
    /// `place`'s name and initials: its workspace's once the sidebar has
    /// read it, else its folder's.
    fn new(place: Place, workspaces: &Workspaces, machines: &Machines) -> Self {
        let (name, initials) = workspaces.find(&[&place]).map_or_else(
            || {
                let folder = repo::folder(&place.path);
                (folder.name.clone().into(), folder.initials().into())
            },
            |ix| {
                let workspace = workspaces.get(ix);
                (workspace.name.clone(), workspace.initials.clone())
            },
        );
        Self {
            machine: machines.remote_label(&place.machine),
            name,
            initials,
            place,
            families: 0,
        }
    }

    /// The element ID of what stands for it, `kind` saying which part: a
    /// menu row, its Only, a chip.
    fn element_id(&self, kind: &str) -> SharedString {
        let Place { machine, path } = &self.place;
        format!("filter-{kind}-{machine}:{}", path.display()).into()
    }
}

/// A machine the filter offers.
struct MachineChoice {
    id: MachineId,
    label: SharedString,
    connection: Connection,
    /// The families of sessions on it; see [`Counts`].
    families: usize,
}

impl MachineChoice {
    fn new(id: &MachineId, machines: &Machines) -> Self {
        Self {
            id: id.clone(),
            label: machines.label(id),
            connection: Connection::of(machines.status(id)),
            families: 0,
        }
    }

    /// See [`ProjectChoice::element_id`].
    fn element_id(&self, kind: &str) -> SharedString {
        format!("filter-{kind}-{}", self.id).into()
    }
}

/// How a machine's connection is doing, as its status dot and the words
/// beside it say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Connection {
    Connected,
    Connecting,
    /// The user stopped its daemon.
    Stopped,
    /// Its daemon or its rho is older than rho-ui.
    Outdated,
    Unreachable,
}

impl Connection {
    /// Of a machine whose connection is doing as `status` says; `None` for
    /// one gone from the settings.
    fn of(status: Option<&MachineStatus>) -> Self {
        match status {
            Some(MachineStatus::Ready { .. }) => Self::Connected,
            Some(MachineStatus::Connecting) => Self::Connecting,
            Some(MachineStatus::Stopped) => Self::Stopped,
            Some(MachineStatus::OlderDaemon { .. } | MachineStatus::OlderRho) => Self::Outdated,
            Some(MachineStatus::Failed(_)) | None => Self::Unreachable,
        }
    }

    /// What its row says beside the label of `machine`.
    fn words(self, machine: &MachineId) -> &'static str {
        match self {
            Self::Connected if machine.is_local() => "this machine",
            Self::Connected => "ssh",
            Self::Connecting => "connecting",
            Self::Stopped => "stopped",
            Self::Outdated => "needs update",
            Self::Unreachable => "can't reach",
        }
    }

    fn color(self, theme: &Theme) -> Hsla {
        match self {
            Self::Connected => theme.success,
            Self::Connecting | Self::Stopped => theme.muted_foreground,
            Self::Outdated => theme.warning,
            Self::Unreachable => theme.danger,
        }
    }
}

/// How many families of sessions, on any shelf, work in each project and
/// run on each machine, whatever the filter and the search show: a session
/// and those it started count once, as the shelf headers count them.
#[derive(Default)]
struct Counts {
    projects: HashMap<Place, usize>,
    machines: HashMap<MachineId, usize>,
}

impl Counts {
    fn of(sidebar: &SidebarView, cx: &App) -> Self {
        let workspaces = sidebar.workspaces.read(cx);
        let projects = sidebar.projects.read(cx);
        let rows = sidebar.list.rows();
        let mut in_project: HashSet<(Place, usize)> = HashSet::new();
        let mut on_machine: HashSet<(&MachineId, usize)> = HashSet::new();
        for (row, top) in rows.iter().zip(sidebar.list.tops()) {
            let machine = &row.id.machine;
            on_machine.insert((machine, top));
            // A session is in the projects the filter finds it in.
            let own = Some(workspaces.get(row.workspace));
            let added = row.added.iter().map(|&ix| workspaces.get(ix));
            for workspace in session_workspaces(row.kind, own, added, Some(projects)) {
                let root = Place::new(machine.clone(), workspace.root_path());
                in_project.insert((root, top));
            }
        }
        let mut counts = Self::default();
        for (project, _) in in_project {
            *counts.projects.entry(project).or_default() += 1;
        }
        for (machine, _) in on_machine {
            *counts.machines.entry(machine.clone()).or_default() += 1;
        }
        counts
    }
}

/// Everything the popover offers: each project, in the order the user's
/// projects list them, and with more than one machine, each machine.
struct Offer {
    projects: Vec<ProjectChoice>,
    machines: Vec<MachineChoice>,
}

impl Offer {
    fn of(sidebar: &SidebarView, cx: &App) -> Self {
        let counts = Counts::of(sidebar, cx);
        let machines = sidebar.machines.read(cx);
        let workspaces = sidebar.workspaces.read(cx);
        let projects = sidebar
            .projects
            .read(cx)
            .roots()
            .map(|place| ProjectChoice {
                families: counts.projects.get(place).copied().unwrap_or_default(),
                ..ProjectChoice::new(place.clone(), workspaces, machines)
            })
            .collect();
        let machines = if machines.several() {
            machines
                .iter()
                .map(|machine| MachineChoice {
                    families: counts
                        .machines
                        .get(&machine.id)
                        .copied()
                        .unwrap_or_default(),
                    ..MachineChoice::new(&machine.id, machines)
                })
                .collect()
        } else {
            Vec::new()
        };
        Self { projects, machines }
    }

    fn is_empty(&self) -> bool {
        self.projects.is_empty() && self.machines.is_empty()
    }
}

/// The filter's button, in the accent color with a count of what it holds
/// while it hides anything, and the popover it opens: see [`menu`]. With
/// more than [`SEARCH_FROM`] projects, the popover's search takes the
/// keyboard as it opens, and is emptied as it closes.
pub(super) fn button(
    sidebar: &SidebarView,
    window: &mut Window,
    cx: &mut Context<'_, SidebarView>,
) -> impl IntoElement {
    let filter = sidebar.list.filter();
    let parts = filter.hidden().len() + filter.projects().len();
    let accent = cx.theme().blue;
    let search = (sidebar.projects.read(cx).roots().count() > SEARCH_FROM).then(|| {
        window.use_keyed_state("sidebar-filter-search", cx, |window, cx| {
            InputState::new(window, cx).placeholder("Find a project")
        })
    });
    let trigger = Button::new("sidebar-filter")
        .ghost()
        .small()
        .icon(Lucide::ListFilter)
        .accessibility_label("Filter")
        .tooltip("Filter by project or machine")
        .when(parts > 0, |button| {
            button.custom(
                ButtonCustomVariant::new(cx)
                    .color(accent.opacity(0.15))
                    .foreground(accent)
                    .hover(accent.opacity(0.25))
                    .active(accent.opacity(0.3)),
            )
        });
    let view = cx.entity().downgrade();
    let popover = Popover::new("sidebar-filter-popover")
        .anchor(Anchor::TopRight)
        .p_1()
        .trigger(trigger)
        .when_some(search.as_ref(), |popover, search| {
            popover.track_focus(&search.read(cx).focus_handle(cx))
        })
        .on_open_change({
            let search = search.clone();
            move |open, window, cx| {
                if let (false, Some(search)) = (*open, &search) {
                    search.update(cx, |search, cx| search.set_value("", window, cx));
                }
            }
        })
        .content(move |_, window, cx| {
            // Read as it is drawn, after the sidebar's own render, so the
            // counts are worked out only while the popover is open.
            let Some(sidebar) = view.upgrade() else {
                return div().into_any_element();
            };
            let (offer, filter) = {
                let sidebar = sidebar.read(cx);
                (Offer::of(sidebar, cx), sidebar.list.filter().clone())
            };
            menu::render(&offer, &filter, search.as_ref(), &view, window, cx).into_any_element()
        });
    Badge::new().count(parts).color(accent).child(popover)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The filter survives a reload, a path that isn't UTF-8 or holds a
    /// tab or a newline too, and what it drops stays dropped.
    #[test]
    fn the_filter_survives_a_reload() {
        let db = StateDb::in_memory();
        let there = MachineId::remote("workstation");
        let mut filter = Filter::default();
        filter.toggle_machine(&there);
        for path in [
            platform::path_from_bytes(b"/w/\xff"),
            "/w/tab\there".into(),
            "/w/two\nlines".into(),
        ] {
            filter.toggle_project(&Place::new(there.clone(), path));
        }
        let mut saved = SavedFilter::load(&db);
        saved.save(filter.clone());
        assert_eq!(SavedFilter::load(&db).filter(), &filter);

        filter.toggle_machine(&there);
        filter.toggle_project(&Place::new(there.clone(), "/w/tab\there"));
        saved.save(filter.clone());
        assert_eq!(SavedFilter::load(&db).filter(), &filter);
    }
}
