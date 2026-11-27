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

use std::rc::Rc;

use super::{on_click, Connection, Filter, MachineChoice, Offer, Part, ProjectChoice, SidebarView};
use crate::machines::MachineId;
use crate::session_list::contains_ignore_case;
use crate::sidebar::badge;

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
        machines: offer
            .machines
            .iter()
            .map(|(machine, _)| machine.id.clone())
            .collect(),
    };
    let projects: Vec<AnyElement> = offer
        .projects
        .iter()
        .filter(|(project, _)| {
            contains_ignore_case(&project.name, query)
                || (project.machine.as_ref())
                    .is_some_and(|machine| contains_ignore_case(machine, query))
        })
        .map(|(project, families)| rows.project(project, *families, window, cx))
        .collect();
    let machines: Vec<AnyElement> = (offer.machines.iter())
        .map(|(machine, families)| rows.machine(machine, *families, window, cx))
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
                .children(machines)
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
                .on_click(on_click(view, Filter::clear)),
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
/// pointer, the view whose filter a click changes, and the machines
/// offered, which a machine's Only hides but for it.
struct Rows<'a> {
    filter: &'a Filter,
    hovered: &'a Entity<Option<SharedString>>,
    view: &'a WeakEntity<SidebarView>,
    machines: Rc<[MachineId]>,
}

impl Rows<'_> {
    /// A project's row: on while the filter keeps to it, dimmed while it
    /// keeps to others.
    fn project(
        &self,
        project: &ProjectChoice,
        families: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let chosen = self.filter.chose(&project.place);
        let place = project.place.clone();
        let only = place.clone();
        self.row(
            Row {
                id: project.element_id(Part::Row),
                only_id: project.element_id(Part::Only),
                name: project.name.clone(),
                sub: project.machine.clone(),
                on: chosen,
                dimmed: !chosen && !self.filter.projects().is_empty(),
                count: Some(families),
                mark: badge(project.initials.clone()).into_any_element(),
            },
            on_click(self.view, move |filter| filter.toggle_project(&place)),
            on_click(self.view, move |filter| filter.only_project(&only)),
            window,
            cx,
        )
    }

    /// A machine's row: on while its sessions show. It says how its
    /// connection is doing, and with none and no session known, a dash for
    /// its count, as rho-ui can't tell.
    fn machine(
        &self,
        machine: &MachineChoice,
        families: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let shown = !self.filter.hides(&machine.id);
        let known = families > 0 || machine.connection == Connection::Connected;
        let id = machine.id.clone();
        let only = machine.id.clone();
        let all = Rc::clone(&self.machines);
        self.row(
            Row {
                id: machine.element_id(Part::Row),
                only_id: machine.element_id(Part::Only),
                name: machine.label.clone(),
                sub: Some(machine.connection.words(&machine.id).into()),
                on: shown,
                dimmed: !shown,
                count: known.then_some(families),
                mark: machine.connection.dot(cx).into_any_element(),
            },
            on_click(self.view, move |filter| filter.toggle_machine(&id)),
            on_click(self.view, move |filter| {
                filter.only_machine(&only, all.iter());
            }),
            window,
            cx,
        )
    }

    fn row(
        &self,
        row: Row,
        toggle: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
        only: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        // Tab reaches each row, and Enter or Space turns it on or off.
        let focus_key = SharedString::from(format!("{}-focus", row.id));
        let focus = (window
            .use_keyed_state(focus_key, cx, |_, cx| cx.focus_handle().tab_stop(true)))
        .read(cx)
        .clone();
        let hovered = self.hovered.read(cx).as_ref() == Some(&row.id);
        let theme = cx.theme();
        // Only shows for the keyboard too, while the row or Only has it.
        let trailing = if hovered || focus.contains_focused(window, cx) {
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
                .child(
                    row.count
                        .map_or_else(|| "–".into(), |count| count.to_string()),
                )
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
            .track_focus(&focus)
            .when(focus.is_focused(window), |this| this.bg(theme.list_hover))
            // As Kit's Button does: a click leaves the keyboard where it was.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
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
    /// Its families of sessions; `None`, drawn as a dash, when rho-ui
    /// can't tell.
    count: Option<usize>,
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
