//! Pinned, settled and snoozed sessions. This is rho-ui's own organization;
//! rho knows nothing about it.
//!
//! rho-ui owns the sessions started or continued in it. Every other session,
//! such as one from rho's terminal UI, starts out settled, so the sidebar
//! doesn't fill with history from elsewhere; snoozing or restoring one makes
//! it rho-ui's. An owned session that was settled comes back when it has new
//! activity; a snoozed session comes back when its time is up. Neither needs a
//! timer: the sidebar asks [`Shelves::shelf`] whenever it rebuilds. A pinned
//! session stays active, above the others, until unpinned; settling or
//! snoozing it would unpin it, so the sidebar offers neither.
//!
//! The state lives in the state database's `shelves` table (see
//! [`StateDb`]), a row per session rho-ui knows something about.

use rho_ui_core::SessionId;

use crate::{
    machines::{MachineId, SessionKey},
    store::StateDb,
};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    /// Settled at this time.
    Settled(i64),
    /// Snoozed until this time.
    Snoozed(i64),
    /// Pinned at this time, which orders the pinned sessions.
    Pinned(i64),
}

/// What rho-ui knows about one session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Entry {
    /// Started or continued in rho-ui.
    owned: bool,
    mark: Option<Mark>,
}

/// Every session rho-ui knows about, in one flat list sorted by machine and
/// session ID so a lookup is a binary search.
#[derive(Debug)]
pub(crate) struct Shelves {
    entries: Vec<(SessionKey, Entry)>,
    db: StateDb,
}

/// For tests: no marks, kept in memory.
#[cfg(test)]
impl Default for Shelves {
    fn default() -> Self {
        Self::load(&StateDb::in_memory(), 0)
    }
}

impl Shelves {
    /// Loads the saved shelves, sorted by ID, dropping snoozes that have
    /// run out.
    pub(crate) fn load(db: &StateDb, now: i64) -> Self {
        // SQLite's default collation compares bytes, as `str` does.
        let entries = db.query(
            "SELECT machine, session_id, owned, settled_at, snoozed_until, pinned_at \
             FROM shelves ORDER BY machine, session_id",
            [],
            |row| {
                let mark = match (row.get(3)?, row.get(4)?, row.get(5)?) {
                    (Some(at), _, _) => Some(Mark::Settled(at)),
                    (None, Some(until), _) if until > now => Some(Mark::Snoozed(until)),
                    (None, _, Some(at)) => Some(Mark::Pinned(at)),
                    (None, _, None) => None,
                };
                let entry = Entry {
                    owned: row.get(2)?,
                    mark,
                };
                let machine = MachineId::from(row.get::<_, String>(0)?.as_str());
                let id = SessionId::from(row.get::<_, String>(1)?.as_str());
                Ok((SessionKey::new(machine, id), entry))
            },
        );
        Self {
            entries,
            db: db.clone(),
        }
    }

    /// For tests: shelves that own `ids`, sessions on this machine, so they
    /// list as active.
    #[cfg(test)]
    pub(crate) fn owning<S: AsRef<str>>(ids: impl IntoIterator<Item = S>) -> Self {
        let mut shelves = Self::default();
        for id in ids {
            shelves.own(&SessionKey::local(id.as_ref()));
        }
        shelves
    }

    /// Where `id` shows, given its last activity and the current time.
    pub(crate) fn shelf(&self, id: &SessionKey, updated: i64, now: i64) -> Shelf {
        let entry = self.entry(id);
        match entry.mark {
            Some(Mark::Settled(at)) if updated <= at => Shelf::Settled,
            Some(Mark::Snoozed(until)) if now < until => Shelf::Snoozed,
            Some(Mark::Pinned(_)) => Shelf::Active,
            _ if entry.owned => Shelf::Active,
            _ => Shelf::Settled,
        }
    }

    /// When a snoozed session wakes, in Unix seconds.
    pub(crate) fn wakes(&self, id: &SessionKey) -> Option<i64> {
        match self.entry(id).mark {
            Some(Mark::Snoozed(until)) => Some(until),
            _ => None,
        }
    }

    /// When `id` was pinned, if it is.
    pub(crate) fn pinned(&self, id: &SessionKey) -> Option<i64> {
        match self.entry(id).mark {
            Some(Mark::Pinned(at)) => Some(at),
            _ => None,
        }
    }

    /// Pins `id` to the top of the active list, and makes it rho-ui's.
    pub(crate) fn pin(&mut self, id: &SessionKey, now: i64) {
        self.change(id, |entry| {
            entry.owned = true;
            entry.mark = Some(Mark::Pinned(now));
        });
    }

    /// Unpins `id`, leaving it active.
    pub(crate) fn unpin(&mut self, id: &SessionKey) {
        self.change(id, |entry| {
            if matches!(entry.mark, Some(Mark::Pinned(_))) {
                entry.mark = None;
            }
        });
    }

    /// Marks `id` as started or continued in rho-ui.
    pub(crate) fn own(&mut self, id: &SessionKey) {
        self.change(id, |entry| entry.owned = true);
    }

    pub(crate) fn settle(&mut self, id: &SessionKey, now: i64) {
        self.change(id, |entry| entry.mark = Some(Mark::Settled(now)));
    }

    /// Snoozing a session makes it rho-ui's, so it wakes up active.
    pub(crate) fn snooze(&mut self, id: &SessionKey, until: i64) {
        self.change(id, |entry| {
            entry.owned = true;
            entry.mark = Some(Mark::Snoozed(until));
        });
    }

    /// Moves `id` to the active list, and makes it rho-ui's.
    pub(crate) fn restore(&mut self, id: &SessionKey) {
        self.change(id, |entry| {
            *entry = Entry {
                owned: true,
                mark: None,
            }
        });
    }

    /// Forgets a deleted session.
    pub(crate) fn forget(&mut self, id: &SessionKey) {
        self.change(id, |entry| *entry = Entry::default());
    }

    fn find(&self, id: &SessionKey) -> Result<usize, usize> {
        self.entries.binary_search_by(|(known, _)| known.cmp(id))
    }

    fn entry(&self, id: &SessionKey) -> Entry {
        self.find(id)
            .map(|at| self.entries[at].1)
            .unwrap_or_default()
    }

    /// Applies `change` to `id`'s entry and saves if anything changed. An
    /// entry that ends up empty is removed.
    fn change(&mut self, id: &SessionKey, change: impl FnOnce(&mut Entry)) {
        let mut entry = self.entry(id);
        let before = entry;
        change(&mut entry);
        if entry == before {
            return;
        }
        match (self.find(id), entry == Entry::default()) {
            (Ok(at), true) => {
                self.entries.remove(at);
            }
            (Ok(at), false) => self.entries[at].1 = entry,
            (Err(at), false) => self.entries.insert(at, (id.clone(), entry)),
            (Err(_), true) => {}
        }
        self.save(id, entry);
    }

    /// Writes `id`'s entry, or deletes an empty one. Best effort: losing a
    /// write only moves a session between lists.
    fn save(&self, id: &SessionKey, entry: Entry) {
        let key = (id.machine.as_str(), id.id.as_str());
        let db = &self.db;
        if entry == Entry::default() {
            db.execute(
                "DELETE FROM shelves WHERE machine = ?1 AND session_id = ?2",
                key,
            );
            return;
        }
        let (settled, snoozed, pinned) = match entry.mark {
            Some(Mark::Settled(at)) => (Some(at), None, None),
            Some(Mark::Snoozed(until)) => (None, Some(until), None),
            Some(Mark::Pinned(at)) => (None, None, Some(at)),
            None => (None, None, None),
        };
        db.execute(
            "INSERT OR REPLACE INTO shelves \
             (machine, session_id, owned, settled_at, snoozed_until, pinned_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (key.0, key.1, entry.owned, settled, snoozed, pinned),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(id: &str) -> SessionKey {
        SessionKey::local(id)
    }

    #[test]
    fn owned_settled_sessions_return_on_new_activity() {
        let mut shelves = Shelves::default();
        shelves.own(&k("a"));
        shelves.settle(&k("a"), 100);
        assert_eq!(shelves.shelf(&k("a"), 100, 200), Shelf::Settled);
        assert_eq!(shelves.shelf(&k("a"), 101, 200), Shelf::Active);
        shelves.forget(&k("a"));
        assert_eq!(
            shelves.shelf(&k("a"), 101, 200),
            Shelf::Settled,
            "forgotten is unowned"
        );
    }

    #[test]
    fn marks_replace_each_other_end_and_stay_per_machine() {
        let mut shelves = Shelves::default();
        for id in ["c", "a", "b"] {
            shelves.own(&k(id));
        }
        shelves.snooze(&k("a"), 500);
        shelves.settle(&k("a"), 10);
        assert_eq!(
            shelves.shelf(&k("a"), 5, 0),
            Shelf::Settled,
            "settle replaces snooze"
        );
        shelves.restore(&k("a"));
        assert_eq!(shelves.shelf(&k("a"), 5, 0), Shelf::Active);
        shelves.pin(&k("a"), 20);
        shelves.unpin(&k("a"));
        assert_eq!(
            (shelves.shelf(&k("a"), 5, 0), shelves.pinned(&k("a"))),
            (Shelf::Active, None),
            "unpinned stays active"
        );
        assert_eq!(shelves.shelf(&k("b"), 0, 0), Shelf::Active);
        assert_eq!(shelves.shelf(&k("c"), 0, 0), Shelf::Active);

        shelves.snooze(&k("b"), 700);
        assert_eq!(shelves.shelf(&k("b"), 0, 650), Shelf::Snoozed);
        assert_eq!(
            shelves.shelf(&k("b"), 0, 700),
            Shelf::Active,
            "the snooze ended"
        );

        // The same ID on another machine is another session.
        let there = SessionKey::new(MachineId::remote("workstation"), SessionId::from("a"));
        shelves.settle(&k("a"), 100);
        assert_eq!(shelves.shelf(&there, 100, 200), Shelf::Settled, "unowned");
        shelves.restore(&there);
        assert_eq!(shelves.shelf(&there, 100, 200), Shelf::Active);
        assert_eq!(shelves.shelf(&k("a"), 100, 200), Shelf::Settled);
    }

    #[test]
    fn shelves_survive_a_reload_and_ended_snoozes_wake_owned() {
        let db = StateDb::in_memory();
        let mut shelves = Shelves::load(&db, 0);
        shelves.own(&k("a"));
        shelves.settle(&k("a"), 100);
        shelves.snooze(&k("z"), 300);
        shelves.snooze(&k("old"), 50);
        shelves.restore(&k("b"));
        shelves.forget(&k("b"));
        shelves.pin(&k("p"), 150);

        let entry = |owned, mark| Entry { owned, mark };
        assert_eq!(
            Shelves::load(&db, 200).entries,
            vec![
                (k("a"), entry(true, Some(Mark::Settled(100)))),
                (k("old"), entry(true, None)),
                (k("p"), entry(true, Some(Mark::Pinned(150)))),
                (k("z"), entry(true, Some(Mark::Snoozed(300)))),
            ],
            "a forgotten session leaves no row"
        );
    }
}
