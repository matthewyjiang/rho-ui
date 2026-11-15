//! The panel at the right of the window: a home for surfaces, the tools
//! that sit beside a session, such as its background work. Ctrl+Alt+B or
//! the toggle at the right of the title bar shows and hides it. Drag its
//! left edge to resize it, and double-click the edge for its default width.
//! The state database remembers both ([`PanelLayout`]).
//!
//! Each surface opens as a tab in the panel's header; hovering a tab swaps
//! its icon for × to close it, and dragging one onto another moves it
//! there. Terminal opens as many tabs as asked, each named for the program
//! it runs; the others open one. With none open, the panel lists the
//! surfaces to open; + in the header opens a menu of the rest. The panel is
//! the window's, not a session's: its tabs stay as sessions change, and
//! each surface shows the session on screen.
//!
//! Ghostty's shortcuts for tabs and splits ([`TerminalShortcut`]) act on
//! the terminal with the keyboard, or else the one under the pointer.
//!
//! To add a surface: add a [`Surface`] variant with its name and icon, and
//! draw it in [`RightPanel::render_surface`]. A surface that keeps state,
//! such as [`diff::DiffSurface`], is a view of its own, made when its tab
//! opens ([`Tab`]) and told which session is on screen.

mod diff;
mod files;
mod terminal;

use std::cmp::Ordering;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia},
    h_flex,
    list::ListItem,
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
    resizable::{resize_handle, HandleEdge},
    v_flex, ActiveTheme as _, Icon, IconName, InteractiveElementExt as _, Selectable as _,
    Sizable as _, StyledExt as _,
};
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, Axis, BorrowAppContext as _, Context, Entity,
    Global, InteractiveElement as _, IntoElement, ParentElement as _, Rems, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window,
};

use self::diff::DiffSurface;
use self::files::FilesSurface;
pub(crate) use self::terminal::{Direction, PaneCommand, TerminalShell};
use self::terminal::{Emptied, TerminalTab};
#[cfg(test)]
pub(crate) use self::terminal::{TerminalView, PROGRAM_POLL};
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
pub(crate) const MIN_PANE_REMS: f32 = 30.;

/// What the panel can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surface {
    /// The session's background work, running and ended.
    Background,
    /// The uncommitted changes where the session works.
    Diff,
    /// The files where the session works, and one opened to read.
    Files,
    /// A shell in the session's folder.
    Terminal,
}

impl Surface {
    /// Every surface, in the order the panel lists them.
    const ALL: [Surface; 4] = [
        Surface::Files,
        Surface::Terminal,
        Surface::Diff,
        Surface::Background,
    ];

    /// The surface's part of its elements' IDs.
    fn id(self) -> &'static str {
        match self {
            Surface::Background => "background",
            Surface::Diff => "diff",
            Surface::Files => "files",
            Surface::Terminal => "terminal",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Surface::Background => "Background work",
            Surface::Diff => "Diff",
            Surface::Files => "Files",
            Surface::Terminal => "Terminal",
        }
    }

    fn icon(self) -> Lucide {
        match self {
            Surface::Background => Lucide::ListChecks,
            Surface::Diff => Lucide::FileDiff,
            Surface::Files => Lucide::FolderTree,
            Surface::Terminal => Lucide::SquareTerminal,
        }
    }

    /// Whether the surface opens a tab each time it is asked: each terminal
    /// runs shells of its own, where another tab of the others would show
    /// the same thing twice.
    fn opens_many(self) -> bool {
        matches!(self, Surface::Terminal)
    }
}

/// Which tab: its surface, and its number among that surface's tabs, from
/// 1 and never reused. Only Terminal opens more than one
/// ([`Surface::opens_many`]); the others are always 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TabKey {
    surface: Surface,
    number: u32,
}

impl TabKey {
    /// The tab's part of its elements' IDs: its surface's, and its number
    /// after the first, as in `terminal-2`.
    fn id(self) -> String {
        match self.number {
            1 => self.surface.id().to_owned(),
            number => format!("{}-{number}", self.surface.id()),
        }
    }
}

/// An open surface, with its view if it keeps state.
enum Tab {
    Background,
    Diff(Entity<DiffSurface>),
    Files(Entity<FilesSurface>),
    /// One of any number of terminal tabs, by its number, with the
    /// subscriptions that redraw the panel as its program changes and close
    /// the tab when its last pane closes.
    Terminal {
        number: u32,
        view: Entity<TerminalTab>,
        _subscriptions: [Subscription; 2],
    },
}

impl Tab {
    fn key(&self) -> TabKey {
        let (surface, number) = match self {
            Tab::Background => (Surface::Background, 1),
            Tab::Diff(_) => (Surface::Diff, 1),
            Tab::Files(_) => (Surface::Files, 1),
            Tab::Terminal { number, .. } => (Surface::Terminal, *number),
        };
        TabKey { surface, number }
    }

    /// What the tab says: a terminal's names the program it runs, as `vim`.
    fn label(&self, cx: &App) -> SharedString {
        match self {
            Tab::Terminal { view, .. } => view
                .read(cx)
                .program(cx)
                .unwrap_or_else(|| Surface::Terminal.label().into()),
            Tab::Background | Tab::Diff(_) | Tab::Files(_) => self.key().surface.label().into(),
        }
    }
}

/// A shortcut for terminal tabs and splits, after Ghostty's, and the action
/// its keys are bound to (`TERMINAL_KEYS` in `main.rs`). It acts on the terminal with the
/// keyboard, or else the one under the pointer; with neither, the key goes
/// on as if unbound.
#[derive(Clone, Copy, Debug, PartialEq, gpui_kit::Action)]
#[action(namespace = rho_ui, no_json)]
pub(crate) enum TerminalShortcut {
    /// A terminal tab, next to the others.
    NewTab,
    /// The next terminal tab, round to the first.
    NextTab,
    /// The previous terminal tab, round to the last.
    PreviousTab,
    /// What a pane of the terminal does ([`PaneCommand`]).
    Pane(PaneCommand),
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

/// Which way [`RightPanel::cycle_terminals`] goes.
#[derive(Clone, Copy)]
enum Step {
    Next,
    Previous,
}

/// What a drag of a tab carries: the tab, what it says, and where it sat
/// when the drag began. It draws the tab under the pointer.
#[derive(Clone)]
struct DragTab {
    key: TabKey,
    label: SharedString,
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
            .child(Icon::new(self.key.surface.icon()).small())
            .child(self.label.clone())
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
        .tooltip_with_action(label, &crate::ToggleRightPanel, Some("RhoApp"))
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
    active: Option<TabKey>,
    /// The tab under the pointer, which shows × in place of its icon.
    hovered: Option<TabKey>,
    /// Scrolls the header's tabs when more are open than fit.
    tab_scroll: ScrollHandle,
    /// The number the next terminal tab gets.
    next_terminal: u32,
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
            tab_scroll: ScrollHandle::new(),
            next_terminal: 1,
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
                Tab::Files(files) => {
                    files.update(cx, |files, cx| files.set_session(session.as_ref(), cx));
                }
                Tab::Terminal { view, .. } => {
                    view.update(cx, |tab, cx| tab.set_session(session.as_ref(), cx));
                }
            }
        }
        self.session = session.map(|view| {
            let redraw = cx.observe(&view, |_, _, cx| cx.notify());
            (view, redraw)
        });
        cx.notify();
    }

    /// Shows `surface`: a new tab of a surface that opens many, else its
    /// tab, opened if it has none. Returns the tab shown.
    fn open(&mut self, surface: Surface, window: &Window, cx: &mut Context<'_, Self>) -> TabKey {
        let open = self
            .tabs
            .iter()
            .map(Tab::key)
            .find(|key| key.surface == surface);
        let key = match open {
            Some(key) if !surface.opens_many() => key,
            _ => {
                let tab = self.new_tab(surface, window, cx);
                let key = tab.key();
                self.tabs.push(tab);
                self.tab_scroll.scroll_to_item(self.tabs.len() - 1);
                key
            }
        };
        self.show(key, cx);
        key
    }

    /// A new tab of `surface`.
    fn new_tab(&mut self, surface: Surface, window: &Window, cx: &mut Context<'_, Self>) -> Tab {
        let session = self.session.as_ref().map(|(view, _)| view);
        match surface {
            Surface::Background => Tab::Background,
            Surface::Diff => {
                let workspaces = self.workspaces.clone();
                Tab::Diff(cx.new(|cx| DiffSurface::new(workspaces, session, cx)))
            }
            Surface::Files => {
                let workspaces = self.workspaces.clone();
                Tab::Files(cx.new(|cx| FilesSurface::new(workspaces, session, cx)))
            }
            Surface::Terminal => {
                let number = self.next_terminal;
                self.next_terminal += 1;
                let key = TabKey { surface, number };
                let (workspaces, shell) = (self.workspaces.clone(), self.shell.clone());
                let view = cx.new(|cx| TerminalTab::new(workspaces, shell, session, window, cx));
                let subscriptions = [
                    cx.observe(&view, |_, _, cx| cx.notify()),
                    cx.subscribe(&view, move |this, _, _: &Emptied, cx| this.close(key, cx)),
                ];
                Tab::Terminal {
                    number,
                    view,
                    _subscriptions: subscriptions,
                }
            }
        }
    }

    /// Shows `key`'s tab.
    fn show(&mut self, key: TabKey, cx: &mut Context<'_, Self>) {
        self.active = Some(key);
        cx.notify();
    }

    /// Closes `key`'s tab. If it was showing, the last tab opened shows,
    /// or the list of surfaces when none is left.
    fn close(&mut self, key: TabKey, cx: &mut Context<'_, Self>) {
        self.tabs.retain(|tab| tab.key() != key);
        if self.hovered == Some(key) {
            self.hovered = None;
        }
        if self.active == Some(key) {
            self.active = self.tabs.last().map(Tab::key);
        }
        cx.notify();
    }

    fn tab(&self, key: TabKey) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.key() == key)
    }

    /// The surfaces + and the list offer: those that open many, and those
    /// without a tab.
    fn openable(&self) -> impl Iterator<Item = Surface> + '_ {
        Surface::ALL.into_iter().filter(|&surface| {
            surface.opens_many() || !self.tabs.iter().any(|tab| tab.key().surface == surface)
        })
    }

    /// Runs `shortcut` on the terminal with the keyboard, or else the one
    /// under the pointer, in the tab on screen. Returns whether there was
    /// one, so the key otherwise goes on as if unbound.
    pub(crate) fn terminal_shortcut(
        &mut self,
        shortcut: TerminalShortcut,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let Some(Tab::Terminal { view, .. }) = self.active.and_then(|key| self.tab(key)) else {
            return false;
        };
        let view = view.clone();
        let Some(pane) = view.read(cx).target(window, cx) else {
            return false;
        };
        match shortcut {
            TerminalShortcut::NewTab => {
                let key = self.open(Surface::Terminal, window, cx);
                self.focus_terminal(key, window, cx);
            }
            TerminalShortcut::NextTab => self.cycle_terminals(Step::Next, window, cx),
            TerminalShortcut::PreviousTab => self.cycle_terminals(Step::Previous, window, cx),
            TerminalShortcut::Pane(command) => {
                view.update(cx, |tab, cx| tab.run(command, pane, window, cx));
            }
        }
        true
    }

    /// Shows the terminal tab after or before the one on screen, counting
    /// terminal tabs alone and going round, and puts the keyboard in it.
    fn cycle_terminals(&mut self, step: Step, window: &mut Window, cx: &mut Context<'_, Self>) {
        let terminals: Vec<TabKey> = self
            .tabs
            .iter()
            .map(Tab::key)
            .filter(|key| key.surface == Surface::Terminal)
            .collect();
        let Some(at) = terminals.iter().position(|&key| Some(key) == self.active) else {
            return;
        };
        let count = terminals.len();
        let next = match step {
            Step::Next => (at + 1) % count,
            Step::Previous => (at + count - 1) % count,
        };
        let key = terminals[next];
        if let Some(index) = self.tabs.iter().position(|tab| tab.key() == key) {
            self.tab_scroll.scroll_to_item(index);
        }
        self.show(key, cx);
        self.focus_terminal(key, window, cx);
    }

    /// Puts the keyboard in the pane that last had it in terminal tab `key`.
    fn focus_terminal(&self, key: TabKey, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(Tab::Terminal { view, .. }) = self.tab(key) {
            let view = view.clone();
            view.update(cx, |tab, cx| tab.focus(window, cx));
        }
    }

    /// The files the Diff surface shows, for UI tests.
    #[cfg(test)]
    pub(crate) fn diff_paths(&self, cx: &App) -> Option<Vec<String>> {
        self.tabs.iter().find_map(|tab| match tab {
            Tab::Diff(diff) => Some(diff.read(cx).paths(cx)),
            Tab::Background | Tab::Files(_) | Tab::Terminal { .. } => None,
        })
    }

    /// The file the Files surface has open, and the text it shows, for UI
    /// tests.
    #[cfg(test)]
    pub(crate) fn open_file(&self, cx: &App) -> Option<(String, String)> {
        self.tabs.iter().find_map(|tab| match tab {
            Tab::Files(files) => files.read(cx).open_file_text(cx),
            Tab::Background | Tab::Diff(_) | Tab::Terminal { .. } => None,
        })
    }

    /// The session's terminal in the pane that last had the keyboard in
    /// the terminal tab on screen, once its shell runs, for UI tests.
    #[cfg(test)]
    pub(crate) fn terminal(&self, cx: &App) -> Option<Entity<TerminalView>> {
        match self.active.and_then(|key| self.tab(key))? {
            Tab::Terminal { view, .. } => view.read(cx).current(cx),
            Tab::Background | Tab::Diff(_) | Tab::Files(_) => None,
        }
    }

    /// The terminal tab on screen's panes, for UI tests.
    #[cfg(test)]
    pub(crate) fn terminal_panes(&self, cx: &App) -> usize {
        match self.active.and_then(|key| self.tab(key)) {
            Some(Tab::Terminal { view, .. }) => view.read(cx).pane_count(),
            _ => 0,
        }
    }

    /// Whether + has anything to offer: a surface shows, so the list
    /// doesn't, and there is a surface to open.
    fn offers_more(&self) -> bool {
        self.active.is_some() && self.openable().next().is_some()
    }

    /// Moves `key`'s tab to `index`, where a drag dropped it.
    fn move_tab(&mut self, key: TabKey, index: usize, cx: &mut Context<'_, Self>) {
        let Some(from) = self.tabs.iter().position(|tab| tab.key() == key) else {
            return;
        };
        let tab = self.tabs.remove(from);
        self.tabs.insert(index.min(self.tabs.len()), tab);
        cx.notify();
    }

    /// The tabs' surfaces, in order, for UI tests.
    #[cfg(test)]
    pub(crate) fn tab_order(&self) -> Vec<Surface> {
        self.tabs.iter().map(|tab| tab.key().surface).collect()
    }

    /// Notes the pointer over `key`'s tab.
    fn hover(&mut self, key: TabKey, cx: &mut Context<'_, Self>) {
        self.hovered = Some(key);
        cx.notify();
    }

    /// Notes the pointer leaving `key`'s tab, unless another took it.
    fn unhover(&mut self, key: TabKey, cx: &mut Context<'_, Self>) {
        if self.hovered == Some(key) {
            self.hovered = None;
            cx.notify();
        }
    }

    /// +'s menu: the surfaces it can open.
    fn surface_menu(
        mut menu: PopupMenu,
        view: &WeakEntity<Self>,
        openable: &[Surface],
    ) -> PopupMenu {
        for &surface in openable {
            let view = view.clone();
            menu = menu.item(
                PopupMenuItem::new(surface.label())
                    .icon(Icon::new(surface.icon()))
                    .on_click(move |_, window, cx| {
                        let _ = view.update(cx, |this, cx| this.open(surface, window, cx));
                    }),
            );
        }
        menu
    }

    /// `tab`'s tab, at `index` in the header: its icon, or × while hovered,
    /// and its label. Dragged onto another tab, it moves there.
    fn render_tab(&self, index: usize, tab: &Tab, cx: &Context<'_, Self>) -> impl IntoElement {
        let key = tab.key();
        let (surface, id) = (key.surface, key.id());
        let label = tab.label(cx);
        // × replaces the icon while hovered, so the tab keeps its width.
        let lead = if self.hovered == Some(key) {
            Button::new(SharedString::from(format!("close-panel-tab-{id}")))
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
                    this.close(key, cx);
                }))
                .into_any_element()
        } else {
            Icon::new(surface.icon()).small().into_any_element()
        };
        let view = cx.entity().downgrade();
        let button = Button::new(SharedString::from(format!("panel-tab-{id}")))
            .ghost()
            .small()
            .selected(self.active == Some(key))
            .accessibility_label(label.clone())
            .child(
                h_flex()
                    .size_4()
                    .items_center()
                    .justify_center()
                    .child(lead),
            )
            // A remote shell's title can run long.
            .child(div().max_w(rems(10.)).truncate().child(label.clone()))
            .on_hover(move |&hovered, _, cx| {
                let _ = view.update(cx, |this, cx| {
                    if hovered {
                        this.hover(key, cx);
                    } else {
                        this.unhover(key, cx);
                    }
                });
            })
            .on_click(cx.listener(move |this, _, _, cx| this.show(key, cx)));
        // Kit's Button takes no drag, so a div around it carries one.
        div()
            .id(SharedString::from(format!("panel-tab-slot-{id}")))
            .flex_shrink_0()
            .border_x_2()
            .border_color(gpui_kit::transparent_black())
            .child(button)
            .on_drag(DragTab { key, label, index }, |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| drag.clone())
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
                this.move_tab(drag.key, index, cx);
            }))
    }

    /// The tabs, +, and the toggle at the right edge, in a row as tall as
    /// the title bar beside it. Tabs that don't fit scroll, + staying put.
    fn render_header(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let menu_view = cx.entity().downgrade();
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| self.render_tab(index, tab, cx));
        let openable: Vec<Surface> = self.openable().collect();
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
                    .gap_1()
                    .child(
                        h_flex()
                            .id("panel-tabs")
                            .min_w_0()
                            .overflow_x_scroll()
                            .track_scroll(&self.tab_scroll)
                            .gap_1()
                            .children(tabs),
                    )
                    .children(self.offers_more().then(|| {
                        Button::new("panel-surfaces")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .accessibility_label("Open a surface")
                            .tooltip("Open a surface")
                            .dropdown_menu(move |menu, _, _| {
                                Self::surface_menu(menu, &menu_view, &openable)
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
        let open = self.openable().map(|surface| {
            row(
                format!("open-surface-{}", surface.id()).into(),
                surface.icon(),
                surface.label(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open(surface, window, cx);
            }))
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
                None => message("Open a session to see its background work.", None),
            },
            Tab::Diff(diff) => diff.clone().into_any_element(),
            Tab::Files(files) => files.clone().into_any_element(),
            Tab::Terminal { view, .. } => view.clone().into_any_element(),
        }
    }
}

/// A message in place of a surface's content, such as why it has none,
/// over `media` such as a spinner.
fn message(text: impl Into<SharedString>, media: Option<AnyElement>) -> AnyElement {
    let mut header = EmptyHeader::new().description(EmptyDescription::new().child(text.into()));
    if let Some(media) = media {
        header = header.media(EmptyMedia::new().child(media));
    }
    Empty::new().h_full().header(header).into_any_element()
}

impl Render for RightPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let body = match self.active.and_then(|key| self.tab(key)) {
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
