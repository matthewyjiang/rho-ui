//! The panel at the right of the window: a home for surfaces, the tools
//! that sit beside a session, such as its background work. Ctrl+Alt+B or
//! the toggle at the right of the title bar shows and hides it. Drag its
//! left edge to resize it, and double-click the edge for its default width.
//! The state database remembers both ([`PanelLayout`]).
//!
//! Each surface opens as a tab in the panel's header; hovering a tab swaps
//! its icon for × to close it, and dragging one onto another moves it
//! there. With none open, the panel lists the surfaces to open; while
//! another isn't open, + in the header opens a menu of them. The panel is
//! the window's, not a session's: its tabs stay as sessions change, and
//! each surface shows the session on screen.
//!
//! To add a surface: add a [`Surface`] variant with its name and icon, and
//! draw it in [`RightPanel::render_surface`]. A surface that keeps state,
//! such as [`diff::DiffSurface`], is a view of its own, made when its tab
//! opens ([`Tab`]) and told which session is on screen.

mod diff;
mod terminal;

use std::cmp::Ordering;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyDescription, EmptyHeader},
    h_flex,
    list::ListItem,
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    resizable::{resize_handle, HandleEdge},
    v_flex, ActiveTheme as _, Icon, IconName, InteractiveElementExt as _, Selectable as _,
    Sizable as _, StyledExt as _,
};
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, Axis, BorrowAppContext as _, Context, Entity,
    Global, InteractiveElement as _, IntoElement, ParentElement as _, Rems, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window,
};

use self::diff::DiffSurface;
pub(crate) use self::terminal::TerminalShell;
use self::terminal::TerminalSurface;
use crate::{
    session_view::SessionView,
    store::{Key, StateDb},
    title_bar::TOP_ROW_REMS,
    workspaces::Workspaces,
};

/// The panel's default width, in rems so it follows the UI scale: at the
/// default scale, 384px, which leaves a 1200px window's transcript 528px
/// beside the open sidebar.
const DEFAULT_WIDTH_REMS: f32 = 24.;

/// The narrowest the panel goes: 256px at the default scale, room for a
/// tab, + and the toggle in its header.
const MIN_WIDTH_REMS: f32 = 16.;

/// The narrowest the panel leaves the main pane: 480px at the default
/// scale. In the UI tests' screenshots the composer's footer lays out its
/// buttons in about 400px, and the composer sits 40px in from the pane's
/// edges.
const MIN_PANE_REMS: f32 = 30.;

/// What the panel can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surface {
    /// The session's background work, running and ended.
    Background,
    /// The uncommitted changes where the session works.
    Diff,
    /// A shell in the session's folder.
    Terminal,
}

impl Surface {
    /// Every surface, in the order the panel lists them.
    const ALL: [Surface; 3] = [Surface::Terminal, Surface::Diff, Surface::Background];

    /// The surface's part of its elements' IDs.
    fn id(self) -> &'static str {
        match self {
            Surface::Background => "background",
            Surface::Diff => "diff",
            Surface::Terminal => "terminal",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Surface::Background => "Background work",
            Surface::Diff => "Diff",
            Surface::Terminal => "Terminal",
        }
    }

    fn icon(self) -> Lucide {
        match self {
            Surface::Background => Lucide::ListChecks,
            Surface::Diff => Lucide::FileDiff,
            Surface::Terminal => Lucide::SquareTerminal,
        }
    }
}

/// An open surface, with its view if it keeps state.
enum Tab {
    Background,
    Diff(Entity<DiffSurface>),
    Terminal(Entity<TerminalSurface>),
}

impl Tab {
    fn surface(&self) -> Surface {
        match self {
            Tab::Background => Surface::Background,
            Tab::Diff(_) => Surface::Diff,
            Tab::Terminal(_) => Surface::Terminal,
        }
    }
}

/// Whether the panel shows, and how wide, remembered across runs under
/// [`Key::RightPanel`] and [`Key::RightPanelWidth`]. A global, as the title
/// bar of every pane offers the toggle while it is hidden.
#[derive(Debug)]
pub(crate) struct PanelLayout {
    shown: bool,
    /// In rems, as the user last left it; [`Self::width`] fits it to the
    /// window.
    width: f32,
    db: StateDb,
}

impl Global for PanelLayout {}

impl PanelLayout {
    /// Loads the saved layout: hidden unless it was shown, at the default
    /// width unless it was resized.
    pub(crate) fn load(db: &StateDb) -> Self {
        let width = db
            .get(Key::RightPanelWidth)
            .and_then(|width| width.parse::<f32>().ok())
            .filter(|width| width.is_finite())
            .unwrap_or(DEFAULT_WIDTH_REMS);
        Self {
            shown: db.get(Key::RightPanel).as_deref() == Some("shown"),
            width,
            db: db.clone(),
        }
    }

    /// Whether the panel shows.
    pub(crate) fn get(cx: &App) -> bool {
        cx.try_global::<Self>().is_some_and(|this| this.shown)
    }

    /// The panel's width in a row `row` wide that also holds a sidebar
    /// `sidebar` wide, both in rems: as the user left it, but leaving the
    /// main pane [`MIN_PANE_REMS`], and never under [`MIN_WIDTH_REMS`].
    pub(crate) fn width(row: f32, sidebar: f32, cx: &App) -> Rems {
        let width = cx
            .try_global::<Self>()
            .map_or(DEFAULT_WIDTH_REMS, |this| this.width);
        rems(fit(width, row - sidebar - MIN_PANE_REMS))
    }

    /// Shows the panel, or hides it, and saves which.
    pub(crate) fn toggle(cx: &mut App) {
        cx.update_global(|this: &mut Self, _| {
            this.shown = !this.shown;
            let value = if this.shown { "shown" } else { "hidden" };
            this.db.set(Key::RightPanel, value);
        });
    }

    /// Makes the panel `width` rems wide, fitted to a row `row` wide beside
    /// a sidebar `sidebar` wide, as a drag of its edge goes. Saved by
    /// [`Self::save_width`] when the drag ends.
    pub(crate) fn resize(width: f32, row: f32, sidebar: f32, cx: &mut App) {
        let width = fit(width, row - sidebar - MIN_PANE_REMS);
        cx.update_global(|this: &mut Self, _| this.width = width);
    }

    /// Saves the width a drag left.
    pub(crate) fn save_width(cx: &App) {
        let this = cx.global::<Self>();
        this.db.set(Key::RightPanelWidth, &this.width.to_string());
    }

    /// Puts the panel back to its default width, and saves it.
    fn reset_width(cx: &mut App) {
        cx.update_global(|this: &mut Self, _| this.width = DEFAULT_WIDTH_REMS);
        Self::save_width(cx);
    }
}

/// `width`, held to the `room` there is, but at least [`MIN_WIDTH_REMS`]:
/// a window too narrow for both squeezes the pane, not the panel.
fn fit(width: f32, room: f32) -> f32 {
    width.min(room).max(MIN_WIDTH_REMS)
}

/// What a drag of the panel's edge carries. The drag draws nothing: the
/// panel itself follows the pointer.
#[derive(Clone, Copy)]
pub(crate) struct ResizeRightPanel;

impl Render for ResizeRightPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        gpui_kit::Empty
    }
}

/// What a drag of a tab carries: its surface, and where it sat when the
/// drag began. It draws the tab under the pointer.
#[derive(Clone, Copy)]
struct DragTab {
    surface: Surface,
    index: usize,
}

impl Render for DragTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .text_sm()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .opacity(0.85)
            .child(Icon::new(self.surface.icon()).small())
            .child(self.surface.label())
    }
}

/// Shows the panel, or hides it. Where it sits, at the window's right edge
/// in the title bar or in the panel's header, it stays put, so a second
/// click undoes the first.
pub(crate) fn toggle_button(cx: &App) -> Button {
    let (icon, label) = if PanelLayout::get(cx) {
        (IconName::PanelRightClose, "Hide panel")
    } else {
        (IconName::PanelRightOpen, "Show panel")
    };
    Button::new("toggle-right-panel")
        .ghost()
        .small()
        .icon(icon)
        .accessibility_label(label)
        .tooltip(format!("{label} (Ctrl+Alt+B)"))
        .on_click(|_, _, cx| PanelLayout::toggle(cx))
}

/// The panel's tabs and what it shows. [`crate::app::RhoApp`] lays it out
/// while [`PanelLayout`], and tells it which session is on screen.
pub(crate) struct RightPanel {
    /// Where surfaces read the session's machine.
    workspaces: Entity<Workspaces>,
    /// What a terminal on this machine runs.
    shell: TerminalShell,
    /// The open surfaces, in the order opened.
    tabs: Vec<Tab>,
    /// One of `tabs`, or `None` for the list of surfaces when none is open.
    active: Option<Surface>,
    /// The tab under the pointer, which shows × in place of its icon.
    hovered: Option<Surface>,
    /// The session on screen, if any, with the subscription that redraws
    /// the panel as it changes.
    session: Option<(Entity<SessionView>, Subscription)>,
}

impl RightPanel {
    pub(crate) fn new(workspaces: Entity<Workspaces>, shell: TerminalShell) -> Self {
        Self {
            workspaces,
            shell,
            tabs: Vec::new(),
            active: None,
            hovered: None,
            session: None,
        }
    }

    /// Follows `session`, the one on screen, or none.
    pub(crate) fn set_session(
        &mut self,
        session: Option<Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.session.as_ref().map(|(view, _)| view) == session.as_ref() {
            return;
        }
        for tab in &self.tabs {
            match tab {
                Tab::Background => {}
                Tab::Diff(diff) => {
                    diff.update(cx, |diff, cx| diff.set_session(session.as_ref(), cx));
                }
                Tab::Terminal(terminal) => {
                    terminal.update(cx, |terminal, cx| {
                        terminal.set_session(session.as_ref(), cx);
                    });
                }
            }
        }
        self.session = session.map(|view| {
            let redraw = cx.observe(&view, |_, _, cx| cx.notify());
            (view, redraw)
        });
        cx.notify();
    }

    /// Shows `surface`, opening its tab if it has none.
    fn open(&mut self, surface: Surface, cx: &mut Context<'_, Self>) {
        if self.tab(surface).is_none() {
            let session = self.session.as_ref().map(|(view, _)| view);
            let tab = match surface {
                Surface::Background => Tab::Background,
                Surface::Diff => {
                    let workspaces = self.workspaces.clone();
                    Tab::Diff(cx.new(|cx| DiffSurface::new(workspaces, session, cx)))
                }
                Surface::Terminal => {
                    let (workspaces, shell) = (self.workspaces.clone(), self.shell.clone());
                    Tab::Terminal(cx.new(|cx| TerminalSurface::new(workspaces, shell, session, cx)))
                }
            };
            self.tabs.push(tab);
        }
        self.active = Some(surface);
        cx.notify();
    }

    /// Closes `surface`'s tab. If it was showing, the last tab opened
    /// shows, or the list of surfaces when none is left.
    fn close(&mut self, surface: Surface, cx: &mut Context<'_, Self>) {
        self.tabs.retain(|tab| tab.surface() != surface);
        if self.hovered == Some(surface) {
            self.hovered = None;
        }
        if self.active == Some(surface) {
            self.active = self.tabs.last().map(Tab::surface);
        }
        cx.notify();
    }

    fn tab(&self, surface: Surface) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.surface() == surface)
    }

    /// The surfaces without a tab, which the list offers.
    fn unopened(&self) -> impl Iterator<Item = Surface> + '_ {
        Surface::ALL
            .into_iter()
            .filter(|&surface| self.tab(surface).is_none())
    }

    /// The files the Diff surface shows, for UI tests.
    #[cfg(test)]
    pub(crate) fn diff_paths(&self, cx: &App) -> Option<Vec<String>> {
        self.tabs.iter().find_map(|tab| match tab {
            Tab::Diff(diff) => Some(diff.read(cx).paths(cx)),
            Tab::Background | Tab::Terminal(_) => None,
        })
    }

    /// The screen of the session's terminal, if the Terminal surface is
    /// open and it runs, for UI tests.
    #[cfg(test)]
    pub(crate) fn terminal_text(&self, cx: &mut App) -> Option<Vec<String>> {
        let terminal = self.tabs.iter().find_map(|tab| match tab {
            Tab::Terminal(terminal) => terminal.read(cx).current(),
            Tab::Background | Tab::Diff(_) => None,
        })?;
        Some(terminal.update(cx, |terminal, _| terminal.text()))
    }

    /// Whether + has anything to offer: a surface shows, so the list
    /// doesn't, and another has no tab yet.
    fn offers_more(&self) -> bool {
        self.active.is_some() && self.unopened().next().is_some()
    }

    /// Moves `surface`'s tab to `index`, where a drag dropped it.
    fn move_tab(&mut self, surface: Surface, index: usize, cx: &mut Context<'_, Self>) {
        let Some(from) = self.tabs.iter().position(|tab| tab.surface() == surface) else {
            return;
        };
        let tab = self.tabs.remove(from);
        self.tabs.insert(index.min(self.tabs.len()), tab);
        cx.notify();
    }

    /// The tabs' surfaces, in order, for UI tests.
    #[cfg(test)]
    pub(crate) fn tab_order(&self) -> Vec<Surface> {
        self.tabs.iter().map(Tab::surface).collect()
    }

    /// Notes the pointer over `surface`'s tab.
    fn hover(&mut self, surface: Surface, cx: &mut Context<'_, Self>) {
        self.hovered = Some(surface);
        cx.notify();
    }

    /// Notes the pointer leaving `surface`'s tab, unless another took it.
    fn unhover(&mut self, surface: Surface, cx: &mut Context<'_, Self>) {
        if self.hovered == Some(surface) {
            self.hovered = None;
            cx.notify();
        }
    }

    /// +'s menu: the surfaces without a tab.
    fn surface_menu(
        mut menu: PopupMenu,
        view: &WeakEntity<Self>,
        unopened: &[Surface],
    ) -> PopupMenu {
        for &surface in unopened {
            let view = view.clone();
            menu = menu.item(
                PopupMenuItem::new(surface.label())
                    .icon(Icon::new(surface.icon()))
                    .on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| this.open(surface, cx));
                    }),
            );
        }
        menu
    }

    /// `surface`'s tab, at `index` in the header: its icon, or × while
    /// hovered, and its label. Dragged onto another tab, it moves there.
    fn render_tab(
        &self,
        index: usize,
        surface: Surface,
        cx: &Context<'_, Self>,
    ) -> impl IntoElement {
        // × replaces the icon while hovered, so the tab keeps its width.
        let lead = if self.hovered == Some(surface) {
            Button::new(SharedString::from(format!(
                "close-panel-tab-{}",
                surface.id()
            )))
            .ghost()
            .xsmall()
            .compact()
            .icon(IconName::Close)
            .accessibility_label("Close")
            .tooltip("Close")
            .on_click(cx.listener(move |this, _, _, cx| {
                // Kit's Button passes the click on, and the tab's own
                // click would open the surface again.
                cx.stop_propagation();
                this.close(surface, cx);
            }))
            .into_any_element()
        } else {
            Icon::new(surface.icon()).small().into_any_element()
        };
        let view = cx.entity().downgrade();
        let button = Button::new(SharedString::from(format!("panel-tab-{}", surface.id())))
            .ghost()
            .small()
            .selected(self.active == Some(surface))
            .accessibility_label(surface.label())
            .child(
                h_flex()
                    .size_4()
                    .items_center()
                    .justify_center()
                    .child(lead),
            )
            .child(surface.label())
            .on_hover(move |&hovered, _, cx| {
                let _ = view.update(cx, |this, cx| {
                    if hovered {
                        this.hover(surface, cx);
                    } else {
                        this.unhover(surface, cx);
                    }
                });
            })
            .on_click(cx.listener(move |this, _, _, cx| this.open(surface, cx)));
        // Kit's Button takes no drag, so a div around it carries one.
        div()
            .id(SharedString::from(format!(
                "panel-tab-slot-{}",
                surface.id()
            )))
            .flex_shrink_0()
            .border_x_2()
            .border_color(gpui_kit::transparent_black())
            .child(button)
            .on_drag(DragTab { surface, index }, |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| *drag)
            })
            // The border marks the side the dragged tab lands on.
            .drag_over::<DragTab>(move |style, drag, _, cx| {
                let color = cx.theme().drag_border;
                match drag.index.cmp(&index) {
                    Ordering::Greater => style.border_l_2().border_color(color),
                    Ordering::Less => style.border_r_2().border_color(color),
                    Ordering::Equal => style,
                }
            })
            .on_drop(cx.listener(move |this, drag: &DragTab, _, cx| {
                this.move_tab(drag.surface, index, cx);
            }))
    }

    /// The tabs, +, and the toggle at the right edge, in a row as tall as
    /// the title bar beside it.
    fn render_header(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let menu_view = cx.entity().downgrade();
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| self.render_tab(index, tab.surface(), cx));
        let unopened: Vec<Surface> = self.unopened().collect();
        h_flex()
            .flex_shrink_0()
            .h(rems(TOP_ROW_REMS))
            .gap_1()
            // The title bar's inset, so the toggle doesn't move.
            .px_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .gap_1()
                    .children(tabs)
                    .children(self.offers_more().then(|| {
                        Button::new("panel-surfaces")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .accessibility_label("Open a surface")
                            .tooltip("Open a surface")
                            .dropdown_menu(move |menu, _, _| {
                                Self::surface_menu(menu, &menu_view, &unopened)
                            })
                    })),
            )
            .child(toggle_button(cx))
    }

    /// The surfaces to open, centered.
    fn render_list(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme();
        let row = |id: SharedString, icon: Lucide, label: &'static str| {
            ListItem::new(id)
                .accessibility_label(label)
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .child(
                    h_flex()
                        .gap_3()
                        .child(Icon::new(icon).small())
                        .child(div().flex_1().child(label)),
                )
        };
        let open = self.unopened().map(|surface| {
            row(
                format!("open-surface-{}", surface.id()).into(),
                surface.icon(),
                surface.label(),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open(surface, cx)))
        });
        v_flex()
            .id("panel-surface-list")
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .px_4()
            .child(div().font_medium().child("Open a surface"))
            .child(v_flex().w_full().max_w(rems(18.)).gap_0p5().children(open))
    }

    /// What `tab`'s surface shows of the session on screen.
    fn render_surface(&self, tab: &Tab, cx: &App) -> AnyElement {
        match tab {
            Tab::Background => match &self.session {
                Some((view, _)) => SessionView::background_surface(view, cx).into_any_element(),
                None => no_session("Open a session to see its background work."),
            },
            Tab::Diff(diff) => diff.clone().into_any_element(),
            Tab::Terminal(terminal) => terminal.clone().into_any_element(),
        }
    }
}

/// A surface's place when it needs a session and none is on screen.
fn no_session(text: &'static str) -> AnyElement {
    Empty::new()
        .h_full()
        .header(EmptyHeader::new().description(EmptyDescription::new().child(text)))
        .into_any_element()
}

impl Render for RightPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let body = match self.active.and_then(|surface| self.tab(surface)) {
            Some(tab) => self.render_surface(tab, cx),
            None => self.render_list(cx).into_any_element(),
        };
        v_flex()
            .id("right-panel")
            .test_support()
            .relative()
            .size_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(body))
            // [`crate::app::RhoApp`] follows the drag, as it lays out the
            // row the panel sits in.
            .child(
                resize_handle::<ResizeRightPanel, ResizeRightPanel>(
                    "right-panel-edge",
                    Axis::Horizontal,
                )
                .inside(HandleEdge::Leading)
                .on_drag(ResizeRightPanel, |drag, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| *drag)
                })
                .on_double_click(|_, _, cx| PanelLayout::reset_width(cx)),
            )
    }
}
