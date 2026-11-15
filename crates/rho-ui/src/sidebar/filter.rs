//! The filter beside the sidebar's search: a menu of the machines to show
//! and the projects to keep to, saved in the state database so it holds
//! across runs. [`Filter`] decides which rows it lets through.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    Selectable as _, Sizable as _,
};
use gpui_kit::{
    Anchor, App, ClickEvent, Context, IntoElement, ScrollStrategy, SharedString, WeakEntity, Window,
};

use rho_ui_core::platform;

use super::SidebarView;
use crate::{
    machines::{MachineId, Place},
    session_list::Filter,
    store::StateDb,
};

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

    pub(super) fn filter(&self) -> &Filter {
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

/// A menu item's click: `change` applied to the filter of `view`.
fn on_click(
    view: &WeakEntity<SidebarView>,
    change: impl Fn(&mut Filter) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |_, _, cx| {
        let _ = view.update(cx, |this, cx| this.change_filter(&change, cx));
    }
}

/// The filter's button, lit while it hides anything, and its menu: with
/// more than one machine, each to show or hide, then each project to keep
/// to.
pub(super) fn button(sidebar: &SidebarView, cx: &mut Context<'_, SidebarView>) -> impl IntoElement {
    let filter = sidebar.list.filter();
    let machines = sidebar.machines.read(cx);
    let workspaces = sidebar.workspaces.read(cx);
    let machine_items: Vec<(MachineId, SharedString, bool)> = if machines.several() {
        machines
            .iter()
            .map(|machine| {
                let shown = !filter.hides(&machine.id);
                (machine.id.clone(), machine.label.clone(), shown)
            })
            .collect()
    } else {
        Vec::new()
    };
    let project_items: Vec<(Place, SharedString, bool)> = sidebar
        .projects
        .read(cx)
        .roots()
        .iter()
        .map(|project| {
            let name = workspaces.find(&[project]).map_or_else(
                || project.path.display().to_string(),
                |ix| workspaces.get(ix).name.to_string(),
            );
            let name = match machines.tag(&project.machine) {
                Some(machine) => format!("{name} · {machine}"),
                None => name,
            };
            (project.clone(), name.into(), filter.chose(project))
        })
        .collect();
    let filtering = !filter.is_empty();
    let view = cx.entity().downgrade();
    Button::new("sidebar-filter")
        .ghost()
        .small()
        .icon(Lucide::ListFilter)
        .accessibility_label("Filter")
        .tooltip("Filter by machine or project")
        .selected(filtering)
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
            let menu = menu_items(menu, &view, &machine_items, &project_items);
            // With any filter at all, even one the items don't show, such
            // as a hidden machine while only one is listed.
            if filtering {
                show_all(menu, &view)
            } else {
                menu
            }
        })
}

fn menu_items(
    mut menu: PopupMenu,
    view: &WeakEntity<SidebarView>,
    machines: &[(MachineId, SharedString, bool)],
    projects: &[(Place, SharedString, bool)],
) -> PopupMenu {
    if !machines.is_empty() {
        menu = menu.label("Machines");
        for (machine, label, shown) in machines {
            let machine = machine.clone();
            menu = menu.item(
                PopupMenuItem::new(label.clone())
                    .checked(*shown)
                    .on_click(on_click(view, move |filter| {
                        filter.toggle_machine(&machine);
                    })),
            );
        }
    }
    if !projects.is_empty() {
        if !machines.is_empty() {
            menu = menu.separator();
        }
        menu = menu.label("Projects");
        for (project, name, chosen) in projects {
            let project = project.clone();
            menu = menu.item(
                PopupMenuItem::new(name.clone())
                    .checked(*chosen)
                    .on_click(on_click(view, move |filter| {
                        filter.toggle_project(&project);
                    })),
            );
        }
    }
    if machines.is_empty() && projects.is_empty() {
        menu = menu.label("Add a project or a machine to filter by");
    }
    menu
}

/// Ends the menu with Show all, which clears the filter.
fn show_all(menu: PopupMenu, view: &WeakEntity<SidebarView>) -> PopupMenu {
    menu.separator().item(
        PopupMenuItem::new("Show all")
            .on_click(on_click(view, |filter| *filter = Filter::default())),
    )
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
