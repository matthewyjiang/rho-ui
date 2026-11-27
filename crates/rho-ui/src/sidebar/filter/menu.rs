//! The popover the filter's button opens: a header with Clear, then each
//! project to keep to, then with more than one machine each machine to
//! show. A click on a row turns it on or off and leaves the popover open;
//! hovering one offers Only, which keeps to it alone. Escape or a click
//! outside closes it.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    list::ListItem,
    scroll::ScrollableElement as _,
    separator::Separator,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    accesskit::Toggled, div, rems, AnyElement, App, Entity, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    WeakEntity, Window,
};

use super::{on_click, Connection, Filter, MachineChoice, Offer, ProjectChoice, SidebarView};
use crate::session_list::contains_ignore_case;

/// The rows' height at most before they scroll: about 13 rows, which
/// leaves the popover inside the smallest window the app opens.
const ROWS_REMS: f32 = 24.;

/// The popover's content: `offer` as `filter` stands, its projects narrowed
/// by `search` when there is one. A row's click changes the filter of
/// `view`.
pub(super) fn render(
    offer: &Offer,
    filter: &Filter,
    search: Option<&Entity<InputState>>,
    view: &WeakEntity<SidebarView>,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    // The row under the pointer, which offers Only; gone with the popover.
    let hovered = window.use_keyed_state("sidebar-filter-hovered", cx, |_, _| None);
    let query = search.map(|search| search.read(cx).value());
    let query = query.as_ref().map_or("", |query| query.trim());
    let rows = Rows {
        filter,
        hovered: &hovered,
        view,
    };
    let projects: Vec<AnyElement> = offer
        .projects
        .iter()
        .filter(|project| {
            contains_ignore_case(&project.name, query)
                || (project.machine.as_ref())
                    .is_some_and(|machine| contains_ignore_case(machine, query))
        })
        .map(|project| rows.project(project, cx))
        .collect();
    let muted = cx.theme().muted_foreground;
    let note = |text: &'static str| div().px_2().py_1().text_xs().text_color(muted).child(text);
    let choices = if offer.is_empty() {
        note("Add a project or a machine to filter by").into_any_element()
    } else {
        v_flex()
            .id("sidebar-filter-choices")
            .max_h(rems(ROWS_REMS))
            .overflow_y_scrollbar()
            .when(!offer.projects.is_empty(), |list| {
                list.child(heading("Projects", cx))
                    .when(projects.is_empty(), |list| {
                        list.child(note("No matching projects"))
                    })
                    .children(projects)
            })
            .when(!offer.machines.is_empty(), |list| {
                list.when(!offer.projects.is_empty(), |list| {
                    list.child(Separator::horizontal().my_1())
                })
                .child(heading("Machines", cx))
                .children(
                    (offer.machines.iter())
                        .map(|machine| rows.machine(machine, &offer.machines, cx)),
                )
            })
            .into_any_element()
    };
    v_flex()
        .id("sidebar-filter-menu")
        .test_support()
        .w_64()
        .gap_1()
        .text_sm()
        .child(header(filter, view))
        .when_some(search, |menu, search| {
            menu.child(
                Input::new(search)
                    .small()
                    .prefix(Icon::new(IconName::Search).small().text_color(muted))
                    .cleanable(true),
            )
        })
        .child(choices)
}

/// "Filter", and Clear while the filter hides anything.
fn header(filter: &Filter, view: &WeakEntity<SidebarView>) -> impl IntoElement {
    h_flex()
        .justify_between()
        .pl_2()
        .child(div().font_semibold().child("Filter"))
        .child(
            Button::new("sidebar-filter-clear")
                .ghost()
                .xsmall()
                .label("Clear")
                .disabled(filter.is_empty())
                .on_click(on_click(view, |filter| *filter = Filter::default())),
        )
}

/// A section's heading, as a menu labels its groups.
fn heading(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .px_2()
        .pt_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// What every row needs: the filter it shows, which row is under the
/// pointer, and the view whose filter a click changes.
struct Rows<'a> {
    filter: &'a Filter,
    hovered: &'a Entity<Option<SharedString>>,
    view: &'a WeakEntity<SidebarView>,
}

impl Rows<'_> {
    /// A project's row: on while the filter keeps to it, dimmed while it
    /// keeps to others.
    fn project(&self, project: &ProjectChoice, cx: &App) -> AnyElement {
        let chosen = self.filter.chose(&project.place);
        let place = project.place.clone();
        let only = place.clone();
        self.row(
            Row {
                id: project.element_id("project"),
                only_id: project.element_id("only-project"),
                name: project.name.clone(),
                sub: project.machine.clone(),
                on: chosen,
                dimmed: !chosen && !self.filter.projects().is_empty(),
                count: project.families.to_string().into(),
                mark: super::super::badge(project.initials.clone()).into_any_element(),
            },
            on_click(self.view, move |filter| filter.toggle_project(&place)),
            on_click(self.view, move |filter| filter.only_project(&only)),
            cx,
        )
    }

    /// A machine's row: on while its sessions show. It says how its
    /// connection is doing, and with none and no session known, a dash for
    /// its count, as rho-ui can't tell.
    fn machine(&self, machine: &MachineChoice, offered: &[MachineChoice], cx: &App) -> AnyElement {
        let shown = !self.filter.hides(&machine.id);
        let count = if machine.families == 0 && machine.connection != Connection::Connected {
            "–".into()
        } else {
            machine.families.to_string().into()
        };
        let id = machine.id.clone();
        let only = machine.id.clone();
        let all: Vec<_> = offered.iter().map(|machine| machine.id.clone()).collect();
        self.row(
            Row {
                id: machine.element_id("machine"),
                only_id: machine.element_id("only-machine"),
                name: machine.label.clone(),
                sub: Some(machine.connection.words(&machine.id).into()),
                on: shown,
                dimmed: !shown,
                count,
                mark: dot(machine.connection, cx).into_any_element(),
            },
            on_click(self.view, move |filter| filter.toggle_machine(&id)),
            on_click(self.view, move |filter| filter.only_machine(&only, &all)),
            cx,
        )
    }

    fn row(
        &self,
        row: Row,
        toggle: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
        only: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme();
        let hovered = self.hovered.read(cx).as_ref() == Some(&row.id);
        let trailing = if hovered {
            div()
                // Only acts on the row; it must not also turn it on or off.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Button::new(row.only_id)
                        .ghost()
                        .xsmall()
                        .label("Only")
                        .accessibility_label(format!("Only {}", row.name))
                        .on_click(only),
                )
                .into_any_element()
        } else {
            div()
                .px_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(row.count)
                .into_any_element()
        };
        let toggled = if row.on {
            Toggled::True
        } else {
            Toggled::False
        };
        let state = self.hovered.clone();
        let id = row.id.clone();
        ListItem::new(row.id)
            .accessibility_label(row.name.clone())
            .aria_toggled(toggled)
            .rounded(theme.radius)
            .px_2()
            .text_sm()
            .on_hover(move |over, _, cx| {
                state.update(cx, |hovered, cx| {
                    if *over {
                        *hovered = Some(id.clone());
                    } else if hovered.as_ref() == Some(&id) {
                        *hovered = None;
                    }
                    cx.notify();
                });
            })
            .on_click(toggle)
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .child(check(toggled, cx))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .when(row.dimmed, |this| this.opacity(0.5))
                            .child(row.mark)
                            .child(div().min_w_0().truncate().child(row.name))
                            .children(row.sub.map(|sub| {
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(sub)
                            })),
                    )
                    .child(div().flex_shrink_0().child(trailing)),
            )
            .into_any_element()
    }
}

/// What a row shows, whichever it stands for.
struct Row {
    id: SharedString,
    only_id: SharedString,
    name: SharedString,
    /// Muted words after the name.
    sub: Option<SharedString>,
    /// Checked: a project kept to, a machine shown.
    on: bool,
    /// Faded: a project the filter leaves out, a hidden machine.
    dimmed: bool,
    /// Its families of sessions, or a dash when unknown.
    count: SharedString,
    /// Its badge or status dot.
    mark: AnyElement,
}

/// A row's check, or the space it takes.
fn check(toggled: Toggled, cx: &App) -> impl IntoElement {
    div()
        .w_4()
        .flex_shrink_0()
        .when(toggled == Toggled::True, |this| {
            this.child(Icon::new(Lucide::Check).small().text_color(cx.theme().blue))
        })
}

/// A machine's status dot, colored by how its connection is doing.
pub(super) fn dot(connection: Connection, cx: &App) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .size_2()
        .rounded_full()
        .bg(connection.color(cx.theme()))
}
