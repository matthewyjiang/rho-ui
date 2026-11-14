//! The background work rho-ui saw each session run, kept across restarts
//! for the session's ended work (see [`crate::background`]). The daemon
//! lists a piece of work only until the session's model has its result,
//! and replays a session's background events only for its turn in
//! progress, so a session opened again in a later run would show nothing
//! that ended before. rho-protocol has no way to ask for a session's ended
//! work; until it does, rho-ui records each piece as it starts and as it
//! ends, and gives the session the record before it attaches
//! ([`Session::remember_background`]). Work it saw running that the daemon
//! no longer lists then ended while rho-ui was away, how unknown; work that
//! started and ended while it was away stays unknown.
//!
//! The record lives in the state database's `background` table (see
//! [`StateDb`]). A session's rows go when rho-ui sees it deleted; one
//! deleted elsewhere, and its subagents' sessions, keep theirs.

use gpui_kit::Global;
use rho_ui_core::{
    BackgroundId, BackgroundKind, BackgroundStatus, BackgroundWork, Session, SessionId,
};

use crate::{machines::SessionKey, store::StateDb};

/// How each kind is stored.
const KINDS: [(BackgroundKind, &str); 3] = [
    (BackgroundKind::Process, "process"),
    (BackgroundKind::Workflow, "workflow"),
    (BackgroundKind::Subagent, "subagent"),
];

/// How each status is stored. [`BackgroundLog::save`] and the schema's
/// CHECK name `running` too.
const STATUSES: [(BackgroundStatus, &str); 5] = [
    (BackgroundStatus::Running, "running"),
    (BackgroundStatus::Completed, "completed"),
    (BackgroundStatus::Failed, "failed"),
    (BackgroundStatus::Cancelled, "cancelled"),
    (BackgroundStatus::Ended, "ended"),
];

/// Every session's recorded background work. A global: each session view
/// loads and records its own.
#[derive(Debug)]
pub(crate) struct BackgroundLog {
    db: StateDb,
}

impl Global for BackgroundLog {}

/// What a session view has recorded of its session's work, so each change
/// is written once. A session that had none recorded starts at the default.
#[derive(Debug, Default)]
pub(crate) struct Recorded {
    /// How many of [`Session::ended_background`] are written.
    ended: usize,
    /// The running work as last written.
    running: Vec<BackgroundWork>,
}

impl BackgroundLog {
    pub(crate) fn new(db: &StateDb) -> Self {
        Self { db: db.clone() }
    }

    /// Gives `session`, which `key` names, the record of its work before
    /// it attaches (see [`Session::remember_background`]), and returns
    /// what is written of it: the work that had ended. What still ran is
    /// written again as the daemon lists it.
    pub(crate) fn remember(&self, key: &SessionKey, session: &mut Session) -> Recorded {
        let seen = self.load(key);
        let ended = seen
            .iter()
            .filter(|work| work.status() != BackgroundStatus::Running)
            .count();
        session.remember_background(seen);
        Recorded {
            ended,
            running: Vec::new(),
        }
    }

    /// Writes what changed in `session`'s work since `recorded`: work that
    /// ended, and running work that started or changed, as a subagent does
    /// when it opens its session.
    pub(crate) fn record(&self, key: &SessionKey, session: &Session, recorded: &mut Recorded) {
        let ended = session.ended_background();
        for work in ended.iter().skip(recorded.ended) {
            self.save(key, work);
        }
        recorded.ended = ended.len();

        let running = || {
            session
                .background()
                .iter()
                .filter(|work| work.status() == BackgroundStatus::Running)
        };
        let unchanged = running().count() == recorded.running.len()
            && running()
                .zip(&recorded.running)
                .all(|(now, then)| same(now, then));
        if unchanged {
            return;
        }
        for work in running() {
            if !recorded.running.iter().any(|then| same(work, then)) {
                self.save(key, work);
            }
        }
        recorded.running = running().cloned().collect();
    }

    /// Forgets a deleted session's work.
    pub(crate) fn forget(&self, session: &SessionKey) {
        self.db.execute(
            "DELETE FROM background WHERE machine = ?1 AND session_id = ?2",
            (session.machine.as_str(), session.id.as_str()),
        );
    }

    /// The record of `session`'s work: what ended, in the order it ended,
    /// then what still ran. A row this build can't read is left out.
    fn load(&self, session: &SessionKey) -> Vec<BackgroundWork> {
        let rows = self.db.query(
            "SELECT kind, work_id, subagent_session, title, status, exit_code \
             FROM background WHERE machine = ?1 AND session_id = ?2 \
             ORDER BY ended IS NULL, ended",
            (session.machine.as_str(), session.id.as_str()),
            |row| {
                let kind = parse(&KINDS, &row.get::<_, String>(0)?);
                let Some(status) = parse(&STATUSES, &row.get::<_, String>(4)?) else {
                    return Ok(None);
                };
                let subagent: Option<String> = row.get(2)?;
                let id = match kind {
                    Some(BackgroundKind::Process) => BackgroundId::process(row.get(1)?),
                    Some(BackgroundKind::Workflow) => BackgroundId::workflow(row.get(1)?),
                    Some(BackgroundKind::Subagent) => BackgroundId::subagent(
                        row.get(1)?,
                        subagent.as_deref().map(SessionId::from).as_ref(),
                    ),
                    Some(BackgroundKind::Other) | None => return Ok(None),
                };
                Ok(Some(BackgroundWork::new(
                    id,
                    row.get(3)?,
                    status,
                    row.get(5)?,
                )))
            },
        );
        rows.into_iter().flatten().collect()
    }

    /// Writes `work`. Work that ended goes after every other the session
    /// has recorded ending, so the record keeps the order rho-ui saw the
    /// ends in, whatever writes were lost. Best effort: a lost write loses
    /// one piece of work from the record. A kind this build doesn't know
    /// has no ID to keep.
    fn save(&self, session: &SessionKey, work: &BackgroundWork) {
        let Some((kind, id)) = work.id().key() else {
            return;
        };
        let (Some(kind), Some(status)) = (name(&KINDS, kind), name(&STATUSES, work.status()))
        else {
            return;
        };
        let subagent = work.session();
        self.db.execute(
            "INSERT OR REPLACE INTO background \
             (machine, session_id, kind, work_id, subagent_session, title, status, exit_code, ended) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, CASE ?7 WHEN 'running' THEN NULL ELSE \
             (SELECT COALESCE(MAX(ended), 0) + 1 FROM background \
              WHERE machine = ?1 AND session_id = ?2) END)",
            (
                session.machine.as_str(),
                session.id.as_str(),
                kind,
                id,
                subagent.as_ref().map(SessionId::as_str),
                work.title(),
                status,
                work.exit_code(),
            ),
        );
    }
}

/// Whether `a` and `b` are recorded alike. [`BackgroundWork`]'s own
/// equality leaves out a subagent's session, which the record keeps.
fn same(a: &BackgroundWork, b: &BackgroundWork) -> bool {
    a == b && a.session() == b.session()
}

fn name<T: Copy + PartialEq>(table: &[(T, &'static str)], value: T) -> Option<&'static str> {
    table
        .iter()
        .find(|(known, _)| *known == value)
        .map(|(_, name)| *name)
}

fn parse<T: Copy>(table: &[(T, &str)], name: &str) -> Option<T> {
    table
        .iter()
        .find(|(_, known)| *known == name)
        .map(|(value, _)| *value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind and status reads back as written, a subagent with its
    /// session, ended work in the order it ended and running work after it.
    #[test]
    fn the_record_reads_back_as_written() {
        let log = BackgroundLog::new(&StateDb::in_memory());
        let key = SessionKey::local("s1");
        let child = SessionId::from("child");
        let work = |id: BackgroundId, status, exit_code| {
            BackgroundWork::new(id, "title".to_owned(), status, exit_code)
        };
        let ended = [
            work(
                BackgroundId::process("p1".to_owned()),
                BackgroundStatus::Completed,
                Some(1),
            ),
            work(
                BackgroundId::workflow("w1".to_owned()),
                BackgroundStatus::Failed,
                None,
            ),
            work(
                BackgroundId::subagent("r1".to_owned(), Some(&child)),
                BackgroundStatus::Cancelled,
                None,
            ),
            work(
                BackgroundId::process("p0".to_owned()),
                BackgroundStatus::Ended,
                None,
            ),
        ];
        let running = work(
            BackgroundId::subagent("r2".to_owned(), None),
            BackgroundStatus::Running,
            None,
        );
        log.save(&key, &running);
        for work in &ended {
            log.save(&key, work);
        }

        let loaded = log.load(&key);
        let want: Vec<_> = ended.iter().chain([&running]).collect();
        assert_eq!(loaded.len(), want.len(), "{loaded:?}");
        for (got, want) in loaded.iter().zip(want) {
            assert!(same(got, want), "{got:?} read back for {want:?}");
        }
        log.forget(&key);
        assert!(log.load(&key).is_empty());
    }
}
