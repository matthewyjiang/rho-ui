//! Pinned, settled and snoozed sessions. This is rho-ui's own organization;
//! rho knows nothing about it.
//!
//! A session with no mark is active for [`QUIET_AFTER`] past its last
//! activity and settled from then on, whichever client started it: every
//! rho-ui reading the same daemon agrees on that with nothing to share, and
//! old history never fills the sidebar. A mark is what the user said of one
//! session: settled until it has new activity, snoozed until a time, pinned
//! above the others until unpinned (settling or snoozing a pinned session
//! would unpin it, so the sidebar offers neither), or kept, which puts it
//! back on the active list as if it had been active just then. Nothing needs
//! a timer: the sidebar asks [`Shelves::shelf`] whenever it rebuilds.
//!
//! The marks live in the state database's `shelves` table (see [`StateDb`]),
//! a row per marked session.

use rho_ui_core::SessionId;

use crate::{
    machines::{MachineId, SessionKey},
    store::StateDb,
};

/// How long a session stays active after its last activity, in seconds: a
/// week. In 98 days of one daemon's model requests (528 sessions), 97 of the
/// 98 returns to a session idle for an hour or more came within 32 hours and
/// the last after 9 days, so a week loses what three days would, and it
/// outlasts a long weekend.
pub(crate) const QUIET_AFTER: i64 = 7 * 24 * 60 * 60;

/// Which list a session shows in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Shelf {
    #[default]
    Active,
    Snoozed,
    Settled,
}

impl Shelf {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Shelf::Active => "Active",
            Shelf::Snoozed => "Snoozed",
            Shelf::Settled => "Settled",
        }
    }
}

/// What the user said of one session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    /// Settled at this time.
    Settled(i64),
    /// Snoozed until this time, and kept from then on.
    Snoozed(i64),
    /// Pinned at this time, which orders the pinned sessions.
    Pinned(i64),
    /// Put back on the active list at this time, by a restore or an unpin.
    Kept(i64),
}

/// Every marked session, in one flat list sorted by machine and session ID
/// so a lookup is a binary search.
#[derive(Debug)]
pub(crate) struct Shelves {
    marks: Vec<(SessionKey, Mark)>,
    db: StateDb,
}

/// For tests: no marks, kept in memory.
#[cfg(test)]
impl Default for Shelves {
    fn default() -> Self {
        Self::load(&StateDb::in_memory())
    }
}

impl Shelves {
    /// Loads the saved marks, sorted by ID.
    pub(crate) fn load(db: &StateDb) -> Self {
        // SQLite's default collation compares bytes, as `str` does.
        let marks = db.query(
            "SELECT machine, session_id, settled_at, snoozed_until, pinned_at, kept_at \
             FROM shelves ORDER BY machine, session_id",
            [],
            |row| {
                let mark = match (row.get(2)?, row.get(3)?, row.get(4)?) {
                    (Some(at), _, _) => Mark::Settled(at),
                    (None, Some(until), _) => Mark::Snoozed(until),
                    (None, None, Some(at)) => Mark::Pinned(at),
                    // The table's CHECK leaves `kept_at` the one set.
                    (None, None, None) => Mark::Kept(row.get(5)?),
                };
                let machine = MachineId::from(row.get::<_, String>(0)?.as_str());
                let id = SessionId::from(row.get::<_, String>(1)?.as_str());
                Ok((SessionKey::new(machine, id), mark))
            },
        );
        Self {
            marks,
            db: db.clone(),
        }
    }

    /// Where `id` shows, given its last activity and the current time.
    pub(crate) fn shelf(&self, id: &SessionKey, updated: i64, now: i64) -> Shelf {
        // When the session last mattered: its last activity, or the later
        // time the user's mark put it back.
        let touched = match self.mark(id) {
            Some(Mark::Pinned(_)) => return Shelf::Active,
            Some(Mark::Snoozed(until)) if now < until => return Shelf::Snoozed,
            Some(Mark::Settled(at)) if updated <= at => return Shelf::Settled,
            Some(Mark::Snoozed(at) | Mark::Kept(at)) => updated.max(at),
            Some(Mark::Settled(_)) | None => updated,
        };
        if now.saturating_sub(touched) > QUIET_AFTER {
            Shelf::Settled
        } else {
            Shelf::Active
        }
    }

    /// When a snoozed session wakes, or woke, in Unix seconds.
    pub(crate) fn wakes(&self, id: &SessionKey) -> Option<i64> {
        match self.mark(id) {
            Some(Mark::Snoozed(until)) => Some(until),
            _ => None,
        }
    }

    /// When `id` was pinned, if it is.
    pub(crate) fn pinned(&self, id: &SessionKey) -> Option<i64> {
        match self.mark(id) {
            Some(Mark::Pinned(at)) => Some(at),
            _ => None,
        }
    }

    /// Pins `id` to the top of the active list.
    pub(crate) fn pin(&mut self, id: &SessionKey, now: i64) {
        self.set(id, Some(Mark::Pinned(now)));
    }

    /// Unpins `id`, keeping it active.
    pub(crate) fn unpin(&mut self, id: &SessionKey, now: i64) {
        if self.pinned(id).is_some() {
            self.set(id, Some(Mark::Kept(now)));
        }
    }

    pub(crate) fn settle(&mut self, id: &SessionKey, now: i64) {
        self.set(id, Some(Mark::Settled(now)));
    }

    pub(crate) fn snooze(&mut self, id: &SessionKey, until: i64) {
        self.set(id, Some(Mark::Snoozed(until)));
    }

    /// Moves `id` to the active list, where it stays as if active at `now`.
    pub(crate) fn restore(&mut self, id: &SessionKey, now: i64) {
        self.set(id, Some(Mark::Kept(now)));
    }

    /// Forgets a deleted session.
    pub(crate) fn forget(&mut self, id: &SessionKey) {
        self.set(id, None);
    }

    fn find(&self, id: &SessionKey) -> Result<usize, usize> {
        self.marks.binary_search_by(|(known, _)| known.cmp(id))
    }

    fn mark(&self, id: &SessionKey) -> Option<Mark> {
        self.find(id).ok().map(|at| self.marks[at].1)
    }

    /// Gives `id` the mark, or none, and saves it if that changed anything.
    /// Best effort: losing a write only moves a session between lists.
    fn set(&mut self, id: &SessionKey, mark: Option<Mark>) {
        let key = (id.machine.as_str(), id.id.as_str());
        match (self.find(id), mark) {
            (Ok(at), Some(mark)) if self.marks[at].1 == mark => return,
            (Ok(at), Some(mark)) => self.marks[at].1 = mark,
            (Ok(at), None) => {
                self.marks.remove(at);
            }
            (Err(at), Some(mark)) => self.marks.insert(at, (id.clone(), mark)),
            (Err(_), None) => return,
        }
        let Some(mark) = mark else {
            self.db.execute(
                "DELETE FROM shelves WHERE machine = ?1 AND session_id = ?2",
                key,
            );
            return;
        };
        let (settled, snoozed, pinned, kept) = match mark {
            Mark::Settled(at) => (Some(at), None, None, None),
            Mark::Snoozed(until) => (None, Some(until), None, None),
            Mark::Pinned(at) => (None, None, Some(at), None),
            Mark::Kept(at) => (None, None, None, Some(at)),
        };
        self.db.execute(
            "INSERT OR REPLACE INTO shelves \
             (machine, session_id, settled_at, snoozed_until, pinned_at, kept_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (key.0, key.1, settled, snoozed, pinned, kept),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60;
    /// Past [`QUIET_AFTER`].
    const LONG: i64 = QUIET_AFTER + 1;

    fn k(id: &str) -> SessionKey {
        SessionKey::local(id)
    }

    /// The rule that puts a session on its shelf, from its mark, its last
    /// activity and the time.
    #[test]
    fn a_session_shelves_by_its_mark_then_its_age() {
        type Marking = fn(&mut Shelves, &SessionKey);
        let none: Marking = |_, _| {};
        let settled: Marking = |shelves, id| shelves.settle(id, 100 * DAY);
        let snoozed: Marking = |shelves, id| shelves.snooze(id, 100 * DAY);
        let kept: Marking = |shelves, id| shelves.restore(id, 100 * DAY);
        let pinned: Marking = |shelves, id| shelves.pin(id, 100 * DAY);
        let unpinned: Marking = |shelves, id| {
            shelves.pin(id, 50 * DAY);
            shelves.unpin(id, 100 * DAY);
        };
        // (why, mark, last activity, now, shelf)
        let cases: [(&str, Marking, i64, i64, Shelf); 13] = [
            (
                "recent",
                none,
                100 * DAY,
                100 * DAY + QUIET_AFTER,
                Shelf::Active,
            ),
            (
                "quiet for long",
                none,
                100 * DAY,
                100 * DAY + LONG,
                Shelf::Settled,
            ),
            (
                "settled since",
                settled,
                100 * DAY,
                100 * DAY,
                Shelf::Settled,
            ),
            (
                "active after the settle",
                settled,
                100 * DAY + 1,
                101 * DAY,
                Shelf::Active,
            ),
            (
                "that activity is old too",
                settled,
                101 * DAY,
                101 * DAY + LONG,
                Shelf::Settled,
            ),
            ("snoozed", snoozed, 99 * DAY, 100 * DAY - 1, Shelf::Snoozed),
            (
                "woke, though long quiet",
                snoozed,
                0,
                100 * DAY,
                Shelf::Active,
            ),
            (
                "woke long ago",
                snoozed,
                0,
                100 * DAY + LONG,
                Shelf::Settled,
            ),
            (
                "kept, though long quiet",
                kept,
                0,
                100 * DAY + QUIET_AFTER,
                Shelf::Active,
            ),
            ("kept long ago", kept, 0, 100 * DAY + LONG, Shelf::Settled),
            (
                "active since it was kept",
                kept,
                110 * DAY,
                110 * DAY + 1,
                Shelf::Active,
            ),
            ("pinned", pinned, 0, 1000 * DAY, Shelf::Active),
            (
                "unpinned stays for a while",
                unpinned,
                0,
                101 * DAY,
                Shelf::Active,
            ),
        ];
        for (why, mark, updated, now, shelf) in cases {
            let mut shelves = Shelves::default();
            mark(&mut shelves, &k("a"));
            assert_eq!(shelves.shelf(&k("a"), updated, now), shelf, "{why}");
        }
    }

    #[test]
    fn marks_replace_each_other_and_stay_per_machine() {
        let mut shelves = Shelves::default();
        shelves.snooze(&k("a"), 500);
        shelves.settle(&k("a"), 10);
        assert_eq!(
            (shelves.shelf(&k("a"), 5, 20), shelves.wakes(&k("a"))),
            (Shelf::Settled, None),
            "settle replaces snooze"
        );
        shelves.pin(&k("a"), 20);
        assert_eq!(shelves.shelf(&k("a"), 5, 20), Shelf::Active);
        shelves.forget(&k("a"));
        assert_eq!(shelves.pinned(&k("a")), None, "a deleted session");

        // The same ID on another machine is another session.
        let there = SessionKey::new(MachineId::remote("workstation"), SessionId::from("a"));
        shelves.settle(&there, 100);
        assert_eq!(shelves.shelf(&there, 100, 200), Shelf::Settled);
        assert_eq!(shelves.shelf(&k("a"), 100, 200), Shelf::Active);
    }

    #[test]
    fn marks_survive_a_reload() {
        let db = StateDb::in_memory();
        let mut shelves = Shelves::load(&db);
        shelves.settle(&k("a"), 100);
        shelves.snooze(&k("z"), 300);
        shelves.restore(&k("k"), 120);
        shelves.restore(&k("b"), 130);
        shelves.forget(&k("b"));
        shelves.pin(&k("p"), 150);

        assert_eq!(
            Shelves::load(&db).marks,
            vec![
                (k("a"), Mark::Settled(100)),
                (k("k"), Mark::Kept(120)),
                (k("p"), Mark::Pinned(150)),
                (k("z"), Mark::Snoozed(300)),
            ],
            "a forgotten session leaves no row"
        );
    }
}
