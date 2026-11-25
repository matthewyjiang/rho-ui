//! The sidebar's data: every listed session of every machine as a flat row,
//! newest first, which shelf each is on, and the items the sidebar's one
//! scrolling list shows: drafts of new sessions the user left unsent,
//! active sessions, the leads ([`Kind::Lead`]) first under a header of
//! their own, each part ranked as an inbox by what they want of the user
//! ([`Attention`]), a gap, then each shelf's header and, when it's open,
//! its newest [`SHELF_PAGE`] sessions, then a Show more that adds as many
//! again. A session another session started folds under that one
//! ([`family`]). A [`Filter`] hides machines and keeps to chosen projects,
//! which no lead is in. Plain data with no GUI code; [`crate::sidebar`]
//! draws it.

use std::{
    cmp::Reverse,
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use web_time::Instant;

use gpui_kit::{EntityId, SharedString};
use rho_ui_core::{JobActivity, SessionMark};

use crate::{
    leads::Kind,
    machines::{MachineId, Place, SessionKey},
    shelf::{self, Shelf},
    unseen::Unseen,
};

mod family;

use family::{Families, FamilyMarks};
pub(crate) use family::{Family, Fold, FoldLine, Pressing};

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
    /// No turn runs, but background work a turn started does, such as a
    /// subagent or a process: the session waits on it.
    Background,
}

impl Activity {
    pub(crate) fn is_idle(self) -> bool {
        self == Activity::Idle
    }
}

/// What a session wants of the user, which orders the unpinned rows of the
/// active list, first to last: the list reads as an inbox.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Attention {
    /// It waits for an answer.
    Asks,
    /// Its turn finished since it was last on screen.
    Done,
    /// At rest, and seen.
    #[default]
    Rests,
    /// A turn, or the background work one left, runs without the user, or
    /// its agent watches a pull request, which wakes it without the user.
    Runs,
}

/// A session from the daemon's saved list.
pub(crate) struct SavedSession {
    pub(crate) id: SessionKey,
    pub(crate) cwd: PathBuf,
    pub(crate) title: Option<SharedString>,
    /// Last activity in Unix seconds; 0 when unknown.
    pub(crate) updated: i64,
    /// How the user marked it, as its daemon keeps it.
    pub(crate) mark: Option<SessionMark>,
    /// Index into [`crate::workspaces::Workspaces`].
    pub(crate) workspace: usize,
    /// The session that started it, as the daemon's list says.
    pub(crate) parent: Option<SessionKey>,
    /// By the agent the daemon's list says it runs as; a plain session's
    /// when it doesn't say.
    pub(crate) kind: Kind,
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
    /// By the agent it runs as, which a new lead knows before the saved
    /// list has it; `None` until the view hears it, which leaves the
    /// saved list's word.
    pub(crate) kind: Option<Kind>,
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
    /// The session that started it, as the daemon's jobs say.
    pub(crate) parent: Option<SessionKey>,
}

/// A new session's draft the user left with a message in its composer,
/// listed above every session until it is sent or discarded.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DraftRow {
    /// The draft's view, which the app keeps.
    pub(crate) id: EntityId,
    /// `draft-<id>`, made once: the row's element ID.
    pub(crate) element_id: SharedString,
    /// A session's draft, or a lead's, which is in no project.
    pub(crate) kind: Kind,
    pub(crate) cwd: Place,
    /// The first line of its message; `None` while it has none, such as a
    /// message of attachments alone.
    pub(crate) title: Option<SharedString>,
    /// Index into [`crate::workspaces::Workspaces`]; `None` for a lead's
    /// draft until its session opens in the leads root.
    pub(crate) workspace: Option<usize>,
}

impl DraftRow {
    pub(crate) fn new(
        id: EntityId,
        kind: Kind,
        cwd: Place,
        title: Option<SharedString>,
        workspace: Option<usize>,
    ) -> Self {
        Self {
            id,
            element_id: format!("draft-{id}").into(),
            kind,
            cwd,
            title,
            workspace,
        }
    }

    pub(crate) fn title(&self) -> SharedString {
        self.title.clone().unwrap_or_else(|| {
            match self.kind {
                Kind::Session => NEW_SESSION,
                Kind::Lead => NEW_LEAD,
            }
            .into()
        })
    }
}

/// A [`JobSession`] as last polled, with the moment its activity began.
struct JobClock {
    id: SessionKey,
    activity: Activity,
    parent: Option<SessionKey>,
}

/// The title of a saved session the agent hasn't named.
pub(crate) const UNTITLED: &str = "Untitled";
/// The title of a session the agent hasn't saved or named yet.
pub(crate) const NEW_SESSION: &str = "New session";
/// The title of a lead's draft before its message has a line.
pub(crate) const NEW_LEAD: &str = "New lead";

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
    /// How the user marked it: as its daemon listed or last said, or as
    /// the user just did, until the daemon answers.
    pub(crate) mark: Option<SessionMark>,
    pub(crate) shelf: Shelf,
    /// The daemon lists it as saved. Unsaved rows are sessions made in this run,
    /// and sessions the daemon runs that it doesn't list as saved.
    pub(crate) saved: bool,
    /// When it was pinned, if it is: pinned rows come first among the
    /// active leads, and among the other active sessions.
    pub(crate) pinned: Option<i64>,
    /// Its turn finished since it was last on screen (see [`Unseen`]): the
    /// row says Done once no work runs. As of the last rebuild.
    pub(crate) done: bool,
    /// Its agent watches a pull request, which the row says instead of
    /// Done or its age while no work runs. An active row's only: nothing
    /// keeps what a shelved session watches current. As of the last
    /// rebuild.
    pub(crate) watching: bool,
    /// The session that started it, as the daemon's list or jobs say,
    /// which the list may not hold.
    pub(crate) parent: Option<SessionKey>,
    /// Whether it folds under the session that started it. As of the last
    /// rebuild.
    pub(crate) family: Family,
    /// A lead, as the saved list or its open view says; one known only
    /// from the daemon's jobs, which name no agent, is taken for a plain
    /// session until either does.
    pub(crate) kind: Kind,
}

impl Row {
    pub(crate) fn title(&self) -> SharedString {
        self.title
            .clone()
            .unwrap_or_else(|| self.placeholder.into())
    }

    /// What the row says of its session at its right; `None` when it says
    /// only its age.
    pub(crate) fn status(&self) -> Option<Status> {
        match self.activity {
            Activity::Working(since) => Some(Status::Working(since)),
            Activity::Waiting(since) => Some(Status::NeedsYou(since)),
            Activity::Background => Some(Status::Background),
            Activity::Idle if self.watching => Some(Status::Watching),
            Activity::Idle if self.done => Some(Status::Done),
            Activity::Idle => None,
        }
    }

    fn attention(&self) -> Attention {
        match self.status() {
            Some(Status::NeedsYou(_)) => Attention::Asks,
            Some(Status::Done) => Attention::Done,
            Some(Status::Working(_) | Status::Background | Status::Watching) => Attention::Runs,
            None => Attention::Rests,
        }
    }
}

/// What a row says of its session, short of its age: its [`Activity`],
/// and at rest, whether its agent watches a PR or its turn is Done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// A turn runs; it started at this moment.
    Working(Instant),
    /// It waits for the user; the first request arrived at this moment.
    NeedsYou(Instant),
    /// No turn runs, but background work a turn started does.
    Background,
    /// Its agent watches a pull request, which wakes it.
    Watching,
    /// Its turn finished since it was last on screen.
    Done,
}

impl Status {
    /// The word the sidebar's row says it with, and the Work surface's.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Status::Working(_) => "Working",
            Status::NeedsYou(_) => "Needs you",
            Status::Background => "Waiting",
            Status::Watching => "Watching",
            Status::Done => "Done",
        }
    }
}

/// One entry in the sidebar's list. Each kind has its own height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    /// A draft, drawn as an active session is. An index into the drafts.
    Draft(usize),
    /// An active session, drawn in full. An index into the rows.
    Session(usize),
    /// A snoozed or settled session, drawn as one line.
    Shelved(usize),
    /// Under a session that started others: how many show, and whether
    /// they do, under it as [`Item::Session`] or [`Item::Shelved`] by its
    /// shelf. A click opens or closes it, unless the search or the filter
    /// holds it open ([`Fold::Showing`]).
    Fold(FoldLine),
    /// A shelf's header, with its count; it opens and folds the shelf.
    Header(Shelf),
    /// The header over the active leads, which come first on the active
    /// shelf. It neither folds nor counts.
    Leads,
    /// Under an open shelf that holds back sessions: shows
    /// [`SHELF_PAGE`] more.
    More(Shelf),
    /// The gap above the shelves. The sidebar stretches it to fill the
    /// space the list leaves, so folded shelves sit at the bottom and
    /// only rise as opened shelves fill it. Search results have none: they
    /// read from the top.
    Gap,
    /// The header over the active sessions that follow the active leads.
    Sessions,
}

impl Item {
    /// Row `ix`, as `shelf` draws it.
    fn row(shelf: Shelf, ix: usize) -> Self {
        match shelf {
            Shelf::Active => Item::Session(ix),
            Shelf::Snoozed | Shelf::Settled => Item::Shelved(ix),
        }
    }
}

pub(crate) struct SessionList {
    /// Unsaved sessions first, then saved ones of every machine, newest
    /// first.
    rows: Vec<Row>,
    /// Listed above the rows, in this order.
    drafts: Vec<DraftRow>,
    /// Open sessions' live state, laid over the saved list.
    live: Vec<LiveSession>,
    /// Sessions with work under way in the daemon. An open session's own
    /// view knows better, so `live` wins over these.
    jobs: Vec<JobClock>,
    /// Sessions whose turn finished since they were last on screen.
    unseen: Unseen,
    /// What the list shows, filtered by the search.
    items: Vec<Item>,
    /// Families the search and the filter show on each shelf, indexed by
    /// [`slot`].
    counts: [usize; 3],
    /// Which shelves show their sessions; both start folded.
    open: [bool; 3],
    /// How many times each shelf's Show more was pressed since it opened.
    more: [usize; 3],
    query: String,
    filter: Filter,
    /// The starters whose fold the user opened, for this run.
    open_folds: HashSet<SessionKey>,
    /// The session on screen, until a rebuild opens the fold it's under.
    reveal: Option<SessionKey>,
}

/// Whether a row passes the machine and project filter, and the search, as
/// of one rebuild.
#[derive(Clone, Copy)]
struct Passes {
    filter: bool,
    search: bool,
}

impl Passes {
    fn both(self) -> bool {
        self.filter && self.search
    }
}

/// How many families, each a session and those it started, an opened shelf
/// shows, and how many each Show more adds, so a long history doesn't build
/// hundreds of rows at once.
pub(crate) const SHELF_PAGE: usize = 25;

/// A shelf's index into per-shelf arrays.
fn slot(shelf: Shelf) -> usize {
    match shelf {
        Shelf::Active => 0,
        Shelf::Snoozed => 1,
        Shelf::Settled => 2,
    }
}

/// For tests: nothing unseen, kept in memory.
#[cfg(test)]
impl Default for SessionList {
    fn default() -> Self {
        Self::new(Unseen::default())
    }
}

impl SessionList {
    /// An empty list, with the sessions `unseen` holds Done once listed.
    pub(crate) fn new(unseen: Unseen) -> Self {
        Self {
            rows: Vec::new(),
            drafts: Vec::new(),
            live: Vec::new(),
            jobs: Vec::new(),
            unseen,
            items: Vec::new(),
            counts: [0; 3],
            open: [false; 3],
            more: [0; 3],
            query: String::new(),
            filter: Filter::default(),
            open_folds: HashSet::new(),
            reveal: None,
        }
    }

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
                kind: live.and_then(|live| live.kind).unwrap_or(session.kind),
                element_id: format!("session-{}", session.id).into(),
                title: live.and_then(|live| live.title.clone()).or(session.title),
                placeholder: live.map_or(UNTITLED, |live| live.placeholder),
                activity: self.activity(&session.id),
                done: false,
                watching: false,
                parent: session.parent.or_else(|| self.job_parent(&session.id)),
                family: Family::Top(FamilyMarks::default()),
                id: session.id,
                cwd: session.cwd,
                workspace: session.workspace,
                updated: session.updated,
                mark: session.mark,
                shelf: Shelf::Active,
                saved: true,
                pinned: None,
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
        if let Some(ix) = self.rows.iter().position(|row| row.id == live.id) {
            let row = &mut self.rows[ix];
            if live.title.is_some() {
                row.title = live.title;
            }
            row.placeholder = live.placeholder;
            row.kind = live.kind.unwrap_or(row.kind);
            // A turn that just ended is new activity, so a settled session
            // stays settled only if nothing happened since.
            if !row.activity.is_idle() && live.activity.is_idle() {
                row.updated = now;
            }
            self.set_activity(ix, live.activity);
        } else {
            self.rows.insert(
                0,
                Row {
                    element_id: format!("session-{}", live.id).into(),
                    parent: self.job_parent(&live.id),
                    family: Family::Top(FamilyMarks::default()),
                    id: live.id,
                    title: live.title,
                    placeholder: live.placeholder,
                    kind: live.kind.unwrap_or(Kind::Session),
                    cwd: live.cwd,
                    workspace,
                    updated: now,
                    activity: live.activity,
                    mark: None,
                    shelf: Shelf::Active,
                    saved: false,
                    pinned: None,
                    done: false,
                    watching: false,
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
            None => job().map_or_default(|job| job.activity),
        }
    }

    /// The session that started session `id`, as the daemon's jobs say.
    fn job_parent(&self, id: &SessionKey) -> Option<SessionKey> {
        let job = self.jobs.iter().find(|job| job.id == *id)?;
        job.parent.clone()
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
                (JobActivity::Background, _) => Activity::Background,
            }
        };
        self.jobs = others;
        self.jobs.extend(jobs.iter().map(|job| JobClock {
            id: job.id.clone(),
            activity: began(job),
            parent: job.parent.clone(),
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
                    mark: None,
                    shelf: Shelf::Active,
                    saved: false,
                    pinned: None,
                    done: false,
                    watching: false,
                    parent: job.parent,
                    family: Family::Top(FamilyMarks::default()),
                    kind: Kind::Session,
                },
            );
        }
        for ix in 0..self.rows.len() {
            // The saved list of a rho from before parents, or one that
            // hasn't listed the session yet, may not say.
            if self.rows[ix].parent.is_none() {
                self.rows[ix].parent = self.job_parent(&self.rows[ix].id);
            }
            let activity = self.activity(&self.rows[ix].id);
            // As in `set_live`: a turn that just ended is new activity,
            // before the daemon's list dates the session by it.
            if !self.rows[ix].activity.is_idle() && activity.is_idle() {
                self.rows[ix].updated = now_unix;
            }
            self.set_activity(ix, activity);
        }
    }

    /// Sets what row `ix`'s session is doing. A turn that stops working
    /// leaves the session unseen, which shows as Done once its background
    /// work ends too; a new turn, or a request for the user, clears it.
    fn set_activity(&mut self, ix: usize, activity: Activity) {
        let row = &mut self.rows[ix];
        match activity {
            Activity::Idle | Activity::Background => {
                if matches!(row.activity, Activity::Working(_)) {
                    self.unseen.insert(&row.id);
                }
            }
            Activity::Working(_) | Activity::Waiting(_) => {
                self.unseen.remove(&row.id);
            }
        }
        row.activity = activity;
    }

    /// Replaces the listed drafts. Returns whether anything changed.
    pub(crate) fn set_drafts(&mut self, drafts: Vec<DraftRow>) -> bool {
        if self.drafts == drafts {
            return false;
        }
        self.drafts = drafts;
        true
    }

    /// Forgets what an open session's view said about it, once the app
    /// stops following the session: its row shows what the daemon's jobs
    /// say again, until the view follows it again and catches up.
    pub(crate) fn forget_live(&mut self, id: &SessionKey) {
        let settled = self
            .live
            .iter()
            .any(|live| live.id == *id && live.activity.is_idle());
        self.live.retain(|live| live.id != *id);
        // The view saw the session settle, perhaps after the last poll of
        // the jobs, which may still have it working: the next poll says.
        if settled {
            self.jobs.retain(|job| job.id != *id);
        }
        if let Some(ix) = self.rows.iter().position(|row| row.id == *id) {
            let activity = self.activity(id);
            self.set_activity(ix, activity);
        }
    }

    /// Shows `title` for session `id` until the daemon's list or the open
    /// session says otherwise, as it will once it has the rename.
    pub(crate) fn rename(&mut self, id: &SessionKey, title: SharedString) {
        if let Some(live) = self.live.iter_mut().find(|live| live.id == *id) {
            live.title = Some(title.clone());
        }
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == *id) {
            row.title = Some(title);
        }
    }

    /// Gives session `id` `mark`, as the user just did, until the daemon's
    /// list says what it saved. Returns whether its row changed; the list
    /// needs a rebuild after it.
    pub(crate) fn set_mark(&mut self, id: &SessionKey, mark: SessionMark) -> bool {
        match self.rows.iter_mut().find(|row| row.id == *id) {
            Some(row) if row.mark != Some(mark) => {
                row.mark = Some(mark);
                true
            }
            _ => false,
        }
    }

    /// Clears session `id`'s Done, as it is on screen: whether it had one.
    /// The list needs a rebuild after it.
    pub(crate) fn mark_seen(&mut self, id: &SessionKey) -> bool {
        self.unseen.remove(id)
    }

    /// Drops a deleted session.
    pub(crate) fn remove(&mut self, id: &SessionKey) {
        self.unseen.remove(id);
        if self.reveal.as_ref() == Some(id) {
            self.reveal = None;
        }
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

    /// Opens a folded shelf or folds an open one. Either way it shows its
    /// first [`SHELF_PAGE`] sessions next time.
    pub(crate) fn toggle(&mut self, which: Shelf) {
        self.open[slot(which)] ^= true;
        self.more[slot(which)] = 0;
    }

    /// Shows [`SHELF_PAGE`] more of `which`'s sessions.
    pub(crate) fn show_more(&mut self, which: Shelf) {
        self.more[slot(which)] += 1;
    }

    /// How many families `which` shows at most.
    fn limit(&self, which: Shelf) -> usize {
        match which {
            Shelf::Active => usize::MAX,
            Shelf::Snoozed | Shelf::Settled => SHELF_PAGE * (self.more[slot(which)] + 1),
        }
    }

    /// How many of the families the search and the filter show on `which`
    /// it holds back.
    pub(crate) fn held_back(&self, which: Shelf) -> usize {
        self.count(which).saturating_sub(self.limit(which))
    }

    /// Whether `shelf` shows its sessions. A search opens every shelf.
    pub(crate) fn is_open(&self, which: Shelf) -> bool {
        which == Shelf::Active || self.open[slot(which)] || !self.query.is_empty()
    }

    /// Puts every row on its shelf and rebuilds the items and counts.
    /// `workspace` gives a workspace's name, for the search, and its
    /// project's folder, for the filter; `watching` holds the sessions
    /// whose agent watches a pull request. A busy session is always active.
    /// The active leads come first, then the other active sessions; in
    /// each, pinned sessions first, the latest pinned first, then the rest
    /// by [`Attention`], newest first within each. An open shelf
    /// shows its newest sessions up to its limit, then a Show more. A
    /// session another started folds under the top-most starter listed, on
    /// its shelf, and shows by the filter, and by the search or its
    /// starter's match; a starter shows for those of its sessions that do.
    pub(crate) fn rebuild<'a>(
        &mut self,
        now: i64,
        watching: &HashSet<SessionKey>,
        workspace: impl Fn(usize) -> (&'a str, &'a Path),
    ) {
        let families = Families::of(&self.rows);
        self.keep_folds(&families);
        self.shelve(&families, now, watching);
        let sift = |machine: &MachineId, kind: Kind, title: Option<&str>, at: Option<usize>| {
            let (name, root) = at.map_or(("", Path::new("")), &workspace);
            let matches = |text: &str| contains_ignore_case(text, &self.query);
            Passes {
                filter: self.filter.shows(machine, root, kind),
                search: self.query.is_empty() || title.is_some_and(matches) || matches(name),
            }
        };
        let passes: Vec<Passes> = self
            .rows
            .iter()
            .map(|row| {
                sift(
                    &row.id.machine,
                    row.kind,
                    row.title.as_deref(),
                    Some(row.workspace),
                )
            })
            .collect();
        let mut items = Vec::with_capacity(self.drafts.len() + self.rows.len() + 2);
        items.extend(
            self.drafts
                .iter()
                .enumerate()
                .filter(|(_, draft)| {
                    let title = draft.title.as_deref();
                    sift(&draft.cwd.machine, draft.kind, title, draft.workspace).both()
                })
                .map(|(ix, _)| Item::Draft(ix)),
        );
        // A family shows when its starter passes the filter and the search,
        // or when any session under it shows.
        let visible = |ix: usize| passes[ix].both() || families.drawn(ix, &passes).next().is_some();
        let mut counts = [0; 3];
        for shelf in [Shelf::Active, Shelf::Snoozed, Shelf::Settled] {
            let mut shown: Vec<(usize, FamilyMarks)> = self
                .rows
                .iter()
                .enumerate()
                .filter_map(|(ix, row)| match row.family {
                    Family::Top(marks) if row.shelf == shelf && visible(ix) => Some((ix, marks)),
                    Family::Top(_) | Family::Started => None,
                })
                .collect();
            // The active by pin, then by what presses most; each shelf's
            // ties by the newest activity in the family.
            match shelf {
                Shelf::Active => shown.sort_by_key(|&(ix, marks)| {
                    let pinned = Reverse(self.rows[ix].pinned);
                    (pinned, marks.attention, Reverse(marks.updated))
                }),
                Shelf::Snoozed | Shelf::Settled => {
                    shown.sort_by_key(|&(_, marks)| Reverse(marks.updated));
                }
            }
            if shelf == Shelf::Active {
                shown = self.push_leads(&mut items, &families, shown, &passes);
            }
            let count = shown.len();
            counts[slot(shelf)] = count;
            // An emptied shelf folds, so it comes back folded.
            if count == 0 && self.query.is_empty() {
                self.open[slot(shelf)] = false;
                self.more[slot(shelf)] = 0;
            }
            if shelf != Shelf::Active && count > 0 {
                items.push(Item::Header(shelf));
            }
            if !self.is_open(shelf) {
                continue;
            }
            let limit = self.limit(shelf);
            for &(ix, _) in shown.iter().take(limit) {
                items.push(Item::row(shelf, ix));
                self.push_family(&mut items, &families, ix, &passes);
            }
            if count > limit {
                items.push(Item::More(shelf));
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

    /// Pushes the leads among `shown`, the active families in order, each
    /// with its family, under their header, then the header of the other
    /// active families when any follow. Returns those others: the active
    /// shelf counts only them.
    fn push_leads(
        &self,
        items: &mut Vec<Item>,
        families: &Families,
        shown: Vec<(usize, FamilyMarks)>,
        passes: &[Passes],
    ) -> Vec<(usize, FamilyMarks)> {
        let (leads, rest): (Vec<_>, Vec<_>) = shown
            .into_iter()
            .partition(|&(ix, _)| self.rows[ix].kind == Kind::Lead);
        if leads.is_empty() {
            return rest;
        }
        items.push(Item::Leads);
        for &(ix, _) in &leads {
            items.push(Item::Session(ix));
            self.push_family(items, families, ix, passes);
        }
        if !rest.is_empty() {
            items.push(Item::Sessions);
        }
        rest
    }

    /// Puts every row on its shelf as of `now`, with its pin, its Done, and
    /// whether its agent watches a pull request (`watching`). A family sits
    /// on its starter's shelf, under its starter's pin: active while any of
    /// it is busy, else placed by its starter's mark and its newest
    /// activity ([`FamilyMarks`]).
    fn shelve(&mut self, families: &Families, now: i64, watching: &HashSet<SessionKey>) {
        for row in &mut self.rows {
            row.done = self.unseen.contains(&row.id);
            row.watching = watching.contains(&row.id);
        }
        for top in 0..self.rows.len() {
            if families.starter(top).is_some() {
                continue;
            }
            let marks = families.marks(&self.rows, top);
            let row = &mut self.rows[top];
            row.family = Family::Top(marks);
            row.pinned = shelf::pinned(row.mark);
            row.shelf = if marks.busy {
                Shelf::Active
            } else {
                shelf::shelf(row.mark, marks.updated, now)
            };
            let placed = row.shelf;
            for &ix in families.members(top) {
                let member = &mut self.rows[ix];
                member.family = Family::Started;
                member.pinned = None;
                member.shelf = placed;
            }
        }
        // Nothing keeps what a shelved session watches current.
        for row in &mut self.rows {
            row.watching &= row.shelf == Shelf::Active;
        }
    }

    /// The sessions on the active shelf the filter shows, under a closed
    /// fold too: what they watch ranks their family. `root` gives a
    /// workspace's project folder.
    pub(crate) fn on_active_shelf<'a>(&self, root: impl Fn(usize) -> &'a Path) -> Vec<SessionKey> {
        self.rows
            .iter()
            .filter(|row| {
                row.shelf == Shelf::Active
                    && self
                        .filter
                        .shows(&row.id.machine, root(row.workspace), row.kind)
            })
            .map(|row| row.id.clone())
            .collect()
    }

    /// How many families the search and the filter show on `which`: a
    /// session and those it started count once. The active shelf leaves out
    /// its leads, which it lists apart and never holds back.
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

    /// The draft an [`Item::Draft`] points at.
    pub(crate) fn draft_at(&self, ix: usize) -> &DraftRow {
        &self.drafts[ix]
    }

    /// The row an [`Item`] points at.
    pub(crate) fn row_at(&self, ix: usize) -> &Row {
        &self.rows[ix]
    }

    pub(crate) fn row(&self, id: &SessionKey) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == *id)
    }

    /// The rows of the sessions `starter` started itself, not those they
    /// started in turn, in the list's order, whatever the search and the
    /// filter show.
    pub(crate) fn started_by<'a>(
        &'a self,
        starter: &'a SessionKey,
    ) -> impl Iterator<Item = &'a Row> + 'a {
        self.rows
            .iter()
            .filter(move |row| row.parent.as_ref() == Some(starter))
    }

    /// How many sessions wait for a person and how many are working,
    /// whatever the search shows.
    pub(crate) fn busy(&self) -> Busy {
        let mut busy = Busy::default();
        for row in &self.rows {
            busy.add(row.activity);
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
    /// Whether a session of `kind` on `machine` in the project at `root`
    /// is listed. A lead is in no project, so only its machine counts.
    fn shows(&self, machine: &MachineId, root: &Path, kind: Kind) -> bool {
        !self.hidden.contains(machine)
            && (kind == Kind::Lead
                || self.projects.is_empty()
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

    /// Forgets `machine`, gone from the settings, which the menu no longer
    /// offers: it isn't hidden, and none of its projects is chosen. Returns
    /// whether the filter changed.
    pub(crate) fn forget_machine(&mut self, machine: &MachineId) -> bool {
        let before = self.clone();
        self.hidden.remove(machine);
        self.projects.retain(|project| project.machine != *machine);
        *self != before
    }

    /// Keeps to the chosen projects that `listed` says the menu offers, or
    /// may yet. Returns whether the filter changed.
    pub(crate) fn keep_projects(&mut self, listed: impl Fn(&Place) -> bool) -> bool {
        let before = self.projects.len();
        self.projects.retain(listed);
        self.projects.len() != before
    }

    /// The filter that hides the machines `hidden` and keeps to the
    /// projects `projects`, as the state database gave them back.
    pub(crate) fn new(hidden: BTreeSet<MachineId>, projects: BTreeSet<Place>) -> Self {
        Self { hidden, projects }
    }

    /// The machines whose sessions it hides.
    pub(crate) fn hidden(&self) -> &BTreeSet<MachineId> {
        &self.hidden
    }

    /// The projects it keeps to; every one when empty.
    pub(crate) fn projects(&self) -> &BTreeSet<Place> {
        &self.projects
    }
}

/// How many sessions are busy, by what they do; see [`SessionList::busy`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Busy {
    pub(crate) waiting: usize,
    pub(crate) working: usize,
}

impl Busy {
    /// Counts a session doing `activity`.
    fn add(&mut self, activity: Activity) {
        match activity {
            Activity::Waiting(_) => self.waiting += 1,
            Activity::Working(_) => self.working += 1,
            // Nobody needs to look at work that runs by itself.
            Activity::Background | Activity::Idle => {}
        }
    }
}

/// Whether `haystack` contains `needle`, ignoring case in any script, so
/// `É` finds `é`: how every list's search matches. Compares lowercase
/// chars as it goes and allocates nothing, so filtering on every
/// keystroke stays cheap.
pub(crate) fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    fn lower(text: &str) -> impl Iterator<Item = char> + '_ {
        text.chars().flat_map(char::to_lowercase)
    }
    needle.is_empty()
        || haystack.char_indices().any(|(at, _)| {
            let mut rest = lower(&haystack[at..]);
            lower(needle).all(|c| rest.next() == Some(c))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{shelf::QUIET_AFTER, store::StateDb};

    pub(super) fn id(text: &str) -> SessionKey {
        SessionKey::local(text)
    }

    pub(super) fn here() -> MachineId {
        MachineId::local()
    }

    pub(super) fn saved(text: &str, title: Option<&str>, updated: i64) -> SavedSession {
        SavedSession {
            id: id(text),
            cwd: PathBuf::from("/w"),
            title: title.map(SharedString::from),
            updated,
            mark: None,
            workspace: 0,
            parent: None,
            kind: Kind::Session,
        }
    }

    fn live(text: &str, title: Option<&str>, activity: Activity) -> LiveSession {
        LiveSession {
            id: id(text),
            cwd: PathBuf::from("/w"),
            title: title.map(SharedString::from),
            placeholder: NEW_SESSION,
            activity,
            kind: Some(Kind::Session),
        }
    }

    /// What the list shows: session titles, indented under the session
    /// that started them, fold lines as `› 2 started, 1 waiting` (`⌄` when
    /// open, `•` when held open), shelf headers as `[Settled 2]`, the gap
    /// above them as `~`, and the break under the leads as `-`.
    pub(super) fn shown(list: &SessionList) -> Vec<String> {
        list.items()
            .iter()
            .map(|&item| match item {
                Item::Draft(ix) => format!("draft {}", list.draft_at(ix).title()),
                Item::Session(ix) | Item::Shelved(ix) => {
                    let row = list.row_at(ix);
                    match row.family {
                        Family::Top(_) => row.title().to_string(),
                        Family::Started => format!("  {}", row.title()),
                    }
                }
                Item::Fold(line) => {
                    let chevron = match line.fold {
                        Fold::Open => "⌄",
                        Fold::Closed => "›",
                        Fold::Showing => "•",
                    };
                    let pressing = match line.pressing {
                        None => String::new(),
                        Some(Pressing::Waiting(n)) => format!(", {n} waiting"),
                        Some(Pressing::Done(n)) => format!(", {n} done"),
                        Some(Pressing::Working(n)) => format!(", {n} working"),
                    };
                    format!("{chevron} {} started{pressing}", line.sessions)
                }
                Item::Header(shelf) => format!("[{} {}]", shelf.label(), list.count(shelf)),
                Item::Leads => "[Leads]".to_owned(),
                Item::More(shelf) => format!("[{} more {}]", shelf.label(), list.held_back(shelf)),
                Item::Gap => "~".to_owned(),
                Item::Sessions => "[Sessions]".to_owned(),
            })
            .collect()
    }

    pub(super) fn rebuild(list: &mut SessionList, now: i64) {
        rebuild_watching(list, now, &[]);
    }

    /// [`rebuild`], with the agents of `watching` watching a pull request.
    fn rebuild_watching(list: &mut SessionList, now: i64, watching: &[&str]) {
        let watching = watching.iter().map(|text| id(text)).collect();
        list.rebuild(now, &watching, |_| ("atlas", Path::new("/w")));
    }

    pub(super) fn mark(list: &mut SessionList, text: &str, mark: SessionMark) {
        assert!(list.set_mark(&id(text), mark), "{text} is listed");
    }

    pub(super) fn job(text: &str, activity: JobActivity, elapsed: Option<u64>) -> JobSession {
        JobSession {
            id: id(text),
            cwd: PathBuf::from("/w"),
            title: Some(SharedString::from(format!("job {text}"))),
            activity,
            turn_elapsed: elapsed.map(Duration::from_secs),
            workspace: 0,
            parent: None,
        }
    }

    /// A turn another client runs makes its long quiet session active, and
    /// leaves it active when it ends. Its clock starts where the daemon says
    /// and keeps that start as the turn goes on.
    #[test]
    fn a_job_makes_its_session_active_and_keeps_its_clock() {
        let now = Instant::now();
        let mut list = SessionList::default();
        list.set_saved(&here(), vec![saved("old", Some("older"), 1)]);
        let today = 1 + QUIET_AFTER + 1;
        rebuild(&mut list, today);
        assert_eq!(shown(&list), ["~", "[Settled 1]"]);

        list.set_jobs(
            &here(),
            vec![job("old", JobActivity::Working, Some(90))],
            now,
            today,
        );
        rebuild(&mut list, today);
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
            today,
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
            today,
        );
        assert_eq!(
            list.row(&id("old")).map(|row| row.activity),
            Some(Activity::Waiting(later))
        );

        list.set_jobs(&here(), Vec::new(), later, today);
        rebuild(&mut list, today);
        assert_eq!(shown(&list), ["older"], "the turn was activity");
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
        rebuild(&mut list, 3);
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
        rebuild(&mut list, 3);
        assert!(shown(&list).is_empty(), "gone with the connection");
    }

    #[test]
    fn live_and_saved_sessions_merge_into_one_row_each() {
        let mut list = SessionList::default();
        list.set_saved(
            &here(),
            vec![saved("old", None, 1), saved("new", Some("newer"), 2)],
        );
        list.set_live(live("fresh", None, Activity::Idle), 0, 3);
        rebuild(&mut list, 3);
        assert_eq!(shown(&list), ["New session", "newer", "Untitled"]);

        // The agent saves it with its title; the live title (none yet)
        // doesn't hide the saved one.
        list.set_saved(
            &here(),
            vec![saved("fresh", Some("named"), 3), saved("old", None, 1)],
        );
        rebuild(&mut list, 3);
        assert_eq!(shown(&list), ["named", "Untitled"]);

        // A live title beats the saved one, and a restart drops the rows
        // the agent never saved.
        list.set_live(live("old", Some("live title"), Activity::Idle), 0, 4);
        list.set_live(live("new", None, Activity::Idle), 0, 4);
        rebuild(&mut list, 4);
        assert_eq!(shown(&list), ["New session", "named", "live title"]);
        list.clear_live(&here());
        rebuild(&mut list, 4);
        assert_eq!(shown(&list), ["named", "live title"]);
    }

    #[test]
    fn busy_sessions_stay_active_and_a_finished_turn_unsettles() {
        let mut list = SessionList::default();
        list.set_saved(&here(), vec![saved("a", Some("a"), 10)]);
        mark(&mut list, "a", SessionMark::Done { at: 20 });
        rebuild(&mut list, 30);
        assert_eq!(list.count(Shelf::Settled), 1);

        let started = Instant::now();
        list.set_live(live("a", None, Activity::Working(started)), 0, 40);
        rebuild(&mut list, 40);
        assert_eq!(list.count(Shelf::Active), 1, "busy shows as active");

        list.set_live(live("a", None, Activity::Idle), 0, 50);
        rebuild(&mut list, 50);
        assert_eq!(list.count(Shelf::Active), 1, "the turn is new activity");
    }

    /// Drafts come first, then pinned sessions, the latest pinned first,
    /// whatever their age; the rest keep newest first.
    #[test]
    fn drafts_then_pinned_sessions_head_the_list() {
        let mut list = SessionList::default();
        list.set_saved(
            &here(),
            vec![
                saved("a", Some("a"), 3),
                saved("b", Some("b"), 2),
                saved("old", Some("old"), 1),
            ],
        );
        mark(&mut list, "old", SessionMark::Pinned { at: 10 });
        mark(&mut list, "b", SessionMark::Pinned { at: 20 });
        rebuild(&mut list, 30);
        assert_eq!(shown(&list), ["b", "old", "a"]);

        mark(&mut list, "b", SessionMark::Kept { at: 30 });
        rebuild(&mut list, 30);
        assert_eq!(shown(&list), ["old", "a", "b"]);

        let draft = DraftRow::new(
            EntityId::from(1_u64),
            Kind::Session,
            Place::new(here(), "/w"),
            Some("half a thought".into()),
            Some(0),
        );
        assert!(list.set_drafts(vec![draft.clone()]));
        assert!(!list.set_drafts(vec![draft]), "unchanged");
        rebuild(&mut list, 30);
        assert_eq!(shown(&list), ["draft half a thought", "old", "a", "b"]);
    }

    /// The active list reads as an inbox: under the pins, a session that
    /// asks comes first, then the Done ones, then those at rest, and the
    /// ones that run by themselves last; newest first within each. A pin
    /// holds its row on top whatever the session does. A session whose
    /// agent watches a pull request runs by itself, Done or not, while it
    /// is active: a settled one's watch isn't kept current.
    #[test]
    fn the_active_list_ranks_sessions_as_an_inbox() {
        let now = Instant::now();
        let names = ["working", "idle", "done", "background", "asking", "pinned"];
        let mut list = SessionList::default();
        list.set_saved(
            &here(),
            names
                .into_iter()
                .zip([6, 5, 4, 3, 2, 1])
                .map(|(name, updated)| saved(name, Some(name), updated))
                .collect(),
        );
        mark(&mut list, "pinned", SessionMark::Pinned { at: 10 });
        for (name, activity) in [
            ("done", Activity::Working(now)),
            ("done", Activity::Idle),
            ("working", Activity::Working(now)),
            ("asking", Activity::Waiting(now)),
            ("background", Activity::Background),
            ("pinned", Activity::Working(now)),
        ] {
            list.set_live(live(name, None, activity), 0, 20);
        }
        rebuild(&mut list, 20);
        assert_eq!(
            shown(&list),
            ["pinned", "asking", "done", "idle", "working", "background"]
        );

        // Its turn over, a session rises to the Done ones, newest first.
        list.set_live(live("working", None, Activity::Idle), 0, 21);
        rebuild(&mut list, 21);
        assert_eq!(
            shown(&list),
            ["pinned", "asking", "working", "done", "idle", "background"]
        );

        // Its agent watches a PR: it will wake by itself, so it isn't Done.
        rebuild_watching(&mut list, 21, &["working"]);
        assert_eq!(
            shown(&list),
            ["pinned", "asking", "done", "idle", "working", "background"]
        );
        // Settled, nothing keeps its watch current: the row doesn't say.
        mark(&mut list, "working", SessionMark::Done { at: 21 });
        rebuild_watching(&mut list, 21, &["working"]);
        assert!(!list.row(&id("working")).expect("the row").watching);
    }

    /// A turn that stops working marks its row Done until the session is
    /// seen or works again, across restarts too; waiting on a person is not
    /// Done, and a turn that leaves background work running waits on it
    /// before Done shows.
    #[test]
    fn a_finished_turn_is_done_until_seen() {
        let done = |list: &mut SessionList| {
            rebuild(list, 0);
            list.row(&id("a")).is_some_and(|row| row.done)
        };
        let db = StateDb::in_memory();
        let mut list = SessionList::new(Unseen::load(&db));
        list.set_saved(&here(), vec![saved("a", Some("a"), 1)]);
        let now = Instant::now();
        list.set_live(live("a", None, Activity::Waiting(now)), 0, 2);
        list.set_live(live("a", None, Activity::Idle), 0, 3);
        assert!(
            !done(&mut list),
            "a request answered no is not a finished turn"
        );

        list.set_live(live("a", None, Activity::Working(now)), 0, 4);
        list.set_live(live("a", None, Activity::Idle), 0, 5);
        assert!(done(&mut list));
        list.set_live(live("a", None, Activity::Working(now)), 0, 6);
        assert!(!done(&mut list), "working again");

        // A turn another client ran, seen through the daemon's jobs.
        list.forget_live(&id("a"));
        list.set_jobs(&here(), vec![job("a", JobActivity::Working, None)], now, 7);
        list.set_jobs(&here(), Vec::new(), now, 8);
        assert!(done(&mut list));
        // The app lists the sessions again when the jobs change.
        list.set_saved(&here(), vec![saved("a", Some("a"), 8)]);
        assert!(done(&mut list), "the daemon's list keeps Done");
        assert!(list.mark_seen(&id("a")));
        assert!(!done(&mut list));

        list.set_live(live("a", None, Activity::Working(now)), 0, 9);
        list.set_live(live("a", None, Activity::Background), 0, 10);
        assert_eq!(
            list.row(&id("a")).map(|row| row.activity),
            Some(Activity::Background)
        );
        list.set_live(live("a", None, Activity::Idle), 0, 11);
        assert!(done(&mut list), "done once the background work ends");

        // A view let go as its turn ends knows more than the last poll,
        // which still had the turn running.
        list.set_live(live("a", None, Activity::Working(now)), 0, 12);
        list.set_jobs(&here(), vec![job("a", JobActivity::Working, None)], now, 12);
        list.set_live(live("a", None, Activity::Idle), 0, 13);
        list.forget_live(&id("a"));
        assert!(done(&mut list), "Done outlives following the session");
        assert_eq!(
            list.row(&id("a")).map(|row| row.activity),
            Some(Activity::Idle)
        );

        // The next run, on the same database, shows it Done until seen.
        let restart = |db: &StateDb| {
            let mut list = SessionList::new(Unseen::load(db));
            list.set_saved(&here(), vec![saved("a", Some("a"), 13)]);
            list
        };
        let mut list = restart(&db);
        assert!(done(&mut list), "Done outlives a restart");
        assert!(list.mark_seen(&id("a")));
        assert!(!done(&mut restart(&db)), "seen stays seen");
    }

    #[test]
    fn shelves_follow_the_active_sessions_and_open_on_click_or_search() {
        let mut list = SessionList::default();
        list.set_saved(
            &here(),
            vec![
                saved("a", Some("Design the UI"), 3),
                saved("b", Some("Fix boot"), 2),
                saved("old", Some("Old work"), 1),
            ],
        );
        mark(&mut list, "a", SessionMark::Done { at: 5 });
        mark(&mut list, "old", SessionMark::Snoozed { until: 100 });
        rebuild(&mut list, 5);
        assert_eq!(
            shown(&list),
            ["Fix boot", "~", "[Snoozed 1]", "[Settled 1]"]
        );

        list.toggle(Shelf::Settled);
        rebuild(&mut list, 5);
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
        mark(&mut list, "a", SessionMark::Kept { at: 5 });
        rebuild(&mut list, 5);
        mark(&mut list, "a", SessionMark::Done { at: 5 });
        rebuild(&mut list, 5);
        assert_eq!(
            shown(&list),
            ["Fix boot", "~", "[Snoozed 1]", "[Settled 1]"]
        );

        list.set_query("work");
        rebuild(&mut list, 5);
        assert_eq!(shown(&list), ["[Snoozed 1]", "Old work"]);
        list.set_query("DESIGN");
        rebuild(&mut list, 5);
        assert_eq!(shown(&list), ["[Settled 1]", "Design the UI"]);
        list.set_query("design ui");
        rebuild(&mut list, 5);
        assert!(list.is_empty(), "words must appear together");
        // Case beyond ASCII, as every list's search matches.
        for (haystack, needle) in [("Café crème", "CRÈME"), ("ÉTÉ", "été")] {
            assert!(
                contains_ignore_case(haystack, needle),
                "{needle:?} in {haystack:?}"
            );
        }
        assert!(!contains_ignore_case("Café", "cafés"), "past the end");
        list.set_query("atl");
        rebuild(&mut list, 5);
        assert_eq!(list.items.len(), 5, "the workspace name matches too");

        // The rail's busy count ignores the search.
        let now = Instant::now();
        list.set_live(live("a", None, Activity::Working(now)), 0, 5);
        list.set_live(live("b", None, Activity::Waiting(now)), 0, 5);
        list.set_query("matches nothing");
        rebuild(&mut list, 5);
        assert!(list.is_empty());
        assert_eq!(
            list.busy(),
            Busy {
                waiting: 1,
                working: 1
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
            mark: None,
            workspace: 1,
            parent: None,
            kind: Kind::Session,
        };
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
        list.rebuild(4, &HashSet::new(), roots);
        assert_eq!(shown(&list), ["here a", "there a", "here b"]);

        // A second answer from one machine replaces only its rows.
        list.set_saved(&there, vec![theirs("a", 5)]);
        list.rebuild(6, &HashSet::new(), roots);
        assert_eq!(shown(&list), ["there a", "here a", "here b"]);

        let mut filter = Filter::default();
        filter.toggle_machine(&there);
        list.set_filter(filter.clone());
        list.rebuild(6, &HashSet::new(), roots);
        assert_eq!(shown(&list), ["here a", "here b"]);

        filter.toggle_machine(&there);
        filter.toggle_project(&Place::new(there.clone(), "/srv"));
        list.set_filter(filter.clone());
        list.rebuild(6, &HashSet::new(), roots);
        assert_eq!(shown(&list), ["there a"]);

        // What the menu no longer offers stops filtering, so nothing stays
        // hidden with no item left to undo it.
        let srv = Place::new(there.clone(), "/srv");
        assert!(!filter.keep_projects(|root| *root == srv));
        assert!(
            filter.keep_projects(|_| false),
            "the chosen project was removed"
        );
        assert!(filter.is_empty());
        filter.toggle_machine(&there);
        filter.toggle_project(&Place::new(there.clone(), "/srv"));
        assert!(filter.forget_machine(&there));
        assert!(filter.is_empty(), "the removed machine's project went too");
    }
}
