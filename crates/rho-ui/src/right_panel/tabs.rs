//! A session's tabs in [`super::RightPanel`]: which surfaces it has open,
//! in what order, which one shows, and the numbers new ones get. Each
//! session has a [`TabSet`] of its own.

use gpui_kit::{App, Entity, ScrollHandle, SharedString, Subscription, WeakEntity};

use super::{
    brief::BriefSurface, context::ContextSurface, diff::DiffSurface, files::FilesSurface,
    lead_work::WorkSurface, pull_request::PullRequestSurface, terminal::TerminalTab, Surface,
};
use crate::{leads::Kind, machines::Place, session_view::SessionView};

/// Which tab of a session's: its surface, and its number among that
/// surface's tabs there, from 1 and never reused. Terminal opens as many as
/// asked ([`Surface::opens_many`]), and Side chat one per fork; the others
/// are always 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TabKey {
    pub(super) surface: Surface,
    pub(super) number: u32,
}

impl TabKey {
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
    /// A lead's work: the sessions it started and the PRs linked to it.
    Work(Entity<WorkSurface>),
    /// A lead's `brief.md`.
    Brief(Entity<BriefSurface>),
    /// How full the session's context window is, and what fills it.
    Context(Entity<ContextSurface>),
    /// The session's PRs, one at a time, with the subscription that
    /// redraws the panel as its label changes.
    PullRequest {
        view: Entity<PullRequestSurface>,
        _redraw: Subscription,
    },
    /// One of any number of terminal tabs, by its number, with the
    /// subscriptions that redraw the panel as its program changes and close
    /// the tab when its last pane closes.
    Terminal {
        number: u32,
        view: Entity<TerminalTab>,
        _subscriptions: [Subscription; 2],
    },
    /// One of any number of side chats, by its number: the view of a fork
    /// of the session, which the app follows as an open session.
    SideChat {
        number: u32,
        view: Entity<SessionView>,
    },
}

impl Tab {
    pub(super) fn key(&self) -> TabKey {
        let (surface, number) = match self {
            Tab::Background => (Surface::Background, 1),
            Tab::Diff(_) => (Surface::Diff, 1),
            Tab::Files(_) => (Surface::Files, 1),
            Tab::Work(_) => (Surface::Work, 1),
            Tab::Brief(_) => (Surface::Brief, 1),
            Tab::Context(_) => (Surface::Context, 1),
            Tab::PullRequest { .. } => (Surface::PullRequest, 1),
            Tab::Terminal { number, .. } => (Surface::Terminal, *number),
            Tab::SideChat { number, .. } => (Surface::SideChat, *number),
        };
        TabKey { surface, number }
    }

    /// What the tab says: a terminal's names the program it runs, as `vim`,
    /// a PR's its number, as `#12`, and a side chat after the first its
    /// own, as `Side chat 2`.
    pub(super) fn label(&self, cx: &App) -> SharedString {
        match self {
            Tab::Terminal { view, .. } => view
                .read(cx)
                .program(cx)
                .unwrap_or_else(|| Surface::Terminal.label().into()),
            Tab::PullRequest { view, .. } => view.read(cx).label(cx),
            Tab::SideChat { number: 1, .. } => Surface::SideChat.label().into(),
            Tab::SideChat { number, .. } => {
                format!("{} {number}", Surface::SideChat.label()).into()
            }
            Tab::Background
            | Tab::Diff(_)
            | Tab::Files(_)
            | Tab::Work(_)
            | Tab::Brief(_)
            | Tab::Context(_) => self.key().surface.label().into(),
        }
    }
}

/// One session's open surfaces, which show while it is on screen.
pub(super) struct TabSet {
    /// The session's view while it is open: one made again for the same
    /// session, as after the app let it go, takes its place.
    pub(super) view: WeakEntity<SessionView>,
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
    /// The number the next side chat's tab gets.
    next_side_chat: u32,
    /// The session's own kind: a lead alone offers [`Surface::LEAD`],
    /// which open as that is decided. `None` until its agent is known,
    /// which may be after the set is made; decided once, so tabs the user
    /// closed stay closed.
    pub(super) kind: Option<Kind>,
}

impl TabSet {
    pub(super) fn new(view: &Entity<SessionView>, turns: Subscription, cx: &App) -> Self {
        let live = view.read(cx).live();
        Self {
            place: Place::new(live.id.machine, live.cwd),
            view: view.downgrade(),
            turns,
            tabs: Vec::new(),
            active: None,
            scroll: ScrollHandle::new(),
            next_terminal: 1,
            next_side_chat: 1,
            kind: None,
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

    /// The session's kind as its view knows it now, for [`Self::kind`]:
    /// `None` while its agent isn't known. A lead's subagent or side chat
    /// is no lead itself ([`SessionView::kind`]).
    pub(super) fn known_kind(&self, cx: &App) -> Option<Kind> {
        let view = self.view.upgrade()?;
        let view = view.read(cx);
        if view.parent().is_some() {
            Some(Kind::Session)
        } else {
            view.agent_kind()
        }
    }

    /// The surfaces + and the list offer: those that open many, and those
    /// without a tab; Side chat while the session can start one
    /// ([`SessionView::side_chat_refusal`]); [`Surface::LEAD`] for a lead
    /// alone.
    pub(super) fn openable(&self, cx: &App) -> Vec<Surface> {
        let side_chats = self
            .view
            .upgrade()
            .is_some_and(|view| view.read(cx).side_chat_refusal().is_none());
        let lead = self.kind == Some(Kind::Lead);
        Surface::ALL
            .into_iter()
            .filter(|surface| lead || !Surface::LEAD.contains(surface))
            .filter(|&surface| match surface {
                Surface::SideChat => side_chats,
                surface => {
                    surface.opens_many()
                        || !self.tabs.iter().any(|tab| tab.key().surface == surface)
                }
            })
            .collect()
    }

    /// The key a new tab of `surface` gets.
    pub(super) fn next_key(&mut self, surface: Surface) -> TabKey {
        let number = match surface {
            Surface::Terminal => {
                self.next_terminal += 1;
                self.next_terminal - 1
            }
            Surface::SideChat => {
                self.next_side_chat += 1;
                self.next_side_chat - 1
            }
            Surface::PullRequest
            | Surface::Background
            | Surface::Diff
            | Surface::Files
            | Surface::Work
            | Surface::Brief
            | Surface::Context => 1,
        };
        TabKey { surface, number }
    }

    /// Adds `tab` at the end, scrolled into view.
    pub(super) fn push(&mut self, tab: Tab) {
        self.tabs.push(tab);
        self.scroll.scroll_to_item(self.tabs.len() - 1);
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
}
