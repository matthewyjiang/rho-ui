//! A Terminal tab: panes ([`TerminalPane`]) split side by side or one above
//! another, any number deep, with a divider to drag between each two.
//! Right-click a pane for its menu; Ghostty's shortcuts ([`PaneCommand`])
//! do the same from the keyboard. The tab is named for the program in the
//! pane that last had a click or the keyboard.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    resizable::{h_resizable, resizable_panel, v_resizable},
    Icon, IconName,
};
use gpui_kit::{
    canvas, div, px, AnyElement, App, AppContext as _, Axis, Bounds, Context, DispatchPhase,
    Entity, EventEmitter, Hitbox, HitboxBehavior, InteractiveElement as _, IntoElement,
    MouseExitEvent, MouseMoveEvent, ParentElement as _, Pixels, Render, SharedString, Styled as _,
    Subscription, WeakEntity, Window,
};

use super::{TerminalPane, TerminalShell, TerminalView};
use crate::{right_panel::TerminalShortcut, session_view::SessionView, workspaces::Workspaces};

/// What a pane of a terminal tab does, from its menu or a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneCommand {
    /// Splits the pane, a new one after it: side by side along
    /// [`Axis::Horizontal`], one above the other along [`Axis::Vertical`].
    Split(Axis),
    /// Closes the pane, and the tab with its last pane.
    Close,
    /// Puts the keyboard in the next pane that way.
    Focus(Direction),
}

/// A way from one pane to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Emitted when the tab's last pane closes, so the panel closes the tab.
pub(crate) struct Emptied;

/// A pane, by a number the tab gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneId(usize);

/// How the tab's panes are laid out.
#[derive(Debug, PartialEq, Eq)]
enum Layout {
    Pane(PaneId),
    /// Two layouts side by side or one above the other; `id`, the pane
    /// whose split made it, keys the divider's place between them.
    Split {
        id: usize,
        axis: Axis,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    /// Splits `target` along `axis`, `new` after it. Returns whether
    /// `target` is here.
    fn split(&mut self, target: PaneId, new: PaneId, axis: Axis) -> bool {
        match self {
            Layout::Pane(pane) if *pane == target => {
                *self = Layout::Split {
                    id: new.0,
                    axis,
                    first: Box::new(Layout::Pane(target)),
                    second: Box::new(Layout::Pane(new)),
                };
                true
            }
            Layout::Pane(_) => false,
            Layout::Split { first, second, .. } => {
                first.split(target, new, axis) || second.split(target, new, axis)
            }
        }
    }

    /// Removes `target`, the other side of its split taking the split's
    /// place. Returns that side's first pane, or `None` when `target` isn't
    /// in a split here.
    fn remove(&mut self, target: PaneId) -> Option<PaneId> {
        let Layout::Split { first, second, .. } = self else {
            return None;
        };
        let other = if **first == Layout::Pane(target) {
            second
        } else if **second == Layout::Pane(target) {
            first
        } else {
            return first.remove(target).or_else(|| second.remove(target));
        };
        let other = std::mem::replace(&mut **other, Layout::Pane(target));
        *self = other;
        Some(self.first_pane())
    }

    fn first_pane(&self) -> PaneId {
        match self {
            Layout::Pane(pane) => *pane,
            Layout::Split { first, .. } => first.first_pane(),
        }
    }
}

/// A pane, with where it was last drawn.
struct Pane {
    id: PaneId,
    view: Entity<TerminalPane>,
    /// Over the whole pane, from the last frame: whether the pointer is on
    /// it, and where it is for [`PaneCommand::Focus`].
    hitbox: Option<Hitbox>,
    /// Redraws the tab as the pane's program changes.
    _redraw: Subscription,
}

/// The view of one Terminal tab of [`crate::right_panel::RightPanel`].
pub(crate) struct TerminalTab {
    workspaces: Entity<Workspaces>,
    shell: TerminalShell,
    /// The session on screen, which a new pane opens a terminal of.
    session: Option<Entity<SessionView>>,
    layout: Layout,
    panes: Vec<Pane>,
    /// The pane that last had a click or the keyboard: the one the tab is
    /// named for, and gives the keyboard when the tab shows.
    active: PaneId,
    /// The next pane's number.
    next_id: usize,
    /// Whether the pointer is in the window, as GPUI keeps the last place
    /// it had there after it leaves.
    pointer_in_window: bool,
}

impl EventEmitter<Emptied> for TerminalTab {}

impl TerminalTab {
    /// A tab of one pane, with a terminal of `session` if there is one.
    pub(crate) fn new(
        workspaces: Entity<Workspaces>,
        shell: TerminalShell,
        session: Option<&Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let first = PaneId(0);
        let mut this = Self {
            workspaces,
            shell,
            session: session.cloned(),
            layout: Layout::Pane(first),
            panes: Vec::new(),
            active: first,
            next_id: 1,
            pointer_in_window: true,
        };
        let pane = this.new_pane(first, cx);
        this.panes.push(pane);
        this
    }

    fn new_pane(&self, id: PaneId, cx: &mut Context<'_, Self>) -> Pane {
        let (workspaces, shell) = (self.workspaces.clone(), self.shell.clone());
        let session = self.session.as_ref();
        let view = cx.new(|cx| TerminalPane::new(workspaces, shell, session, cx));
        let redraw = cx.observe(&view, |_, _, cx| cx.notify());
        Pane {
            id,
            view,
            hitbox: None,
            _redraw: redraw,
        }
    }

    fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.id == id)
    }

    /// Shows `session`'s terminals in every pane.
    pub(crate) fn set_session(
        &mut self,
        session: Option<&Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) {
        self.session = session.cloned();
        for pane in &self.panes {
            pane.view
                .update(cx, |pane, cx| pane.set_session(session, cx));
        }
    }

    /// The program the active pane runs, if it can be named.
    pub(crate) fn program(&self, cx: &App) -> Option<SharedString> {
        self.pane(self.active)?.view.read(cx).program().cloned()
    }

    /// Pane `id`'s terminal of the session on screen, once it starts.
    fn terminal(&self, id: PaneId, cx: &App) -> Option<Entity<TerminalView>> {
        self.pane(id)?.view.read(cx).terminal().cloned()
    }

    /// The pane a shortcut acts on: the one with the keyboard, or else,
    /// while the keyboard is elsewhere, the one under the pointer. The
    /// keyboard comes first as moving it between panes leaves the pointer
    /// where it was.
    pub(crate) fn target(&self, window: &Window, cx: &App) -> Option<PaneId> {
        let focused = self
            .panes
            .iter()
            .find(|pane| self.has_keyboard(pane.id, window, cx));
        // Not `is_hovered`, which counts nothing hovered after a key press,
        // as this always follows one.
        let pointer = window.mouse_position();
        let hovered = || {
            self.panes.iter().find(|pane| {
                self.pointer_in_window
                    && pane
                        .hitbox
                        .as_ref()
                        .is_some_and(|hitbox| hitbox.is_hovered_at(pointer, window))
            })
        };
        focused.or_else(hovered).map(|pane| pane.id)
    }

    /// Whether pane `id`'s terminal has the keyboard.
    fn has_keyboard(&self, id: PaneId, window: &Window, cx: &App) -> bool {
        self.terminal(id, cx)
            .is_some_and(|view| view.read(cx).focus.is_focused(window))
    }

    /// Puts the keyboard in the active pane, starting its shell first if
    /// the pane hasn't shown yet.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if let Some(pane) = self.pane(self.active) {
            pane.view.update(cx, TerminalPane::start_if_new);
        }
        if let Some(view) = self.terminal(self.active, cx) {
            let handle = view.read(cx).focus.clone();
            window.focus(&handle, cx);
        }
    }

    /// Does `command` to pane `id`.
    pub(crate) fn run(
        &mut self,
        command: PaneCommand,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match command {
            PaneCommand::Split(axis) => {
                let new = PaneId(self.next_id);
                if self.layout.split(id, new, axis) {
                    self.next_id += 1;
                    let pane = self.new_pane(new, cx);
                    self.panes.push(pane);
                    self.activate(new, window, cx);
                }
            }
            PaneCommand::Close => {
                let had_keyboard = self.has_keyboard(id, window, cx);
                if self.layout == Layout::Pane(id) {
                    cx.emit(Emptied);
                } else if let Some(next) = self.layout.remove(id) {
                    self.panes.retain(|pane| pane.id != id);
                    // The keyboard moves on only from the closed pane, not
                    // from wherever it was when the pointer chose the pane.
                    if had_keyboard {
                        self.activate(next, window, cx);
                    } else if self.active == id {
                        self.active = next;
                    }
                }
            }
            PaneCommand::Focus(direction) => {
                let bounds = |pane: &Pane| pane.hitbox.as_ref().map(|hitbox| hitbox.bounds);
                let from = self.pane(id).and_then(bounds);
                let others = self
                    .panes
                    .iter()
                    .filter(|pane| pane.id != id)
                    .filter_map(|pane| Some((pane.id, bounds(pane)?)));
                if let Some(next) = from.and_then(|from| neighbor(from, direction, others)) {
                    self.activate(next, window, cx);
                }
            }
        }
        cx.notify();
    }

    /// Makes pane `id` the active one and gives it the keyboard.
    fn activate(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.active = id;
        self.focus(window, cx);
    }

    /// Makes pane `id` the active one, as a click in it does.
    fn select(&mut self, id: PaneId, cx: &mut Context<'_, Self>) {
        if self.active != id {
            self.active = id;
            cx.notify();
        }
    }

    fn set_hitbox(&mut self, id: PaneId, hitbox: Hitbox) {
        if let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == id) {
            pane.hitbox = Some(hitbox);
        }
    }

    /// The active pane's terminal of the session on screen, for UI tests.
    #[cfg(test)]
    pub(crate) fn current(&self, cx: &App) -> Option<Entity<TerminalView>> {
        self.terminal(self.active, cx)
    }

    /// How many panes the tab has, for UI tests.
    #[cfg(test)]
    pub(crate) fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Pane `id`'s menu: copy and paste, and what [`PaneCommand`]s do, with
    /// the keys bound to the same commands.
    fn pane_menu(menu: PopupMenu, tab: &WeakEntity<Self>, id: PaneId, cx: &App) -> PopupMenu {
        let terminal = tab.upgrade().and_then(|tab| tab.read(cx).terminal(id, cx));
        let selected = terminal
            .as_ref()
            .is_some_and(|view| view.read(cx).selected_text().is_some());
        let command = |label: &'static str, icon: Icon, command: PaneCommand| {
            let tab = tab.clone();
            PopupMenuItem::new(label)
                .icon(icon)
                .action(Box::new(TerminalShortcut::Pane(command)))
                .on_click(move |_, window, cx| {
                    let _ = tab.update(cx, |tab, cx| tab.run(command, id, window, cx));
                })
        };
        let copy = terminal.clone();
        menu.item(
            PopupMenuItem::new("Copy")
                .icon(IconName::Copy)
                .disabled(!selected)
                .on_click(move |_, _, cx| {
                    if let Some(view) = &copy {
                        view.update(cx, |view, cx| view.copy(cx));
                    }
                }),
        )
        .item(
            PopupMenuItem::new("Paste")
                .icon(Icon::new(Lucide::ClipboardPaste))
                .on_click(move |_, _, cx| {
                    if let Some(view) = &terminal {
                        view.update(cx, |view, cx| {
                            view.paste_clipboard(cx);
                            cx.notify();
                        });
                    }
                }),
        )
        .separator()
        .item(command(
            "Split right",
            Icon::new(Lucide::Columns2),
            PaneCommand::Split(Axis::Horizontal),
        ))
        .item(command(
            "Split down",
            Icon::new(Lucide::Rows2),
            PaneCommand::Split(Axis::Vertical),
        ))
        .separator()
        .item(command(
            "Close",
            Icon::new(IconName::Close),
            PaneCommand::Close,
        ))
    }

    fn render_layout(&self, layout: &Layout, cx: &Context<'_, Self>) -> AnyElement {
        match layout {
            Layout::Pane(id) => self.render_pane(*id, cx),
            Layout::Split {
                id,
                axis,
                first,
                second,
            } => {
                let id = ("terminal-split", *id);
                let group = match axis {
                    Axis::Horizontal => h_resizable(id),
                    Axis::Vertical => v_resizable(id),
                };
                group
                    .child(resizable_panel().child(self.render_layout(first, cx)))
                    .child(resizable_panel().child(self.render_layout(second, cx)))
                    .into_any_element()
            }
        }
    }

    fn render_pane(&self, id: PaneId, cx: &Context<'_, Self>) -> AnyElement {
        let Some(pane) = self.pane(id) else {
            return div().into_any_element();
        };
        let tab = cx.entity().downgrade();
        // Raw GPUI: a hitbox over the pane, which Kit has no element for,
        // tells the pointer over any of it from the last frame.
        let hitbox = canvas(
            {
                let tab = tab.clone();
                move |bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
                    let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                    let _ = tab.update(cx, |tab, _| tab.set_hitbox(id, hitbox));
                }
            },
            {
                let tab = tab.clone();
                move |_, (), window: &mut Window, _: &mut App| tab_follows_pointer(&tab, window)
            },
        )
        .absolute()
        .size_full();
        div()
            .id(("terminal-pane", id.0))
            .test_support()
            .relative()
            .size_full()
            .child(hitbox)
            .child(pane.view.clone())
            .capture_any_mouse_down(cx.listener(move |this, _, _, cx| this.select(id, cx)))
            .context_menu(move |menu, _, cx| Self::pane_menu(menu, &tab, id, cx))
            .into_any_element()
    }
}

/// Notes the pointer leaving the window, and coming back, for
/// [`TerminalTab::target`]. Raw GPUI: Kit has no listener for a window's
/// edges.
fn tab_follows_pointer(tab: &WeakEntity<TerminalTab>, window: &mut Window) {
    let exits = tab.clone();
    window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
        if phase == DispatchPhase::Capture {
            let _ = exits.update(cx, |tab, _| tab.pointer_in_window = false);
        }
    });
    let moves = tab.clone();
    window.on_mouse_event(move |_: &MouseMoveEvent, phase, _, cx| {
        if phase == DispatchPhase::Capture {
            let _ = moves.update(cx, |tab, _| tab.pointer_in_window = true);
        }
    });
}

impl Render for TerminalTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(self.render_layout(&self.layout, cx))
    }
}

/// The pane next to `from` toward `to`: of those past its edge that way and
/// beside it across, the nearest, then the most in line with its middle.
fn neighbor(
    from: Bounds<Pixels>,
    to: Direction,
    others: impl Iterator<Item = (PaneId, Bounds<Pixels>)>,
) -> Option<PaneId> {
    let overlap = |a: (Pixels, Pixels), b: (Pixels, Pixels)| a.0 < b.1 && b.0 < a.1;
    let across = |bounds: &Bounds<Pixels>| match to {
        Direction::Left | Direction::Right => (bounds.top(), bounds.bottom()),
        Direction::Up | Direction::Down => (bounds.left(), bounds.right()),
    };
    let middle = |bounds: &Bounds<Pixels>| match to {
        Direction::Left | Direction::Right => bounds.center().y,
        Direction::Up | Direction::Down => bounds.center().x,
    };
    others
        .filter_map(|(id, bounds)| {
            let gap = match to {
                Direction::Left => from.left() - bounds.right(),
                Direction::Right => bounds.left() - from.right(),
                Direction::Up => from.top() - bounds.bottom(),
                Direction::Down => bounds.top() - from.bottom(),
            };
            // Neighbors share an edge: in the UI test's split, 817 + 191.5 is
            // 1008.5, the right pane's left. Half a pixel allows for rounding.
            let past = gap > -px(0.5);
            let off_middle = (middle(&bounds) - middle(&from)).abs();
            (past && overlap(across(&bounds), across(&from))).then_some((id, gap, off_middle))
        })
        .min_by(|a, b| {
            f32::from(a.1)
                .total_cmp(&f32::from(b.1))
                .then(f32::from(a.2).total_cmp(&f32::from(b.2)))
        })
        .map(|(id, _, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{point, size};

    /// Focusing a direction goes to the pane that way, past panes on another
    /// side: here, one pane on the left, and two stacked on the right.
    #[test]
    fn focus_goes_to_the_pane_that_way() {
        let rect =
            |x: f32, y: f32, w: f32, h: f32| Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        let panes = [
            (PaneId(0), rect(0., 0., 100., 200.)),
            (PaneId(1), rect(100., 0., 100., 100.)),
            (PaneId(2), rect(100., 100., 100., 100.)),
        ];
        let from = |id: usize, to: Direction| {
            let (_, bounds) = panes[id];
            let others = panes.iter().copied().filter(|(other, _)| other.0 != id);
            neighbor(bounds, to, others).map(|pane| pane.0)
        };
        for (start, to, want) in [
            (0, Direction::Right, Some(1)),
            (0, Direction::Left, None),
            (1, Direction::Down, Some(2)),
            (2, Direction::Up, Some(1)),
            (2, Direction::Left, Some(0)),
            (1, Direction::Right, None),
        ] {
            assert_eq!(from(start, to), want, "from {start} going {to:?}");
        }
    }
}
