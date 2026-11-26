//! Pull requests: each session's, as its machine's rho daemon reads them
//! with its own `gh`. rho-ui only asks: `list` for the sidebar's rows and
//! the title bar, `show` for the PR on the Pull request surface, `link`
//! and `unlink` for the user's links; and each change to a followed
//! session's set arrives as an event ([`PullRequests::replaced`]). What rho-ui does *to* a PR (comment,
//! merge, ready, draft) still runs `gh` on the session's machine
//! ([`PullRequests::act`]); see [`PullRequests`].

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use web_time::Instant;

use gpui_kit::{App, Context, Entity, EntityId, Image, SharedString, Task};

use rho_ui_core::{
    BranchLookup, Daemon, PullRequestDetail, PullRequestError, PullRequestId, PullRequestRead,
    PullRequestSummary, PullRequestTarget, SessionId, SessionPullRequestShowResponse,
    SessionPullRequests,
};

pub(crate) use self::avatars::every_post;
use self::avatars::Avatars;
pub(crate) use self::reading::{Reading, Seen};

use crate::{
    folders::Reader,
    forge::{self, AuthState, Check, CliProbe, ForgeCli, Forges, PrAction},
    machines::{MachineId, SessionKey},
    pictures::{Signing, Site},
    workspaces::Workspaces,
};

mod avatars;
mod reading;

/// How often the PR on screen is shown again: the protocol asks for `show`
/// within 30 s to keep the daemon reading it, and the daemon's lease lasts
/// two of these, 60 s, so a request that takes a second still renews it in
/// time (rho's `crates/rho/src/app/pull_requests/interest.rs`).
const SHOW_EVERY: Duration = Duration::from_secs(30);

/// How often a machine's listed sessions are listed again while the window
/// has the focus. A `list` reads no GitHub: it answers from the daemon's
/// last read and runs `git` twice a session, 3 to 4 ms each, so about
/// 0.2 s of `git` a minute for 30 rows. Nothing pushes a change to a
/// session rho-ui lists but doesn't follow, so this is how far a row
/// trails the daemon's read; it renews the daemon's 5-minute lease five
/// times over.
const LIST_EVERY: Duration = Duration::from_secs(60);

/// How often a machine is listed again instead of [`LIST_EVERY`] while
/// its last answer holds a listed session still looking, with no trouble:
/// a daemon's first read of 100 summaries took 3.0 s, and the marks of a
/// just-started app shouldn't wait a minute. It ends by itself: a read
/// that fails sets a trouble.
const LOOKING_EVERY: Duration = Duration::from_secs(5);

/// The one host whose branches a daemon looks up.
const GITHUB: &str = "github.com";

/// What an answer says when `PullRequests` went away first.
const GONE: &str = "rho-ui is closing.";

/// Which of a session's pull requests the Pull request surface shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Which {
    /// The one of the branch its workspace has checked out.
    Branch,
    /// One linked to it, by the user or the agent.
    Linked(PullRequestId),
}

/// What the Pull request surface on screen shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    pub(crate) session: SessionKey,
    pub(crate) which: Which,
}

/// A session's pull request as its sidebar row and its title bar mark it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mark<'a> {
    /// Its branch's, else the last linked one read.
    pub(crate) pr: &'a PullRequestSummary,
    /// Whether the session's agent watches `pr`.
    pub(crate) watched: bool,
    /// How many more the session links.
    pub(crate) more: usize,
}

/// Whether a machine's daemon serves pull requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Serving {
    /// Not answered since it connected.
    Asking,
    Served,
    /// It is from before pull requests (`MethodNotFound`): nothing more is
    /// asked of it until it connects again.
    Unserved,
}

/// Whether the window has the focus: requests wait while it doesn't, so
/// the daemon's leases lapse and it stops reading.
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

/// Whether a request loop's request runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run {
    Idle,
    Running,
    /// Running, and something asked for another request after it.
    RunningThenAgain,
}

/// One request loop's timing: whether a request runs, what the last one
/// was for and when it started, and the task that runs it or waits for
/// the next.
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

    /// Whether the last request, or the one under way, was for `key`.
    fn was_for(&self, key: &K) -> bool {
        self.last.as_ref().is_some_and(|(last, _)| last == key)
    }

    /// Asks for another request once the one under way ends; whether one
    /// is under way.
    fn again(&mut self) -> bool {
        if self.run == Run::Running {
            self.run = Run::RunningThenAgain;
        }
        self.is_running()
    }

    /// Notes a request for `key` starting `now`.
    fn start(&mut self, key: K, now: Instant) {
        self.run = Run::Running;
        self.last = Some((key, now));
    }

    /// Ends the request under way: whether another was asked for.
    fn finish(&mut self) -> bool {
        let again = self.run == Run::RunningThenAgain;
        self.run = Run::Idle;
        again
    }

    /// Drops the request under way, or the timer waiting for the next:
    /// whether another was asked for after it.
    fn cancel(&mut self) -> bool {
        self.task = Task::ready(());
        self.finish()
    }

    /// How long until a request for `key` is due, `period` after the last
    /// one for it; `None` when it is due now.
    fn due_in(&self, key: &K, period: Duration, now: Instant) -> Option<Duration> {
        let (_, at) = self.last.as_ref().filter(|_| self.was_for(key))?;
        (*at + period)
            .checked_duration_since(now)
            .filter(|wait| !wait.is_zero())
    }
}

/// Says on stderr that the picture at `url` didn't load, and `why`: by
/// its name, as its query may hold a token.
#[expect(clippy::print_stderr, reason = "the desktop app has no log")]
fn report_avatar(url: &str, why: &str) {
    let picture = avatars::name(url);
    eprintln!("rho-ui: the profile picture at {picture} didn't load, so initials show: {why}");
}

/// Runs `then` after `wait`, unless the task is dropped first.
fn after(
    wait: Duration,
    then: impl FnOnce(&mut PullRequests, &mut Context<'_, PullRequests>) + 'static,
    cx: &mut Context<'_, PullRequests>,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(wait).await;
        let _ = this.update(cx, then);
    })
}

/// Whether a request loop's next request is due now, or waits: for a
/// timer, or for the request under way, whose end arms the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Due {
    Now,
    Armed,
}

/// Arms `cadence`'s next request for `key`, `period` after its last one:
/// nothing while one runs, as its end arms the next; else a timer that
/// runs `request` once it is due, or [`Due::Now`], for the caller to send
/// it.
fn arm<K: PartialEq>(
    cadence: &mut Cadence<K>,
    key: &K,
    period: Duration,
    request: impl FnOnce(&mut PullRequests, &mut Context<'_, PullRequests>) + 'static,
    cx: &mut Context<'_, PullRequests>,
) -> Due {
    if cadence.is_running() {
        return Due::Armed;
    }
    match cadence.due_in(key, period, cx.background_executor().now()) {
        Some(wait) => {
            cadence.task = after(wait, request, cx);
            Due::Armed
        }
        None => Due::Now,
    }
}

/// How sentences name `machine`.
pub(crate) fn on(machine: &MachineId) -> String {
    if machine.is_local() {
        "this machine".to_owned()
    } else {
        machine.to_string()
    }
}

/// What is said of a machine whose daemon serves no pull requests.
fn unserved(machine: &MachineId) -> SharedString {
    let on = on(machine);
    format!("Update rho on {on} to see its pull requests here.").into()
}

/// [`PullRequests::which`] of a session whose set is `set`, if known.
fn next_which(set: Option<&SessionPullRequests>, shown: &Which) -> Which {
    let Some(set) = set else {
        return shown.clone();
    };
    let branch = set.branch_pull_request().map(|pr| &pr.id);
    match shown {
        Which::Linked(id) if Some(id) == branch => return Which::Branch,
        Which::Linked(id) if set.link(id).is_some() => return shown.clone(),
        Which::Linked(_) | Which::Branch => {}
    }
    // A lookup that can't finish (gh missing or signed out) has a
    // trouble: it finds nothing.
    let looking = set.branch.as_ref().is_none_or(|branch| {
        matches!(branch.lookup, BranchLookup::Looking) && branch.trouble.is_none()
    });
    if branch.is_some() || looking {
        return Which::Branch;
    }
    let read = set
        .linked
        .iter()
        .rev()
        .find(|linked| linked.summary.is_some());
    read.or(set.linked.last())
        .map_or(Which::Branch, |linked| Which::Linked(linked.id.clone()))
}

/// What is said of a machine that isn't connected.
fn not_connected(machine: &MachineId) -> SharedString {
    let on = on(machine);
    format!("Not connected to {on}.").into()
}

/// Whether the daemon is still looking up something of `set`, with no
/// trouble to say why it can't: it will have an answer in seconds.
fn still_looking(set: &SessionPullRequests) -> bool {
    let branch = set.branch.as_ref().is_some_and(|branch| {
        matches!(branch.lookup, BranchLookup::Looking) && branch.trouble.is_none()
    });
    let linked = set
        .linked
        .iter()
        .any(|linked| linked.summary.is_none() && linked.trouble.is_none());
    branch || linked
}

/// The machine, CLI and account that comment on a PR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Poster {
    pub(crate) machine: MachineId,
    program: PathBuf,
    pub(crate) login: SharedString,
}

/// What a machine's `gh` can do on a host, as its check says.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Gh {
    Use {
        program: PathBuf,
        login: SharedString,
    },
    /// Its check is under way, or not started.
    Checking,
    /// It can't, and why.
    Unusable(SharedString),
}

impl Gh {
    /// What `machine`'s `gh` can do on `host`, by its `check`.
    fn of(machine: &MachineId, host: &str, check: Option<&Check>) -> Self {
        let on = on(machine);
        match check {
            None | Some(Check::Checking) => Gh::Checking,
            Some(Check::Failed(failed)) => {
                Gh::Unusable(format!("Couldn't check gh on {on}: {failed}").into())
            }
            Some(Check::Done(CliProbe::Missing)) => Gh::Unusable(
                format!("gh isn't installed on {on}. See Settings → Source control.").into(),
            ),
            Some(Check::Done(CliProbe::Broken { why, .. })) => {
                Gh::Unusable(format!("gh doesn't run on {on}: {why}").into())
            }
            Some(Check::Done(CliProbe::Found(status))) => {
                // One that couldn't reach the forge when checked may be
                // signed in: acting says if it isn't.
                let account = status.accounts.iter().find(|account| {
                    account.active
                        && account.host == host
                        && !matches!(account.auth, AuthState::Refused(_))
                });
                let Some(account) = account else {
                    return Gh::Unusable(
                        format!(
                            "gh on {on} isn't signed in to {host}. See Settings → Source control."
                        )
                        .into(),
                    );
                };
                Gh::Use {
                    program: status.path.clone(),
                    login: account.login.clone(),
                }
            }
        }
    }
}

/// What one connected machine's daemon said of its sessions' pull
/// requests.
struct MachinePrs {
    /// Which of the machine's connections this is; an answer sent on an
    /// earlier one is dropped.
    connection: u64,
    daemon: Daemon,
    serving: Serving,
    /// Each session's set, replaced whole by every answer and event, never
    /// merged.
    sessions: HashMap<SessionId, SessionPullRequests>,
    /// Each pull request's detail, from the last `show` that carried one:
    /// per machine, as two machines may read one PR as two accounts.
    details: HashMap<PullRequestId, PullRequestDetail>,
    /// Why the daemon itself last failed to answer, until it answers: what
    /// befell the machine, not what it refused of one request, which goes
    /// back to whoever asked.
    failed: Option<SharedString>,
    /// Reissues `list` for this machine's listed sessions.
    list: Cadence<()>,
}

impl MachinePrs {
    /// Replaces `session`'s set with `set`: whether it changed.
    fn store(&mut self, session: &SessionId, set: SessionPullRequests) -> bool {
        if self.sessions.get(session) == Some(&set) {
            return false;
        }
        self.sessions.insert(session.clone(), set);
        true
    }

    /// Takes `answer` to a request to `machine`'s daemon. An answer means
    /// the daemon serves pull requests and whatever failed before is over;
    /// a refusal comes back as people read it, noted first as
    /// [`Self::refused`] says.
    fn take<T>(
        &mut self,
        machine: &MachineId,
        answer: Result<T, PullRequestError>,
    ) -> Result<T, SharedString> {
        match answer {
            Ok(value) => {
                self.serving = Serving::Served;
                self.failed = None;
                Ok(value)
            }
            Err(error) => Err(self.refused(machine, error)),
        }
    }

    /// What `error` says of `machine`, as people read it: a daemon from
    /// before pull requests is [`Serving::Unserved`] from now on, and a
    /// failure of the daemon itself is [`Self::failed`]; what it refused
    /// of the request alone (`Invalid`) is only the caller's.
    fn refused(&mut self, machine: &MachineId, error: PullRequestError) -> SharedString {
        match error {
            PullRequestError::Unserved => {
                self.serving = Serving::Unserved;
                unserved(machine)
            }
            PullRequestError::Invalid(message) => message.into(),
            PullRequestError::Daemon(error) => {
                let why = SharedString::from(error.to_string());
                self.failed = Some(why.clone());
                why
            }
        }
    }

    /// Whether the daemon serves pull requests, and why it last failed: to
    /// tell whether an answer changed them.
    fn health(&self) -> (Serving, Option<SharedString>) {
        (self.serving, self.failed.clone())
    }
}

/// Each session's pull requests, as each machine's daemon serves them.
/// Two request loops keep the daemons reading what is on screen: `list`
/// per machine for the sidebar's rows and the title bar, every
/// [`LIST_EVERY`], and `show` for the Pull request surface, every
/// [`SHOW_EVERY`]. Each runs one request at a time: a request asked for
/// while one is in flight is folded into one more after it. Neither runs
/// while the window is unfocused, for a machine that isn't connected, or
/// for one whose daemon is [`Serving::Unserved`].
pub(crate) struct PullRequests {
    /// The `gh` that acts: [`Self::poster`].
    forges: Entity<Forges>,
    /// How to run `gh` and `curl` on a machine.
    workspaces: Entity<Workspaces>,
    machines: HashMap<MachineId, MachinePrs>,
    connections: u64,
    /// The sidebar's sessions, and the one on screen.
    listed: Vec<SessionKey>,
    shown: Option<Shown>,
    /// The surface that asked for `shown`: two may show the same, and only
    /// the one that asked last can stop it.
    shown_by: Option<EntityId>,
    /// Reissues `show` for what is on screen.
    show: Cadence<Shown>,
    /// What the next `show` asks.
    next: PullRequestRead,
    /// Why the daemon refused the last `show`, and what it showed, until a
    /// `show` is answered.
    show_refused: Option<(Shown, SharedString)>,
    focus: Focus,
    avatars: Avatars,
    _forges_changed: gpui_kit::Subscription,
}

impl PullRequests {
    pub(crate) fn new(
        forges: Entity<Forges>,
        workspaces: Entity<Workspaces>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        // A machine's CLI check ending changes who comments.
        let forges_changed = cx.observe(&forges, |_, _, cx| cx.notify());
        Self {
            forges,
            workspaces,
            machines: HashMap::new(),
            connections: 0,
            listed: Vec::new(),
            shown: None,
            shown_by: None,
            show: Cadence::new(),
            next: PullRequestRead::Cached,
            show_refused: None,
            focus: Focus::Focused,
            avatars: Avatars::default(),
            _forges_changed: forges_changed,
        }
    }

    /// `machine` connected to `daemon`, replacing what an earlier
    /// connection said: its listed sessions are listed, and what is on
    /// screen of it shown, at once.
    pub(crate) fn connected(
        &mut self,
        machine: &MachineId,
        daemon: Daemon,
        cx: &mut Context<'_, Self>,
    ) {
        self.connections += 1;
        self.machines.insert(
            machine.clone(),
            MachinePrs {
                connection: self.connections,
                daemon,
                serving: Serving::Asking,
                sessions: HashMap::new(),
                details: HashMap::new(),
                failed: None,
                list: Cadence::new(),
            },
        );
        cx.notify();
        self.list_now(machine, cx);
        if self
            .shown
            .as_ref()
            .is_some_and(|shown| shown.session.machine == *machine)
        {
            // Who comments on it and fetches its images, checked afresh:
            // the connection dropped what was known.
            self.forges
                .update(cx, |forges, cx| forges.ensure_checked(machine, cx));
            self.show_now(cx);
        }
    }

    /// `machine`'s connection is gone, or the machine is: what it said
    /// goes with it. A `show` still waiting on that connection goes too,
    /// as it would hold up the show of every machine's PRs.
    pub(crate) fn forget(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        if self.machines.remove(machine).is_none() {
            return;
        }
        let waiting = self.show.is_running()
            && self
                .show
                .last
                .as_ref()
                .is_some_and(|(shown, _)| shown.session.machine == *machine);
        // One asked for after it, of another machine's PR, goes now.
        if waiting && self.show.cancel() {
            self.show_now(cx);
        }
        cx.notify();
    }

    /// `session`'s daemon says its set is now `set`. When that moved the PR
    /// on screen past the detail last read of it, it is shown again now.
    pub(crate) fn replaced(
        &mut self,
        session: &SessionKey,
        set: SessionPullRequests,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(entry) = self.machines.get_mut(&session.machine) else {
            return;
        };
        // An event answers no request, so it leaves `failed` alone: only
        // an answer says the daemon is over what kept it from answering.
        if entry.serving == Serving::Asking {
            entry.serving = Serving::Served;
        }
        if !entry.store(&session.id, set) {
            return;
        }
        cx.notify();
        self.show_if_moved(session, cx);
    }

    /// The window gained the focus or lost it: requests wait while it is
    /// unfocused, and whatever is due goes when it comes back.
    pub(crate) fn set_focus(&mut self, focus: Focus, cx: &mut Context<'_, Self>) {
        if self.focus == focus {
            return;
        }
        self.focus = focus;
        if focus == Focus::Focused {
            self.show_when_due(cx);
            let machines: Vec<MachineId> = self.machines.keys().cloned().collect();
            for machine in machines {
                self.list_when_due(&machine, cx);
            }
        }
    }

    /// Lists `listed` for the sidebar: its sessions and the one on screen,
    /// each on its machine. A machine that gained one is listed at once.
    pub(crate) fn set_listed(&mut self, listed: Vec<SessionKey>, cx: &mut Context<'_, Self>) {
        if self.listed == listed {
            return;
        }
        let gained: HashSet<MachineId> = {
            let before: HashSet<&SessionKey> = self.listed.iter().collect();
            listed
                .iter()
                .filter(|session| !before.contains(session))
                .map(|session| session.machine.clone())
                .collect()
        };
        self.listed = listed;
        for machine in gained {
            self.list_now(&machine, cx);
        }
    }

    /// Shows `shown` for `surface`, the Pull request surface on screen, in
    /// place of what another showed. What wasn't shown in the last
    /// [`SHOW_EVERY`] is shown at once.
    pub(crate) fn show(&mut self, surface: EntityId, shown: Shown, cx: &mut Context<'_, Self>) {
        self.shown_by = Some(surface);
        if self.shown.as_ref() == Some(&shown) {
            return;
        }
        if self.show.is_running() && !self.show.was_for(&shown) {
            // One for this follows the one under way for another.
            self.show.again();
        }
        let machine = shown.session.machine.clone();
        self.shown = Some(shown);
        // Who comments on it.
        self.forges
            .update(cx, |forges, cx| forges.ensure_checked(&machine, cx));
        self.show_when_due(cx);
        cx.notify();
    }

    /// Stops showing for `surface`, as it left the screen. Another surface
    /// may show already, as tabs switch: what it shows stays.
    pub(crate) fn hide(&mut self, surface: EntityId, cx: &mut Context<'_, Self>) {
        if self.shown_by != Some(surface) {
            return;
        }
        self.shown_by = None;
        self.shown = None;
        if !self.show.is_running() {
            // Nothing to show: a timer waiting for the next show goes.
            self.show.task = Task::ready(());
        }
        cx.notify();
    }

    /// Shows what is on screen now with `read: fresh`, as a turn ended,
    /// which may have changed the PR.
    pub(crate) fn show_fresh(&mut self, cx: &mut Context<'_, Self>) {
        self.next = PullRequestRead::Fresh;
        self.show_now(cx);
    }

    /// Refresh, or an action done: [`Self::show_fresh`], and pictures that
    /// didn't load are asked for again, and so is the check of a `gh` that
    /// can't comment.
    pub(crate) fn refresh(&mut self, cx: &mut Context<'_, Self>) {
        self.avatars.forget_failed();
        if let Some(shown) = &self.shown {
            let machine = shown.session.machine.clone();
            let check = self.forges.read(cx).check(&machine, ForgeCli::Gh);
            let unusable = matches!(
                Gh::of(&machine, self.host_of(shown), check),
                Gh::Unusable(_)
            );
            self.forges.update(cx, |forges, cx| {
                if unusable {
                    // A CLI installed or signed in since its check.
                    forges.recheck(&machine, cx);
                } else {
                    // One that couldn't start, its machine not connected.
                    forges.ensure_checked(&machine, cx);
                }
            });
        }
        self.show_fresh(cx);
    }

    /// Whether `machine`'s daemon serves pull requests; `None` while the
    /// machine isn't connected.
    pub(crate) fn serving(&self, machine: &MachineId) -> Option<Serving> {
        self.machines.get(machine).map(|entry| entry.serving)
    }

    /// Whether a show is on its way, or waits for the one on its way: a
    /// test that counts the daemon's shows waits for none before it counts.
    #[cfg(test)]
    pub(crate) fn is_showing(&self) -> bool {
        self.show.is_running()
    }

    /// `session`'s set, once its daemon answered with one.
    pub(crate) fn of(&self, session: &SessionKey) -> Option<&SessionPullRequests> {
        self.machines
            .get(&session.machine)?
            .sessions
            .get(&session.id)
    }

    /// The summary of what `shown` names, once the daemon found or read
    /// it: what [`Self::reading`] shows as [`Seen::Pr`].
    pub(crate) fn summary(&self, shown: &Shown) -> Option<&PullRequestSummary> {
        match self.reading(shown).seen {
            Seen::Pr(summary) => Some(summary),
            Seen::Disconnected(_) | Seen::Unserved(_) | Seen::Looking | Seen::Nothing(_) => None,
        }
    }

    /// The detail of PR `id` as `machine`'s daemon last showed it; it may
    /// be older than its summary.
    pub(crate) fn detail(
        &self,
        machine: &MachineId,
        id: &PullRequestId,
    ) -> Option<&PullRequestDetail> {
        self.machines.get(machine)?.details.get(id)
    }

    /// The PR that marks `session` on its sidebar row and in its title
    /// bar: its branch's, else the last linked one read, which the Pull
    /// request surface opens on ([`Self::which`]).
    pub(crate) fn mark(&self, session: &SessionKey) -> Option<Mark<'_>> {
        let set = self.of(session)?;
        let pr = set.branch_pull_request().or_else(|| {
            set.linked
                .iter()
                .rev()
                .find_map(|linked| linked.summary.as_ref())
        })?;
        Some(Mark {
            pr,
            watched: set.link(&pr.id).is_some_and(|link| link.watched),
            more: set
                .linked
                .iter()
                .filter(|linked| linked.id != pr.id)
                .count(),
        })
    }

    /// Every session whose agent watches a pull request, as the daemons
    /// last said: its daemon wakes it when the PR changes, so the session
    /// isn't done while it does. Of a session no longer listed, that is
    /// what its daemon said while it was.
    pub(crate) fn watching(&self) -> HashSet<SessionKey> {
        self.machines
            .iter()
            .flat_map(|(machine, entry)| {
                entry
                    .sessions
                    .iter()
                    .filter(|(_, set)| set.linked.iter().any(|linked| linked.watched))
                    .map(|(session, _)| SessionKey::new(machine.clone(), session.clone()))
            })
            .collect()
    }

    /// Which of `session`'s PRs its Pull request surface shows next, as
    /// it shows `shown`: the same PR while the session still has it, so a
    /// PR the agent links never takes the tab; the branch's when `shown`
    /// is the branch's PR as a link. Else, as when it was unlinked or the
    /// branch's lookup found nothing: the branch's PR, while it has one
    /// or is still being looked up; else the PR linked last, read if one
    /// is ([`Self::mark`]'s); else the branch's empty state, which offers
    /// to link one. While the session's set isn't known, as while its
    /// machine reconnects, `shown` stays.
    pub(crate) fn which(&self, session: &SessionKey, shown: &Which) -> Which {
        next_which(self.of(session), shown)
    }

    /// The PRs the Pull request surface can switch between: the branch's,
    /// then the rest linked, in the order linked, each with its title once
    /// read.
    pub(crate) fn choices(&self, session: &SessionKey) -> Vec<(&PullRequestId, Option<&str>)> {
        let Some(set) = self.of(session) else {
            return Vec::new();
        };
        let branch = set.branch_pull_request();
        let linked = set
            .linked
            .iter()
            .filter(|linked| branch.is_none_or(|pr| pr.id != linked.id))
            .map(|linked| {
                let title = linked.summary.as_ref().map(|pr| pr.title.as_str());
                (&linked.id, title)
            });
        branch
            .map(|pr| (&pr.id, Some(pr.title.as_str())))
            .into_iter()
            .chain(linked)
            .collect()
    }

    /// Links the PR `text` names to `session`, as the user typed it; the
    /// daemon reads it. Answers with the PR it named, or why not, as the
    /// daemon says it.
    pub(crate) fn link(
        &mut self,
        session: &SessionKey,
        text: String,
        cx: &mut Context<'_, Self>,
    ) -> Task<Result<PullRequestId, SharedString>> {
        let entry = match self.served(&session.machine) {
            Ok(entry) => entry,
            Err(why) => return Task::ready(Err(why)),
        };
        let (daemon, connection, session) =
            (entry.daemon.clone(), entry.connection, session.clone());
        cx.spawn(async move |this, cx| {
            let answer = daemon.link_pull_request(&session.id, &text).await;
            this.update(cx, |this, cx| {
                let Some(entry) = this.current(&session.machine, connection) else {
                    return Err(not_connected(&session.machine));
                };
                let linked = entry.take(&session.machine, answer).map(|response| {
                    entry.store(&session.id, response.pull_requests);
                    response.pull_request
                });
                cx.notify();
                linked
            })
            .unwrap_or_else(|_| Err(GONE.into()))
        })
    }

    /// Unlinks PR `id` from `session`, ending the agent's watch on it if
    /// it watched it. Answers with why not, as the daemon says it.
    pub(crate) fn unlink(
        &mut self,
        session: &SessionKey,
        id: &PullRequestId,
        cx: &mut Context<'_, Self>,
    ) -> Task<Result<(), SharedString>> {
        let entry = match self.served(&session.machine) {
            Ok(entry) => entry,
            Err(why) => return Task::ready(Err(why)),
        };
        // The link's own `url`, never `owner/name#N`, which the daemon
        // reads as github.com.
        let Some(text) = entry
            .sessions
            .get(&session.id)
            .and_then(|set| set.link(id))
            .map(|link| link.url.clone())
        else {
            // Unlinked already.
            return Task::ready(Ok(()));
        };
        let (daemon, connection, session) =
            (entry.daemon.clone(), entry.connection, session.clone());
        cx.spawn(async move |this, cx| {
            let answer = daemon.unlink_pull_request(&session.id, &text).await;
            this.update(cx, |this, cx| {
                let Some(entry) = this.current(&session.machine, connection) else {
                    return Err(not_connected(&session.machine));
                };
                let unlinked = entry.take(&session.machine, answer).map(|set| {
                    entry.store(&session.id, set);
                });
                cx.notify();
                unlinked
            })
            .unwrap_or_else(|_| Err(GONE.into()))
        })
    }

    /// The picture of a post's author, served at `url`
    /// ([`rho_ui_core::PullRequestPost::avatar_url`]), once fetched.
    pub(crate) fn avatar(&self, url: &str) -> Option<Arc<Image>> {
        self.avatars.get(url)
    }

    /// Who comments on the PR on screen: the session machine's `gh`, with
    /// its account there for the PR's host, as the session's own work goes;
    /// or why no one can.
    pub(crate) fn poster(&self, cx: &App) -> Result<Poster, SharedString> {
        let shown = self
            .shown
            .as_ref()
            .ok_or_else(|| SharedString::from("No pull request is on screen."))?;
        let machine = &shown.session.machine;
        let check = self.forges.read(cx).check(machine, ForgeCli::Gh);
        match Gh::of(machine, self.host_of(shown), check) {
            Gh::Use { program, login } => Ok(Poster {
                machine: machine.clone(),
                program,
                login,
            }),
            Gh::Checking => Err("Checking gh…".into()),
            Gh::Unusable(why) => Err(why),
        }
    }

    /// The forge whose pictures PR `shown` shows, and how its machine's
    /// `gh` signs in to it.
    pub(crate) fn picture_site(&self, shown: &Shown, cx: &App) -> Site {
        let machine = &shown.session.machine;
        let host = self.host_of(shown);
        let check = self.forges.read(cx).check(machine, ForgeCli::Gh);
        let signing = match Gh::of(machine, host, check) {
            Gh::Use { program, .. } => Signing::With(program),
            Gh::Checking => Signing::Checking,
            Gh::Unusable(_) => Signing::Without,
        };
        Site {
            host: host.to_owned(),
            signing,
        }
    }

    /// Does `action` to PR `id` as [`Self::poster`], then
    /// [refreshes](Self::refresh) to show what it did.
    pub(crate) fn act(
        &mut self,
        id: PullRequestId,
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
                let _ = this.update(cx, PullRequests::refresh);
            }
            result.map_err(SharedString::from)
        })
    }

    /// The host of the PR `shown` names: its summary's, a link's own, or
    /// github.com, the one host whose branches a daemon looks up.
    fn host_of<'a>(&'a self, shown: &'a Shown) -> &'a str {
        match (self.summary(shown), &shown.which) {
            (Some(summary), _) => &summary.id.host,
            (None, Which::Linked(id)) => &id.host,
            (None, Which::Branch) => GITHUB,
        }
    }

    /// `machine`'s state, if it is connected and its daemon may serve pull
    /// requests; else what to say instead.
    fn served(&self, machine: &MachineId) -> Result<&MachinePrs, SharedString> {
        let entry = self
            .machines
            .get(machine)
            .ok_or_else(|| not_connected(machine))?;
        if entry.serving == Serving::Unserved {
            return Err(unserved(machine));
        }
        Ok(entry)
    }

    /// `machine`'s state, while it is still on `connection`: an answer sent
    /// on an earlier one is dropped.
    fn current(&mut self, machine: &MachineId, connection: u64) -> Option<&mut MachinePrs> {
        self.machines
            .get_mut(machine)
            .filter(|entry| entry.connection == connection)
    }

    /// Shows what is on screen again now if it is of `session`, whose set
    /// just changed, and its summary moved past the detail last read of it,
    /// or no detail was read yet.
    fn show_if_moved(&mut self, session: &SessionKey, cx: &mut Context<'_, Self>) {
        let moved = self
            .shown
            .as_ref()
            .filter(|shown| shown.session == *session)
            .and_then(|shown| self.summary(shown))
            .is_some_and(|summary| {
                self.detail(&session.machine, &summary.id)
                    .is_none_or(|detail| detail.summary != *summary)
            });
        if moved {
            self.show_now(cx);
        }
    }

    /// Shows now if what is on screen wasn't shown in the last
    /// [`SHOW_EVERY`], else once it is due.
    fn show_when_due(&mut self, cx: &mut Context<'_, Self>) {
        let Some(shown) = &self.shown else {
            return;
        };
        if self.serving(&shown.session.machine) == Some(Serving::Unserved) {
            return;
        }
        if arm(&mut self.show, shown, SHOW_EVERY, Self::show_now, cx) == Due::Now {
            self.show_now(cx);
        }
    }

    /// Shows what is on screen now, or once the show under way ends.
    fn show_now(&mut self, cx: &mut Context<'_, Self>) {
        if self.show.again() || self.focus == Focus::Unfocused {
            return;
        }
        let Some(shown) = self.shown.clone() else {
            return;
        };
        let Ok(entry) = self.served(&shown.session.machine) else {
            // Its connection, if it comes, shows it.
            return;
        };
        let target = match &shown.which {
            Which::Branch => PullRequestTarget::Branch,
            Which::Linked(id) => match entry.sessions.get(&shown.session.id) {
                // Not answered since it connected: the branch's show answers
                // with the set, and the set shows the link
                // ([`Self::show_if_moved`]).
                None => PullRequestTarget::Branch,
                Some(set) => match set.link(id) {
                    // Named by the link's own `url`, as `unlink` names it.
                    Some(link) => PullRequestTarget::Linked {
                        pull_request: link.url.clone(),
                    },
                    // Unlinked, as its tab closes.
                    None => return,
                },
            },
        };
        let (daemon, connection) = (entry.daemon.clone(), entry.connection);
        let read = std::mem::take(&mut self.next);
        self.show
            .start(shown.clone(), cx.background_executor().now());
        self.show.task = cx.spawn(async move |this, cx| {
            let answer = daemon
                .show_pull_request(&shown.session.id, target, read)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.showed(&shown, connection, answer, cx);
                if this.show.finish() {
                    this.show_now(cx);
                } else {
                    this.show_when_due(cx);
                }
            });
        });
    }

    /// Takes the answer to a `show` of `shown` sent on `connection`. A
    /// refusal stays with what it showed, and what was shown before stays,
    /// with why it may be out of date.
    fn showed(
        &mut self,
        shown: &Shown,
        connection: u64,
        answer: Result<SessionPullRequestShowResponse, PullRequestError>,
        cx: &mut Context<'_, Self>,
    ) {
        let machine = &shown.session.machine;
        let Some(entry) = self.current(machine, connection) else {
            return;
        };
        let before = entry.health();
        let mut stored = false;
        let mut read = None;
        let refused = match entry.take(machine, answer) {
            Ok(response) => {
                stored = entry.store(&shown.session.id, response.pull_requests);
                if let Some(detail) = response.detail {
                    let id = detail.summary.id.clone();
                    if entry.details.get(&id) != Some(&*detail) {
                        entry.details.insert(id.clone(), *detail);
                        read = Some(id);
                    }
                }
                None
            }
            Err(why) => Some((shown.clone(), why)),
        };
        let mut changed = stored || entry.health() != before;
        if self.show_refused != refused {
            self.show_refused = refused;
            changed = true;
        }
        let detail = read.and_then(|id| self.machines.get(machine)?.details.get(&id));
        if let Some(detail) = detail {
            self.avatars.queue(detail, machine);
            changed = true;
        }
        if changed {
            cx.notify();
            self.fetch_next_avatar(cx);
        }
        if stored {
            self.show_if_moved(&shown.session, cx);
        }
    }

    /// How often `machine` is listed: [`LOOKING_EVERY`] while its last
    /// answer holds a listed session still looking, else [`LIST_EVERY`].
    fn list_period(&self, machine: &MachineId) -> Duration {
        let Some(entry) = self.machines.get(machine) else {
            return LIST_EVERY;
        };
        let looking = self
            .listed
            .iter()
            .filter(|session| session.machine == *machine)
            .filter_map(|session| entry.sessions.get(&session.id))
            .any(still_looking);
        if looking {
            LOOKING_EVERY
        } else {
            LIST_EVERY
        }
    }

    /// Lists `machine`'s listed sessions now if they are due, else once
    /// they are; not for a daemon from before pull requests.
    fn list_when_due(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let period = self.list_period(machine);
        let Some(entry) = self.machines.get_mut(machine) else {
            return;
        };
        if entry.serving == Serving::Unserved {
            return;
        }
        let then = machine.clone();
        let list = arm(
            &mut entry.list,
            &(),
            period,
            move |this, cx| this.list_now(&then, cx),
            cx,
        );
        if list == Due::Now {
            self.list_now(machine, cx);
        }
    }

    /// Lists `machine`'s listed sessions now, or once the list under way
    /// ends.
    fn list_now(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let sessions: Vec<SessionId> = self
            .listed
            .iter()
            .filter(|session| session.machine == *machine)
            .map(|session| session.id.clone())
            .collect();
        let focused = self.focus == Focus::Focused;
        let Some(entry) = self.machines.get_mut(machine) else {
            return;
        };
        if entry.list.again()
            || !focused
            || entry.serving == Serving::Unserved
            || sessions.is_empty()
        {
            return;
        }
        let (daemon, connection) = (entry.daemon.clone(), entry.connection);
        entry.list.start((), cx.background_executor().now());
        let machine = machine.clone();
        entry.list.task = cx.spawn(async move |this, cx| {
            let answer = daemon.list_pull_requests(&sessions).await;
            let _ = this.update(cx, |this, cx| {
                this.list_answered(&machine, connection, answer, cx);
            });
        });
    }

    /// Takes the answer to a `list` sent to `machine` on `connection`, and
    /// schedules the next.
    fn list_answered(
        &mut self,
        machine: &MachineId,
        connection: u64,
        answer: Result<Vec<(SessionId, SessionPullRequests)>, PullRequestError>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(entry) = self.current(machine, connection) else {
            return;
        };
        let before = entry.health();
        // What the daemon refused of the list alone has no one to tell: the
        // next list asks again.
        let sets = entry.take(machine, answer).unwrap_or_default();
        let mut stored = Vec::new();
        for (session, set) in sets {
            if entry.store(&session, set) {
                stored.push(session);
            }
        }
        let changed = !stored.is_empty() || entry.health() != before;
        let again = entry.list.finish();
        // Every notify redraws the sidebar and syncs the panel's tabs: only
        // when something changed.
        if changed {
            cx.notify();
        }
        // As for an event: only the shown session's own set can move its PR.
        for session in stored {
            self.show_if_moved(&SessionKey::new(machine.clone(), session), cx);
        }
        if again {
            self.list_now(machine, cx);
        } else {
            self.list_when_due(machine, cx);
        }
    }

    /// Fetches the next picture waiting, unless a fetch runs; its end
    /// starts the one after. One that doesn't load leaves its author's
    /// initials, and says why on stderr.
    fn fetch_next_avatar(&mut self, cx: &mut Context<'_, Self>) {
        let (runner, url) = loop {
            let Some((machine, url)) = self.avatars.start_next() else {
                return;
            };
            match self.workspaces.read(cx).reader(&machine) {
                Reader::Unread => self.avatars.abandon(&url, &machine),
                runner @ (Reader::Here | Reader::There(_)) => break (runner, url),
            }
        };
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let picture = forge::picture::avatar(runner, &url, &executor).await;
            let picture = picture.inspect_err(|why| report_avatar(&url, why)).ok();
            let _ = this.update(cx, |this, cx| {
                this.avatars.finish(&url, picture);
                cx.notify();
                this.fetch_next_avatar(cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use rho_ui_core::{
        BranchLookup, BranchPullRequest, LinkedPullRequest, PullRequestId, PullRequestLinkOrigin,
        PullRequestRollup, PullRequestState, PullRequestSummary, PullRequestTrouble,
        SessionPullRequests,
    };

    use super::{next_which, Which};

    fn id(number: u32) -> PullRequestId {
        PullRequestId::new("github.com", "acme", "widgets", number)
    }

    fn summary(number: u32) -> PullRequestSummary {
        PullRequestSummary::new(
            id(number),
            "Widgets",
            format!("https://github.com/acme/widgets/pull/{number}"),
            PullRequestState::Open,
            PullRequestRollup::Passing,
            "abc",
            "2026-11-16T00:00:00Z",
        )
    }

    /// A link to PR `number`, not read yet.
    fn unread(number: u32) -> LinkedPullRequest {
        let url = format!("https://github.com/acme/widgets/pull/{number}");
        LinkedPullRequest::new(id(number), url, PullRequestLinkOrigin::Agent, 1_767_312_100)
    }

    /// A link to PR `number`, read.
    fn link(number: u32) -> LinkedPullRequest {
        let mut link = unread(number);
        link.summary = Some(summary(number));
        link
    }

    /// A set whose branch's lookup is `lookup`, with `trouble`, linking
    /// `linked`.
    fn set(
        lookup: BranchLookup,
        trouble: Option<PullRequestTrouble>,
        linked: Vec<LinkedPullRequest>,
    ) -> SessionPullRequests {
        let branch = BranchPullRequest::new(Some("feature".to_owned()), lookup, trouble);
        SessionPullRequests::new(Some(branch), linked)
    }

    fn found(number: u32) -> BranchLookup {
        BranchLookup::Found {
            pull_request: Box::new(summary(number)),
        }
    }

    /// The surface keeps what it shows until the user or an unlink moves
    /// it, never flashing to another PR on a passing state; and falls back
    /// to the PR the user most likely wants.
    #[test]
    fn the_pr_shown_stays_until_it_goes() {
        let linked = Which::Linked(id(13));
        let cases = [
            (
                "a machine reconnecting keeps the PR",
                None,
                linked.clone(),
                linked.clone(),
            ),
            (
                "the agent linking another keeps the PR",
                Some(set(BranchLookup::None, None, vec![link(13), link(14)])),
                linked.clone(),
                linked.clone(),
            ),
            (
                "the branch's PR picked as a link is the branch's",
                Some(set(found(12), None, vec![link(12)])),
                Which::Linked(id(12)),
                Which::Branch,
            ),
            (
                "the branch's PR stays the branch's while it is looked up again",
                Some(set(BranchLookup::Looking, None, vec![link(12)])),
                Which::Branch,
                Which::Branch,
            ),
            (
                "unlinked, the branch's PR shows",
                Some(set(found(12), None, vec![])),
                linked.clone(),
                Which::Branch,
            ),
            (
                "with no branch PR, the last link read shows",
                Some(set(BranchLookup::None, None, vec![link(13), unread(14)])),
                Which::Branch,
                linked.clone(),
            ),
            (
                "a lookup that can't finish finds nothing",
                Some(set(
                    BranchLookup::Looking,
                    Some(PullRequestTrouble::NotInstalled),
                    vec![link(13)],
                )),
                Which::Branch,
                linked,
            ),
            (
                "with nothing linked, the branch's empty state shows",
                Some(set(BranchLookup::None, None, vec![])),
                Which::Branch,
                Which::Branch,
            ),
        ];
        for (case, set, shown, next) in cases {
            assert_eq!(next_which(set.as_ref(), &shown), next, "{case}");
        }
    }
}
