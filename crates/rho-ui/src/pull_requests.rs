//! Pull requests: which one a session's branch has, and what it says, read
//! through a forge CLI (so far GitHub's `gh`) on some machine. What is
//! watched is read in one batched query per poll ([`FetchPlan`]), with a
//! PR's full detail only when it changed; see [`PullRequests`].

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::Duration,
};

use web_time::Instant;

use gpui_kit::{BackgroundExecutor, Context, Entity, EntityId, SharedString, Task};

use rho_ui_core::SessionId;

pub(crate) use self::model::*;

use crate::{
    folders::Reader,
    forge::{self, AuthState, Check, CliProbe, ForgeCli, Forges},
    machines::{MachineId, SessionKey},
    repo::Head,
    store::StateDb,
    time,
    workspaces::{Workspace, Workspaces},
};

pub(crate) mod model;

/// How often the PR on screen is read again: often enough to follow a CI
/// run (the last one here took 21 min 40 s, so about 44 polls), at one
/// GraphQL point a poll, 120 an hour of GitHub's 5,000.
const POLL: Duration = Duration::from_secs(30);

/// How often the sidebar's PRs are read again while the window has the
/// focus: one query per host and reader, at one point each, so 12 points
/// an hour. A row only needs a PR's state and its checks' rollup, which
/// the surface follows every [`POLL`] for the session on screen.
const INDEX_POLL: Duration = Duration::from_secs(300);

/// How much of a host's GraphQL budget to leave for everything else that
/// spends it, such as the agent's own `gh` calls: at or below this,
/// polling waits for the budget to reset, and says so. A poll costs one
/// point (measured), so this holds back 25 minutes of polling.
pub(crate) const BUDGET_FLOOR: u32 = 50;

/// The PRs linked by hand to each session, in the order linked, each shown
/// in a Pull request tab of its own beside the branch's; kept in the state
/// database's `pr_links` table. A global.
#[derive(Debug)]
pub(crate) struct PrLinks {
    links: HashMap<SessionKey, Vec<PrId>>,
    db: StateDb,
}

impl gpui_kit::Global for PrLinks {}

impl PrLinks {
    /// Loads every saved link.
    pub(crate) fn load(db: &StateDb) -> Self {
        let rows = db.query(
            "SELECT machine, session_id, host, owner, name, number FROM pr_links ORDER BY rowid",
            [],
            |row| {
                let machine = MachineId::from(row.get::<_, String>(0)?.as_str());
                let session = SessionId::from(row.get::<_, String>(1)?.as_str());
                let repo = RepoId {
                    host: row.get(2)?,
                    owner: row.get(3)?,
                    name: row.get(4)?,
                };
                let id = PrId {
                    repo,
                    number: row.get(5)?,
                };
                Ok((SessionKey::new(machine, session), id))
            },
        );
        let mut links: HashMap<SessionKey, Vec<PrId>> = HashMap::new();
        for (session, id) in rows {
            links.entry(session).or_default().push(id);
        }
        Self {
            links,
            db: db.clone(),
        }
    }

    /// The PRs linked to `session`, in the order linked.
    pub(crate) fn get(&self, session: &SessionKey) -> &[PrId] {
        self.links.get(session).map_or(&[], Vec::as_slice)
    }

    /// Links `id` to `session`, after those linked before; a PR linked
    /// already stays where it is.
    pub(crate) fn add(&mut self, session: &SessionKey, id: PrId) {
        let linked = self.links.entry(session.clone()).or_default();
        if linked.contains(&id) {
            return;
        }
        self.db.execute(
            "INSERT OR IGNORE INTO pr_links (machine, session_id, host, owner, name, number) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            Self::row(session, &id),
        );
        linked.push(id);
    }

    /// Unlinks `id` from `session`.
    pub(crate) fn remove(&mut self, session: &SessionKey, id: &PrId) {
        self.db.execute(
            "DELETE FROM pr_links WHERE machine = ?1 AND session_id = ?2 \
             AND host = ?3 AND owner = ?4 AND name = ?5 AND number = ?6",
            Self::row(session, id),
        );
        if let Some(linked) = self.links.get_mut(session) {
            linked.retain(|linked| linked != id);
            if linked.is_empty() {
                self.links.remove(session);
            }
        }
    }

    /// The `pr_links` row of `id` linked to `session`, in its columns'
    /// order.
    fn row<'a>(
        session: &'a SessionKey,
        id: &'a PrId,
    ) -> (&'a str, &'a str, &'a str, &'a str, &'a str, u32) {
        let repo = &id.repo;
        (
            session.machine.as_str(),
            session.id.as_str(),
            &repo.host,
            &repo.owner,
            &repo.name,
            id.number,
        )
    }
}

/// What the Pull request surface can say of a branch.
#[derive(Debug)]
pub(crate) enum Lookup<'a> {
    /// Not asked yet, or being asked.
    Looking,
    /// The branch is the repository's default, which PRs merge into.
    DefaultBranch,
    Found(&'a Found),
}

/// Why what [`Lookup`] says may be out of date, or missing.
#[derive(Debug)]
pub(crate) enum Trouble<'a> {
    /// No machine can ask the forge, and why.
    NoReader(&'a SharedString),
    /// Polling waits for the host's API budget to reset.
    Paused(&'a Budget),
    /// The last poll failed, and why.
    Failed(&'a SharedString),
}

/// What [`PullRequests::refresh`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refresh {
    /// What is watched, and a PR's detail if it changed.
    Changed,
    /// The PR's detail too, changed or not, as the user asked, from a
    /// fresh check of the CLIs that couldn't read it.
    Everything,
}

/// Whether the window has the focus: polls wait while it doesn't.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Focused,
    Unfocused,
}

impl Focus {
    /// Whether `window` has the focus now.
    pub(crate) fn of(window: &gpui_kit::Window) -> Self {
        if window.is_window_active() {
            Focus::Focused
        } else {
            Focus::Unfocused
        }
    }
}

/// Whether an answer moved a repository to the session's own machine,
/// which should be asked next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ingested {
    Done,
    AskTheSession,
}

/// Whether a polling loop's read runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run {
    Idle,
    Running,
    /// Running, and something asked for another read after it.
    RunningThenAgain,
}

/// One polling loop's timing: whether a read runs, what the last one was
/// for and when it started, and the task that runs it or waits for the
/// next.
struct Cadence<K> {
    run: Run,
    /// On the executor's clock.
    last: Option<(K, Instant)>,
    task: Task<()>,
}

impl<K: PartialEq> Cadence<K> {
    fn new() -> Self {
        Self {
            run: Run::Idle,
            last: None,
            task: Task::ready(()),
        }
    }

    fn is_running(&self) -> bool {
        self.run != Run::Idle
    }

    /// Whether the last read, or the one under way, was for `key`.
    fn was_for(&self, key: &K) -> bool {
        self.last.as_ref().is_some_and(|(last, _)| last == key)
    }

    /// Asks for another read once the one under way ends; whether one is
    /// under way.
    fn again(&mut self) -> bool {
        if self.run == Run::Running {
            self.run = Run::RunningThenAgain;
        }
        self.is_running()
    }

    /// Notes a read for `key` starting `now`.
    fn start(&mut self, key: K, now: Instant) {
        self.run = Run::Running;
        self.last = Some((key, now));
    }

    /// Ends the read under way: whether another was asked for.
    fn finish(&mut self) -> bool {
        let again = self.run == Run::RunningThenAgain;
        self.run = Run::Idle;
        again
    }

    /// How long until a read for `key` is due, `period` after the last
    /// one for it; `None` when it is due now.
    fn due_in(&self, key: &K, period: Duration, now: Instant) -> Option<Duration> {
        let (_, at) = self.last.as_ref().filter(|_| self.was_for(key))?;
        (*at + period)
            .checked_duration_since(now)
            .filter(|wait| !wait.is_zero())
    }
}

/// Runs `then` after `wait`, unless the task is dropped first.
fn after(
    wait: Duration,
    then: fn(&mut PullRequests, &mut Context<'_, PullRequests>),
    cx: &mut Context<'_, PullRequests>,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(wait).await;
        let _ = this.update(cx, then);
    })
}

/// Why the PR on screen can't be polled yet.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Blocked {
    /// Waiting for a check of a machine's CLI, which picks the reader.
    WaitingForCheck,
    /// No machine can ask the forge, and why; a check that ends may
    /// change that.
    NoReader(SharedString),
}

/// Why a session has no [`Target`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoTarget {
    /// Its folder isn't in a git repository.
    NotGit,
    /// Its checkout is on no branch, but this commit.
    Detached(String),
    /// Its repository has no origin remote on a forge.
    NoForge,
}

impl Target {
    /// The PR of the branch `workspace` is on.
    ///
    /// # Errors
    ///
    /// When it is on none, or its repository is on no forge: why.
    pub(crate) fn branch(workspace: &Workspace) -> Result<Self, NoTarget> {
        let branch = match workspace.head() {
            None => return Err(NoTarget::NotGit),
            Some(Head::Detached(hash)) => return Err(NoTarget::Detached(hash.clone())),
            Some(Head::Branch(branch)) => branch.clone(),
        };
        let repo = workspace
            .origin()
            .and_then(RepoId::from_url)
            .ok_or(NoTarget::NoForge)?;
        Ok(Target::Branch(BranchId { repo, branch }))
    }

    /// What a session's sidebar row marks: its [branch's](Self::branch)
    /// PR, else the first PR linked to it in `links`.
    ///
    /// # Errors
    ///
    /// When it has neither: why it has no branch's.
    pub(crate) fn of(
        session: &SessionKey,
        workspace: &Workspace,
        links: &PrLinks,
    ) -> Result<Self, NoTarget> {
        Self::branch(workspace).or_else(|why| {
            let first = links.get(session).first().ok_or(why)?;
            Ok(Target::Pr(first.clone()))
        })
    }
}

/// What the Pull request surface follows, and the machine of the session
/// it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Watch {
    pub(crate) machine: MachineId,
    pub(crate) target: Target,
}

/// One query: what it asks of a host, the machine whose CLI runs it, and
/// the machine of the sessions it is for.
struct Read {
    /// A repository this machine's account can't see moves to the
    /// sessions' machine.
    session_machine: MachineId,
    host: String,
    machine: MachineId,
    program: PathBuf,
    plan: FetchPlan,
}

impl Read {
    /// Asks it, run on its machine by `runner`.
    async fn run(
        &self,
        runner: Reader,
        executor: &BackgroundExecutor,
    ) -> Result<Batch, forge::QueryError> {
        let program = self.program.clone();
        forge::query_prs(runner, program, &self.host, &self.plan, executor).await
    }
}

/// The machine, CLI and account that comment on a PR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Poster {
    pub(crate) machine: MachineId,
    program: PathBuf,
    pub(crate) login: SharedString,
}

/// Which machine runs the CLI for a watch.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ReaderChoice {
    Use {
        machine: MachineId,
        program: PathBuf,
    },
    /// A check of a machine's CLI is under way, or not started.
    Wait,
    None(SharedString),
}

/// The PRs of what is watched, read through the forge CLIs [`Forges`]
/// found. One poll at a time: a request while one runs is folded into one
/// more after it.
pub(crate) struct PullRequests {
    forges: Entity<Forges>,
    workspaces: Entity<Workspaces>,
    on_screen: Option<Watch>,
    /// The surface that asked for `on_screen`: two sessions may watch the
    /// same target, and only the one that asked last can stop it.
    shown_by: Option<EntityId>,
    found: HashMap<Target, Found>,
    details: HashMap<PrId, PrDetail>,
    default_branches: HashMap<RepoId, Option<String>>,
    /// Why the last poll of a target failed, until one answers.
    failed: HashMap<Target, SharedString>,
    /// Repositories this machine's account can't see, read from the
    /// session's machine instead until Refresh.
    hidden: HashSet<RepoId>,
    budgets: HashMap<String, Budget>,
    focus: Focus,
    /// What the next poll reads of the PR on screen.
    next: Refresh,
    /// The PR on screen's polling, keyed by its target.
    poll: Cadence<Target>,
    blocked: Option<Blocked>,
    /// The sidebar's sessions, read every [`INDEX_POLL`].
    listed: Vec<Watch>,
    index: Cadence<()>,
    _forges_changed: gpui_kit::Subscription,
}

impl PullRequests {
    pub(crate) fn new(
        forges: Entity<Forges>,
        workspaces: Entity<Workspaces>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        // A machine's CLI check ending may give a waiting poll its reader.
        let forges_changed = cx.observe(&forges, |this, _, cx| {
            if this.blocked.is_some() {
                this.poll_now(cx);
            }
            if this.unread_listed() {
                this.poll_index(cx);
            }
        });
        Self {
            forges,
            workspaces,
            on_screen: None,
            shown_by: None,
            found: HashMap::new(),
            details: HashMap::new(),
            default_branches: HashMap::new(),
            failed: HashMap::new(),
            hidden: HashSet::new(),
            budgets: HashMap::new(),
            focus: Focus::Focused,
            next: Refresh::Changed,
            poll: Cadence::new(),
            blocked: None,
            listed: Vec::new(),
            index: Cadence::new(),
            _forges_changed: forges_changed,
        }
    }

    /// What is known of `target`'s PR.
    pub(crate) fn lookup(&self, target: &Target) -> Lookup<'_> {
        if let Target::Branch(branch) = target {
            if self.is_default(branch) {
                return Lookup::DefaultBranch;
            }
        }
        self.found
            .get(target)
            .map_or(Lookup::Looking, Lookup::Found)
    }

    /// The PR found for `target`, if one was.
    pub(crate) fn pr(&self, target: &Target) -> Option<&PrSummary> {
        match self.lookup(target) {
            Lookup::Found(Found::Pr(summary)) => Some(summary),
            _ => None,
        }
    }

    /// The detail of PR `id`, once read; it may be older than its summary.
    pub(crate) fn detail(&self, id: &PrId) -> Option<&PrDetail> {
        self.details.get(id)
    }

    /// Why what is known of `target` may be out of date, if it may.
    pub(crate) fn trouble(&self, target: &Target) -> Option<Trouble<'_>> {
        let on_screen = self
            .on_screen
            .as_ref()
            .is_some_and(|watch| watch.target == *target);
        if let (Some(Blocked::NoReader(why)), true) = (&self.blocked, on_screen) {
            return Some(Trouble::NoReader(why));
        }
        if let Some(budget) = self.paused(&target.repo().host) {
            return Some(Trouble::Paused(budget));
        }
        self.failed.get(target).map(Trouble::Failed)
    }

    fn is_default(&self, branch: &BranchId) -> bool {
        self.default_branches
            .get(&branch.repo)
            .and_then(Option::as_deref)
            == Some(branch.branch.as_str())
    }

    /// The host's budget, while polling waits for it to reset.
    fn paused(&self, host: &str) -> Option<&Budget> {
        self.budgets.get(host).filter(|budget| {
            budget.remaining <= BUDGET_FLOOR && budget.resets_at > time::now_unix()
        })
    }

    /// Watches `watch` for `surface`, the Pull request surface on screen,
    /// in place of what another watched. A target not read in the last
    /// [`POLL`] is read at once.
    pub(crate) fn show(&mut self, surface: EntityId, watch: Watch, cx: &mut Context<'_, Self>) {
        self.shown_by = Some(surface);
        if self.on_screen.as_ref() == Some(&watch) {
            return;
        }
        self.blocked = None;
        if self.poll.is_running() && !self.poll.was_for(&watch.target) {
            // One for this follows the one under way for another.
            self.poll.again();
        }
        self.on_screen = Some(watch);
        self.poll_when_due(cx);
        cx.notify();
    }

    /// Stops watching for `surface`, as it left the screen. Another surface
    /// may show already, as tabs switch: what it watches stays.
    pub(crate) fn hide(&mut self, surface: EntityId, cx: &mut Context<'_, Self>) {
        if self.shown_by != Some(surface) {
            return;
        }
        self.shown_by = None;
        self.on_screen = None;
        self.blocked = None;
        if !self.poll.is_running() {
            // Nothing to poll for: a timer waiting for the next poll goes.
            self.poll.task = Task::ready(());
        }
        cx.notify();
    }

    /// Reads what is watched now, as when the session's turn ended or on
    /// Refresh.
    pub(crate) fn refresh(&mut self, refresh: Refresh, cx: &mut Context<'_, Self>) {
        if refresh == Refresh::Everything {
            self.next = Refresh::Everything;
            if let Some(watch) = self.on_screen.clone() {
                // This machine's account may see it now.
                self.hidden.remove(watch.target.repo());
                if let Some(Blocked::NoReader(_)) = self.blocked {
                    // A CLI installed or signed in since its check.
                    self.blocked = None;
                    for machine in [MachineId::local(), watch.machine] {
                        self.forges
                            .update(cx, |forges, cx| forges.recheck(&machine, cx));
                    }
                }
            }
        }
        self.poll_now(cx);
    }

    /// The window gained the focus or lost it: polls wait while it is
    /// unfocused, and one runs when it comes back, if one is due.
    pub(crate) fn set_focus(&mut self, focus: Focus, cx: &mut Context<'_, Self>) {
        if self.focus == focus {
            return;
        }
        self.focus = focus;
        if focus == Focus::Focused {
            self.poll_when_due(cx);
            self.poll_index_when_due(cx);
        }
    }

    /// Polls now if the target on screen wasn't read in the last [`POLL`],
    /// else once it is due.
    fn poll_when_due(&mut self, cx: &mut Context<'_, Self>) {
        let Some(watch) = &self.on_screen else {
            return;
        };
        if self.poll.is_running() {
            // It schedules the next when it ends.
            return;
        }
        let now = cx.background_executor().now();
        match self.poll.due_in(&watch.target, POLL, now) {
            Some(wait) => self.poll.task = after(wait, Self::poll_now, cx),
            None => self.poll_now(cx),
        }
    }

    /// Polls now, or once the poll under way ends.
    fn poll_now(&mut self, cx: &mut Context<'_, Self>) {
        if self.poll.again() {
            return;
        }
        let Some(watch) = self.on_screen.clone() else {
            return;
        };
        if self.focus == Focus::Unfocused {
            return;
        }
        let choice = self.reader_for(&watch, cx);
        let blocked = match &choice {
            // The check's end polls again.
            ReaderChoice::Wait => Some(Blocked::WaitingForCheck),
            ReaderChoice::None(why) => Some(Blocked::NoReader(why.clone())),
            ReaderChoice::Use { .. } => None,
        };
        if blocked != self.blocked {
            self.blocked = blocked;
            cx.notify();
        }
        let ReaderChoice::Use { machine, program } = choice else {
            return;
        };
        let host = watch.target.repo().host.clone();
        if let Some(budget) = self.paused(&host) {
            let wait = budget.resets_at - time::now_unix();
            let wait = Duration::from_secs(wait.unsigned_abs());
            self.poll.task = after(wait, Self::poll_now, cx);
            return;
        }
        let plan = self.plan(&watch);
        if plan.is_empty() {
            self.poll.task = after(POLL, Self::poll_now, cx);
            return;
        }
        let runner = self.workspaces.read(cx).reader(&machine);
        let read = Read {
            session_machine: watch.machine.clone(),
            host,
            machine,
            program,
            plan,
        };
        let executor = cx.background_executor().clone();
        self.next = Refresh::Changed;
        self.poll.start(watch.target.clone(), executor.now());
        self.poll.task = cx.spawn(async move |this, cx| {
            let answer = read.run(runner, &executor).await;
            let _ = this.update(cx, |this, cx| {
                let ingested = this.ingest(&read, answer, cx);
                if ingested == Ingested::AskTheSession || this.detail_owed(&watch, &read.plan) {
                    this.poll.again();
                }
                if this.poll.finish() {
                    this.poll_now(cx);
                } else {
                    this.poll.task = after(POLL, Self::poll_now, cx);
                }
            });
        });
    }

    /// What the next poll asks: the branch on screen, unless it is its
    /// repository's default, or the PR linked; and the PR's detail when it
    /// changed since the last read, while its checks run, or on Refresh.
    fn plan(&self, watch: &Watch) -> FetchPlan {
        let mut plan = match &watch.target {
            Target::Branch(branch) if self.is_default(branch) => return FetchPlan::default(),
            Target::Branch(branch) => FetchPlan {
                branches: vec![branch.clone()],
                ..FetchPlan::default()
            },
            Target::Pr(id) => FetchPlan {
                links: vec![id.clone()],
                ..FetchPlan::default()
            },
        };
        if let Some(Found::Pr(summary)) = self.found.get(&watch.target) {
            let read = self.details.get(&summary.id);
            let stale = self.detail_stale(summary);
            // The rollup reads failed once one check failed, while others
            // may still run.
            let running = summary.change.rollup == Rollup::Pending
                || read.is_some_and(|detail| {
                    detail.checks.iter().any(|check| {
                        matches!(check.status, CheckStatus::Running | CheckStatus::Pending)
                    })
                });
            if stale || running || self.next == Refresh::Everything {
                plan.details.push(summary.id.clone());
            }
        }
        plan
    }

    /// Whether PR `summary` has no detail read, or changed since it was.
    fn detail_stale(&self, summary: &PrSummary) -> bool {
        self.details
            .get(&summary.id)
            .is_none_or(|detail| detail.summary.change != summary.change)
    }

    /// Whether the PR on screen is [stale](Self::detail_stale) and `plan`
    /// didn't ask for it: so it is read at once, not in a loop when the
    /// answer just failed to read it.
    fn detail_owed(&self, watch: &Watch, plan: &FetchPlan) -> bool {
        let Some(Found::Pr(summary)) = self.found.get(&watch.target) else {
            return false;
        };
        self.detail_stale(summary) && !plan.details.contains(&summary.id)
    }

    /// Takes the answer to `read`, which may move a repository to its
    /// sessions' machine.
    fn ingest(
        &mut self,
        read: &Read,
        answer: Result<Batch, forge::QueryError>,
        cx: &mut Context<'_, Self>,
    ) -> Ingested {
        let plan = &read.plan;
        let mut ingested = Ingested::Done;
        let batch = match answer {
            Ok(batch) => batch,
            Err(error) => {
                if matches!(error, forge::QueryError::SignedOut(_)) {
                    let machine = read.machine.clone();
                    self.forges
                        .update(cx, |forges, cx| forges.recheck(&machine, cx));
                }
                // What was read before stays on screen, with why it may be
                // out of date.
                let why = SharedString::from(error.to_string());
                for target in plan.targets() {
                    self.failed.insert(target, why.clone());
                }
                cx.notify();
                return ingested;
            }
        };
        if let Some(budget) = batch.budget {
            self.budgets.insert(read.host.clone(), budget);
        }
        self.default_branches.extend(batch.default_branches);
        for (repo, why) in batch.missing {
            let read_here = read.machine.is_local() && !read.session_machine.is_local();
            if read_here && self.hidden.insert(repo.clone()) {
                // The session's own machine may see it: ask it next.
                ingested = Ingested::AskTheSession;
                continue;
            }
            for target in plan.targets().filter(|target| *target.repo() == repo) {
                self.failed.insert(target, why.clone().into());
            }
        }
        for (id, detail) in batch.details {
            match detail {
                Some(detail) => self.details.insert(id, detail),
                None => self.details.remove(&id),
            };
        }
        let branches = batch.branches.into_iter();
        let links = batch.links.into_iter();
        let found = branches
            .map(|(branch, found)| (Target::Branch(branch), found))
            .chain(links.map(|(id, found)| (Target::Pr(id), found)));
        for (target, found) in found {
            self.failed.remove(&target);
            self.found.insert(target, found);
        }
        cx.notify();
        ingested
    }

    /// Watches `listed` for the sidebar: its sessions, each on its machine.
    /// One not read yet is read at once.
    pub(crate) fn set_listed(&mut self, listed: Vec<Watch>, cx: &mut Context<'_, Self>) {
        if self.listed == listed {
            return;
        }
        self.listed = listed;
        if self.unread_listed() {
            self.poll_index(cx);
        }
    }

    /// Whether a listed session's PR was never asked for: neither found,
    /// nor failed (that waits for the next poll), nor a default branch's.
    fn unread_listed(&self) -> bool {
        self.listed.iter().any(|watch| {
            matches!(self.lookup(&watch.target), Lookup::Looking)
                && !self.failed.contains_key(&watch.target)
        })
    }

    /// Reads the listed sessions' PRs now, one query per host and reader,
    /// each in turn; or once the reads under way end.
    fn poll_index(&mut self, cx: &mut Context<'_, Self>) {
        if self.index.again() || self.focus == Focus::Unfocused {
            return;
        }
        let reads = self.index_reads(cx);
        if reads.is_empty() {
            self.index.task = after(INDEX_POLL, Self::poll_index, cx);
            return;
        }
        let workspaces = self.workspaces.read(cx);
        let reads: Vec<(Read, Reader)> = reads
            .into_iter()
            .map(|read| {
                let runner = workspaces.reader(&read.machine);
                (read, runner)
            })
            .collect();
        let executor = cx.background_executor().clone();
        self.index.start((), executor.now());
        self.index.task = cx.spawn(async move |this, cx| {
            let mut again = false;
            for (read, runner) in reads {
                let answer = read.run(runner, &executor).await;
                let ingested = this.update(cx, |this, cx| this.ingest(&read, answer, cx));
                again |= matches!(ingested, Ok(Ingested::AskTheSession));
            }
            let _ = this.update(cx, |this, cx| {
                if this.index.finish() || again {
                    this.poll_index(cx);
                } else {
                    this.index.task = after(INDEX_POLL, Self::poll_index, cx);
                }
            });
        });
    }

    /// Reads the listed sessions when [`INDEX_POLL`] has passed since the
    /// last read, as the window gains the focus.
    fn poll_index_when_due(&mut self, cx: &mut Context<'_, Self>) {
        if self.index.is_running() {
            // It schedules the next when it ends.
            return;
        }
        let now = cx.background_executor().now();
        match self.index.due_in(&(), INDEX_POLL, now) {
            Some(wait) => self.index.task = after(wait, Self::poll_index, cx),
            None => self.poll_index(cx),
        }
    }

    /// The listed sessions' lookups, one read per session machine, reader
    /// and host: but not the session on screen, which its own poll reads,
    /// nor a default branch, nor a host whose budget is spent, nor one
    /// whose reader isn't known yet (its check's end asks again).
    fn index_reads(&mut self, cx: &mut Context<'_, Self>) -> Vec<Read> {
        let mut reads: Vec<Read> = Vec::new();
        for watch in self.listed.clone() {
            let skip = match &watch.target {
                Target::Branch(branch) => self.is_default(branch),
                Target::Pr(_) => false,
            };
            if skip || self.on_screen.as_ref() == Some(&watch) {
                continue;
            }
            let ReaderChoice::Use { machine, program } = self.reader_for(&watch, cx) else {
                continue;
            };
            let host = &watch.target.repo().host;
            if self.paused(host).is_some() {
                continue;
            }
            let ix = reads.iter().position(|read| {
                read.session_machine == watch.machine
                    && read.machine == machine
                    && read.host == *host
            });
            let ix = ix.unwrap_or_else(|| {
                reads.push(Read {
                    session_machine: watch.machine.clone(),
                    host: host.clone(),
                    machine,
                    program,
                    plan: FetchPlan::default(),
                });
                reads.len() - 1
            });
            let plan = &mut reads[ix].plan;
            match watch.target {
                Target::Branch(branch) => plan.branches.push(branch),
                Target::Pr(id) => plan.links.push(id),
            }
        }
        reads
    }

    /// Who comments on the PR on screen: the session machine's CLI, with
    /// its account there, as the session's own work goes; or why no one
    /// can.
    pub(crate) fn poster(&self, cx: &gpui_kit::App) -> Result<Poster, SharedString> {
        let watch = self
            .on_screen
            .as_ref()
            .ok_or_else(|| SharedString::from("No pull request is on screen."))?;
        let host = watch.target.repo().host.as_str();
        let forges = self.forges.read(cx);
        let check = |machine: &MachineId| forges.check(machine, ForgeCli::Gh);
        match choose(&[&watch.machine], host, check) {
            ReaderChoice::Use { machine, program } => {
                let login = match check(&machine) {
                    Some(Check::Done(CliProbe::Found(status))) => status
                        .accounts
                        .iter()
                        .find(|account| account.active && account.host == host)
                        .map(|account| account.login.clone()),
                    _ => None,
                };
                Ok(Poster {
                    machine,
                    program,
                    login: login.unwrap_or_default(),
                })
            }
            ReaderChoice::Wait => Err("Checking gh…".into()),
            ReaderChoice::None(why) => Err(why),
        }
    }

    /// Does `action` to PR `id` as [`Self::poster`], then reads the PR
    /// again to show what it did.
    pub(crate) fn act(
        &mut self,
        id: PrId,
        action: PrAction,
        cx: &mut Context<'_, Self>,
    ) -> Task<Result<(), SharedString>> {
        let poster = match self.poster(cx) {
            Ok(poster) => poster,
            Err(why) => return Task::ready(Err(why)),
        };
        let reader = self.workspaces.read(cx).reader(&poster.machine);
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let result = forge::act(reader, poster.program, &id, &action, &executor).await;
            if result.is_ok() {
                let _ = this.update(cx, |this, cx| this.refresh(Refresh::Everything, cx));
            }
            result.map_err(SharedString::from)
        })
    }

    /// [`Self::choose_reader`], once the checks it needs are asked for.
    fn reader_for(&mut self, watch: &Watch, cx: &mut Context<'_, Self>) -> ReaderChoice {
        for machine in [MachineId::local(), watch.machine.clone()] {
            self.forges
                .update(cx, |forges, cx| forges.ensure_checked(&machine, cx));
        }
        self.choose_reader(watch, cx)
    }

    /// Which machine asks the forge for `watch`: this one when its CLI is
    /// signed in to the host, which needs no ssh; else the session's own.
    fn choose_reader(&self, watch: &Watch, cx: &gpui_kit::App) -> ReaderChoice {
        let forges = self.forges.read(cx);
        let repo = watch.target.repo();
        let local = MachineId::local();
        let mut candidates = vec![];
        if !self.hidden.contains(repo) || watch.machine.is_local() {
            candidates.push(&local);
        }
        if !watch.machine.is_local() {
            candidates.push(&watch.machine);
        }
        choose(&candidates, &repo.host, |machine| {
            forges.check(machine, ForgeCli::Gh)
        })
    }
}

/// The first of `machines` whose CLI is signed in to `host` (or may be),
/// waiting while
/// one before it is still being checked; with none, why not, as the last
/// machine's check says.
fn choose<'a>(
    machines: &[&MachineId],
    host: &str,
    check: impl Fn(&MachineId) -> Option<&'a Check>,
) -> ReaderChoice {
    let mut why = SharedString::from("No machine can ask GitHub.");
    for &machine in machines {
        let on = if machine.is_local() {
            "this machine".to_owned()
        } else {
            machine.to_string()
        };
        match check(machine) {
            None | Some(Check::Checking) => return ReaderChoice::Wait,
            Some(Check::Failed(failed)) => {
                why = format!("Couldn't check gh on {on}: {failed}").into();
            }
            Some(Check::Done(CliProbe::Missing)) => {
                why = format!("gh isn't installed on {on}. See Settings → Source control.").into();
            }
            Some(Check::Done(CliProbe::Broken { why: broken, .. })) => {
                why = format!("gh doesn't run on {on}: {broken}").into();
            }
            Some(Check::Done(CliProbe::Found(status))) => {
                // One that couldn't reach the forge when checked may be
                // signed in: its query says if it isn't.
                let signed_in = status.accounts.iter().any(|account| {
                    account.active
                        && account.host == host
                        && !matches!(account.auth, AuthState::Refused(_))
                });
                if signed_in {
                    return ReaderChoice::Use {
                        machine: machine.clone(),
                        program: status.path.clone(),
                    };
                }
                why =
                    format!("gh on {on} isn't signed in to {host}. See Settings → Source control.")
                        .into();
            }
        }
    }
    ReaderChoice::None(why)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{Account, CliStatus, FoundBy};

    /// The reader is this machine when its gh is signed in to the host,
    /// else the session's machine; it waits on a check under way, and says
    /// why when no machine can read.
    #[test]
    fn the_reader_prefers_this_machine() {
        let signed_in = |host: &str| {
            Check::Done(CliProbe::Found(CliStatus {
                path: PathBuf::from("/bin/gh"),
                found_by: FoundBy::Path,
                version: "2.99.0".into(),
                accounts: vec![Account {
                    host: host.to_owned().into(),
                    login: "octocat".into(),
                    active: true,
                    auth: AuthState::SignedIn,
                    git_protocol: None,
                    token_source: None,
                    scopes: None,
                }],
            }))
        };
        let local = MachineId::local();
        let remote = MachineId::remote("box");
        let use_ = |machine: &MachineId| ReaderChoice::Use {
            machine: machine.clone(),
            program: PathBuf::from("/bin/gh"),
        };
        let both = [&local, &remote];
        let reader = |checks: [Option<Check>; 2]| {
            choose(&both, "github.com", |machine| {
                if machine.is_local() {
                    checks[0].as_ref()
                } else {
                    checks[1].as_ref()
                }
            })
        };
        assert_eq!(reader([Some(signed_in("github.com")), None]), use_(&local));
        let with_auth = |host: &str, auth: AuthState| match signed_in(host) {
            Check::Done(CliProbe::Found(mut status)) => {
                status.accounts[0].auth = auth;
                Check::Done(CliProbe::Found(status))
            }
            _ => unreachable!(),
        };
        assert_eq!(
            reader([
                Some(with_auth(
                    "github.com",
                    AuthState::Unreachable("offline".into())
                )),
                Some(signed_in("github.com"))
            ]),
            use_(&local),
            "offline when checked, which says nothing of its token"
        );
        assert_eq!(
            reader([
                Some(with_auth("github.com", AuthState::Refused("401".into()))),
                Some(signed_in("github.com"))
            ]),
            use_(&remote),
            "its token refused"
        );
        assert_eq!(
            reader([Some(signed_in("ghe.corp")), Some(signed_in("github.com"))]),
            use_(&remote),
            "this machine signed in to another host"
        );
        assert_eq!(
            reader([Some(Check::Checking), Some(signed_in("github.com"))]),
            ReaderChoice::Wait
        );
        assert!(matches!(
            reader([
                Some(Check::Done(CliProbe::Missing)),
                Some(Check::Done(CliProbe::Missing))
            ]),
            ReaderChoice::None(_)
        ));
    }
}
