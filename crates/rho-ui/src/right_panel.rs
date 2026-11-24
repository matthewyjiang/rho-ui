//! The panel at the right of the window: a home for surfaces, the tools
//! that sit beside a session, such as its background work. Ctrl+Alt+B or
//! the toggle at the right of the title bar shows and hides it. Drag its
//! left edge to resize it, and double-click the edge for its default width.
//! The state database remembers both ([`PanelLayout`]).
//!
//! Each surface opens as a tab in the panel's header; hovering a tab swaps
//! its icon for × to close it, and dragging one onto another moves it
//! there. Terminal opens as many tabs as asked, each named for the program
//! it runs; Pull request opens one for the branch's PR and one for each
//! other PR linked to the session, by hand or by its agent, each named for
//! its number; the others open one. With none open, the panel lists the
//! surfaces to open; + in the header opens a menu of the rest. Each
//! session has its own tabs ([`TabSet`]): switching sessions switches
//! tabs, and a session's first time on screen it has none.
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
mod tabs;

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
    Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, Axis, BorrowAppContext as _, Context, Entity,
    Global, InteractiveElement as _, IntoElement, ParentElement as _, Rems, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window,
};
use rho_ui_core::{PullRequestId, SessionPullRequests};

use self::diff::DiffSurface;
use self::files::FilesSurface;
use self::pull_request::{unlink_tooltip, PullRequestSurface, ShowLinked, Visibility};
use self::tabs::{Tab, TabKey, TabSet};
pub(crate) use self::terminal::{Direction, PaneCommand, TerminalShell};
use self::terminal::{Emptied, TerminalTab};
#[cfg(test)]
pub(crate) use self::terminal::{TerminalView, PROGRAM_POLL};
use crate::{
    machines::{Place, SessionKey},
    pull_requests::{PullRequests, Which},
    session_view::{SessionEvent, SessionView},
    store::{Key, StateDb},
    title_bar::TOP_ROW_REMS,
    window_chrome::{self, DragArea, MovesWindow as _},
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
    /// main pane [`MIN_PANE_REMS`], and never under [`least`].
    pub(crate) fn width(row: f32, sidebar: f32, cx: &App) -> Rems {
        let width = cx
            .try_global::<Self>()
            .map_or(DEFAULT_WIDTH_REMS, |this| this.width);
        rems(fit(width, row - sidebar - MIN_PANE_REMS, cx))
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
        let width = fit(width, row - sidebar - MIN_PANE_REMS, cx);
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

/// `width`, held to the `room` there is, but at least [`least`]: a window
/// too narrow for both squeezes the pane, not the panel.
fn fit(width: f32, room: f32, cx: &App) -> f32 {
    width.min(room).max(least(cx))
}

/// The narrowest the panel goes, in rems: [`MIN_WIDTH_REMS`], and on
/// Windows the room the window buttons take of its header besides.
fn least(cx: &App) -> f32 {
    MIN_WIDTH_REMS + window_chrome::caption_room_rems(cx)
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
    /// What the Pull request surface reads PRs through, and which PRs are
    /// linked to each session.
    prs: Entity<PullRequests>,
    /// Tells the Pull request surface when the panel shows or hides, and
    /// opens and closes the tabs of PRs as they are linked and unlinked.
    _subscriptions: [Subscription; 2],
}

impl RightPanel {
    pub(crate) fn new(
        workspaces: Entity<Workspaces>,
        shell: TerminalShell,
        prs: Entity<PullRequests>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let subscriptions = [
            cx.observe_global::<PanelLayout>(|this, cx| this.sync_shown(cx)),
            // A session's set of PRs answered or changed: its tabs follow.
            cx.observe(&prs, |this, _, cx| this.sync_all_links(cx)),
        ];
        Self {
            workspaces,
            shell,
            sets: HashMap::new(),
            session: None,
            hovered: None,
            prs,
            _subscriptions: subscriptions,
        }
    }

    /// The session on screen.
    fn on_screen(&self) -> Option<&SessionKey> {
        self.session.as_ref().map(|(key, _)| key)
    }

    /// The tabs of the session on screen.
    fn set(&self) -> Option<&TabSet> {
        self.sets.get(self.on_screen()?)
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
    /// shown, for the session on screen, in a panel that shows. Its PR is
    /// asked for only then.
    fn sync_shown(&self, cx: &mut Context<'_, Self>) {
        let shown = PanelLayout::get(cx);
        for (key, set) in &self.sets {
            for tab in &set.tabs {
                let Tab::PullRequest { view, .. } = tab else {
                    continue;
                };
                let on_screen =
                    shown && self.on_screen() == Some(key) && set.active == Some(tab.key());
                let visibility = if on_screen {
                    Visibility::Shown
                } else {
                    Visibility::Hidden
                };
                view.update(cx, |view, cx| view.set_visibility(visibility, cx));
            }
        }
    }

    /// Shows the tabs of `session`, the one on screen, or none. Its first
    /// time on screen, it has none open; coming back, its Diff and Files
    /// read again.
    pub(crate) fn set_session(
        &mut self,
        session: Option<Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) {
        let key = session.as_ref().map(|view| view.read(cx).live().id);
        let unchanged = self.on_screen() == key.as_ref()
            && self.set().map(|set| set.view.entity_id())
                == session.as_ref().map(Entity::entity_id);
        if unchanged {
            return;
        }
        self.session = None;
        self.hovered = None;
        if let (Some(view), Some(key)) = (session, key) {
            let stale = self
                .sets
                .get(&key)
                .map(|set| set.view.entity_id() != view.entity_id());
            if stale != Some(false) {
                let turns = Self::follow_turns(&key, &view, cx);
                if let Some(set) = self.sets.get_mut(&key) {
                    // A view made again for the session.
                    set.view = view.downgrade();
                    set.turns = turns;
                } else {
                    let set = TabSet::new(&view, turns, cx);
                    self.sets.insert(key.clone(), set);
                }
            }
            if stale.is_some() {
                // What changed while it was away shows.
                self.reload(&key, Reload::Shown, cx);
            }
            let redraw = cx.observe(&view, |_, _, cx| cx.notify());
            self.session = Some((key, redraw));
        }
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
    /// ended, its PR on screen (which is asked for again as it comes back).
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
        let session = self.on_screen()?.clone();
        let set = self.sets.get_mut(&session)?;
        let open = set
            .tabs
            .iter()
            .map(Tab::key)
            .find(|key| key.surface == surface);
        let key = match open {
            Some(key) if !surface.opens_many() => key,
            _ => {
                let key = set.next_key(surface);
                let place = set.place.clone();
                // Made apart from the set, as the tab takes the panel's
                // own entities.
                let tab = self.new_tab(&session, &place, key, window, cx);
                self.sets.get_mut(&session)?.push(tab);
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
            Surface::PullRequest => self.pr_tab(session, key.number, None, cx),
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

    /// A Pull request tab of `session`, numbered `number`: of PR `linked`,
    /// or with none, of its branch's.
    fn pr_tab(
        &self,
        session: &SessionKey,
        number: u32,
        linked: Option<PullRequestId>,
        cx: &mut Context<'_, Self>,
    ) -> Tab {
        let which = linked.clone().map_or(Which::Branch, Which::Linked);
        let prs = self.prs.clone();
        let key = session.clone();
        let pr = cx.new(|cx| PullRequestSurface::new(prs, key, which, cx));
        let session = session.clone();
        let subscriptions = [
            // Its label, the PR's number, comes once the PR is found.
            cx.observe(&pr, |_, _, cx| cx.notify()),
            cx.subscribe(&pr, move |this, _, ShowLinked(id): &ShowLinked, cx| {
                // Its link may not have reached the observer of `prs` yet.
                this.sync_links(&session, cx);
                this.show_link(&session, id, cx);
            }),
        ];
        Tab::PullRequest {
            number,
            linked,
            view: pr,
            _subscriptions: subscriptions,
        }
    }

    /// [`Self::sync_links`] for every session.
    fn sync_all_links(&mut self, cx: &mut Context<'_, Self>) {
        let sessions: Vec<SessionKey> = self.sets.keys().cloned().collect();
        for session in &sessions {
            self.sync_links(session, cx);
        }
    }

    /// Opens a tab for each PR linked to `session` that has none, after its
    /// other Pull request tabs, and closes the tab of each unlinked; while
    /// its branch's Pull request tab is open, which they go with. The links
    /// are the daemon's ([`PullRequests::linked_tabs`]), the agent's too;
    /// until it has looked the session's branch up, as before its machine
    /// answers, its tabs stay as they are.
    fn sync_links(&mut self, session: &SessionKey, cx: &mut Context<'_, Self>) {
        let Some(linked) = self.prs.read(cx).linked_tabs(session) else {
            return;
        };
        let Some(set) = self.sets.get_mut(session) else {
            return;
        };
        if set.tab(TabKey::BRANCH_PR).is_none() {
            return;
        }
        let (unlinked, missing) = set.link_changes(&linked);
        if unlinked.is_empty() && missing.is_empty() {
            return;
        }
        set.remove(|key| unlinked.contains(&key));
        let numbered: Vec<(u32, PullRequestId)> = missing
            .into_iter()
            .map(|id| (set.next_link(), id))
            .collect();
        let tabs: Vec<Tab> = numbered
            .into_iter()
            .map(|(number, id)| self.pr_tab(session, number, Some(id), cx))
            .collect();
        if let Some(set) = self.sets.get_mut(session) {
            for tab in tabs {
                set.insert_pr(tab);
            }
        }
        self.forget_closed_hover();
        self.sync_shown(cx);
        cx.notify();
    }

    /// Shows the tab of PR `id`, linked to `session`, if it is on screen:
    /// the branch's tab when `id` is the branch's own PR, which has no
    /// linked tab of its own.
    fn show_link(&mut self, session: &SessionKey, id: &PullRequestId, cx: &mut Context<'_, Self>) {
        if self.on_screen() != Some(session) {
            return;
        }
        let is_branch_pr = self
            .prs
            .read(cx)
            .of(session)
            .and_then(SessionPullRequests::branch_pull_request)
            .is_some_and(|pr| pr.id == *id);
        let Some(set) = self.set() else {
            return;
        };
        let shows = |tab: &Tab| match tab.linked() {
            Some(linked) => linked == id,
            None => is_branch_pr && tab.key() == TabKey::BRANCH_PR,
        };
        let Some(index) = set.tabs.iter().position(shows) else {
            return;
        };
        set.scroll.scroll_to_item(index);
        self.show(set.tabs[index].key(), cx);
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
    /// the tabs of linked PRs with it, which stay linked; closing one of
    /// those unlinks it through its surface (ending the agent's watch, if
    /// it watches it), and the observer of `prs` closes it once the
    /// daemon's set no longer links it. A refusal shows on that surface.
    fn close(&mut self, session: &SessionKey, key: TabKey, cx: &mut Context<'_, Self>) {
        let Some(set) = self.sets.get_mut(session) else {
            return;
        };
        if let Some(Tab::PullRequest {
            linked: Some(_),
            view,
            ..
        }) = set.tab(key)
        {
            view.clone().update(cx, PullRequestSurface::unlink);
            return;
        }
        if key == TabKey::BRANCH_PR {
            set.remove(|key| key.surface == Surface::PullRequest);
        } else {
            set.remove(|removed| removed == key);
        }
        self.forget_closed_hover();
        self.sync_shown(cx);
        cx.notify();
    }

    /// Forgets the tab under the pointer if it closed.
    fn forget_closed_hover(&mut self) {
        if self.hovered.and_then(|key| self.tab(key)).is_none() {
            self.hovered = None;
        }
    }

    /// The tab `key` of the session on screen.
    fn tab(&self, key: TabKey) -> Option<&Tab> {
        self.set()?.tab(key)
    }

    /// The tab on screen.
    fn active_tab(&self) -> Option<&Tab> {
        self.set()?.active_tab()
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
        if let Some(index) = set.position(key) {
            set.scroll.scroll_to_item(index);
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
        if let Some(set) = self.set_mut() {
            set.move_to(key, index);
            cx.notify();
        }
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
        // On a linked PR's tab it unlinks, which asks the daemon: it is
        // offered only while the daemon can be asked, as Unlink is.
        let prs = self.prs.read(cx);
        let close = self
            .session
            .as_ref()
            .filter(|_| self.hovered == Some(key))
            .and_then(|(session, _)| match tab.linked() {
                Some(linked) => prs.asks(&session.machine).then(|| {
                    let link = prs.of(session).and_then(|set| set.link(linked));
                    (session, "Unlink", unlink_tooltip(link))
                }),
                None => Some((session, "Close", "Close")),
            });
        let lead = if let Some((session, close, tooltip)) = close {
            let session = session.clone();
            Button::new(SharedString::from(format!("close-panel-tab-{id}")))
                .ghost()
                .xsmall()
                .compact()
                .icon(IconName::Close)
                .accessibility_label(close)
                .tooltip(tooltip)
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
    /// The room between + and the toggle moves the window, where the app
    /// draws the title bar.
    fn render_header(&self, window: &Window, cx: &Context<'_, Self>) -> impl IntoElement {
        let menu_view = cx.entity().downgrade();
        let set = self.set();
        let tabs = set.into_iter().flat_map(|set| {
            let tabs = set.tabs.iter().enumerate();
            tabs.map(move |(index, tab)| self.render_tab(set, index, tab, cx))
        });
        let openable: Vec<Surface> = set.into_iter().flat_map(TabSet::openable).collect();
        let scroll = set.map_or_else(ScrollHandle::new, |set| set.scroll.clone());
        h_flex()
            .flex_shrink_0()
            .h(rems(TOP_ROW_REMS))
            .gap_1()
            // The title bar's inset, so the toggle doesn't move.
            .pl_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .gap_1()
                    .child(
                        h_flex()
                            .id("panel-tabs")
                            .min_w_0()
                            .overflow_x_scroll()
                            .track_scroll(&scroll)
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
                    }))
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .moves_window(DragArea::PanelHeader, window, cx),
                    ),
            )
            .child(toggle_button(cx))
            .map(|row| match window_chrome::caption_room(window, cx) {
                Some(room) => row.child(room),
                None => row.pr_4(),
            })
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
            .px_4()
            .child(v_flex().w_full().max_w(rems(18.)).gap_0p5().children(open))
    }

    /// What `tab`'s surface shows of `set`'s session.
    fn render_surface(set: &TabSet, tab: &Tab, cx: &App) -> AnyElement {
        match tab {
            // The session on screen is open.
            Tab::Background => set.view.upgrade().map_or_else(
                || div().into_any_element(),
                |view| SessionView::background_surface(&view, cx).into_any_element(),
            ),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
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
            .child(self.render_header(window, cx))
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
