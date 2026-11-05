//! Settled and snoozed threads. This is rho-ui's own organization; rho knows
//! nothing about it. Marks live in one small text file, one per line:
//!
//! ```text
//! settled <session id> <unix seconds when settled>
//! snoozed <session id> <unix seconds when it wakes>
//! ```
//!
//! A settled thread comes back when it has activity after it was settled; a
//! snoozed thread comes back when its time is up. Neither needs a timer: the
//! sidebar asks [`Shelves::shelf`] whenever it rebuilds.

use std::path::PathBuf;

/// Which list a thread shows in.
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

/// Every mark in one flat list, sorted by session ID so a lookup is a
/// binary search.
#[derive(Debug, Default)]
pub(crate) struct Shelves {
    marks: Vec<(String, Mark)>,
    /// Where to save; `None` keeps marks in memory only (tests).
    path: Option<PathBuf>,
}

impl Shelves {
    /// Loads `$XDG_STATE_HOME/rho-ui/shelves`, or `~/.local/state/...`,
    /// dropping snoozes that have run out. A missing or unreadable file
    /// starts empty.
    pub(crate) fn load(now: i64) -> Self {
        let path = state_dir().map(|dir| dir.join("shelves"));
        let marks = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| parse(&text, now))
            .unwrap_or_default();
        Self { marks, path }
    }

    /// Where `id` shows, given its last activity and the current time.
    pub(crate) fn shelf(&self, id: &str, updated: i64, now: i64) -> Shelf {
        match self.find(id).ok().map(|at| self.marks[at].1) {
            Some(Mark::Settled(at)) if updated <= at => Shelf::Settled,
            Some(Mark::Snoozed(until)) if now < until => Shelf::Snoozed,
            _ => Shelf::Active,
        }
    }

    pub(crate) fn settle(&mut self, id: &str, now: i64) {
        self.set(id, Some(Mark::Settled(now)));
    }

    pub(crate) fn snooze(&mut self, id: &str, until: i64) {
        self.set(id, Some(Mark::Snoozed(until)));
    }

    /// Moves `id` back to the active list.
    pub(crate) fn restore(&mut self, id: &str) {
        self.set(id, None);
    }

    fn find(&self, id: &str) -> Result<usize, usize> {
        self.marks
            .binary_search_by(|(marked, _)| marked.as_str().cmp(id))
    }

    fn set(&mut self, id: &str, mark: Option<Mark>) {
        match (self.find(id), mark) {
            (Ok(at), Some(mark)) => self.marks[at].1 = mark,
            (Ok(at), None) => {
                self.marks.remove(at);
            }
            (Err(at), Some(mark)) => self.marks.insert(at, (id.to_owned(), mark)),
            (Err(_), None) => return,
        }
        self.save();
    }

    /// Best effort: losing a mark only moves a thread back to the active
    /// list. Writes a temporary file and renames it, so a crash never
    /// leaves half a file.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let text: String = self
            .marks
            .iter()
            .map(|(id, mark)| match mark {
                Mark::Settled(at) => format!("settled {id} {at}\n"),
                Mark::Snoozed(until) => format!("snoozed {id} {until}\n"),
            })
            .collect();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let temporary = path.with_extension("tmp");
        if std::fs::write(&temporary, text).is_ok() {
            let _ = std::fs::rename(&temporary, path);
        }
    }
}

/// Reads marks, sorted by ID. Skips lines it cannot read, so one bad line
/// costs one mark, and snoozes that ended before `now`.
fn parse(text: &str, now: i64) -> Vec<(String, Mark)> {
    let mut marks: Vec<(String, Mark)> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let (kind, id, time) = (fields.next()?, fields.next()?, fields.next()?);
            let time = time.parse().ok()?;
            let mark = match kind {
                "settled" => Mark::Settled(time),
                "snoozed" if time > now => Mark::Snoozed(time),
                _ => return None,
            };
            Some((id.to_owned(), mark))
        })
        .collect();
    marks.sort_by(|a, b| a.0.cmp(&b.0));
    marks.dedup_by(|a, b| a.0 == b.0);
    marks
}

fn state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(base.join("rho-ui"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_threads_return_on_new_activity() {
        let mut shelves = Shelves::default();
        shelves.settle("a", 100);
        assert_eq!(shelves.shelf("a", 90, 200), Shelf::Settled);
        assert_eq!(shelves.shelf("a", 100, 200), Shelf::Settled);
        assert_eq!(shelves.shelf("a", 101, 200), Shelf::Active);
        assert_eq!(shelves.shelf("b", 90, 200), Shelf::Active);
    }

    #[test]
    fn snoozed_threads_wake_on_time_and_marks_replace_each_other() {
        let mut shelves = Shelves::default();
        shelves.snooze("a", 500);
        shelves.settle("c", 1);
        shelves.settle("b", 1);
        assert_eq!(shelves.shelf("a", 0, 499), Shelf::Snoozed);
        assert_eq!(shelves.shelf("a", 0, 500), Shelf::Active);
        shelves.settle("a", 10);
        assert_eq!(
            shelves.shelf("a", 5, 0),
            Shelf::Settled,
            "settle replaces snooze"
        );
        shelves.restore("a");
        assert_eq!(shelves.shelf("a", 5, 0), Shelf::Active);
        assert_eq!(
            shelves.shelf("b", 0, 0),
            Shelf::Settled,
            "other marks stay findable"
        );
        assert_eq!(shelves.shelf("c", 0, 0), Shelf::Settled);
    }

    #[test]
    fn parsing_sorts_and_skips_bad_lines_and_ended_snoozes() {
        let text = "snoozed z 300\nnonsense\nsettled a 100\nsnoozed old 50\nsettled c notanumber\n";
        assert_eq!(
            parse(text, 200),
            vec![
                ("a".to_owned(), Mark::Settled(100)),
                ("z".to_owned(), Mark::Snoozed(300)),
            ]
        );
    }
}
