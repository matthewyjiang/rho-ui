//! Sessions a session started through session control, as rho's `sessions`
//! tool does, fold under it: one level deep, under the top-most session up
//! the parents that the list holds. [`Families`] works out which row each
//! folds under and what each family comes to ([`FamilyMarks`]), which
//! shelves, ranks and dates it, and [`SessionList::push_family`] lays one
//! out under its starter. An active family's fold leaves out the sessions
//! of it that are settled or snoozed on their own ([`Row::away`]), but for
//! the one on screen.

use std::collections::{HashMap, HashSet};

use crate::{
    leads::Kind,
    machines::SessionKey,
    shelf::{self, Shelf},
};

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
    /// Where it is when none of it is busy: by its starter's mark and its
    /// newest activity.
    pub(super) rest: Shelf,
}

/// The most pressing thing of the sessions under a fold: how many wait for
/// the user, else how many are Done, else how many work. A lead's are
/// never Done here: see [`Row::attention_under`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pressing {
    Waiting(usize),
    Done(usize),
    Working(usize),
}

impl Pressing {
    /// The one thing a fold line says of `rows`, the sessions that show
    /// under a starter of kind `starter`.
    fn of<'a>(rows: impl Iterator<Item = &'a Row>, starter: Kind) -> Option<Self> {
        let (mut waiting, mut done, mut working) = (0, 0, 0);
        for row in rows {
            match row.attention_under(starter) {
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

/// The session on screen, as the sidebar last said: its row shows under
/// its starter even when it is away from the fold ([`Row::away`]).
pub(super) struct OnScreen {
    pub(super) id: SessionKey,
    /// Whether a rebuild has opened the fold it is under: once, so the user
    /// can close it again.
    revealed: bool,
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

    /// The shelf it would be on by itself as of `now`, as a session under a
    /// family whose shelf at rest is `rest`, when that is Settled or Snoozed
    /// and not `rest`: settled or snoozed apart from its family, as rho
    /// settles a session once its last linked pull request merges. One
    /// settled with a settled family, or snoozed with a snoozed one, isn't.
    pub(super) fn apart_from(&self, rest: Shelf, now: i64) -> Option<Shelf> {
        if !self.activity.is_idle() {
            return None;
        }
        Some(shelf::shelf(self.mark, self.updated, now))
            .filter(|&own| own != Shelf::Active && own != rest)
    }

    /// Whether it is settled or snoozed apart from its family ([`Row::apart`])
    /// while the family shows on the active list: its starter's fold leaves
    /// it out, but for a search or while it is on screen, and nothing counts
    /// it as wanting the user.
    pub(crate) fn away(&self) -> bool {
        self.shelf == Shelf::Active && self.apart.is_some()
    }

    /// What it presses on the family it folds in, under a starter of kind
    /// `starter`: its own, but a lead reads its sessions' results itself,
    /// so one of those that is Done rests there. Its own row still says
    /// Done.
    fn attention_under(&self, starter: Kind) -> Attention {
        match (starter, self.attention()) {
            (Kind::Lead, Attention::Done) => Attention::Rests,
            (_, attention) => attention,
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

    /// The marks of top row `top`'s family as of `now`, from what its rows
    /// do, are Done and watch as they stand, each session under it by what
    /// it presses on the family ([`Row::attention_under`]). One settled or
    /// snoozed apart from it ([`Row::apart`]) presses on nothing: on the
    /// active shelf, where alone the family ranks, it is away.
    pub(super) fn marks(&self, rows: &[Row], top: usize, now: i64) -> FamilyMarks {
        let starter = &rows[top];
        let members = || self.members(top).iter().map(|&ix| &rows[ix]);
        let updated = members()
            .map(|row| row.updated)
            .fold(starter.updated, i64::max);
        let rest = shelf::shelf(starter.mark, updated, now);
        let attention = members()
            .filter(|row| row.apart_from(rest, now).is_none())
            .map(|row| row.attention_under(starter.kind))
            .fold(starter.attention(), Ord::min);
        FamilyMarks {
            updated,
            attention,
            busy: std::iter::once(starter)
                .chain(members())
                .any(|row| !row.activity.is_idle()),
            rest,
        }
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

    /// Puts session `id` on screen, or none. The fold it is under opens at
    /// the first rebuild that lists it under its starter, which may come a
    /// poll later, and the user can close it again. While it is on screen
    /// the fold shows and counts it even when it is away ([`Row::away`]),
    /// as when it was opened from its lead's Work.
    pub(crate) fn set_on_screen(&mut self, id: Option<&SessionKey>) {
        self.on_screen = id.map(|id| OnScreen {
            id: id.clone(),
            revealed: false,
        });
    }

    /// The sessions of the family `starter` heads that a move marks,
    /// itself first: each one the daemon lists as saved, so they stay
    /// where the family was put if they lose their starter. The daemon
    /// keeps no mark for a session it has saved nothing of, such as a
    /// running subagent's; under its starter that one moves with it
    /// anyway. One settled or snoozed apart from the family
    /// ([`Row::away`]) keeps its own mark.
    pub(crate) fn family(&self, starter: &SessionKey) -> Vec<SessionKey> {
        std::iter::once(starter.clone())
            .chain(self.moved_with(starter))
            .collect()
    }

    /// The sessions under `starter` that a pin of it brings back, as
    /// Restore would: those [`Self::family`] marks when the family rests
    /// snoozed, which were snoozed with it. None otherwise: an active
    /// family's are where they are, and in a settled family one settled
    /// with it can't be told apart from one rho settled on its own when its
    /// pull request merged.
    pub(crate) fn pinned_with(&self, starter: &SessionKey) -> Vec<SessionKey> {
        let snoozed = self.row(starter).is_some_and(
            |row| matches!(row.family, Family::Top(marks) if marks.rest == Shelf::Snoozed),
        );
        if snoozed {
            self.moved_with(starter)
        } else {
            Vec::new()
        }
    }

    /// The sessions under `starter` that a move of it marks; see
    /// [`Self::family`].
    fn moved_with(&self, starter: &SessionKey) -> Vec<SessionKey> {
        let families = Families::of(&self.rows);
        let top = self.rows.iter().position(|row| row.id == *starter);
        let members = top.map_or(&[][..], |ix| families.members(ix));
        members
            .iter()
            .map(|&ix| &self.rows[ix])
            .filter(|row| row.saved && row.apart.is_none())
            .map(|row| row.id.clone())
            .collect()
    }

    /// The session on screen, as [`Self::set_on_screen`] last said.
    pub(crate) fn on_screen(&self) -> Option<&SessionKey> {
        self.on_screen.as_ref().map(|shown| &shown.id)
    }

    /// Readies the folds for a rebuild: forgets those of sessions the list
    /// no longer holds on their own, and opens the one the session on
    /// screen is under, once it is under one. A starter keeps its fold
    /// while none of its sessions is listed, as between a job's end and
    /// the saved list that names it.
    pub(super) fn keep_folds(&mut self, families: &Families) {
        let rows = &self.rows;
        let position = |id: &SessionKey| rows.iter().position(|row| row.id == *id);
        self.open_folds
            .retain(|id| position(id).is_some_and(|ix| families.starter(ix).is_none()));
        let Some(shown) = self.on_screen.as_mut().filter(|shown| !shown.revealed) else {
            return;
        };
        if let Some(starter) = position(&shown.id).and_then(|ix| families.starter(ix)) {
            self.open_folds.insert(rows[starter].id.clone());
            shown.revealed = true;
        }
    }

    /// The rows under top row `top` that show, by `passes`: those the
    /// filter shows that match the search, or whose starter does. One away
    /// from the fold ([`Row::away`]) shows only for a search, which opens
    /// every shelf, or while it is on screen.
    pub(super) fn drawn<'a>(
        &'a self,
        families: &'a Families,
        top: usize,
        passes: &'a [Passes],
    ) -> impl Iterator<Item = usize> + 'a {
        let searching = !self.query.is_empty();
        let on_screen = self.on_screen();
        families.members(top).iter().copied().filter(move |&ix| {
            let row = &self.rows[ix];
            passes[ix].filter
                && (passes[ix].search || passes[top].search)
                && (searching || !row.away() || on_screen == Some(&row.id))
        })
    }

    /// Pushes the fold line under top row `top` when any of the sessions
    /// under it show (see [`Self::drawn`]), then, while it's open, those,
    /// laid out on its shelf.
    pub(super) fn push_family(
        &self,
        items: &mut Vec<Item>,
        families: &Families,
        top: usize,
        passes: &[Passes],
    ) {
        let mut drawn: Vec<usize> = self.drawn(families, top, passes).collect();
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
            pressing: Pressing::of(drawn.iter().map(|&ix| &self.rows[ix]), starter.kind),
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
    /// saved, not one it only runs, which it would refuse, nor one settled
    /// on its own while the family is elsewhere, which a snooze's end or a
    /// restore would bring back. A pin brings back the same sessions of a
    /// snoozed family, and none of an active or a settled one.
    #[test]
    fn a_move_marks_the_family_sessions_the_daemon_has_saved() {
        let mut list = SessionList::default();
        let merged = SavedSession {
            mark: Some(SessionMark::Done { at: 2 }),
            ..started("w0", "Merge", 2, "lead")
        };
        let sessions = vec![
            started("w1", "Write", 4, "lead"),
            saved("lead", None, 3),
            merged,
        ];
        list.set_saved(&here(), sessions);
        let running = JobSession {
            parent: Some(id("lead")),
            ..job("w2", JobActivity::Working, None)
        };
        list.set_jobs(&here(), vec![running], Instant::now(), 5);
        rebuild(&mut list, 5);
        assert_eq!(list.family(&id("lead")), [id("lead"), id("w1")]);
        assert!(list.pinned_with(&id("lead")).is_empty(), "active");

        // Snoozed as it was moved, the family's Restore, or its pin, leaves
        // Merge out.
        list.set_jobs(&here(), Vec::new(), Instant::now(), 6);
        for snoozed in ["lead", "w1"] {
            mark(&mut list, snoozed, SessionMark::Snoozed { until: 100 });
        }
        rebuild(&mut list, 6);
        assert_eq!(list.family(&id("lead")), [id("lead"), id("w1")]);
        assert_eq!(list.pinned_with(&id("lead")), [id("w1")]);

        // Settled, Write reads as Merge does.
        for settled in ["lead", "w1"] {
            mark(&mut list, settled, SessionMark::Done { at: 7 });
        }
        rebuild(&mut list, 7);
        assert!(list.pinned_with(&id("lead")).is_empty(), "settled");
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
        list.set_on_screen(Some(&id("w4")));
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

    /// On the active shelf, a session that is settled or snoozed on its own,
    /// as rho settles one when its last linked pull request merges, leaves
    /// its family's fold: the fold neither draws nor counts it, its line
    /// says nothing of it, it doesn't rank the family, and it isn't on the
    /// active list that the title's count reads. A search finds it, and it
    /// shows while it is on screen; the end
    /// of its snooze or new activity brings it back, by its shelf's rule; a
    /// family off the active shelf shows all of it, and so does one snoozed
    /// together that a busy session wakes.
    #[test]
    fn a_session_settled_on_its_own_leaves_its_active_familys_fold() {
        let mut list = SessionList::default();
        let snoozed = SavedSession {
            mark: Some(SessionMark::Snoozed { until: 200 }),
            ..started("w3", "Review", 25, "lead")
        };
        list.set_saved(
            &here(),
            vec![
                saved("other", Some("Other"), 80),
                snoozed,
                started("w2", "Test", 20, "lead"),
                started("w1", "Write", 10, "lead"),
                saved("lead", Some("Lead"), 5),
            ],
        );
        // Test's turn ends off screen, then its pull request merges.
        let testing = || job("w2", JobActivity::Working, None);
        list.set_jobs(&here(), vec![testing()], Instant::now(), 60);
        list.set_jobs(&here(), Vec::new(), Instant::now(), 60);
        mark(&mut list, "w2", SessionMark::Done { at: 70 });
        list.toggle_fold(&id("lead"));
        rebuild(&mut list, 100);
        // Test's Done would rank Lead over Other.
        assert_eq!(shown(&list), ["Other", "Lead", "⌄ 1 started", "  Write"]);
        let active: Vec<&str> = list
            .rows()
            .iter()
            .filter(|row| row.shelf == Shelf::Active && !row.away())
            .map(|row| row.id.id.as_str())
            .collect();
        assert_eq!(active, ["other", "w1", "lead"]);

        list.set_query("test");
        rebuild(&mut list, 100);
        assert_eq!(shown(&list), ["Lead", "• 1 started, 1 done", "  Test"]);
        list.set_query("");
        // Opened from elsewhere, as from the lead's Work, until another is.
        list.set_on_screen(Some(&id("w2")));
        rebuild(&mut list, 100);
        let opened = ["Other", "Lead", "⌄ 2 started, 1 done", "  Test", "  Write"];
        assert_eq!(shown(&list), opened);
        list.set_on_screen(Some(&id("other")));
        rebuild(&mut list, 100);
        assert_eq!(shown(&list), ["Other", "Lead", "⌄ 1 started", "  Write"]);
        list.set_on_screen(None);
        rebuild(&mut list, 200);
        let woke = ["Other", "Lead", "⌄ 2 started", "  Review", "  Write"];
        assert_eq!(shown(&list), woke);

        list.set_jobs(&here(), vec![testing()], Instant::now(), 210);
        list.set_jobs(&here(), Vec::new(), Instant::now(), 211);
        rebuild(&mut list, 211);
        let back = [
            "Lead",
            "⌄ 3 started, 1 done",
            "  Test",
            "  Review",
            "  Write",
            "Other",
        ];
        assert_eq!(shown(&list), back);

        // Test merges again, and Lead settles.
        mark(&mut list, "w2", SessionMark::Done { at: 300 });
        mark(&mut list, "lead", SessionMark::Done { at: 300 });
        list.toggle(Shelf::Settled);
        rebuild(&mut list, 300);
        let settled = [
            "[Settled 1]",
            "Lead",
            "⌄ 3 started, 1 done",
            "  Review",
            "  Test",
            "  Write",
        ];
        assert_eq!(shown(&list)[2..], settled);

        // Snoozed together, then woken by Write's turn.
        for snoozed in ["lead", "w1", "w2", "w3"] {
            mark(&mut list, snoozed, SessionMark::Snoozed { until: 1000 });
        }
        let writes = job("w1", JobActivity::Working, None);
        list.set_jobs(&here(), vec![writes], Instant::now(), 310);
        rebuild(&mut list, 310);
        let woken = [
            "Lead",
            "⌄ 3 started, 1 done",
            "  Test",
            "  Review",
            "  Write",
        ];
        assert_eq!(shown(&list)[..5], woken);
    }

    /// Active leads list first, under their header, each over the sessions
    /// it started, then the other active sessions, which alone the active
    /// shelf counts; a settled lead lists on its shelf. A lead reads its
    /// sessions' results itself: its fold line says who asks, else who
    /// works, never who is Done, and their Done doesn't rank it, though its
    /// own does. A lead is in no project, so a project filter that hides
    /// the others leaves it.
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
                // Newer than any turn here ends.
                lead("next", "Next", 20),
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
            "Next",
            "Lead",
            "› 1 started",
            "[Sessions]",
            "Other",
            "~",
            "[Settled 1]",
        ];
        assert_eq!(shown(&list), listed);
        assert_eq!(list.count(Shelf::Active), 1);

        let writes = job("w1", JobActivity::Working, None);
        list.set_jobs(&here(), vec![writes], Instant::now(), 11);
        rebuild(&mut list, 11);
        let working = ["[Leads]", "Next", "Lead", "› 1 started, 1 working"];
        assert_eq!(shown(&list)[..4], working);
        // Write's turn ends off screen, and a session Lead starts asks.
        let asks = JobSession {
            parent: Some(id("lead")),
            ..job("w2", JobActivity::Waiting, None)
        };
        list.set_jobs(&here(), vec![asks], Instant::now(), 12);
        rebuild(&mut list, 12);
        let asking = ["[Leads]", "Lead", "› 2 started, 1 waiting", "Next"];
        assert_eq!(shown(&list)[..4], asking);
        list.set_jobs(&here(), Vec::new(), Instant::now(), 13);
        rebuild(&mut list, 13);
        assert_eq!(
            shown(&list)[..4],
            ["[Leads]", "Next", "Lead", "› 1 started"]
        );
        // Lead's own turn ends off screen.
        list.set_jobs(
            &here(),
            vec![job("lead", JobActivity::Working, None)],
            Instant::now(),
            14,
        );
        list.set_jobs(&here(), Vec::new(), Instant::now(), 15);
        rebuild(&mut list, 15);
        assert_eq!(
            shown(&list)[..4],
            ["[Leads]", "Lead", "› 1 started", "Next"]
        );

        // Kept to a project none of them is in.
        let mut filter = Filter::default();
        filter.toggle_project(&Place::new(here(), "/srv"));
        list.set_filter(filter);
        rebuild(&mut list, 15);
        let kept = ["[Leads]", "Lead", "Next", "~", "[Settled 1]"];
        assert_eq!(shown(&list), kept);
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
            list.rebuild(10, &HashSet::new(), super::super::tests::named(roots));
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
