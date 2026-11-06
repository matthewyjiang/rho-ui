//! The sidebar's data: every listed session as a flat row, which shelf each
//! is on, and the items the sidebar's one scrolling list shows: active
//! sessions, a gap, then each shelf's header and, when it's open, its sessions.
//! Plain data with no GUI code; [`crate::sidebar`] draws it.

use std::{cmp::Reverse, path::PathBuf, time::Instant};

use gpui_kit::SharedString;
use rho_ui_core::SessionId;

use crate::shelf::{Shelf, Shelves};

/// What a session is doing, for the status at the right of its row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Activity {
    #[default]
    Idle,
    /// A turn is running; it started at this moment.
    Working(Instant),
    /// Approvals are waiting; the first arrived at this moment.
    Waiting(Instant),
}

impl Activity {
    pub(crate) fn is_idle(self) -> bool {
        self == Activity::Idle
    }
}

/// A session from the agent's saved list.
pub(crate) struct SavedSession {
    pub(crate) id: SessionId,
    pub(crate) cwd: PathBuf,
    pub(crate) title: Option<SharedString>,
    /// Last activity in Unix seconds; 0 when unknown.
    pub(crate) updated: i64,
    /// Index into [`crate::workspaces::Workspaces`].
    pub(crate) workspace: usize,
}

/// What an open session knows that the saved list may not have yet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LiveSession {
    pub(crate) id: SessionId,
    pub(crate) cwd: PathBuf,
    /// What the session's own header shows: the agent's title, or until it
    /// sends one, the first prompt.
    pub(crate) title: Option<SharedString>,
    /// What the header shows before either: [`NEW_SESSION`] or
    /// [`UNTITLED`]. The row shows the same.
    pub(crate) placeholder: &'static str,
    pub(crate) activity: Activity,
}

/// The title of a saved session the agent hasn't named.
pub(crate) const UNTITLED: &str = "Untitled";
/// The title of a session the agent hasn't saved or named yet.
pub(crate) const NEW_SESSION: &str = "New session";

/// One listed session.
pub(crate) struct Row {
    pub(crate) id: SessionId,
    /// `session-<id>`, made once: the row's element ID.
    pub(crate) element_id: SharedString,
    /// The open session's title, else the one the agent lists.
    pub(crate) title: Option<SharedString>,
    /// Shown while `title` is `None`.
    placeholder: &'static str,
    pub(crate) cwd: PathBuf,
    /// Index into [`crate::workspaces::Workspaces`].
    pub(crate) workspace: usize,
    /// Last activity in Unix seconds; 0 when unknown.
    pub(crate) updated: i64,
    pub(crate) activity: Activity,
    pub(crate) shelf: Shelf,
    /// The agent has saved it. Unsaved rows are sessions made in this run.
    pub(crate) saved: bool,
}

impl Row {
    pub(crate) fn title(&self) -> SharedString {
        self.title
            .clone()
            .unwrap_or_else(|| self.placeholder.into())
    }
}

/// One entry in the sidebar's list. Each kind has its own height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    /// An active session, drawn in full. An index into the rows.
    Session(usize),
    /// A snoozed or settled session, drawn as one line.
    Shelved(usize),
    /// A shelf's header, with its count; it opens and folds the shelf.
    Header(Shelf),
    /// The gap above the shelves. The sidebar stretches it to fill the
    /// space the list leaves, so folded shelves sit at the bottom and
    /// only rise as opened shelves fill it. Search results have none: they
    /// read from the top.
    Gap,
}

#[derive(Default)]
pub(crate) struct SessionList {
    /// Unsaved sessions first, then saved ones, newest first.
    rows: Vec<Row>,
    /// Open sessions' live state, laid over the saved list.
    live: Vec<LiveSession>,
    /// What the list shows, filtered by the search.
    items: Vec<Item>,
    /// Sessions matching the search on each shelf, indexed by [`slot`].
    counts: [usize; 3],
    /// Which shelves show their sessions; both start folded.
    open: [bool; 3],
    query: String,
}

/// A shelf's index into per-shelf arrays.
fn slot(shelf: Shelf) -> usize {
    match shelf {
        Shelf::Active => 0,
        Shelf::Snoozed => 1,
        Shelf::Settled => 2,
    }
}

impl SessionList {
    /// Replaces the saved sessions. Unsaved sessions the agent now lists
    /// become saved rows; live titles and activity carry over.
    pub(crate) fn set_saved(&mut self, saved: Vec<SavedSession>) {
        self.rows
            .retain(|row| !row.saved && !saved.iter().any(|session| session.id == row.id));
        let unsaved = self.rows.len();
        for session in saved {
            let live = self.live.iter().find(|live| live.id == session.id);
            self.rows.push(Row {
                element_id: format!("session-{}", session.id).into(),
                title: live.and_then(|live| live.title.clone()).or(session.title),
                placeholder: live.map_or(UNTITLED, |live| live.placeholder),
                activity: live.map(|live| live.activity).unwrap_or_default(),
                id: session.id,
                cwd: session.cwd,
                workspace: session.workspace,
                updated: session.updated,
                shelf: Shelf::Active,
                saved: true,
            });
        }
        self.rows[unsaved..].sort_by_key(|row| Reverse(row.updated));
    }

    /// Records an open session's title and activity; `workspace` is its
    /// workspace index. A session the saved list doesn't have yet goes on top.
    /// Returns whether anything changed.
    pub(crate) fn set_live(&mut self, live: LiveSession, workspace: usize, now: i64) -> bool {
        match self.live.iter_mut().find(|known| known.id == live.id) {
            Some(known) if *known == live => return false,
            Some(known) => *known = live.clone(),
            None => self.live.push(live.clone()),
        }
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == live.id) {
            if live.title.is_some() {
                row.title = live.title;
            }
            row.placeholder = live.placeholder;
            // A turn that just ended is new activity, so a settled session
            // stays settled only if nothing happened since.
            if !row.activity.is_idle() && live.activity.is_idle() {
                row.updated = now;
            }
            row.activity = live.activity;
        } else {
            self.rows.insert(
                0,
                Row {
                    element_id: format!("session-{}", live.id).into(),
                    id: live.id,
                    title: live.title,
                    placeholder: live.placeholder,
                    cwd: live.cwd,
                    workspace,
                    updated: now,
                    activity: live.activity,
                    shelf: Shelf::Active,
                    saved: false,
                },
            );
        }
        true
    }

    /// Drops a deleted session.
    pub(crate) fn remove(&mut self, id: &SessionId) {
        self.rows.retain(|row| row.id != *id);
        self.live.retain(|live| live.id != *id);
    }

    /// Forgets every open session, after the agent restarts.
    pub(crate) fn clear_live(&mut self) {
        self.live.clear();
        self.rows.retain(|row| row.saved);
        for row in &mut self.rows {
            row.activity = Activity::Idle;
        }
    }

    pub(crate) fn set_query(&mut self, query: &str) {
        query.clone_into(&mut self.query);
    }

    /// Opens a folded shelf or folds an open one.
    pub(crate) fn toggle(&mut self, which: Shelf) {
        self.open[slot(which)] ^= true;
    }

    /// Whether `shelf` shows its sessions. A search opens every shelf.
    pub(crate) fn is_open(&self, which: Shelf) -> bool {
        which == Shelf::Active || self.open[slot(which)] || !self.query.is_empty()
    }

    /// Puts every row on its shelf and rebuilds the items and counts.
    /// `workspace_name` gives a workspace's name, for the search. A busy session
    /// is always active.
    pub(crate) fn rebuild<'a>(
        &mut self,
        shelves: &Shelves,
        now: i64,
        workspace_name: impl Fn(usize) -> &'a str,
    ) {
        for row in &mut self.rows {
            row.shelf = if row.activity.is_idle() {
                shelves.shelf(row.id.as_str(), row.updated, now)
            } else {
                Shelf::Active
            };
        }
        let matches = |row: &Row| {
            let matches = |text: &str| contains_ignore_case(text, &self.query);
            self.query.is_empty()
                || row.title.as_deref().is_some_and(matches)
                || matches(workspace_name(row.workspace))
        };
        let mut items = Vec::with_capacity(self.rows.len() + 2);
        let mut counts = [0; 3];
        for shelf in [Shelf::Active, Shelf::Snoozed, Shelf::Settled] {
            let start = items.len();
            for (ix, row) in self.rows.iter().enumerate() {
                if row.shelf == shelf && matches(row) {
                    counts[slot(shelf)] += 1;
                    items.push(match shelf {
                        Shelf::Active => Item::Session(ix),
                        Shelf::Snoozed | Shelf::Settled => Item::Shelved(ix),
                    });
                }
            }
            let count = counts[slot(shelf)];
            // An emptied shelf folds, so it comes back folded.
            if count == 0 && self.query.is_empty() {
                self.open[slot(shelf)] = false;
            }
            if shelf != Shelf::Active && count > 0 {
                if !self.is_open(shelf) {
                    items.truncate(start);
                }
                items.insert(start, Item::Header(shelf));
            }
        }
        let first_header = items
            .iter()
            .position(|item| matches!(item, Item::Header(_)));
        if let (Some(first), true) = (first_header, self.query.is_empty()) {
            items.insert(first, Item::Gap);
        }
        self.counts = counts;
        self.items = items;
    }

    /// Sessions matching the search on `shelf`.
    pub(crate) fn count(&self, which: Shelf) -> usize {
        self.counts[slot(which)]
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// What the list shows, in order.
    pub(crate) fn items(&self) -> &[Item] {
        &self.items
    }

    /// The row an [`Item`] points at.
    pub(crate) fn row_at(&self, ix: usize) -> &Row {
        &self.rows[ix]
    }

    pub(crate) fn row(&self, id: &SessionId) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == *id)
    }
}

/// Whether `haystack` contains `needle`, ignoring ASCII case. Allocates
/// nothing, so filtering on every keystroke stays cheap.
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    needle.is_empty()
        || haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> SessionId {
        SessionId::from(text)
    }

    fn saved(text: &str, title: Option<&str>, updated: i64) -> SavedSession {
        SavedSession {
            id: id(text),
            cwd: PathBuf::from("/w"),
            title: title.map(SharedString::from),
            updated,
            workspace: 0,
        }
    }

    fn live(text: &str, title: Option<&str>, activity: Activity) -> LiveSession {
        LiveSession {
            id: id(text),
            cwd: PathBuf::from("/w"),
            title: title.map(SharedString::from),
            placeholder: NEW_SESSION,
            activity,
        }
    }

    /// What the list shows: session titles, shelf headers as `[Settled 2]`,
    /// and the gap above them as `~`.
    fn shown(list: &SessionList) -> Vec<String> {
        list.items()
            .iter()
            .map(|&item| match item {
                Item::Session(ix) | Item::Shelved(ix) => list.row_at(ix).title().to_string(),
                Item::Header(shelf) => format!("[{} {}]", shelf.label(), list.count(shelf)),
                Item::Gap => "~".to_owned(),
            })
            .collect()
    }

    /// Every session these tests list, owned so it shows as active.
    fn owned() -> Shelves {
        Shelves::owning(["a", "b", "old", "new", "fresh"])
    }

    fn rebuild(list: &mut SessionList, shelves: &Shelves, now: i64) {
        list.rebuild(shelves, now, |_| "atlas");
    }

    #[test]
    fn unsaved_sessions_sit_on_top_until_the_agent_lists_them() {
        let mut list = SessionList::default();
        list.set_saved(vec![saved("old", None, 1), saved("new", Some("newer"), 2)]);
        list.set_live(live("fresh", None, Activity::Idle), 0, 3);
        rebuild(&mut list, &owned(), 3);
        assert_eq!(shown(&list), ["New session", "newer", "Untitled"]);

        // The agent saves it with its title; the live title (none yet)
        // doesn't hide the saved one.
        list.set_saved(vec![
            saved("fresh", Some("named"), 3),
            saved("old", None, 1),
        ]);
        rebuild(&mut list, &owned(), 3);
        assert_eq!(shown(&list), ["named", "Untitled"]);
    }

    #[test]
    fn live_titles_win_and_restarts_drop_unsaved_rows() {
        let mut list = SessionList::default();
        list.set_saved(vec![saved("a", Some("saved title"), 1)]);
        list.set_live(live("a", Some("live title"), Activity::Idle), 0, 2);
        list.set_live(live("b", None, Activity::Idle), 0, 2);
        rebuild(&mut list, &owned(), 2);
        assert_eq!(shown(&list), ["New session", "live title"]);

        list.clear_live();
        rebuild(&mut list, &owned(), 2);
        assert_eq!(shown(&list), ["live title"]);
    }

    #[test]
    fn busy_sessions_stay_active_and_a_finished_turn_unsettles() {
        let mut shelves = owned();
        let mut list = SessionList::default();
        list.set_saved(vec![saved("a", Some("a"), 10)]);
        shelves.settle("a", 20);
        rebuild(&mut list, &shelves, 30);
        assert_eq!(list.count(Shelf::Settled), 1);

        let started = Instant::now();
        list.set_live(live("a", None, Activity::Working(started)), 0, 40);
        rebuild(&mut list, &shelves, 40);
        assert_eq!(list.count(Shelf::Active), 1, "busy shows as active");

        list.set_live(live("a", None, Activity::Idle), 0, 50);
        rebuild(&mut list, &shelves, 50);
        assert_eq!(list.count(Shelf::Active), 1, "the turn is new activity");
    }

    #[test]
    fn shelves_follow_the_active_sessions_and_open_on_click_or_search() {
        let mut shelves = owned();
        let mut list = SessionList::default();
        list.set_saved(vec![
            saved("a", Some("Design the UI"), 3),
            saved("b", Some("Fix boot"), 2),
            saved("old", Some("Old work"), 1),
        ]);
        shelves.settle("a", 5);
        shelves.snooze("old", 100);
        rebuild(&mut list, &shelves, 5);
        assert_eq!(
            shown(&list),
            ["Fix boot", "~", "[Snoozed 1]", "[Settled 1]"]
        );

        list.toggle(Shelf::Settled);
        rebuild(&mut list, &shelves, 5);
        assert_eq!(
            shown(&list),
            [
                "Fix boot",
                "~",
                "[Snoozed 1]",
                "[Settled 1]",
                "Design the UI"
            ]
        );

        // A search opens every shelf and counts only matches.
        // Emptied, it folds.
        shelves.restore("a");
        rebuild(&mut list, &shelves, 5);
        shelves.settle("a", 5);
        rebuild(&mut list, &shelves, 5);
        assert_eq!(
            shown(&list),
            ["Fix boot", "~", "[Snoozed 1]", "[Settled 1]"]
        );

        list.set_query("work");
        rebuild(&mut list, &shelves, 5);
        assert_eq!(shown(&list), ["[Snoozed 1]", "Old work"]);
        list.set_query("DESIGN");
        rebuild(&mut list, &shelves, 5);
        assert_eq!(shown(&list), ["[Settled 1]", "Design the UI"]);
        list.set_query("design ui");
        rebuild(&mut list, &shelves, 5);
        assert!(list.is_empty(), "words must appear together");
        list.set_query("atl");
        rebuild(&mut list, &shelves, 5);
        assert_eq!(list.items.len(), 5, "the workspace name matches too");
    }
}
