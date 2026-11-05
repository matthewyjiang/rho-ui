//! The sidebar's data: every listed thread as a flat row, which shelf each
//! is on, and which rows the current list and search show. Plain data with
//! no GUI code; [`crate::sidebar`] draws it.

use std::{cmp::Reverse, path::PathBuf, time::Instant};

use gpui_kit::SharedString;
use rho_ui_core::SessionId;

use crate::shelf::{Shelf, Shelves};

/// What a thread is doing, for the status at the right of its row.
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

/// A thread from the agent's saved list.
pub(crate) struct SavedThread {
    pub(crate) id: SessionId,
    pub(crate) cwd: PathBuf,
    pub(crate) title: Option<SharedString>,
    /// Last activity in Unix seconds; 0 when unknown.
    pub(crate) updated: i64,
    /// Index into [`crate::projects::Projects`].
    pub(crate) project: usize,
}

/// What an open thread knows that the saved list may not have yet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LiveThread {
    pub(crate) id: SessionId,
    pub(crate) cwd: PathBuf,
    /// The agent's title, once it has one.
    pub(crate) title: Option<SharedString>,
    /// What the thread's own header shows until then: [`NEW_THREAD`] or
    /// [`UNTITLED`]. The row shows the same.
    pub(crate) placeholder: &'static str,
    pub(crate) activity: Activity,
}

/// The title of a saved thread the agent hasn't named.
pub(crate) const UNTITLED: &str = "Untitled";
/// The title of a thread the agent hasn't saved or named yet.
pub(crate) const NEW_THREAD: &str = "New thread";

/// One listed thread.
pub(crate) struct Row {
    pub(crate) id: SessionId,
    /// `session-<id>`, made once: the row's element ID.
    pub(crate) element_id: SharedString,
    /// The agent's title, if it has given one.
    pub(crate) title: Option<SharedString>,
    /// Shown while `title` is `None`.
    placeholder: &'static str,
    pub(crate) cwd: PathBuf,
    /// Index into [`crate::projects::Projects`].
    pub(crate) project: usize,
    /// Last activity in Unix seconds; 0 when unknown.
    pub(crate) updated: i64,
    pub(crate) activity: Activity,
    pub(crate) shelf: Shelf,
    /// The agent has saved it. Unsaved rows are threads made in this run.
    pub(crate) saved: bool,
}

impl Row {
    pub(crate) fn title(&self) -> SharedString {
        self.title
            .clone()
            .unwrap_or_else(|| self.placeholder.into())
    }
}

#[derive(Default)]
pub(crate) struct ThreadList {
    /// Unsaved threads first, then saved ones, newest first.
    rows: Vec<Row>,
    /// Open threads' live state, laid over the saved list.
    live: Vec<LiveThread>,
    /// Indices into `rows`: the current list, filtered by the search.
    shown: Vec<usize>,
    snoozed: usize,
    settled: usize,
    list: Shelf,
    query: String,
}

impl ThreadList {
    /// Replaces the saved threads. Unsaved threads the agent now lists
    /// become saved rows; live titles and activity carry over.
    pub(crate) fn set_saved(&mut self, saved: Vec<SavedThread>) {
        self.rows
            .retain(|row| !row.saved && !saved.iter().any(|thread| thread.id == row.id));
        let unsaved = self.rows.len();
        for thread in saved {
            let live = self.live.iter().find(|live| live.id == thread.id);
            self.rows.push(Row {
                element_id: format!("session-{}", thread.id).into(),
                title: live.and_then(|live| live.title.clone()).or(thread.title),
                placeholder: live.map_or(UNTITLED, |live| live.placeholder),
                activity: live.map(|live| live.activity).unwrap_or_default(),
                id: thread.id,
                cwd: thread.cwd,
                project: thread.project,
                updated: thread.updated,
                shelf: Shelf::Active,
                saved: true,
            });
        }
        self.rows[unsaved..].sort_by_key(|row| Reverse(row.updated));
    }

    /// Records an open thread's title and activity; `project` is its
    /// project index. A thread the saved list doesn't have yet goes on top.
    /// Returns whether anything changed.
    pub(crate) fn set_live(&mut self, live: LiveThread, project: usize, now: i64) -> bool {
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
            // A turn that just ended is new activity, so a settled thread
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
                    project,
                    updated: now,
                    activity: live.activity,
                    shelf: Shelf::Active,
                    saved: false,
                },
            );
        }
        true
    }

    /// Forgets every open thread, after the agent restarts.
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

    pub(crate) fn show(&mut self, list: Shelf) {
        self.list = list;
    }

    /// Puts every row on its shelf and rebuilds the shown rows and counts.
    /// `project_name` gives a project's name, for the search. A busy thread
    /// is always active. Leaves a shelf list that has emptied.
    pub(crate) fn rebuild<'a>(
        &mut self,
        shelves: &Shelves,
        now: i64,
        project_name: impl Fn(usize) -> &'a str,
    ) {
        for row in &mut self.rows {
            row.shelf = if row.activity.is_idle() {
                shelves.shelf(row.id.as_str(), row.updated, now)
            } else {
                Shelf::Active
            };
        }
        self.snoozed = self.count(Shelf::Snoozed);
        self.settled = self.count(Shelf::Settled);
        if self.list != Shelf::Active && self.count(self.list) == 0 {
            self.list = Shelf::Active;
        }
        self.shown.clear();
        for (ix, row) in self.rows.iter().enumerate() {
            let matches = |text: &str| contains_ignore_case(text, &self.query);
            if row.shelf == self.list
                && (self.query.is_empty()
                    || row.title.as_deref().is_some_and(matches)
                    || matches(project_name(row.project)))
            {
                self.shown.push(ix);
            }
        }
    }

    fn count(&self, which: Shelf) -> usize {
        self.rows.iter().filter(|row| row.shelf == which).count()
    }

    /// The list being shown.
    pub(crate) fn list(&self) -> Shelf {
        self.list
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// How many rows the current list shows.
    pub(crate) fn len(&self) -> usize {
        self.shown.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.shown.is_empty()
    }

    /// The `ix`th shown row.
    pub(crate) fn shown(&self, ix: usize) -> Option<&Row> {
        self.shown.get(ix).map(|&row| &self.rows[row])
    }

    pub(crate) fn row(&self, id: &SessionId) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == *id)
    }

    /// How many threads each shelf holds.
    pub(crate) fn shelf_count(&self, which: Shelf) -> usize {
        match which {
            Shelf::Active => self.rows.len() - self.snoozed - self.settled,
            Shelf::Snoozed => self.snoozed,
            Shelf::Settled => self.settled,
        }
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

    fn saved(text: &str, title: Option<&str>, updated: i64) -> SavedThread {
        SavedThread {
            id: id(text),
            cwd: PathBuf::from("/w"),
            title: title.map(SharedString::from),
            updated,
            project: 0,
        }
    }

    fn live(text: &str, title: Option<&str>, activity: Activity) -> LiveThread {
        LiveThread {
            id: id(text),
            cwd: PathBuf::from("/w"),
            title: title.map(SharedString::from),
            placeholder: NEW_THREAD,
            activity,
        }
    }

    fn shown_titles(list: &ThreadList) -> Vec<String> {
        (0..list.len())
            .filter_map(|ix| list.shown(ix))
            .map(|row| row.title().to_string())
            .collect()
    }

    fn rebuild(list: &mut ThreadList, shelves: &Shelves, now: i64) {
        list.rebuild(shelves, now, |_| "project");
    }

    #[test]
    fn unsaved_threads_sit_on_top_until_the_agent_lists_them() {
        let mut list = ThreadList::default();
        list.set_saved(vec![saved("old", None, 1), saved("new", Some("newer"), 2)]);
        list.set_live(live("fresh", None, Activity::Idle), 0, 3);
        rebuild(&mut list, &Shelves::default(), 3);
        assert_eq!(shown_titles(&list), ["New thread", "newer", "Untitled"]);

        // The agent saves it with its title; the live title (none yet)
        // doesn't hide the saved one.
        list.set_saved(vec![
            saved("fresh", Some("named"), 3),
            saved("old", None, 1),
        ]);
        rebuild(&mut list, &Shelves::default(), 3);
        assert_eq!(shown_titles(&list), ["named", "Untitled"]);
    }

    #[test]
    fn live_titles_win_and_restarts_drop_unsaved_rows() {
        let mut list = ThreadList::default();
        list.set_saved(vec![saved("a", Some("saved title"), 1)]);
        list.set_live(live("a", Some("live title"), Activity::Idle), 0, 2);
        list.set_live(live("b", None, Activity::Idle), 0, 2);
        rebuild(&mut list, &Shelves::default(), 2);
        assert_eq!(shown_titles(&list), ["New thread", "live title"]);

        list.clear_live();
        rebuild(&mut list, &Shelves::default(), 2);
        assert_eq!(shown_titles(&list), ["live title"]);
    }

    #[test]
    fn busy_threads_stay_active_and_a_finished_turn_unsettles() {
        let mut shelves = Shelves::default();
        let mut list = ThreadList::default();
        list.set_saved(vec![saved("a", Some("a"), 10)]);
        shelves.settle("a", 20);
        rebuild(&mut list, &shelves, 30);
        assert_eq!(list.shelf_count(Shelf::Settled), 1);

        let started = Instant::now();
        list.set_live(live("a", None, Activity::Working(started)), 0, 40);
        rebuild(&mut list, &shelves, 40);
        assert_eq!(list.shelf_count(Shelf::Active), 1, "busy shows as active");

        list.set_live(live("a", None, Activity::Idle), 0, 50);
        rebuild(&mut list, &shelves, 50);
        assert_eq!(
            list.shelf_count(Shelf::Active),
            1,
            "the turn is new activity"
        );
    }

    #[test]
    fn an_emptied_shelf_list_returns_to_active_and_search_filters() {
        let mut shelves = Shelves::default();
        let mut list = ThreadList::default();
        list.set_saved(vec![
            saved("a", Some("Design the UI"), 2),
            saved("b", Some("Fix boot"), 1),
        ]);
        shelves.settle("a", 5);
        list.show(Shelf::Settled);
        rebuild(&mut list, &shelves, 5);
        assert_eq!(shown_titles(&list), ["Design the UI"]);

        shelves.restore("a");
        rebuild(&mut list, &shelves, 5);
        assert_eq!(list.list(), Shelf::Active);

        list.set_query("design ui");
        rebuild(&mut list, &shelves, 5);
        assert!(list.is_empty(), "words must appear together");
        list.set_query("DESIGN");
        rebuild(&mut list, &shelves, 5);
        assert_eq!(shown_titles(&list), ["Design the UI"]);
        list.set_query("proj");
        rebuild(&mut list, &shelves, 5);
        assert_eq!(list.len(), 2, "the project name matches too");
    }
}
