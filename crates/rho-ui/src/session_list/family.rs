//! Sessions a session started through session control, as rho's `sessions`
//! tool does, fold under it: one level deep, under the top-most session up
//! the parents that the list holds. [`Families`] works out which row each
//! folds under, [`SessionList::shelve`] puts each family on its starter's
//! shelf, and [`SessionList::push_family`] lays one out under its starter.

use std::collections::{HashMap, HashSet};

use crate::{
    machines::SessionKey,
    shelf::{Shelf, Shelves},
};

use super::{Busy, Item, Row, SessionList};

/// Where a row sits in the list, as of the last rebuild.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    /// On its own: nothing the list holds started it.
    Top,
    /// Folded under this session, the top-most up its parents that the
    /// list holds.
    Started(SessionKey),
}

/// Whether a starter's fold shows the sessions it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fold {
    Open,
    Closed,
}

/// The line under a starter's row: how many of the sessions it started
/// the list shows, how many of those are busy, and whether they show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FoldLine {
    /// The starter's index into the rows.
    pub(crate) starter: usize,
    pub(crate) sessions: usize,
    pub(crate) busy: Busy,
    pub(crate) fold: Fold,
}

/// Which rows fold under which, as of one rebuild.
pub(super) struct Families {
    /// For each row, the row it folds under; see [`starters`].
    starters: Vec<Option<usize>>,
    /// The rows that fold under each starter, newest first.
    members: HashMap<usize, Vec<usize>>,
}

impl Families {
    /// Works out who started whom among `rows`.
    pub(super) fn of(rows: &[Row]) -> Self {
        let parents: Vec<_> = rows
            .iter()
            .map(|row| (&row.id, row.parent.as_ref()))
            .collect();
        let starters = starters(&parents);
        let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
        for (ix, starter) in starters.iter().enumerate() {
            if let Some(starter) = *starter {
                members.entry(starter).or_default().push(ix);
            }
        }
        Self { starters, members }
    }

    /// The rows that fold under row `ix`, newest first: none unless it
    /// started some.
    pub(super) fn members(&self, ix: usize) -> &[usize] {
        self.members.get(&ix).map_or(&[][..], Vec::as_slice)
    }
}

impl SessionList {
    /// Opens the fold of `starter`, a session that started others, or
    /// closes it. Open folds last this run.
    pub(crate) fn toggle_fold(&mut self, starter: &SessionKey) {
        if !self.open_folds.remove(starter) {
            self.open_folds.insert(starter.clone());
        }
    }

    /// Opens the fold session `id` is under, as it comes on screen: at the
    /// next rebuild that lists it. The user can close it again.
    pub(crate) fn reveal(&mut self, id: &SessionKey) {
        self.reveal = Some(id.clone());
    }

    /// Opens the fold the session to reveal is under, once it is listed.
    pub(super) fn open_revealed(&mut self, families: &Families) {
        let Some(id) = &self.reveal else {
            return;
        };
        let Some(ix) = self.rows.iter().position(|row| row.id == *id) else {
            return;
        };
        if let Some(starter) = families.starters[ix] {
            self.open_folds.insert(self.rows[starter].id.clone());
        }
        self.reveal = None;
    }

    /// Puts every row on its shelf, as of `now`, and sets what it shows.
    /// A family is on its starter's shelf, which takes the marks: active
    /// while any of it is busy, and dated by its newest session. `watching`
    /// holds the sessions whose agent watches a pull request.
    pub(super) fn shelve(
        &mut self,
        families: &Families,
        shelves: &Shelves,
        now: i64,
        watching: &HashSet<SessionKey>,
    ) {
        for ix in 0..self.rows.len() {
            if families.starters[ix].is_some() {
                continue;
            }
            let (rows, members) = (&self.rows, families.members(ix));
            let busy = |m: &usize| !rows[*m].activity.is_idle();
            let updated = members
                .iter()
                .fold(rows[ix].updated, |newest, &m| newest.max(rows[m].updated));
            let shelf = if busy(&ix) || members.iter().any(busy) {
                Shelf::Active
            } else {
                shelves.shelf(&rows[ix].id, updated, now)
            };
            self.rows[ix].shelf = shelf;
        }
        for ix in 0..self.rows.len() {
            let (shelf, family, pinned) = match families.starters[ix] {
                Some(starter) => {
                    let starter = &self.rows[starter];
                    (starter.shelf, Family::Started(starter.id.clone()), None)
                }
                None => {
                    let row = &self.rows[ix];
                    (row.shelf, Family::Top, shelves.pinned(&row.id))
                }
            };
            let row = &mut self.rows[ix];
            row.shelf = shelf;
            row.family = family;
            row.pinned = pinned;
            row.done = self.unseen.contains(&row.id);
            row.watching = row.shelf == Shelf::Active && watching.contains(&row.id);
        }
    }

    /// Pushes the fold line under top row `starter`, if it started any
    /// sessions (`family`), then, while it's open, those that show, laid
    /// out by the starter's shelf: all of them when the starter matches
    /// the search (`matched`, by row); else only those that match, with
    /// the fold open.
    pub(super) fn push_family(
        &self,
        items: &mut Vec<Item>,
        starter: usize,
        family: &[usize],
        matched: &[bool],
    ) {
        if family.is_empty() {
            return;
        }
        let shelf = self.rows[starter].shelf;
        let mut shown: Vec<usize> = family
            .iter()
            .copied()
            .filter(|&ix| matched[starter] || matched[ix])
            .collect();
        if shelf == Shelf::Active {
            // Stable, so sessions that tie keep their order, newest first.
            shown.sort_by_key(|&ix| self.rows[ix].attention());
        }
        let fold = if !matched[starter] || self.open_folds.contains(&self.rows[starter].id) {
            Fold::Open
        } else {
            Fold::Closed
        };
        let mut busy = Busy::default();
        for &ix in &shown {
            busy.add(self.rows[ix].activity);
        }
        items.push(Item::Fold(FoldLine {
            starter,
            sessions: shown.len(),
            busy,
            fold,
        }));
        if fold == Fold::Open {
            items.extend(shown.into_iter().map(|ix| Item::row(shelf, ix)));
        }
    }
}

/// For each of `rows`, given as its session and the session that started
/// it, the index of the row it folds under: the top-most up its parents
/// that `rows` holds, or `None` for a row that sits on its own. A parent
/// `rows` lacks ends the walk, as one on another machine does: a key
/// names its machine. A row whose parents loop sits on its own.
fn starters(rows: &[(&SessionKey, Option<&SessionKey>)]) -> Vec<Option<usize>> {
    let index: HashMap<&SessionKey, usize> = rows
        .iter()
        .enumerate()
        .map(|(ix, &(id, _))| (id, ix))
        .collect();
    let parent = |ix: usize| rows[ix].1.and_then(|parent| index.get(parent).copied());
    (0..rows.len())
        .map(|ix| {
            let mut seen = HashSet::from([ix]);
            let mut top = None;
            let mut next = parent(ix);
            while let Some(up) = next {
                if !seen.insert(up) {
                    return None;
                }
                top = Some(up);
                next = parent(up);
            }
            top
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    #[test]
    fn a_row_folds_under_the_top_most_starter_the_list_holds() {
        let there = SessionKey::new(MachineId::remote("box"), "lead".into());
        // Each row's session, and the session that started it.
        let table: [(&str, Option<&str>, Option<usize>); 8] = [
            ("lead", None, None),
            ("mid", Some("lead"), Some(0)),
            // Two levels down, it still folds under the top.
            ("leaf", Some("mid"), Some(0)),
            // Its starter isn't listed.
            ("orphan", Some("gone"), None),
            // Parents that loop, or run into a loop: each on its own.
            ("a", Some("b"), None),
            ("b", Some("a"), None),
            ("tail", Some("a"), None),
            ("own", Some("own"), None),
        ];
        let keys: Vec<(SessionKey, Option<SessionKey>)> = table
            .iter()
            .map(|&(id, parent, _)| (SessionKey::local(id), parent.map(SessionKey::local)))
            .chain([(SessionKey::local("far"), Some(there))])
            .collect();
        let rows: Vec<_> = keys.iter().map(|(id, parent)| (id, parent.as_ref())).collect();
        let expected: Vec<_> = table
            .iter()
            .map(|&(_, _, starter)| starter)
            // The same session ID on another machine is another session.
            .chain([None])
            .collect();
        assert_eq!(starters(&rows), expected);
    }
}
