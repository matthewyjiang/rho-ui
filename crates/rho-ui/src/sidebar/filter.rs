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

use super::SidebarView;
use crate::{
    machines::{MachineId, Place},
    session_list::Filter,
    store::{Key, StateDb},
};

/// The sidebar's [`Filter`], kept in the state database's
/// [`Key::SidebarFilter`].
pub(crate) struct SavedFilter {
    filter: Filter,
    db: StateDb,
}

impl SavedFilter {
    pub(crate) fn load(db: &StateDb) -> Self {
        let filter = db
            .get(Key::SidebarFilter)
            .map(|text| Filter::from_text(&text))
            .unwrap_or_default();
        Self {
            filter,
            db: db.clone(),
        }
    }

    pub(super) fn filter(&self) -> &Filter {
        &self.filter
    }

    fn save(&mut self, filter: Filter) {
        self.db.set(Key::SidebarFilter, &filter.to_text());
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
            menu_items(menu, &view, &machine_items, &project_items)
        })
}

fn menu_items(
    mut menu: PopupMenu,
    view: &WeakEntity<SidebarView>,
    machines: &[(MachineId, SharedString, bool)],
    projects: &[(Place, SharedString, bool)],
) -> PopupMenu {
    let filtering = machines.iter().any(|(_, _, shown)| !shown)
        || projects.iter().any(|(_, _, chosen)| *chosen);
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
    if filtering {
        menu = menu.separator().item(
            PopupMenuItem::new("Show all")
                .on_click(on_click(view, |filter| *filter = Filter::default())),
        );
    }
    menu
}
