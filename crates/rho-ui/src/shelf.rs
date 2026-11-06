//! Settled and snoozed sessions. This is rho-ui's own organization; rho knows
//! nothing about it.
//!
//! rho-ui owns the sessions started or continued in it. Every other session,
//! such as one from rho's terminal UI, starts out settled, so the sidebar
//! doesn't fill with history from elsewhere; snoozing or restoring one makes
//! it rho-ui's. An owned session that was settled comes back when it has new
//! activity; a snoozed session comes back when its time is up. Neither needs a
//! timer: the sidebar asks [`Shelves::shelf`] whenever it rebuilds.
//!
//! The state lives in one small text file, one line per fact:
//!
//! ```text
//! owned <session id>
//! settled <session id> <unix seconds when settled>
//! snoozed <session id> <unix seconds when it wakes>
//! ```

use std::{fmt::Write as _, path::PathBuf};

use crate::store;

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
}

/// What rho-ui knows about one session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Entry {
    /// Started or continued in rho-ui.
    owned: bool,
    mark: Option<Mark>,
}

/// Every session rho-ui knows about, in one flat list sorted by session ID so
/// a lookup is a binary search.
#[derive(Debug, Default)]
pub(crate) struct Shelves {
    entries: Vec<(String, Entry)>,
    /// Where to save; `None` keeps changes in memory only (tests).
    path: Option<PathBuf>,
}

impl Shelves {
    /// Loads `$XDG_STATE_HOME/rho-ui/shelves`, or `~/.local/state/...`,
    /// dropping snoozes that have run out. A missing or unreadable file
    /// starts empty.
    pub(crate) fn load(now: i64) -> Self {
        let path = store::xdg_dir("XDG_STATE_HOME", ".local/state").map(|dir| dir.join("shelves"));
        let entries = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| parse(&text, now))
            .unwrap_or_default();
        Self { entries, path }
    }

    /// For tests: shelves that own `ids`, so they list as active.
    #[cfg(test)]
    pub(crate) fn owning<S: AsRef<str>>(ids: impl IntoIterator<Item = S>) -> Self {
        let mut shelves = Self::default();
        for id in ids {
            shelves.own(id.as_ref());
        }
        shelves
    }

    /// Where `id` shows, given its last activity and the current time.
    pub(crate) fn shelf(&self, id: &str, updated: i64, now: i64) -> Shelf {
        let entry = self.entry(id);
        match entry.mark {
            Some(Mark::Settled(at)) if updated <= at => Shelf::Settled,
            Some(Mark::Snoozed(until)) if now < until => Shelf::Snoozed,
            _ if entry.owned => Shelf::Active,
            _ => Shelf::Settled,
        }
    }

    /// When a snoozed session wakes, in Unix seconds.
    pub(crate) fn wakes(&self, id: &str) -> Option<i64> {
        match self.entry(id).mark {
            Some(Mark::Snoozed(until)) => Some(until),
            _ => None,
        }
    }

    /// Marks `id` as started or continued in rho-ui.
    pub(crate) fn own(&mut self, id: &str) {
        self.change(id, |entry| entry.owned = true);
    }

    pub(crate) fn settle(&mut self, id: &str, now: i64) {
        self.change(id, |entry| entry.mark = Some(Mark::Settled(now)));
    }

    /// Snoozing a session makes it rho-ui's, so it wakes up active.
    pub(crate) fn snooze(&mut self, id: &str, until: i64) {
        self.change(id, |entry| {
            entry.owned = true;
            entry.mark = Some(Mark::Snoozed(until));
        });
    }

    /// Moves `id` to the active list, and makes it rho-ui's.
    pub(crate) fn restore(&mut self, id: &str) {
        self.change(id, |entry| {
            *entry = Entry {
                owned: true,
                mark: None,
            }
        });
    }

    /// Forgets a deleted session.
    pub(crate) fn forget(&mut self, id: &str) {
        self.change(id, |entry| *entry = Entry::default());
    }

    fn find(&self, id: &str) -> Result<usize, usize> {
        self.entries
            .binary_search_by(|(known, _)| known.as_str().cmp(id))
    }

    fn entry(&self, id: &str) -> Entry {
        self.find(id)
            .map(|at| self.entries[at].1)
            .unwrap_or_default()
    }

    /// Applies `change` to `id`'s entry and saves if anything changed. An
    /// entry that ends up empty is removed.
    fn change(&mut self, id: &str, change: impl FnOnce(&mut Entry)) {
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
            (Err(at), false) => self.entries.insert(at, (id.to_owned(), entry)),
            (Err(_), true) => {}
        }
        self.save();
    }

    /// Best effort: losing a line only moves a session between lists. Writes
    /// a temporary file and renames it, so a crash never leaves half a file.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let mut text = String::new();
        for (id, entry) in &self.entries {
            // Writing to a String can't fail.
            if entry.owned {
                let _ = writeln!(text, "owned {id}");
            }
            let _ = match entry.mark {
                Some(Mark::Settled(at)) => writeln!(text, "settled {id} {at}"),
                Some(Mark::Snoozed(until)) => writeln!(text, "snoozed {id} {until}"),
                None => Ok(()),
            };
        }
        store::write_atomic(path, &text);
    }
}

/// Reads entries, sorted by ID. Skips lines it cannot read, so one bad line
/// costs one fact. A snooze that ended before `now` leaves the session owned
/// and active.
fn parse(text: &str, now: i64) -> Vec<(String, Entry)> {
    let mut facts: Vec<(&str, Entry)> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let (kind, id) = (fields.next()?, fields.next()?);
            let time = fields.next().and_then(|time| time.parse().ok());
            let entry = match (kind, time) {
                ("owned", _) => Entry {
                    owned: true,
                    mark: None,
                },
                ("settled", Some(at)) => Entry {
                    owned: false,
                    mark: Some(Mark::Settled(at)),
                },
                // Snoozing makes a session rho-ui's, and files from before
                // `owned` lines existed only say so this way.
                ("snoozed", Some(until)) => Entry {
                    owned: true,
                    mark: (until > now).then_some(Mark::Snoozed(until)),
                },
                _ => return None,
            };
            Some((id, entry))
        })
        .collect();
    facts.sort_by(|a, b| a.0.cmp(b.0));
    let mut entries: Vec<(String, Entry)> = Vec::with_capacity(facts.len());
    for (id, fact) in facts {
        match entries.last_mut() {
            Some((last, entry)) if last == id => {
                entry.owned |= fact.owned;
                entry.mark = entry.mark.or(fact.mark);
            }
            _ => entries.push((id.to_owned(), fact)),
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_from_elsewhere_start_settled_until_rho_ui_takes_them() {
        let mut shelves = Shelves::default();
        assert_eq!(shelves.shelf("tui", 500, 600), Shelf::Settled);
        shelves.restore("tui");
        assert_eq!(shelves.shelf("tui", 500, 600), Shelf::Active);

        shelves.snooze("other", 700);
        assert_eq!(shelves.shelf("other", 0, 650), Shelf::Snoozed);
        assert_eq!(
            shelves.shelf("other", 0, 700),
            Shelf::Active,
            "a snooze takes it"
        );
    }

    #[test]
    fn owned_settled_sessions_return_on_new_activity() {
        let mut shelves = Shelves::default();
        shelves.own("a");
        shelves.settle("a", 100);
        assert_eq!(shelves.shelf("a", 100, 200), Shelf::Settled);
        assert_eq!(shelves.shelf("a", 101, 200), Shelf::Active);
        shelves.forget("a");
        assert_eq!(
            shelves.shelf("a", 101, 200),
            Shelf::Settled,
            "forgotten is unowned"
        );
    }

    #[test]
    fn marks_replace_each_other_and_stay_findable() {
        let mut shelves = Shelves::default();
        for id in ["c", "a", "b"] {
            shelves.own(id);
        }
        shelves.snooze("a", 500);
        shelves.settle("a", 10);
        assert_eq!(
            shelves.shelf("a", 5, 0),
            Shelf::Settled,
            "settle replaces snooze"
        );
        shelves.restore("a");
        assert_eq!(shelves.shelf("a", 5, 0), Shelf::Active);
        assert_eq!(shelves.shelf("b", 0, 0), Shelf::Active);
        assert_eq!(shelves.shelf("c", 0, 0), Shelf::Active);
    }

    #[test]
    fn parsing_merges_facts_skips_bad_lines_and_ended_snoozes_still_own() {
        let text = "snoozed z 300\nnonsense\nsettled a 100\nowned a\nsnoozed old 50\nsettled c x\n";
        assert_eq!(
            parse(text, 200),
            vec![
                (
                    "a".to_owned(),
                    Entry {
                        owned: true,
                        mark: Some(Mark::Settled(100))
                    }
                ),
                (
                    "old".to_owned(),
                    Entry {
                        owned: true,
                        mark: None
                    }
                ),
                (
                    "z".to_owned(),
                    Entry {
                        owned: true,
                        mark: Some(Mark::Snoozed(300))
                    }
                ),
            ]
        );
    }
}
