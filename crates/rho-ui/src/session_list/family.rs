//! Sessions a session started through session control, as rho's `sessions`
//! tool does, fold under it: one level deep, under the top-most session up
//! the parents that the list holds. [`Families`] works out which row each
//! folds under and what each family comes to ([`FamilyMarks`]), which
//! shelves, ranks and dates it, and [`SessionList::push_family`] lays one
//! out under its starter.

use std::collections::{HashMap, HashSet};

use crate::{machines::SessionKey, shelf::Shelf};

use super::{Activity, Attention, Item, Passes, Row, SessionList};

/// Where a row sits in the list, as of the last rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    /// On its own, over the sessions it started if any, with what the
    /// family comes to.
    Top(FamilyMarks),
    /// Folded under the top-most session up its parents that the list
    /// holds.
    Started,
}

/// What a top row's family, the row and the sessions under it, comes to:
/// worked out once a rebuild, it places, dates and ranks the family, and
/// says what moves its row offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FamilyMarks {
    /// The newest last activity in it, in Unix seconds.
    pub(super) updated: i64,
    /// The most pressing in it, which ranks it on the active shelf.
    pub(super) attention: Attention,
    /// Any session of it is busy, which keeps it active.
    pub(crate) busy: bool,
}

/// The most pressing thing of the sessions under a fold: how many wait for
/// the user, else how many are Done, else how many work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pressing {
    Waiting(usize),
    Done(usize),
    Working(usize),
}

impl Pressing {
    /// The one thing a fold line says of `rows`, the sessions that show
    /// under it.
    fn of<'a>(rows: impl Iterator<Item = &'a Row>) -> Option<Self> {
        let (mut waiting, mut done, mut working) = (0, 0, 0);
        for row in rows {
            match row.attention() {
                Attention::Asks => waiting += 1,
                Attention::Done => done += 1,
                Attention::Runs if matches!(row.activity, Activity::Working(_)) => working += 1,
                Attention::Rests | Attention::Runs => {}
            }
        }
        if waiting > 0 {
            Some(Self::Waiting(waiting))
        } else if done > 0 {
            Some(Self::Done(done))
        } else if working > 0 {
            Some(Self::Working(working))
        } else {
            None
        }
    }
}

/// Whether a starter's fold shows the sessions it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fold {
    Open,
    Closed,
    /// Open because the search or the filter shows some of them and not
    /// the starter, which shows only for them: the user can't close it.
    Showing,
}

/// The line under a starter's row: how many of the sessions it started
/// show under it, the most pressing thing of those, and whether they
/// show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FoldLine {
    /// The starter's index into the rows.
    pub(crate) starter: usize,
    pub(crate) sessions: usize,
    pub(crate) pressing: Option<Pressing>,
    pub(crate) fold: Fold,
}

impl Row {
    /// Whether it is busy, or for a starter, whether any of its family is,
    /// as of the last rebuild. Its row offers no move then, nor Delete.
    pub(crate) fn busy(&self) -> bool {
        match self.family {
            Family::Top(marks) => marks.busy,
            Family::Started => !self.activity.is_idle(),
        }
    }
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

    /// The row that row `ix` folds under, if any.
    pub(super) fn starter(&self, ix: usize) -> Option<usize> {
        self.starters[ix]
    }

    /// The rows that fold under row `ix`, newest first: none unless it
    /// started some.
    pub(super) fn members(&self, ix: usize) -> &[usize] {
        self.members.get(&ix).map_or(&[][..], Vec::as_slice)
    }

    /// The marks of top row `top`'s family, from what its rows do, are
    /// Done and watch as they stand.
    pub(super) fn marks(&self, rows: &[Row], top: usize) -> FamilyMarks {
        let starter = &rows[top];
        let mut marks = FamilyMarks {
            updated: starter.updated,
            attention: starter.attention(),
            busy: !starter.activity.is_idle(),
        };
        for row in self.members(top).iter().map(|&ix| &rows[ix]) {
            marks.updated = marks.updated.max(row.updated);
            marks.attention = marks.attention.min(row.attention());
            marks.busy |= !row.activity.is_idle();
        }
        marks
    }

    /// The rows under top row `top` that show, by `passes`: those the
    /// filter shows that match the search, or whose starter does.
    pub(super) fn drawn<'a>(
        &'a self,
        top: usize,
        passes: &'a [Passes],
    ) -> impl Iterator<Item = usize> + 'a {
        self.members(top)
            .iter()
            .copied()
            .filter(move |&ix| passes[ix].filter && (passes[ix].search || passes[top].search))
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

    /// Opens the fold session `id`, on screen now, is under, at the first
    /// rebuild that lists it under its starter, which may come a poll
    /// later. The user can close it again. Another session, or `None`,
    /// drops the request.
    pub(crate) fn reveal(&mut self, id: Option<&SessionKey>) {
        self.reveal = id.cloned();
    }

    /// The sessions of the family `starter` heads that a move marks,
    /// itself first: each one the daemon lists as saved, so they stay
    /// where the family was put if they lose their starter. The daemon
    /// keeps no mark for a session it has saved nothing of, such as a
    /// running subagent's; under its starter that one moves with it
    /// anyway.
    pub(crate) fn family(&self, starter: &SessionKey) -> Vec<SessionKey> {
        let families = Families::of(&self.rows);
        let top = self.rows.iter().position(|row| row.id == *starter);
        let members = top.map_or(&[][..], |ix| families.members(ix));
        let saved = members
            .iter()
            .map(|&ix| &self.rows[ix])
            .filter(|row| row.saved);
        std::iter::once(starter.clone())
            .chain(saved.map(|row| row.id.clone()))
            .collect()
    }

    /// Readies the folds for a rebuild: forgets those of sessions the list
    /// no longer holds on their own, and opens the one the session to
    /// reveal is under, once it is under one. A starter keeps its fold
    /// while none of its sessions is listed, as between a job's end and
    /// the saved list that names it.
    pub(super) fn keep_folds(&mut self, families: &Families) {
        let rows = &self.rows;
        let position = |id: &SessionKey| rows.iter().position(|row| row.id == *id);
        self.open_folds
            .retain(|id| position(id).is_some_and(|ix| families.starter(ix).is_none()));
        let revealed = self.reveal.as_ref().and_then(position);
        if let Some(starter) = revealed.and_then(|ix| families.starter(ix)) {
            self.open_folds.insert(rows[starter].id.clone());
            self.reveal = None;
        }
    }

    /// Pushes the fold line under top row `top` when any of the sessions
    /// under it show (see [`Families::drawn`]), then, while it's open,
    /// those, laid out on its shelf.
    pub(super) fn push_family(
        &self,
        items: &mut Vec<Item>,
        families: &Families,
        top: usize,
        passes: &[Passes],
    ) {
        let mut drawn: Vec<usize> = families.drawn(top, passes).collect();
        if drawn.is_empty() {
            return;
        }
        let starter = &self.rows[top];
        if starter.shelf == Shelf::Active {
            // Stable, so sessions that tie keep their order, newest first.
            drawn.sort_by_key(|&ix| self.rows[ix].attention());
        }
        let fold = match (passes[top].both(), self.open_folds.contains(&starter.id)) {
            (false, _) => Fold::Showing,
            (true, true) => Fold::Open,
            (true, false) => Fold::Closed,
        };
        items.push(Item::Fold(FoldLine {
            starter: top,
            sessions: drawn.len(),
            pressing: Pressing::of(drawn.iter().map(|&ix| &self.rows[ix])),
            fold,
        }));
        if fold != Fold::Closed {
            items.extend(drawn.into_iter().map(|ix| Item::row(starter.shelf, ix)));
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
    use std::{collections::HashSet, path::Path};

    use web_time::Instant;

    use rho_ui_core::{JobActivity, SessionMark};

    use super::*;
    use crate::{
        leads::Kind,
        machines::{MachineId, Place},
        session_list::{
            tests::{here, id, job, mark, rebuild, saved, shown},
            Filter, JobSession, SavedSession,
        },
    };

    /// A saved session that session `parent` started.
    fn started(text: &str, title: &str, updated: i64, parent: &str) -> SavedSession {
        SavedSession {
            parent: Some(id(parent)),
            ..saved(text, Some(title), updated)
        }
    }

    /// A move of a family asks the daemon to mark the sessions it has
    /// saved, not one it only runs, which it would refuse.
    #[test]
    fn a_move_marks_the_family_sessions_the_daemon_has_saved() {
        let mut list = SessionList::default();
        let sessions = vec![started("w1", "Write", 4, "lead"), saved("lead", None, 3)];
        list.set_saved(&here(), sessions);
        let running = JobSession {
            parent: Some(id("lead")),
            ..job("w2", JobActivity::Working, None)
        };
        list.set_jobs(&here(), vec![running], Instant::now(), 5);
        assert_eq!(list.family(&id("lead")), [id("lead"), id("w1")]);
    }

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
        let rows: Vec<_> = keys
            .iter()
            .map(|(id, parent)| (id, parent.as_ref()))
            .collect();
        let expected: Vec<_> = table
            .iter()
            .map(|&(_, _, starter)| starter)
            // The same session ID on another machine is another session.
            .chain([None])
            .collect();
        assert_eq!(starters(&rows), expected);
    }

    /// A session's started sessions are those it started itself, not those
    /// they started in turn, which fold under the same top row; a starter
    /// another session started has its own.
    #[test]
    fn started_by_lists_only_the_sessions_a_session_started_itself() {
        let mut list = SessionList::default();
        list.set_saved(
            &here(),
            vec![
                started("leaf", "Check", 5, "w1"),
                started("w2", "Test", 4, "lead"),
                started("w1", "Write", 3, "lead"),
                saved("lead", Some("Lead"), 2),
            ],
        );
        let table: [(&str, &[&str]); 2] = [("lead", &["w2", "w1"]), ("w1", &["leaf"])];
        for (starter, expected) in table {
            let got: Vec<SessionKey> = list
                .started_by(&id(starter))
                .map(|row| row.id.clone())
                .collect();
            let expected: Vec<SessionKey> = expected.iter().copied().map(id).collect();
            assert_eq!(got, expected, "started by {starter}");
        }
    }

    /// Sessions a session started fold under it, on its shelf: a settled
    /// family stays settled together, dated by its newest session, until
    /// one of it is busy, a session with no saved row yet included, which
    /// makes it active. It ranks by its most pressing session, and its fold
    /// line says who asks, else who is Done, else who works. The session on
    /// screen opens the fold it is under once the list knows its starter.
    #[test]
    fn a_family_lives_on_its_starters_shelf_and_a_busy_member_keeps_it_active() {
        let mut list = SessionList::default();
        list.set_saved(
            &here(),
            vec![
                saved("other", Some("Other"), 6),
                started("w2", "Test", 5, "lead"),
                started("w1", "Write", 4, "lead"),
                saved("solo", Some("Solo"), 4),
                saved("lead", Some("Lead"), 3),
            ],
        );
        // At rest, the family sits by its newest session, over Solo.
        rebuild(&mut list, 10);
        assert_eq!(shown(&list), ["Other", "Lead", "› 2 started", "Solo"]);
        mark(&mut list, "lead", SessionMark::Done { at: 10 });
        mark(&mut list, "solo", SessionMark::Done { at: 10 });
        rebuild(&mut list, 10);
        assert_eq!(shown(&list), ["Other", "~", "[Settled 2]"]);
        list.toggle(Shelf::Settled);
        list.toggle_fold(&id("lead"));
        rebuild(&mut list, 10);
        // Newer than Solo by its newest session, though Lead itself isn't.
        let settled = ["Lead", "⌄ 2 started", "  Test", "  Write", "Solo"];
        assert_eq!(shown(&list)[3..], settled);

        let asks = JobSession {
            parent: Some(id("lead")),
            ..job("w3", JobActivity::Waiting, None)
        };
        let writes = job("w1", JobActivity::Working, None);
        list.set_jobs(&here(), vec![asks, writes], Instant::now(), 11);
        rebuild(&mut list, 11);
        let asking = [
            "Lead",
            "⌄ 3 started, 1 waiting",
            "  job w3",
            "  Test",
            "  Write",
        ];
        assert_eq!(shown(&list)[..5], asking);
        assert_eq!(shown(&list)[5..], ["Other", "~", "[Settled 1]", "Solo"]);

        // Write's turn ends off screen while Test works: Done beats work,
        // and ranks the family over Other at rest.
        let testing = || job("w2", JobActivity::Working, None);
        list.set_jobs(&here(), vec![testing()], Instant::now(), 12);
        list.toggle_fold(&id("lead"));
        rebuild(&mut list, 12);
        assert_eq!(shown(&list)[..3], ["Lead", "› 2 started, 1 done", "Other"]);

        // On screen before the list says who started it, it shows on its
        // own until a poll says, which opens the fold.
        list.reveal(Some(&id("w4")));
        let unknown = job("w4", JobActivity::Working, None);
        list.set_jobs(&here(), vec![testing(), unknown], Instant::now(), 13);
        rebuild(&mut list, 13);
        assert_eq!(
            shown(&list)[..4],
            ["Lead", "› 2 started, 1 done", "Other", "job w4"]
        );
        let known = JobSession {
            parent: Some(id("lead")),
            ..job("w4", JobActivity::Working, None)
        };
        list.set_jobs(&here(), vec![testing(), known], Instant::now(), 14);
        rebuild(&mut list, 14);
        let revealed = [
            "Lead",
            "⌄ 3 started, 1 done",
            "  Write",
            "  job w4",
            "  Test",
        ];
        assert_eq!(shown(&list)[..5], revealed);
    }

    /// Active leads list first, under their header, each over the sessions
    /// it started, then the other active sessions, which alone the active
    /// shelf counts; a settled lead lists on its shelf. A lead is in no
    /// project, so a project filter that hides the others leaves it.
    #[test]
    fn active_leads_list_apart_over_their_families() {
        let lead = |text: &str, title: &str, updated: i64| SavedSession {
            kind: Kind::Lead,
            ..saved(text, Some(title), updated)
        };
        let mut list = SessionList::default();
        list.set_saved(
            &here(),
            vec![
                saved("other", Some("Other"), 6),
                started("w1", "Write", 5, "lead"),
                lead("lead", "Lead", 3),
                lead("past", "Past", 2),
            ],
        );
        mark(&mut list, "past", SessionMark::Done { at: 10 });
        rebuild(&mut list, 10);
        let listed = [
            "[Leads]",
            "Lead",
            "› 1 started",
            "[Sessions]",
            "Other",
            "~",
            "[Settled 1]",
        ];
        assert_eq!(shown(&list), listed);
        assert_eq!(list.count(Shelf::Active), 1);

        // Kept to a project none of them is in.
        let mut filter = Filter::default();
        filter.toggle_project(&Place::new(here(), "/srv"));
        list.set_filter(filter);
        rebuild(&mut list, 10);
        assert_eq!(shown(&list), ["[Leads]", "Lead", "~", "[Settled 1]"]);
    }

    /// A search or a filter that shows only sessions a session started
    /// shows their starter with the fold held open and only those sessions;
    /// one that shows the starter shows the rest of its family the filter
    /// shows, folded as the user left it.
    #[test]
    fn a_search_that_matches_a_member_shows_its_starter_with_the_fold_open() {
        let mut list = SessionList::default();
        list.set_saved(
            &here(),
            vec![
                SavedSession {
                    workspace: 1,
                    ..started("w2", "Test the parser", 5, "lead")
                },
                started("w1", "Write the parser", 4, "lead"),
                saved("lead", Some("Plan the release"), 3),
                saved("other", Some("Other work"), 2),
            ],
        );
        let relist = |list: &mut SessionList| {
            let roots = |ix: usize| match ix {
                0 => ("atlas", Path::new("/w")),
                _ => ("srv", Path::new("/srv")),
            };
            list.rebuild(10, &HashSet::new(), roots);
        };
        list.set_query("write");
        relist(&mut list);
        let write = ["Plan the release", "• 1 started", "  Write the parser"];
        assert_eq!(shown(&list), write);
        assert_eq!(list.count(Shelf::Active), 1);

        list.set_query("plan");
        relist(&mut list);
        assert_eq!(shown(&list), ["Plan the release", "› 2 started"]);

        // Kept to the starter's project: only Write folds under it, and
        // the line says nothing of Test, which asks out of sight.
        list.set_query("");
        let asks = job("w2", JobActivity::Waiting, None);
        list.set_jobs(&here(), vec![asks], Instant::now(), 10);
        let mut filter = Filter::default();
        filter.toggle_project(&Place::new(here(), "/w"));
        list.set_filter(filter.clone());
        relist(&mut list);
        assert_eq!(
            shown(&list),
            ["Plan the release", "› 1 started", "Other work"]
        );

        // Kept to another project, the starter shows only for Test.
        filter.toggle_project(&Place::new(here(), "/w"));
        filter.toggle_project(&Place::new(here(), "/srv"));
        list.set_filter(filter);
        relist(&mut list);
        let srv = [
            "Plan the release",
            "• 1 started, 1 waiting",
            "  Test the parser",
        ];
        assert_eq!(shown(&list), srv);
    }
}
