//! A session's tabs in [`super::RightPanel`]: which surfaces it has open,
//! in what order, which one shows, and the numbers new ones get. Each
//! session has a [`TabSet`] of its own.

use gpui_kit::{App, Entity, ScrollHandle, SharedString, Subscription};

use super::{
    diff::DiffSurface, files::FilesSurface, pull_request::PullRequestSurface,
    terminal::TerminalTab, Surface,
};
use crate::{machines::Place, pull_requests::PrId, session_view::SessionView};

/// Which tab of a session's: its surface, and its number among that
/// surface's tabs there, from 1 and never reused. Terminal opens as many as
/// asked ([`Surface::opens_many`]); Pull request opens
/// [`TabKey::BRANCH_PR`] for the branch's PR and one more for each PR
/// linked by hand; the others are always 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TabKey {
    pub(super) surface: Surface,
    pub(super) number: u32,
}

impl TabKey {
    /// The Pull request tab of the session's branch, which the tabs of PRs
    /// linked by hand open and close with.
    pub(super) const BRANCH_PR: TabKey = TabKey {
        surface: Surface::PullRequest,
        number: 1,
    };

    /// The tab's part of its elements' IDs: its surface's, and its number
    /// after the first, as in `terminal-2`.
    pub(super) fn id(self) -> String {
        match self.number {
            1 => self.surface.id().to_owned(),
            number => format!("{}-{number}", self.surface.id()),
        }
    }
}

/// An open surface, with its view if it keeps state.
pub(super) enum Tab {
    Background,
    Diff(Entity<DiffSurface>),
    Files(Entity<FilesSurface>),
    /// A PR of the session, by its number ([`TabKey::BRANCH_PR`] for its
    /// branch's), the PR it shows if linked by hand, and the subscriptions
    /// that redraw the panel as its label changes and show the tab of a PR
    /// just linked.
    PullRequest {
        number: u32,
        linked: Option<PrId>,
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
    pub(super) fn key(&self) -> TabKey {
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
    pub(super) fn label(&self, cx: &App) -> SharedString {
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
    pub(super) fn linked(&self) -> Option<&PrId> {
        match self {
            Tab::PullRequest { linked, .. } => linked.as_ref(),
            Tab::Background | Tab::Diff(_) | Tab::Files(_) | Tab::Terminal { .. } => None,
        }
    }
}

/// One session's open surfaces, which show while it is on screen.
pub(super) struct TabSet {
    /// The session's view: one made again for the same session, as after a
    /// failed attach, takes its place.
    pub(super) view: Entity<SessionView>,
    /// The session's folder, where its surfaces read.
    pub(super) place: Place,
    /// Tells the surfaces when the view's turn ends; held, not read.
    pub(super) turns: Subscription,
    /// In the order opened, or as dragged.
    pub(super) tabs: Vec<Tab>,
    /// One of `tabs`, or `None` for the list of surfaces when none is open.
    pub(super) active: Option<TabKey>,
    /// Scrolls the header's tabs when more are open than fit.
    pub(super) scroll: ScrollHandle,
    /// The number the next terminal tab gets.
    next_terminal: u32,
    /// The number the next tab of a PR linked by hand gets.
    next_link: u32,
}

impl TabSet {
    pub(super) fn new(view: Entity<SessionView>, turns: Subscription, cx: &App) -> Self {
        let live = view.read(cx).live();
        Self {
            place: Place::new(live.id.machine, live.cwd),
            view,
            turns,
            tabs: Vec::new(),
            active: None,
            scroll: ScrollHandle::new(),
            next_terminal: 1,
            next_link: TabKey::BRANCH_PR.number + 1,
        }
    }

    pub(super) fn tab(&self, key: TabKey) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.key() == key)
    }

    /// The tab on screen.
    pub(super) fn active_tab(&self) -> Option<&Tab> {
        self.tab(self.active?)
    }

    pub(super) fn position(&self, key: TabKey) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.key() == key)
    }

    /// The surfaces + and the list offer: those that open many, and those
    /// without a tab.
    pub(super) fn openable(&self) -> impl Iterator<Item = Surface> + '_ {
        Surface::ALL.into_iter().filter(|&surface| {
            surface.opens_many() || !self.tabs.iter().any(|tab| tab.key().surface == surface)
        })
    }

    /// The key a new tab of `surface` gets.
    pub(super) fn next_key(&mut self, surface: Surface) -> TabKey {
        let number = match surface {
            Surface::Terminal => {
                self.next_terminal += 1;
                self.next_terminal - 1
            }
            Surface::PullRequest => TabKey::BRANCH_PR.number,
            Surface::Background | Surface::Diff | Surface::Files => 1,
        };
        TabKey { surface, number }
    }

    /// The number the next tab of a PR linked by hand gets.
    pub(super) fn next_link(&mut self) -> u32 {
        self.next_link += 1;
        self.next_link - 1
    }

    /// Adds `tab` at the end, scrolled into view.
    pub(super) fn push(&mut self, tab: Tab) {
        self.tabs.push(tab);
        self.scroll.scroll_to_item(self.tabs.len() - 1);
    }

    /// Adds a linked PR's `tab` after the last Pull request tab.
    pub(super) fn insert_pr(&mut self, tab: Tab) {
        let after = self
            .tabs
            .iter()
            .rposition(|tab| tab.key().surface == Surface::PullRequest);
        self.tabs
            .insert(after.map_or(self.tabs.len(), |at| at + 1), tab);
    }

    /// Takes the tabs whose keys `remove` picks out. If one was showing,
    /// the last tab left shows, or the list of surfaces when none is.
    pub(super) fn remove(&mut self, remove: impl Fn(TabKey) -> bool) {
        self.tabs.retain(|tab| !remove(tab.key()));
        if self.active.is_some_and(&remove) {
            self.active = self.tabs.last().map(Tab::key);
        }
    }

    /// Moves tab `key` to `index`, where a drag dropped it.
    pub(super) fn move_to(&mut self, key: TabKey, index: usize) {
        let Some(from) = self.position(key) else {
            return;
        };
        let tab = self.tabs.remove(from);
        self.tabs.insert(index.min(self.tabs.len()), tab);
    }

    /// How its tabs differ from the PRs `linked` to the session: the tabs
    /// of PRs no longer linked, and the PRs linked without a tab.
    pub(super) fn link_changes(&self, linked: &[PrId]) -> (Vec<TabKey>, Vec<PrId>) {
        let unlinked = self
            .tabs
            .iter()
            .filter(|tab| tab.linked().is_some_and(|id| !linked.contains(id)))
            .map(Tab::key)
            .collect();
        let missing = linked
            .iter()
            .filter(|&id| !self.tabs.iter().any(|tab| tab.linked() == Some(id)))
            .cloned()
            .collect();
        (unlinked, missing)
    }
}
