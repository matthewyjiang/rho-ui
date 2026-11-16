//! The panel at the right of the window: a home for surfaces, the tools
//! that sit beside a session, such as its background work. Ctrl+Alt+B or
//! the toggle at the right of the title bar shows and hides it. Drag its
//! left edge to resize it, and double-click the edge for its default width.
//! The state database remembers both ([`PanelLayout`]).
//!
//! Each surface opens as a tab in the panel's header; hovering a tab swaps
//! its icon for × to close it, and dragging one onto another moves it
//! there. Terminal opens as many tabs as asked, each named for the program
//! it runs; Pull request opens one for the branch's PR and one for each PR
//! linked by hand, each named for its number; the others open one. With
//! none open, the panel lists the surfaces to open; + in the header opens a
//! menu of the rest. Each session has its own tabs ([`TabSet`]): switching
//! sessions switches tabs, and a session's first time on screen it has
//! none.
//!
//! Ghostty's shortcuts for tabs and splits ([`TerminalShortcut`]) act on
//! the terminal with the keyboard, or else the one under the pointer.
//!
//! To add a surface: add a [`Surface`] variant with its name and icon, and
//! draw it in [`RightPanel::render_surface`]. A surface that keeps state,
//! such as [`diff::DiffSurface`], is a view of its own, made when its tab
//! opens ([`Tab`]) for the session the tab belongs to.

mod diff;
mod files;
mod pull_request;

pub(crate) use self::pull_request::state_look;
mod terminal;

use std::{cmp::Ordering, collections::HashMap};

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
use self::pull_request::{LinkEvent, PullRequestSurface, Shows, Visibility};
pub(crate) use self::terminal::{Direction, PaneCommand, TerminalShell};
use self::terminal::{Emptied, TerminalTab};
#[cfg(test)]
pub(crate) use self::terminal::{TerminalView, PROGRAM_POLL};
use crate::{
    machines::{Place, SessionKey},
    pull_requests::{PrId, PrLinks, PullRequests},
    session_view::{SessionEvent, SessionView},
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
    /// The pull request of the branch the session works on.
    PullRequest,
    /// A shell in the session's folder.
    Terminal,
}

impl Surface {
    /// Every surface, in the order the panel lists them.
    const ALL: [Surface; 5] = [
        Surface::Files,
        Surface::Terminal,
        Surface::Diff,
        Surface::PullRequest,
        Surface::Background,
    ];

    /// The surface's part of its elements' IDs.
    fn id(self) -> &'static str {
        match self {
            Surface::Background => "background",
            Surface::Diff => "diff",
            Surface::Files => "files",
            Surface::PullRequest => "pull-request",
            Surface::Terminal => "terminal",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Surface::Background => "Background work",
            Surface::Diff => "Diff",
            Surface::Files => "Files",
            Surface::PullRequest => "Pull request",
            Surface::Terminal => "Terminal",
        }
    }

    fn icon(self) -> Lucide {
        match self {
            Surface::Background => Lucide::ListChecks,
            Surface::Diff => Lucide::FileDiff,
            Surface::Files => Lucide::FolderTree,
            Surface::PullRequest => Lucide::GitPullRequest,
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

/// Which tab of a session's: its surface, and its number among that
/// surface's tabs there, from 1 and never reused. Terminal opens as many as
/// asked ([`Surface::opens_many`]); Pull request opens 1 for the branch's PR
/// and one more for each PR linked by hand; the others are always 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TabKey {
    surface: Surface,
    number: u32,
}

impl TabKey {
    /// The Pull request tab of the session's branch, which the tabs of PRs
    /// linked by hand open and close with.
    const BRANCH_PR: TabKey = TabKey {
        surface: Surface::PullRequest,
        number: 1,
    };

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
    /// A PR of the session, by its number ([`TabKey::BRANCH_PR`] for its
    /// branch's), with the subscriptions that redraw the panel as its label
    /// changes and open and close tabs as PRs are linked and unlinked.
    PullRequest {
        number: u32,
        view: Entity<PullRequestSurface>,
        _subscriptions: [Subscription; 2],
    },
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
            Tab::PullRequest { number, .. } => (Surface::PullRequest, *number),
            Tab::Terminal { number, .. } => (Surface::Terminal, *number),
        };
        TabKey { surface, number }
    }

    /// What the tab says: a terminal's names the program it runs, as `vim`,
    /// and a PR's its number, as `#12`.
    fn label(&self, cx: &App) -> SharedString {
        match self {
            Tab::Terminal { view, .. } => view
                .read(cx)
                .program(cx)
                .unwrap_or_else(|| Surface::Terminal.label().into()),
            Tab::PullRequest { view, .. } => view.read(cx).label(cx),
            Tab::Background | Tab::Diff(_) | Tab::Files(_) => self.key().surface.label().into(),
        }
    }

    /// The PR linked by hand that the tab shows, if it shows one.
    fn linked<'a>(&self, cx: &'a App) -> Option<&'a PrId> {
        match self {
            Tab::PullRequest { view, .. } => view.read(cx).linked(),
            Tab::Background | Tab::Diff(_) | Tab::Files(_) | Tab::Terminal { .. } => None,
        }
    }
}

/// One session's open surfaces, which show while it is on screen.
struct TabSet {
    /// The session's view: one made again for the same session, as after a
    /// failed attach, takes its place.
    view: Entity<SessionView>,
    /// The session's folder, where its surfaces read.
    place: Place,
    /// Tells the surfaces when the view's turn ends; held, not read.
    turns: Subscription,
    /// In the order opened, or as dragged.
    tabs: Vec<Tab>,
    /// One of `tabs`, or `None` for the list of surfaces when none is open.
    active: Option<TabKey>,
    /// The number the next terminal tab gets.
    next_terminal: u32,
    /// The number the next tab of a PR linked by hand gets.
    next_link: u32,
}

impl TabSet {
    fn new(view: Entity<SessionView>, turns: Subscription, cx: &App) -> Self {
        let live = view.read(cx).live();
        Self {
            place: Place::new(live.id.machine, live.cwd),
            view,
            turns,
            tabs: Vec::new(),
            active: None,
            next_terminal: 1,
            next_link: 2,
        }
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

/// Why [`RightPanel::reload`] reads a session's surfaces again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reload {
    /// The session came back on screen.
    Shown,
    /// Its turn ended.
    TurnEnded,
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
/// while [`PanelLayout`], and tells it which session is on screen. Each
/// session has tabs of its own ([`TabSet`]), which show while it does.
pub(crate) struct RightPanel {
    /// Where surfaces read the session's machine.
    workspaces: Entity<Workspaces>,
    /// What a terminal on this machine runs.
    shell: TerminalShell,
    /// Each session's tabs, from the first time it is on screen. They live
    /// on while others show, a terminal's shells too.
    sets: HashMap<SessionKey, TabSet>,
    /// The session on screen, if any, with the subscription that redraws
    /// the panel as it changes.
    session: Option<(SessionKey, Subscription)>,
    /// The tab under the pointer, which shows × in place of its icon.
    hovered: Option<TabKey>,
    /// Scrolls the header's tabs when more are open than fit.
    tab_scroll: ScrollHandle,
    /// What the Pull request surface reads PRs through.
    prs: Entity<PullRequests>,
    /// Tells the Pull request surface when the panel shows or hides.
    _layout: Subscription,
}

impl RightPanel {
    pub(crate) fn new(
        workspaces: Entity<Workspaces>,
        shell: TerminalShell,
        prs: Entity<PullRequests>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        Self {
            workspaces,
            shell,
            sets: HashMap::new(),
            session: None,
            hovered: None,
            tab_scroll: ScrollHandle::new(),
            prs,
            _layout: cx.observe_global::<PanelLayout>(|this, cx| this.sync_shown(cx)),
        }
    }

    /// The tabs of the session on screen.
    fn set(&self) -> Option<&TabSet> {
        self.sets.get(&self.session.as_ref()?.0)
    }

    fn set_mut(&mut self) -> Option<&mut TabSet> {
        self.sets.get_mut(&self.session.as_ref()?.0)
    }

    /// Whether something in the panel has the keyboard.
    pub(crate) fn has_keyboard(&self, window: &Window, cx: &App) -> bool {
        let mut tabs = self.sets.values().flat_map(|set| &set.tabs);
        tabs.any(|tab| match tab {
            Tab::PullRequest { view, .. } => view.read(cx).has_keyboard(window, cx),
            Tab::Terminal { view, .. } => view.read(cx).has_keyboard_anywhere(window, cx),
            Tab::Background | Tab::Diff(_) | Tab::Files(_) => false,
        })
    }

    /// Tells each Pull request surface whether it is on screen: its tab
    /// shown, for the session on screen, in a panel that shows. It polls
    /// only then.
    fn sync_shown(&self, cx: &mut Context<'_, Self>) {
        let shown = PanelLayout::get(cx);
        let on_screen = self.session.as_ref().map(|(key, _)| key);
        for (key, set) in &self.sets {
            for tab in &set.tabs {
                let Tab::PullRequest { view, .. } = tab else {
                    continue;
                };
                let visibility = if shown && on_screen == Some(key) && set.active == Some(tab.key())
                {
                    Visibility::Shown
                } else {
                    Visibility::Hidden
                };
                view.update(cx, |view, cx| view.set_visibility(visibility, cx));
            }
        }
    }

    /// Shows the tabs of `session`, the one on screen, or none. Its first
    /// time on screen, it has none open.
    pub(crate) fn set_session(
        &mut self,
        session: Option<Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) {
        let key = session.as_ref().map(|view| view.read(cx).live().id);
        let same_view = |set: &TabSet| session.as_ref() == Some(&set.view);
        if self.session.as_ref().map(|(key, _)| key) == key.as_ref()
            && self.set().is_none_or(same_view)
        {
            return;
        }
        self.session = None;
        if let (Some(view), Some(key)) = (session, key) {
            let turns = Self::follow_turns(&key, &view, cx);
            if let Some(set) = self.sets.get_mut(&key) {
                if set.view != view {
                    set.view = view.clone();
                    set.turns = turns;
                }
                self.reload(&key, Reload::Shown, cx);
            } else {
                let set = TabSet::new(view.clone(), turns, cx);
                self.sets.insert(key.clone(), set);
            }
            let redraw = cx.observe(&view, |_, _, cx| cx.notify());
            self.session = Some((key, redraw));
        }
        self.hovered = None;
        self.sync_shown(cx);
        cx.notify();
    }

    /// Reads `session`'s surfaces again whenever `view`'s turn ends.
    fn follow_turns(
        session: &SessionKey,
        view: &Entity<SessionView>,
        cx: &mut Context<'_, Self>,
    ) -> Subscription {
        let session = session.clone();
        cx.subscribe(view, move |this, _, event: &SessionEvent, cx| {
            if matches!(event, SessionEvent::Saved) {
                this.reload(&session, Reload::TurnEnded, cx);
            }
        })
    }

    /// Reads what `session`'s Diff and Files show again, and when its turn
    /// ended, its PR on screen (which polls on its own as it comes back).
    fn reload(&self, session: &SessionKey, why: Reload, cx: &mut Context<'_, Self>) {
        let Some(set) = self.sets.get(session) else {
            return;
        };
        for tab in &set.tabs {
            match tab {
                Tab::Diff(diff) => diff.update(cx, DiffSurface::reload),
                Tab::Files(files) => files.update(cx, FilesSurface::reload),
                Tab::PullRequest { view, .. } if why == Reload::TurnEnded => {
                    view.update(cx, PullRequestSurface::turn_ended);
                }
                Tab::Background | Tab::PullRequest { .. } | Tab::Terminal { .. } => {}
            }
        }
    }

    /// Shows `surface` for the session on screen: a new tab of a surface
    /// that opens many, else its tab, opened if it has none. Pull request
    /// opens the tabs of the PRs linked to it too. Returns the tab shown,
    /// or `None` with no session on screen.
    fn open(
        &mut self,
        surface: Surface,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<TabKey> {
        let session = self.session.as_ref()?.0.clone();
        let set = self.sets.get_mut(&session)?;
        let open = set
            .tabs
            .iter()
            .map(Tab::key)
            .find(|key| key.surface == surface);
        let key = match open {
            Some(key) if !surface.opens_many() => key,
            _ => {
                let number = match surface {
                    Surface::Terminal => {
                        set.next_terminal += 1;
                        set.next_terminal - 1
                    }
                    _ => 1,
                };
                let key = TabKey { surface, number };
                let place = set.place.clone();
                let tab = self.new_tab(&session, &place, key, window, cx);
                let set = self.sets.get_mut(&session)?;
                set.tabs.push(tab);
                self.tab_scroll.scroll_to_item(set.tabs.len() - 1);
                if surface == Surface::PullRequest {
                    self.sync_links(&session, cx);
                }
                key
            }
        };
        self.show(key, cx);
        Some(key)
    }

    /// A new tab `key` of `session`, which works in `place`.
    fn new_tab(
        &self,
        session: &SessionKey,
        place: &Place,
        key: TabKey,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Tab {
        let workspaces = self.workspaces.clone();
        let place = place.clone();
        match key.surface {
            Surface::Background => Tab::Background,
            Surface::Diff => Tab::Diff(cx.new(|cx| DiffSurface::new(workspaces, place, cx))),
            Surface::Files => Tab::Files(cx.new(|cx| FilesSurface::new(workspaces, place, cx))),
            Surface::PullRequest => self.pr_tab(session, &place, key.number, Shows::Branch, cx),
            Surface::Terminal => {
                let shell = self.shell.clone();
                let tab = cx.new(|cx| TerminalTab::new(workspaces, shell, place, window, cx));
                let session = session.clone();
                let subscriptions = [
                    cx.observe(&tab, |_, _, cx| cx.notify()),
                    cx.subscribe(&tab, move |this, _, _: &Emptied, cx| {
                        this.close(&session, key, cx);
                    }),
                ];
                Tab::Terminal {
                    number: key.number,
                    view: tab,
                    _subscriptions: subscriptions,
                }
            }
        }
    }

    /// A Pull request tab of `session`, which works in `place`, numbered
    /// `number`, showing `shows`.
    fn pr_tab(
        &self,
        session: &SessionKey,
        place: &Place,
        number: u32,
        shows: Shows,
        cx: &mut Context<'_, Self>,
    ) -> Tab {
        let (prs, workspaces) = (self.prs.clone(), &self.workspaces);
        let key = session.clone();
        let pr = cx.new(|cx| PullRequestSurface::new(prs, workspaces, key, place, shows, cx));
        let session = session.clone();
        let subscriptions = [
            // Its label, the PR's number, comes once the PR is found.
            cx.observe(&pr, |_, _, cx| cx.notify()),
            cx.subscribe(&pr, move |this, _, event: &LinkEvent, cx| {
                this.sync_links(&session, cx);
                if let LinkEvent::Linked(id) = event {
                    this.show_link(&session, id, cx);
                }
            }),
        ];
        Tab::PullRequest {
            number,
            view: pr,
            _subscriptions: subscriptions,
        }
    }

    /// Opens a tab for each PR linked to `session` that has none, after its
    /// other Pull request tabs, and closes the tab of each unlinked; while
    /// its branch's Pull request tab is open, which they go with.
    fn sync_links(&mut self, session: &SessionKey, cx: &mut Context<'_, Self>) {
        let Some(set) = self.sets.get(session) else {
            return;
        };
        if set.tab(TabKey::BRANCH_PR).is_none() {
            return;
        }
        let linked = cx.global::<PrLinks>().get(session).to_vec();
        let shown: Vec<PrId> = set
            .tabs
            .iter()
            .filter_map(|tab| tab.linked(cx).cloned())
            .collect();
        let unlinked: Vec<TabKey> = set
            .tabs
            .iter()
            .filter(|tab| tab.linked(cx).is_some_and(|id| !linked.contains(id)))
            .map(Tab::key)
            .collect();
        let place = set.place.clone();
        self.remove_tabs(session, |key| unlinked.contains(&key));
        for id in linked.into_iter().filter(|id| !shown.contains(id)) {
            let Some(set) = self.sets.get_mut(session) else {
                return;
            };
            let number = set.next_link;
            set.next_link += 1;
            let tab = self.pr_tab(session, &place, number, Shows::Linked(id), cx);
            let Some(set) = self.sets.get_mut(session) else {
                return;
            };
            let after = set
                .tabs
                .iter()
                .rposition(|tab| tab.key().surface == Surface::PullRequest);
            set.tabs
                .insert(after.map_or(set.tabs.len(), |at| at + 1), tab);
        }
        self.sync_shown(cx);
        cx.notify();
    }

    /// Shows the tab of PR `id`, linked to `session`, if it is on screen.
    fn show_link(&mut self, session: &SessionKey, id: &PrId, cx: &mut Context<'_, Self>) {
        if self.session.as_ref().map(|(key, _)| key) != Some(session) {
            return;
        }
        let Some(set) = self.set() else {
            return;
        };
        let Some(index) = set.tabs.iter().position(|tab| tab.linked(cx) == Some(id)) else {
            return;
        };
        let key = set.tabs[index].key();
        self.tab_scroll.scroll_to_item(index);
        self.show(key, cx);
    }

    /// Shows the tab `key` of the session on screen.
    fn show(&mut self, key: TabKey, cx: &mut Context<'_, Self>) {
        if let Some(set) = self.set_mut() {
            set.active = Some(key);
        }
        self.sync_shown(cx);
        cx.notify();
    }

    /// Closes `session`'s tab `key`. The branch's Pull request tab takes
    /// the tabs of PRs linked by hand with it, which stay linked; closing
    /// one of those unlinks it.
    fn close(&mut self, session: &SessionKey, key: TabKey, cx: &mut Context<'_, Self>) {
        let linked = self
            .sets
            .get(session)
            .and_then(|set| set.tab(key))
            .and_then(|tab| tab.linked(cx).cloned());
        if let Some(id) = linked {
            cx.update_global::<PrLinks, _>(|links, _| links.remove(session, &id));
            self.sync_links(session, cx);
            return;
        }
        if key == TabKey::BRANCH_PR {
            self.remove_tabs(session, |key| key.surface == Surface::PullRequest);
        } else {
            self.remove_tabs(session, |removed| removed == key);
        }
        self.sync_shown(cx);
        cx.notify();
    }

    /// Takes the tabs whose keys `remove` picks out of `session`'s. If one
    /// was showing, the last tab left shows, or the list of surfaces when
    /// none is.
    fn remove_tabs(&mut self, session: &SessionKey, remove: impl Fn(TabKey) -> bool) {
        let on_screen = self.session.as_ref().map(|(key, _)| key) == Some(session);
        if on_screen && self.hovered.is_some_and(&remove) {
            self.hovered = None;
        }
        let Some(set) = self.sets.get_mut(session) else {
            return;
        };
        set.tabs.retain(|tab| !remove(tab.key()));
        if set.active.is_some_and(&remove) {
            set.active = set.tabs.last().map(Tab::key);
        }
    }

    /// The tab `key` of the session on screen.
    fn tab(&self, key: TabKey) -> Option<&Tab> {
        self.set()?.tab(key)
    }

    /// The tab on screen.
    fn active_tab(&self) -> Option<&Tab> {
        let set = self.set()?;
        set.tab(set.active?)
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
        let Some(Tab::Terminal { view, .. }) = self.active_tab() else {
            return false;
        };
        let view = view.clone();
        let Some(pane) = view.read(cx).target(window, cx) else {
            return false;
        };
        match shortcut {
            TerminalShortcut::NewTab => {
                if let Some(key) = self.open(Surface::Terminal, window, cx) {
                    self.focus_terminal(key, window, cx);
                }
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
        let Some(set) = self.set() else {
            return;
        };
        let terminals: Vec<TabKey> = set
            .tabs
            .iter()
            .map(Tab::key)
            .filter(|key| key.surface == Surface::Terminal)
            .collect();
        let Some(at) = terminals.iter().position(|&key| Some(key) == set.active) else {
            return;
        };
        let count = terminals.len();
        let next = match step {
            Step::Next => (at + 1) % count,
            Step::Previous => (at + count - 1) % count,
        };
        let key = terminals[next];
        if let Some(index) = set.tabs.iter().position(|tab| tab.key() == key) {
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
        self.set()?.tabs.iter().find_map(|tab| match tab {
            Tab::Diff(diff) => Some(diff.read(cx).paths(cx)),
            _ => None,
        })
    }

    /// The file the Files surface has open, and the text it shows, for UI
    /// tests.
    #[cfg(test)]
    pub(crate) fn open_file(&self, cx: &App) -> Option<(String, String)> {
        self.set()?.tabs.iter().find_map(|tab| match tab {
            Tab::Files(files) => files.read(cx).open_file_text(cx),
            _ => None,
        })
    }

    /// The session's terminal in the pane that last had the keyboard in
    /// the terminal tab on screen, once its shell runs, for UI tests.
    #[cfg(test)]
    pub(crate) fn terminal(&self, cx: &App) -> Option<Entity<TerminalView>> {
        match self.active_tab()? {
            Tab::Terminal { view, .. } => view.read(cx).current(cx),
            _ => None,
        }
    }

    /// The terminal tab on screen's panes, for UI tests.
    #[cfg(test)]
    pub(crate) fn terminal_panes(&self, cx: &App) -> usize {
        match self.active_tab() {
            Some(Tab::Terminal { view, .. }) => view.read(cx).pane_count(),
            _ => 0,
        }
    }

    /// Whether + has anything to offer: a surface shows, so the list
    /// doesn't, and there is a surface to open.
    fn offers_more(&self) -> bool {
        self.set()
            .is_some_and(|set| set.active.is_some() && set.openable().next().is_some())
    }

    /// Moves the on-screen session's tab `key` to `index`, where a drag
    /// dropped it.
    fn move_tab(&mut self, key: TabKey, index: usize, cx: &mut Context<'_, Self>) {
        let Some(set) = self.set_mut() else {
            return;
        };
        let Some(from) = set.tabs.iter().position(|tab| tab.key() == key) else {
            return;
        };
        let tab = set.tabs.remove(from);
        set.tabs.insert(index.min(set.tabs.len()), tab);
        cx.notify();
    }

    /// The on-screen session's tabs' surfaces, in order, for UI tests.
    #[cfg(test)]
    pub(crate) fn tab_order(&self) -> Vec<Surface> {
        self.set()
            .map(|set| set.tabs.iter().map(|tab| tab.key().surface).collect())
            .unwrap_or_default()
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
    fn render_tab(
        &self,
        set: &TabSet,
        index: usize,
        tab: &Tab,
        cx: &Context<'_, Self>,
    ) -> impl IntoElement {
        let key = tab.key();
        let (surface, id) = (key.surface, key.id());
        let label = tab.label(cx);
        // × replaces the icon while hovered, so the tab keeps its width.
        let lead = if let (true, Some((session, _))) = (self.hovered == Some(key), &self.session) {
            let session = session.clone();
            let close = if tab.linked(cx).is_some() {
                "Unlink"
            } else {
                "Close"
            };
            Button::new(SharedString::from(format!("close-panel-tab-{id}")))
                .ghost()
                .xsmall()
                .compact()
                .icon(IconName::Close)
                .accessibility_label(close)
                .tooltip(close)
                .on_click(cx.listener(move |this, _, _, cx| {
                    // Kit's Button passes the click on, and the tab's own
                    // click would open the surface again.
                    cx.stop_propagation();
                    this.close(&session, key, cx);
                }))
                .into_any_element()
        } else {
            Icon::new(surface.icon()).small().into_any_element()
        };
        let view = cx.entity().downgrade();
        let button = Button::new(SharedString::from(format!("panel-tab-{id}")))
            .ghost()
            .small()
            .selected(set.active == Some(key))
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
        let set = self.set();
        let tabs = set.into_iter().flat_map(|set| {
            let tabs = set.tabs.iter().enumerate();
            tabs.map(move |(index, tab)| self.render_tab(set, index, tab, cx))
        });
        let openable: Vec<Surface> = set.into_iter().flat_map(TabSet::openable).collect();
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

    /// The surfaces `set` can open, centered.
    fn render_list(set: &TabSet, cx: &Context<'_, Self>) -> impl IntoElement {
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
        let open = set.openable().map(|surface| {
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

    /// What `tab`'s surface shows of `set`'s session.
    fn render_surface(set: &TabSet, tab: &Tab, cx: &App) -> AnyElement {
        match tab {
            Tab::Background => SessionView::background_surface(&set.view, cx).into_any_element(),
            Tab::Diff(diff) => diff.clone().into_any_element(),
            Tab::Files(files) => files.clone().into_any_element(),
            Tab::PullRequest { view, .. } => view.clone().into_any_element(),
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
        let body = match self.set() {
            None => message("Open a session to use its surfaces.", None),
            Some(set) => match set.active.and_then(|key| set.tab(key)) {
                Some(tab) => Self::render_surface(set, tab, cx),
                None => Self::render_list(set, cx).into_any_element(),
            },
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
