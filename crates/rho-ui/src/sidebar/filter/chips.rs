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
    scroll::ScrollableElement as _,
    tag::Tag,
    ActiveTheme as _, Icon, IconName, Sizable as _,
};
use gpui_kit::{
    div, rems, AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, WeakEntity,
};

use super::{on_click, Filter, MachineChoice, Part, ProjectChoice, SidebarView};
use crate::sidebar::badge;

/// The chips' height at most before they scroll, so many chips can't push
/// out the list: three lines of them, each a chip with an xsmall ×
/// (`h_5`, 1.25rem) inside its 1px border, with `gap_1` (0.25rem) between,
/// 4.25rem and 6px; at a 16px rem, 74px of 76px. The fourth line starts
/// below it at any UI scale.
const CHIPS_REMS: f32 = 4.75;

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
            .on_click(on_click(&view, Filter::clear))
            .into_any_element(),
    );
    // Kit's scroll area names the element it scrolls after its own call
    // site and fills its parent, so a parent sized by the chips holds the
    // ID tests find them by.
    Some(
        div()
            .id("sidebar-filter-chips")
            .test_support()
            .flex_shrink_0()
            .mx_3()
            .mb_2()
            .child(
                div()
                    .id("sidebar-filter-chips-scroll")
                    .max_h(rems(CHIPS_REMS))
                    .overflow_y_scrollbar()
                    .child(h_flex().flex_wrap().gap_1().children(chips)),
            ),
    )
}

/// A chosen project's chip: its badge and name, its machine's label when
/// that isn't this machine, as two machines' checkouts share a name, and ×
/// to stop keeping to it.
fn project_chip(project: &ProjectChoice, view: &WeakEntity<SidebarView>, cx: &App) -> AnyElement {
    let place = project.place.clone();
    chip(
        cx,
        badge(project.initials.clone()).into_any_element(),
        h_flex()
            .min_w_0()
            .gap_1()
            .child(div().min_w_0().truncate().child(project.name.clone()))
            .children((project.machine.clone()).map(|machine| {
                (div().min_w_0().truncate())
                    .text_color(cx.theme().muted_foreground)
                    .child(machine)
            }))
            .into_any_element(),
        Button::new(project.element_id(Part::Chip))
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
        machine.connection.dot(cx).into_any_element(),
        div()
            .min_w_0()
            .truncate()
            .line_through()
            .text_color(cx.theme().muted_foreground)
            .child(machine.label.clone())
            .into_any_element(),
        Button::new(machine.element_id(Part::Chip))
            .accessibility_label(format!("Show {}", machine.label))
            .on_click(on_click(view, move |filter| filter.show_machine(&id))),
    )
}

/// A chip of `mark` and `label`, ended by `remove`, drawn as ×. A label
/// too long for the sidebar is cut short, so × stays in it.
fn chip(cx: &App, mark: AnyElement, label: AnyElement, remove: Button) -> AnyElement {
    Tag::secondary()
        .max_w_full()
        .min_w_0()
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
                .flex_shrink_0()
                .ghost()
                .xsmall()
                .rounded_full()
                .icon(Icon::new(IconName::Close)),
        )
        .into_any_element()
}
