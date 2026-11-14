//! The sidebar's data: every listed session of every machine as a flat row,
//! newest first, which shelf each is on, and the items the sidebar's one
//! scrolling list shows: active sessions, a gap, then each shelf's header
//! and, when it's open, its sessions. A [`Filter`] hides machines and keeps
//! to chosen projects. Plain data with no GUI code; [`crate::sidebar`] draws
//! it.

use std::{
    cmp::Reverse,
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui_kit::SharedString;
use rho_ui_core::JobActivity;

use crate::{
    machines::{MachineId, Place, SessionKey},
    shelf::{Shelf, Shelves},
};

/// What a session is doing, for the status at the right of its row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Activity {
    #[default]
    Idle,
    /// A turn is running; it started at this moment.
    Working(Instant),
    /// The session waits for a person; the first request arrived at this
    /// moment.
    Waiting(Instant),
}

impl Activity {
    pub(crate) fn is_idle(self) -> bool {
        self == Activity::Idle
    }
}

/// A session from the daemon's saved list.
pub(crate) struct SavedSession {
    pub(crate) id: SessionKey,
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
    pub(crate) id: SessionKey,
    pub(crate) cwd: PathBuf,
    /// What the session's own header shows: the agent's title, or until it
    /// sends one, the first prompt.
    pub(crate) title: Option<SharedString>,
    /// What the header shows before either: [`NEW_SESSION`] or
    /// [`UNTITLED`]. The row shows the same.
    pub(crate) placeholder: &'static str,
    pub(crate) activity: Activity,
}

/// A session with work under way in the daemon, whichever client started
/// it. It gives a session that isn't open here its activity, and lists one
/// the saved list doesn't have.
pub(crate) struct JobSession {
    pub(crate) id: SessionKey,
    pub(crate) cwd: PathBuf,
    pub(crate) title: Option<SharedString>,
    pub(crate) activity: JobActivity,
    /// How long its turn has run, when the daemon says.
    pub(crate) turn_elapsed: Option<Duration>,
    /// Index into [`crate::workspaces::Workspaces`].
    pub(crate) workspace: usize,
}

/// A [`JobSession`] as last polled, with the moment its activity began.
struct JobClock {
    id: SessionKey,
    activity: Activity,
}

/// The title of a saved session the agent hasn't named.
pub(crate) const UNTITLED: &str = "Untitled";
/// The title of a session the agent hasn't saved or named yet.
pub(crate) const NEW_SESSION: &str = "New session";

/// One listed session.
pub(crate) struct Row {
    pub(crate) id: SessionKey,
    /// `session-<machine>-<id>`, made once: the row's element ID.
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
    /// The daemon lists it as saved. Unsaved rows are sessions made in this run,
    /// and sessions the daemon runs that it doesn't list as saved.
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
    /// Unsaved sessions first, then saved ones of every machine, newest
    /// first.
    rows: Vec<Row>,
    /// Open sessions' live state, laid over the saved list.
    live: Vec<LiveSession>,
    /// Sessions with work under way in the daemon. An open session's own
    /// view knows better, so `live` wins over these.
    jobs: Vec<JobClock>,
    /// What the list shows, filtered by the search.
    items: Vec<Item>,
    /// Sessions matching the search on each shelf, indexed by [`slot`].
    counts: [usize; 3],
    /// Which shelves show their sessions; both start folded.
    open: [bool; 3],
    query: String,
    filter: Filter,
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
    /// Replaces the saved sessions of `machine`, whose daemon listed
    /// `saved`. Unsaved sessions the daemon now lists become saved rows;
    /// live titles and activity carry over.
    pub(crate) fn set_saved(&mut self, machine: &MachineId, saved: Vec<SavedSession>) {
        self.rows.retain(|row| {
            let listed = || saved.iter().any(|session| session.id == row.id);
            if row.saved {
                row.id.machine != *machine
            } else {
                !listed()
            }
        });
        // Keep the unsaved rows on top.
        self.rows.sort_by_key(|row| row.saved);
        let unsaved = self.rows.iter().take_while(|row| !row.saved).count();
        for session in saved {
            let live = self.live.iter().find(|live| live.id == session.id);
            self.rows.push(Row {
                element_id: format!("session-{}", session.id).into(),
                title: live.and_then(|live| live.title.clone()).or(session.title),
                placeholder: live.map_or(UNTITLED, |live| live.placeholder),
                activity: self.activity(&session.id),
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

    /// What session `id` is doing: as its open view says, else as the
    /// daemon's jobs do.
    fn activity(&self, id: &SessionKey) -> Activity {
        let live = self.live.iter().find(|live| live.id == *id);
        let job = || self.jobs.iter().find(|job| job.id == *id);
        match live {
            Some(live) => live.activity,
            None => job().map(|job| job.activity).unwrap_or_default(),
        }
    }

    /// Replaces the jobs of `machine`'s daemon. A session that isn't open
    /// here takes its activity from them, and one the saved list doesn't
    /// have gets a row while its job lasts. A job that goes on keeps the
    /// moment its activity began, so its clock doesn't jump between polls.
    pub(crate) fn set_jobs(
        &mut self,
        machine: &MachineId,
        jobs: Vec<JobSession>,
        now: Instant,
        now_unix: i64,
    ) {
        let (before, others): (Vec<_>, Vec<_>) = std::mem::take(&mut self.jobs)
            .into_iter()
            .partition(|job| job.id.machine == *machine);
        let began = |job: &JobSession| {
            let was = before.iter().find(|was| was.id == job.id);
            match (job.activity, was.map(|was| was.activity)) {
                (JobActivity::Working, Some(was @ Activity::Working(_)))
                | (JobActivity::Waiting, Some(was @ Activity::Waiting(_))) => was,
                (JobActivity::Working, _) => Activity::Working(
                    job.turn_elapsed
                        .and_then(|elapsed| now.checked_sub(elapsed))
                        .unwrap_or(now),
                ),
                (JobActivity::Waiting, _) => Activity::Waiting(now),
            }
        };
        self.jobs = others;
        self.jobs.extend(jobs.iter().map(|job| JobClock {
            id: job.id.clone(),
            activity: began(job),
        }));
        let is_open = |id: &SessionKey| self.live.iter().any(|live| live.id == *id);
        let has_job = |id: &SessionKey| self.jobs.iter().any(|job| job.id == *id);
        self.rows
            .retain(|row| row.saved || is_open(&row.id) || has_job(&row.id));
        for job in jobs {
            if self.rows.iter().any(|row| row.id == job.id) {
                continue;
            }
            self.rows.insert(
                0,
                Row {
                    element_id: format!("session-{}", job.id).into(),
                    id: job.id,
                    title: job.title,
                    placeholder: UNTITLED,
                    cwd: job.cwd,
                    workspace: job.workspace,
                    updated: now_unix,
                    activity: Activity::Idle,
                    shelf: Shelf::Active,
                    saved: false,
                },
            );
        }
        for ix in 0..self.rows.len() {
            self.rows[ix].activity = self.activity(&self.rows[ix].id);
        }
    }

    /// Forgets what an open session's view said about it, once the view is
    /// gone: its row shows what the daemon's jobs say again.
    pub(crate) fn close_live(&mut self, id: &SessionKey) {
        self.live.retain(|live| live.id != *id);
        if let Some(ix) = self.rows.iter().position(|row| row.id == *id) {
            self.rows[ix].activity = self.activity(id);
        }
    }

    /// Drops a deleted session.
    pub(crate) fn remove(&mut self, id: &SessionKey) {
        self.rows.retain(|row| row.id != *id);
        self.live.retain(|live| live.id != *id);
        self.jobs.retain(|job| job.id != *id);
    }

    /// Forgets every open session and job of `machine`, once the
    /// connection that followed them is gone.
    pub(crate) fn clear_live(&mut self, machine: &MachineId) {
        self.live.retain(|live| live.id.machine != *machine);
        self.jobs.retain(|job| job.id.machine != *machine);
        self.rows
            .retain(|row| row.saved || row.id.machine != *machine);
        for row in &mut self.rows {
            if row.id.machine == *machine {
                row.activity = Activity::Idle;
            }
        }
    }

    /// Drops every session of `machine`, gone from the settings.
    pub(crate) fn remove_machine(&mut self, machine: &MachineId) {
        self.clear_live(machine);
        self.rows.retain(|row| row.id.machine != *machine);
    }

    pub(crate) fn filter(&self) -> &Filter {
        &self.filter
    }

    pub(crate) fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
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
    /// `workspace` gives a workspace's name, for the search, and its
    /// project's folder, for the filter. A busy session is always active.
    pub(crate) fn rebuild<'a>(
        &mut self,
        shelves: &Shelves,
        now: i64,
        workspace: impl Fn(usize) -> (&'a str, &'a Path),
    ) {
        for row in &mut self.rows {
            row.shelf = if row.activity.is_idle() {
                shelves.shelf(&row.id, row.updated, now)
            } else {
                Shelf::Active
            };
        }
        let matches = |row: &Row| {
            let (name, root) = workspace(row.workspace);
            let matches = |text: &str| contains_ignore_case(text, &self.query);
            self.filter.shows(&row.id.machine, root)
                && (self.query.is_empty()
                    || row.title.as_deref().is_some_and(matches)
                    || matches(name))
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

    pub(crate) fn row(&self, id: &SessionKey) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == *id)
    }

    /// How many sessions wait for a person and how many are working,
    /// whatever the search shows.
    pub(crate) fn busy(&self) -> Busy {
        let mut busy = Busy::default();
        for row in &self.rows {
            match row.activity {
                Activity::Waiting(_) => busy.waiting += 1,
                Activity::Working(_) => busy.working += 1,
                Activity::Idle => {}
            }
        }
        busy
    }
}

/// Which sessions the sidebar lists: every machine's but the hidden ones,
/// and when any projects are chosen, only sessions in those.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Filter {
    hidden: BTreeSet<MachineId>,
    projects: BTreeSet<Place>,
}

impl Filter {
    /// Whether a session on `machine` in the project at `root` is listed.
    fn shows(&self, machine: &MachineId, root: &Path) -> bool {
        !self.hidden.contains(machine)
            && (self.projects.is_empty()
                || self
                    .projects
                    .iter()
                    .any(|project| project.machine == *machine && project.path == root))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.hidden.is_empty() && self.projects.is_empty()
    }

    pub(crate) fn hides(&self, machine: &MachineId) -> bool {
        self.hidden.contains(machine)
    }

    pub(crate) fn chose(&self, project: &Place) -> bool {
        self.projects.contains(project)
    }

    /// Hides `machine`'s sessions, or shows them again.
    pub(crate) fn toggle_machine(&mut self, machine: &MachineId) {
        if !self.hidden.remove(machine) {
            self.hidden.insert(machine.clone());
        }
    }

    /// Adds `project` to the chosen projects, or takes it off.
    pub(crate) fn toggle_project(&mut self, project: &Place) {
        if !self.projects.remove(project) {
            self.projects.insert(project.clone());
        }
    }

    /// The filter as the state database keeps it: a line per hidden
    /// machine (`machine <id>`) and chosen project (`project <machine>
    /// <path>`, tab-separated). A path that isn't UTF-8 isn't kept.
    pub(crate) fn to_text(&self) -> String {
        let machines = self.hidden.iter().map(|id| format!("machine\t{id}"));
        let projects = self.projects.iter().filter_map(|project| {
            let path = project.path.to_str()?;
            Some(format!("project\t{}\t{path}", project.machine))
        });
        machines.chain(projects).collect::<Vec<_>>().join("\n")
    }

    /// Reads [`Self::to_text`]'s lines, skipping any it can't.
    pub(crate) fn from_text(text: &str) -> Self {
        let mut filter = Self::default();
        for line in text.lines() {
            let mut fields = line.split('\t');
            match (fields.next(), fields.next(), fields.next()) {
                (Some("machine"), Some(id), None) => {
                    filter.hidden.insert(MachineId::from(id));
                }
                (Some("project"), Some(machine), Some(path)) => {
                    filter
                        .projects
                        .insert(Place::new(MachineId::from(machine), path));
                }
                _ => {}
            }
        }
        filter
    }
}

/// How many sessions are busy, by what they do; see [`SessionList::busy`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Busy {
    pub(crate) waiting: usize,
    pub(crate) working: usize,
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

    fn id(text: &str) -> SessionKey {
        SessionKey::local(text)
    }

    fn here() -> MachineId {
        MachineId::local()
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
        list.rebuild(shelves, now, |_| ("atlas", Path::new("/w")));
    }

    fn job(text: &str, activity: JobActivity, elapsed: Option<u64>) -> JobSession {
        JobSession {
            id: id(text),
            cwd: PathBuf::from("/w"),
            title: Some(SharedString::from(format!("job {text}"))),
            activity,
            turn_elapsed: elapsed.map(Duration::from_secs),
            workspace: 0,
        }
    }

    /// A turn another client runs makes its session active, though the
    /// session is not rho-ui's, and its clock starts where the daemon says
    /// and keeps that start as the turn goes on.
    #[test]
    fn a_job_makes_its_session_active_and_keeps_its_clock() {
        let now = Instant::now();
        let mut list = SessionList::default();
        list.set_saved(&here(), vec![saved("old", Some("older"), 1)]);
        let nobodys = Shelves::default();
        rebuild(&mut list, &nobodys, 3);
        assert_eq!(shown(&list), ["~", "[Settled 1]"]);

        list.set_jobs(
            &here(),
            vec![job("old", JobActivity::Working, Some(90))],
            now,
            3,
        );
        rebuild(&mut list, &nobodys, 3);
        assert_eq!(shown(&list), ["older"]);
        let began = now
            .checked_sub(Duration::from_secs(90))
            .expect("uptime over 90 s");
        assert_eq!(
            list.row(&id("old")).map(|row| row.activity),
            Some(Activity::Working(began))
        );

        // A second later the daemon says 91 s; the row's clock doesn't move.
        let later = now + Duration::from_secs(1);
        list.set_jobs(
            &here(),
            vec![job("old", JobActivity::Working, Some(91))],
            later,
            4,
        );
        assert_eq!(
            list.row(&id("old")).map(|row| row.activity),
            Some(Activity::Working(began))
        );

        // Waiting is another activity, with its own start.
        list.set_jobs(
            &here(),
            vec![job("old", JobActivity::Waiting, Some(92))],
            later,
            4,
        );
        assert_eq!(
            list.row(&id("old")).map(|row| row.activity),
            Some(Activity::Waiting(later))
        );

        list.set_jobs(&here(), Vec::new(), later, 5);
        rebuild(&mut list, &nobodys, 5);
        assert_eq!(
            shown(&list),
            ["~", "[Settled 1]"],
            "its turn over, it settles again"
        );
    }

    /// A running session the saved list lacks, such as one that has saved
    /// nothing yet, is listed while its job lasts. An open session's own
    /// view outranks the poll.
    #[test]
    fn a_job_without_a_saved_session_gets_a_row_while_it_runs() {
        let now = Instant::now();
        let mut list = SessionList::default();
        list.set_jobs(
            &here(),
            vec![job("fresh", JobActivity::Working, None)],
            now,
            3,
        );
        rebuild(&mut list, &Shelves::default(), 3);
        assert_eq!(shown(&list), ["job fresh"]);
        assert_eq!(
            list.row(&id("fresh")).map(|row| row.activity),
            Some(Activity::Working(now))
        );

        list.set_live(live("fresh", None, Activity::Idle), 0, 3);
        assert_eq!(
            list.row(&id("fresh")).map(|row| row.activity),
            Some(Activity::Idle)
        );

        list.clear_live(&here());
        rebuild(&mut list, &Shelves::default(), 3);
        assert!(shown(&list).is_empty(), "gone with the connection");
    }

    #[test]
    fn unsaved_sessions_sit_on_top_until_the_agent_lists_them() {
        let mut list = SessionList::default();
        list.set_saved(
            &here(),
            vec![saved("old", None, 1), saved("new", Some("newer"), 2)],
        );
        list.set_live(live("fresh", None, Activity::Idle), 0, 3);
        rebuild(&mut list, &owned(), 3);
        assert_eq!(shown(&list), ["New session", "newer", "Untitled"]);

        // The agent saves it with its title; the live title (none yet)
        // doesn't hide the saved one.
        list.set_saved(
            &here(),
            vec![saved("fresh", Some("named"), 3), saved("old", None, 1)],
        );
        rebuild(&mut list, &owned(), 3);
        assert_eq!(shown(&list), ["named", "Untitled"]);
    }

    #[test]
    fn live_titles_win_and_restarts_drop_unsaved_rows() {
        let mut list = SessionList::default();
        list.set_saved(&here(), vec![saved("a", Some("saved title"), 1)]);
        list.set_live(live("a", Some("live title"), Activity::Idle), 0, 2);
        list.set_live(live("b", None, Activity::Idle), 0, 2);
        rebuild(&mut list, &owned(), 2);
        assert_eq!(shown(&list), ["New session", "live title"]);

        list.clear_live(&here());
        rebuild(&mut list, &owned(), 2);
        assert_eq!(shown(&list), ["live title"]);
    }

    #[test]
    fn busy_sessions_stay_active_and_a_finished_turn_unsettles() {
        let mut shelves = owned();
        let mut list = SessionList::default();
        list.set_saved(&here(), vec![saved("a", Some("a"), 10)]);
        shelves.settle(&id("a"), 20);
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
        list.set_saved(
            &here(),
            vec![
                saved("a", Some("Design the UI"), 3),
                saved("b", Some("Fix boot"), 2),
                saved("old", Some("Old work"), 1),
            ],
        );
        shelves.settle(&id("a"), 5);
        shelves.snooze(&id("old"), 100);
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
        shelves.restore(&id("a"));
        rebuild(&mut list, &shelves, 5);
        shelves.settle(&id("a"), 5);
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

    #[test]
    fn busy_counts_every_session_whatever_the_search_shows() {
        let mut list = SessionList::default();
        let now = Instant::now();
        list.set_live(live("a", None, Activity::Working(now)), 0, 1);
        list.set_live(live("b", None, Activity::Waiting(now)), 0, 1);
        list.set_live(live("c", None, Activity::Working(now)), 0, 1);
        list.set_live(live("d", None, Activity::Idle), 0, 1);
        list.set_query("matches nothing");
        rebuild(&mut list, &owned(), 1);
        assert!(list.is_empty());
        assert_eq!(
            list.busy(),
            Busy {
                waiting: 1,
                working: 2
            }
        );
    }

    /// Two machines' sessions list together, newest first, whatever order
    /// their daemons answer in, even with the same session IDs; a filter
    /// hides a machine or keeps to chosen projects.
    #[test]
    fn machines_list_together_by_recency_and_filter_apart() {
        let there = MachineId::remote("workstation");
        let theirs = |text: &str, updated| SavedSession {
            id: SessionKey::new(there.clone(), rho_ui_core::SessionId::from(text)),
            cwd: PathBuf::from("/srv"),
            title: Some(SharedString::from(format!("there {text}"))),
            updated,
            workspace: 1,
        };
        let mut owned = Shelves::owning(["a", "b"]);
        owned.restore(&SessionKey::new(
            there.clone(),
            rho_ui_core::SessionId::from("a"),
        ));
        let mut list = SessionList::default();
        list.set_saved(
            &here(),
            vec![saved("a", Some("here a"), 3), saved("b", Some("here b"), 1)],
        );
        list.set_saved(&there, vec![theirs("a", 2)]);
        let roots = |ix: usize| match ix {
            0 => ("atlas", Path::new("/w")),
            _ => ("srv", Path::new("/srv")),
        };
        list.rebuild(&owned, 4, roots);
        assert_eq!(shown(&list), ["here a", "there a", "here b"]);

        // A second answer from one machine replaces only its rows.
        list.set_saved(&there, vec![theirs("a", 5)]);
        list.rebuild(&owned, 6, roots);
        assert_eq!(shown(&list), ["there a", "here a", "here b"]);

        let mut filter = Filter::default();
        filter.toggle_machine(&there);
        list.set_filter(filter.clone());
        list.rebuild(&owned, 6, roots);
        assert_eq!(shown(&list), ["here a", "here b"]);

        filter.toggle_machine(&there);
        filter.toggle_project(&Place::new(there.clone(), "/srv"));
        list.set_filter(filter.clone());
        list.rebuild(&owned, 6, roots);
        assert_eq!(shown(&list), ["there a"]);
        assert_eq!(Filter::from_text(&filter.to_text()), filter);
    }
}
