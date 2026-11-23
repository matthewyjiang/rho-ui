//! What the daemon's job list (`rho/job/list`) says: which sessions have
//! work under way, for the sidebar, and the subagent and workflow runs each
//! session started. A session's own background list names its runs; the
//! job list adds what that lacks: where a queued run waits, why a failed
//! one failed, and the agent runs a run started in turn, such as a
//! workflow's agent nodes.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use rho_protocol::{BackgroundItem, JobId, JobKind, JobState, JobSummary};

use crate::{
    background::{BackgroundId, BackgroundKind},
    daemon::SessionId,
};

/// One listing of the daemon's jobs. Nothing pushes changes to it; list
/// again with [`crate::Daemon::jobs`] to refresh.
#[derive(Clone, Debug, Default)]
pub struct Jobs {
    /// The sessions with work under way, whichever client started them.
    pub active: Vec<Job>,
    /// The runs each session started.
    pub runs: Runs,
}

impl Jobs {
    pub(crate) fn from_list(jobs: Vec<JobSummary>) -> Self {
        Self {
            active: Job::from_list(&jobs),
            runs: Runs::from_list(jobs),
        }
    }
}

/// What a session's job is doing, from `rho/job/list`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobActivity {
    /// A turn runs, or waits for a slot.
    Working,
    /// The session waits for a person: an approval or a question.
    Waiting,
    /// No turn runs, but subagents or workflow runs a turn started still
    /// do. The daemon's job list doesn't show background processes, so a
    /// session with only those has no job.
    Background,
}

/// A session with work under way in the daemon, whichever client started it.
#[derive(Clone, Debug)]
pub struct Job {
    session: SessionId,
    cwd: PathBuf,
    title: Option<String>,
    activity: JobActivity,
    turn_elapsed: Option<Duration>,
}

impl Job {
    /// The session the job runs. Pass to [`crate::Daemon::attach`] to
    /// follow it.
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    /// The workspace the job runs in.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The session's title, once it has one.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// What the job is doing.
    pub fn activity(&self) -> JobActivity {
        self.activity
    }

    /// How long the turn under way has run, when the daemon says. It grows
    /// between two listings of the same job.
    pub fn turn_elapsed(&self) -> Option<Duration> {
        self.turn_elapsed
    }

    /// Whether `other` is this job as an earlier or later listing would
    /// show it: everything but [`Self::turn_elapsed`] is the same. Two
    /// listings that differ only in elapsed time changed nothing to show.
    pub fn is_same_work(&self, other: &Job) -> bool {
        self.session == other.session
            && self.cwd == other.cwd
            && self.title == other.title
            && self.activity == other.activity
    }

    /// The sessions with work under way among `jobs`, a whole listing.
    fn from_list(jobs: &[JobSummary]) -> Vec<Self> {
        // A subagent or workflow run names the job that started it.
        let started: HashSet<&JobId> = jobs
            .iter()
            .filter(|job| job.kind != JobKind::Session && job.state.in_progress())
            .filter_map(|job| job.parent.as_ref())
            .collect();
        jobs.iter()
            .filter_map(|job| Self::from_wire(job, &started))
            .collect()
    }

    /// The job as the sidebar needs it, or `None` for one that is not a
    /// session with work under way. `started` holds the jobs whose
    /// subagent or workflow runs are under way.
    fn from_wire(job: &JobSummary, started: &HashSet<&JobId>) -> Option<Self> {
        // A subagent's or workflow's session belongs to the session that
        // started it, and can't be attached to.
        if job.kind != JobKind::Session {
            return None;
        }
        let live = job.live.as_ref();
        let asks = live.is_some_and(|live| !live.inputs.is_empty());
        let activity = match job.state {
            JobState::WaitingForInput => JobActivity::Waiting,
            // A subagent's question waits whether or not a turn runs.
            _ if asks => JobActivity::Waiting,
            JobState::Running | JobState::Queued => JobActivity::Working,
            _ if started.contains(&job.id) => JobActivity::Background,
            _ => return None,
        };
        Some(Self {
            session: job.session.clone()?.into(),
            cwd: job.workspace.clone(),
            turn_elapsed: live
                .and_then(|live| live.turn_elapsed_ms)
                .map(Duration::from_millis),
            title: live.and_then(|live| live.title.clone()),
            activity,
        })
    }
}

/// The subagent and workflow runs in a job listing, to put together for the
/// session that started them with [`Self::of`]. The daemon keeps a record
/// of every run, so ended ones are here too, with how they ended.
#[derive(Clone, Debug, Default)]
pub struct Runs {
    /// The whole listing, oldest first.
    jobs: Vec<JobSummary>,
    /// The subagent and workflow jobs each job started, as indices into
    /// `jobs`, oldest first.
    started: HashMap<JobId, Vec<usize>>,
}

impl Runs {
    fn from_list(jobs: Vec<JobSummary>) -> Self {
        let mut started: HashMap<JobId, Vec<usize>> = HashMap::new();
        for (ix, job) in jobs.iter().enumerate() {
            if let (Some(parent), JobKind::Subagent | JobKind::Workflow) = (&job.parent, job.kind) {
                started.entry(parent.clone()).or_default().push(ix);
            }
        }
        Self { jobs, started }
    }

    /// The runs `session` started, oldest first, each with those it
    /// started in turn. A subagent's own session has runs too, when the
    /// subagent delegates.
    pub fn of(&self, session: &SessionId) -> Vec<Run> {
        let writers: Vec<&JobId> = self
            .jobs
            .iter()
            .filter(|job| {
                job.session
                    .as_ref()
                    .is_some_and(|id| id.as_str() == session.as_str())
            })
            .map(|job| &job.id)
            .collect();
        let mut seen = writers.iter().copied().collect();
        self.started_by(&writers, &mut seen)
    }

    /// The runs the jobs `parents` started. A workflow run driven more than
    /// once has a job for each drive: they make one run, where the newest
    /// drive is, holding the agent runs of every drive. `seen` holds the
    /// jobs placed so far, so a daemon whose parents loop can't recurse
    /// forever.
    fn started_by<'a>(&'a self, parents: &[&JobId], seen: &mut HashSet<&'a JobId>) -> Vec<Run> {
        let mut children: Vec<usize> = parents
            .iter()
            .filter_map(|parent| self.started.get(*parent))
            .flatten()
            .copied()
            .collect();
        children.sort_unstable();
        let mut runs: Vec<(Run, Vec<&JobId>)> = Vec::new();
        for job in children.into_iter().map(|ix| &self.jobs[ix]) {
            if !seen.insert(&job.id) {
                continue;
            }
            let id = match (job.kind, &job.run, &job.workflow_run) {
                (JobKind::Subagent, Some(run), _) => BackgroundItem::Subagent {
                    run: run.clone(),
                    session: job.session.clone(),
                },
                (JobKind::Workflow, _, Some(run)) => BackgroundItem::Workflow { run: run.clone() },
                // A run with no ID matches nothing the session lists.
                _ => continue,
            };
            let id = BackgroundId(id);
            let title = job.live.as_ref().and_then(|live| live.title.clone());
            let status = RunStatus::of(job);
            match runs.iter_mut().find(|(run, _)| run.id == id) {
                Some((run, drives)) => {
                    run.status = status;
                    run.title = title.or_else(|| run.title.take());
                    drives.push(&job.id);
                }
                None => runs.push((
                    Run {
                        id,
                        title,
                        status,
                        runs: Vec::new(),
                    },
                    vec![&job.id],
                )),
            }
        }
        runs.into_iter()
            .map(|(mut run, drives)| {
                run.runs = self.started_by(&drives, seen);
                run
            })
            .collect()
    }
}

/// One subagent or workflow run, as the job list says it.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    id: BackgroundId,
    title: Option<String>,
    status: RunStatus,
    runs: Vec<Run>,
}

/// Where a run is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunStatus {
    /// Waiting for a free agent slot.
    Queued {
        /// Its place in line, 1 for the next to start; `None` until the
        /// daemon places it.
        position: Option<u32>,
    },
    /// Under way. A state a newer daemon sends counts as this, so an
    /// unknown run never looks ended.
    Running,
    /// It finished by itself.
    Completed,
    /// It failed, timed out, or the daemon stopped before it ended.
    Failed {
        /// Why, when the daemon says.
        error: Option<String>,
    },
    /// Someone stopped it.
    Cancelled,
}

impl Run {
    /// The same ID as the run's item in its session's background list.
    pub fn id(&self) -> &BackgroundId {
        &self.id
    }

    /// A subagent or a workflow: [`Runs`] makes no other kind.
    pub fn kind(&self) -> BackgroundKind {
        match self.id.0 {
            BackgroundItem::Workflow { .. } => BackgroundKind::Workflow,
            _ => BackgroundKind::Subagent,
        }
    }

    /// A subagent's own session, once it opened one. Follow it with
    /// [`crate::Daemon::observe`].
    pub fn session(&self) -> Option<SessionId> {
        self.id.session()
    }

    /// A subagent session's title, once it has one, while the run is under
    /// way: the daemon titles only the runs it follows.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Where it is.
    pub fn status(&self) -> &RunStatus {
        &self.status
    }

    /// The runs this one started, oldest first: a workflow's agent nodes,
    /// or a subagent's own subagents.
    pub fn runs(&self) -> &[Run] {
        &self.runs
    }
}

impl RunStatus {
    fn of(job: &JobSummary) -> Self {
        let failed = || Self::Failed {
            error: job.error.clone(),
        };
        match job.state {
            JobState::Idle | JobState::Queued => Self::Queued {
                position: job.queue.map(|queue| queue.position),
            },
            JobState::Completed => Self::Completed,
            JobState::Failed | JobState::TimedOut | JobState::Interrupted => failed(),
            JobState::Cancelled => Self::Cancelled,
            _ => Self::Running,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn summary(
        id: &str,
        kind: &str,
        state: &str,
        parent: Option<&str>,
        live: &serde_json::Value,
    ) -> JobSummary {
        let summary = json!({"id": id, "kind": kind, "state": state, "parent": parent,
            "workspace": "/w", "session": format!("s-{id}"), "created_at": 0, "updated_at": 0,
            "live": live});
        serde_json::from_value(summary).expect("a valid job summary")
    }

    fn job(kind: &str, state: &str, live: &serde_json::Value) -> Option<Job> {
        Job::from_list(&[summary("j1", kind, state, None, live)]).pop()
    }

    fn live(elapsed_ms: u64, inputs: &serde_json::Value) -> serde_json::Value {
        json!({"title": "t", "model": {"provider": "p", "model": "m"},
            "turn_elapsed_ms": elapsed_ms, "inputs": inputs})
    }

    #[test]
    fn only_sessions_with_work_under_way_are_jobs_to_show() {
        let none = json!([]);
        let activity = |job: Option<Job>| job.map(|job| job.activity());
        assert_eq!(
            activity(job("session", "running", &live(5, &none))),
            Some(JobActivity::Working)
        );
        assert_eq!(
            activity(job("session", "queued", &live(5, &none))),
            Some(JobActivity::Working)
        );
        assert_eq!(
            activity(job("session", "waiting_for_input", &live(5, &none))),
            Some(JobActivity::Waiting)
        );
        assert_eq!(activity(job("session", "completed", &live(5, &none))), None);
        assert_eq!(activity(job("session", "idle", &json!(null))), None);
        assert_eq!(
            activity(job("subagent", "running", &live(5, &none))),
            None,
            "a subagent's session can't be attached to"
        );

        // A subagent's question keeps its parent waiting between turns.
        let asks = json!([{"id": 1, "asker": "subagent", "ask": {"kind": "question",
            "title": "Which?", "questions": []}}]);
        assert_eq!(
            activity(job("session", "completed", &live(5, &asks))),
            Some(JobActivity::Waiting)
        );
    }

    /// A session whose turn ended while a subagent or workflow run it
    /// started goes on waits on that run, and only while it runs.
    #[test]
    fn a_session_waits_on_the_runs_its_turn_started() {
        let cases = [
            ("subagent", "running", Some(JobActivity::Background)),
            ("subagent", "queued", Some(JobActivity::Background)),
            ("workflow", "running", Some(JobActivity::Background)),
            ("subagent", "completed", None),
        ];
        for (kind, state, want) in cases {
            let jobs = vec![
                summary("j1", "session", "completed", None, &live(5, &json!([]))),
                summary("j2", kind, state, Some("j1"), &serde_json::Value::Null),
            ];
            let activities: Vec<_> = Job::from_list(&jobs).iter().map(Job::activity).collect();
            assert_eq!(activities, Vec::from_iter(want), "a {state} {kind}");
        }
    }

    /// Each session's runs nest as the daemon's parents say, and carry
    /// what the session's background list lacks: a queued run's place in
    /// line and a failed one's error. A workflow's drives make one run. A
    /// run whose parent isn't listed belongs to no session.
    #[test]
    fn runs_nest_under_the_session_that_started_them() {
        let run = |id: &str, kind: &str, state: &str, parent: &str, more: serde_json::Value| {
            let mut job = json!({"id": id, "kind": kind, "state": state, "parent": parent,
                "workspace": "/w", "created_at": 0, "updated_at": 0});
            job.as_object_mut()
                .expect("an object")
                .extend(more.as_object().expect("an object").clone());
            serde_json::from_value::<JobSummary>(job).expect("a valid job summary")
        };
        // The workflow failed a node on its first drive and was driven
        // again: one run, where the second drive is, with both drives'
        // nodes.
        let queue = json!({"budget": "agent_concurrency", "limit": 10, "position": 2});
        let jobs = vec![
            summary("j1", "session", "completed", None, &json!(null)),
            run(
                "w1",
                "workflow",
                "failed",
                "j1",
                json!({"workflow_run": "wf"}),
            ),
            run(
                "b",
                "subagent",
                "failed",
                "w1",
                json!({"run": "rb", "error": "boom"}),
            ),
            run(
                "w2",
                "workflow",
                "running",
                "j1",
                json!({"workflow_run": "wf"}),
            ),
            run(
                "a",
                "subagent",
                "queued",
                "w2",
                json!({"run": "ra", "session": "s-a", "queue": queue}),
            ),
            run("c", "subagent", "running", "j1", json!({"run": "rc"})),
            run("x", "subagent", "running", "gone", json!({"run": "rx"})),
        ];
        let runs = Jobs::from_list(jobs).runs;
        let of_j1 = runs.of(&SessionId::from("s-j1"));
        let ids: Vec<_> = of_j1
            .iter()
            .map(|run| (run.id().key(), run.status().clone()))
            .collect();
        assert_eq!(
            ids,
            [
                (Some((BackgroundKind::Workflow, "wf")), RunStatus::Running),
                (Some((BackgroundKind::Subagent, "rc")), RunStatus::Running)
            ]
        );
        let nodes: Vec<_> = of_j1[0]
            .runs()
            .iter()
            .map(|run| (run.status().clone(), run.session()))
            .collect();
        assert_eq!(
            nodes,
            [
                (
                    RunStatus::Failed {
                        error: Some("boom".to_owned())
                    },
                    None
                ),
                (
                    RunStatus::Queued { position: Some(2) },
                    Some(SessionId::from("s-a"))
                ),
            ]
        );
        assert_eq!(runs.of(&SessionId::from("s-a")), Vec::<Run>::new());
    }

    /// The daemon reports a running turn's elapsed time anew at each
    /// listing. If that alone made a job look changed, the sidebar would
    /// reload the session list at every poll while anything runs.
    #[test]
    fn elapsed_time_alone_does_not_change_a_job() {
        let at = |ms| job("session", "running", &live(ms, &json!([]))).expect("a running job");
        let (earlier, later) = (at(1_000), at(2_000));
        assert_ne!(earlier.turn_elapsed(), later.turn_elapsed());
        assert!(earlier.is_same_work(&later));

        let waiting = job("session", "waiting_for_input", &live(2_000, &json!([])));
        assert!(!earlier.is_same_work(&waiting.expect("a waiting job")));
    }
}
