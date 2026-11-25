//! Pinned, settled and snoozed sessions: which list each session shows in.
//!
//! A session with no mark is active for [`QUIET_AFTER`] past its last
//! activity and settled from then on, whichever client started it: every
//! rho-ui reading the same daemon agrees on that with nothing to share, and
//! old history never fills the sidebar. A mark is what the user said of one
//! session: settled until it has new activity, snoozed until a time, pinned
//! above the others until unpinned (settling or snoozing a pinned session
//! would unpin it, so the sidebar offers neither), or kept, which puts it
//! back on the active list as if it had been active just then. Nothing needs
//! a timer: the sidebar asks [`shelf`] whenever it rebuilds.
//!
//! The daemon keeps each session's mark ([`SessionMark`]; settled is its
//! `done`), so every rho-ui of that daemon, on any machine, shows the same
//! lists. It also marks a session done itself when the last of the pull
//! requests linked to it merges.

use rho_ui_core::SessionMark;

/// How long a session stays active after its last activity, in seconds: a
/// week. One daemon's model requests over 98 days (528 sessions) hold 98
/// returns to a session idle for an hour or more: 97 within 32 hours and one
/// after 9 days. Three days would catch the same 97; a week also outlasts a
/// long weekend.
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

/// Where a session with `mark` shows, given its last activity and the
/// current time, all in Unix seconds.
pub(crate) fn shelf(mark: Option<SessionMark>, updated: i64, now: i64) -> Shelf {
    // When the session last mattered: its last activity, or the later
    // time the user's mark put it back.
    let touched = match mark {
        Some(SessionMark::Pinned { .. }) => return Shelf::Active,
        Some(SessionMark::Snoozed { until }) if now < secs(until) => return Shelf::Snoozed,
        Some(SessionMark::Done { at }) if updated <= secs(at) => return Shelf::Settled,
        Some(SessionMark::Snoozed { until: at } | SessionMark::Kept { at }) => {
            updated.max(secs(at))
        }
        Some(SessionMark::Done { .. }) | None => updated,
    };
    if now.saturating_sub(touched) > QUIET_AFTER {
        Shelf::Settled
    } else {
        Shelf::Active
    }
}

/// When a session with `mark` was pinned, if it is.
pub(crate) fn pinned(mark: Option<SessionMark>) -> Option<i64> {
    match mark {
        Some(SessionMark::Pinned { at }) => Some(secs(at)),
        _ => None,
    }
}

/// When a snoozed session wakes, or woke, in Unix seconds.
pub(crate) fn wakes(mark: Option<SessionMark>) -> Option<i64> {
    match mark {
        Some(SessionMark::Snoozed { until }) => Some(secs(until)),
        _ => None,
    }
}

/// A mark's time on the sidebar's clock, which counts in `i64`.
fn secs(time: u64) -> i64 {
    i64::try_from(time).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60;
    /// Past [`QUIET_AFTER`].
    const LONG: i64 = QUIET_AFTER + 1;

    /// The rule that puts a session on its shelf, from its mark, its last
    /// activity and the time.
    #[test]
    fn a_session_shelves_by_its_mark_then_its_age() {
        /// `100 * DAY`, as a mark holds it.
        const AT: u64 = 100 * 24 * 60 * 60;
        let settled = Some(SessionMark::Done { at: AT });
        let snoozed = Some(SessionMark::Snoozed { until: AT });
        let kept = Some(SessionMark::Kept { at: AT });
        let pinned = Some(SessionMark::Pinned { at: AT });
        // (why, mark, last activity, now, shelf)
        let cases: [(&str, Option<SessionMark>, i64, i64, Shelf); 12] = [
            (
                "recent",
                None,
                100 * DAY,
                100 * DAY + QUIET_AFTER,
                Shelf::Active,
            ),
            (
                "quiet for long",
                None,
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
                "kept, as a restore or an unpin leaves it, though long quiet",
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
        ];
        for (why, mark, updated, now, expected) in cases {
            assert_eq!(shelf(mark, updated, now), expected, "{why}");
        }
    }
}
