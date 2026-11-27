//! The filter as chips under the sidebar's search, while it hides
//! anything, so a missing session never goes unexplained: one for each
//! project it keeps to, one struck through for each machine it hides, each
//! with × to take that part off, and a dashed Clear that takes it all off.
//! They wrap onto more lines in a narrow sidebar. With no filter, there is
//! no row, and the list starts where it would.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    tag::Tag,
    ActiveTheme as _, Icon, IconName, Sizable as _,
};
use gpui_kit::{
    div, AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, WeakEntity,
};

use super::{menu::dot, on_click, Filter, MachineChoice, ProjectChoice, SidebarView};

/// The chips of the sidebar's filter, or nothing when it hides nothing.
/// Projects come in the order the user's projects list them, and machines
/// in the settings' order.
pub(in crate::sidebar) fn chips(
    sidebar: &SidebarView,
    cx: &mut Context<'_, SidebarView>,
) -> Option<impl IntoElement> {
    let filter = sidebar.list.filter();
    if filter.is_empty() {
        return None;
    }
    let view = cx.entity().downgrade();
    let machines = sidebar.machines.read(cx);
    let workspaces = sidebar.workspaces.read(cx);
    let listed: Vec<_> = sidebar.projects.read(cx).roots().cloned().collect();
    // A chosen project not listed yet, while its machine's clone folder
    // hasn't been read, comes last.
    let projects = (listed.iter())
        .filter(|place| filter.chose(place))
        .chain(
            filter
                .projects()
                .iter()
                .filter(|place| !listed.contains(place)),
        )
        .map(|place| ProjectChoice::new(place.clone(), workspaces, machines));
    let hidden = (machines.iter().map(|machine| &machine.id))
        .filter(|id| filter.hides(id))
        .chain((filter.hidden().iter()).filter(|id| machines.get(id).is_none()))
        .map(|id| MachineChoice::new(id, machines));
    let mut chips: Vec<AnyElement> = projects
        .map(|project| project_chip(&project, &view, cx))
        .collect();
    chips.extend(hidden.map(|machine| machine_chip(&machine, &view, cx)));
    chips.push(
        Button::new("sidebar-filter-chips-clear")
            .ghost()
            .xsmall()
            .rounded_full()
            .border_1()
            .border_dashed()
            .border_color(cx.theme().border)
            .label("Clear")
            .accessibility_label("Clear the filter")
            .on_click(on_click(&view, |filter| *filter = Filter::default()))
            .into_any_element(),
    );
    Some(
        h_flex()
            .id("sidebar-filter-chips")
            .test_support()
            .flex_shrink_0()
            .flex_wrap()
            .gap_1()
            .px_3()
            .pb_2()
            .children(chips),
    )
}

/// A chosen project's chip: its badge and name, its machine's label when
/// that isn't this machine, as two machines' checkouts share a name, and ×
/// to stop keeping to it.
fn project_chip(project: &ProjectChoice, view: &WeakEntity<SidebarView>, cx: &App) -> AnyElement {
    let place = project.place.clone();
    chip(
        cx,
        super::super::badge(project.initials.clone()).into_any_element(),
        h_flex()
            .gap_1()
            .child(project.name.clone())
            .children(
                (project.machine.clone())
                    .map(|machine| div().text_color(cx.theme().muted_foreground).child(machine)),
            )
            .into_any_element(),
        Button::new(project.element_id("chip-project"))
            .accessibility_label(format!("Stop keeping to {}", project.name))
            .on_click(on_click(view, move |filter| filter.drop_project(&place))),
    )
}

/// A hidden machine's chip: its status dot and its label struck through,
/// and × to show it again.
fn machine_chip(machine: &MachineChoice, view: &WeakEntity<SidebarView>, cx: &App) -> AnyElement {
    let id = machine.id.clone();
    chip(
        cx,
        dot(machine.connection, cx).into_any_element(),
        div()
            .line_through()
            .text_color(cx.theme().muted_foreground)
            .child(machine.label.clone())
            .into_any_element(),
        Button::new(machine.element_id("chip-machine"))
            .accessibility_label(format!("Show {}", machine.label))
            .on_click(on_click(view, move |filter| filter.show_machine(&id))),
    )
}

/// A chip of `mark` and `label`, ended by `remove`, drawn as ×.
fn chip(cx: &App, mark: AnyElement, label: AnyElement, remove: Button) -> AnyElement {
    Tag::secondary()
        .rounded_full()
        .gap_1()
        .py_0()
        .pl_1()
        .pr_0()
        .text_color(cx.theme().foreground)
        .child(mark)
        .child(label)
        .child(
            remove
                .ghost()
                .xsmall()
                .rounded_full()
                .icon(Icon::new(IconName::Close)),
        )
        .into_any_element()
}
