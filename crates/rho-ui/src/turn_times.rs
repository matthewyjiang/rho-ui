//! How long each turn took, kept across restarts for "Worked for". The
//! daemon replays a session's conversation without times, so rho-ui records
//! each turn it watches under its prompt's message ID, which the daemon
//! gives the prompt again when the session replays.
//!
//! The times live in the state database's `turn_times` table (see
//! [`StateDb`]), the only one that grows with every turn. So it keeps a
//! 64-bit hash of the message ID rather than the ID: rho's IDs run to 46
//! characters, and the hash cuts the table to about a third. Two IDs share
//! a hash with odds of about 1 in 10^11 at 20,000 turns, and then one turn
//! would show the other's time.

use std::{collections::HashMap, time::Duration};

use gpui_kit::Global;

use crate::store::StateDb;

/// Turn times by the [`key`] of their prompt's message ID. A global: every
/// session view reads and records them.
#[derive(Debug)]
pub(crate) struct TurnTimes {
    times: HashMap<i64, Duration>,
    db: StateDb,
}

impl Global for TurnTimes {}

impl TurnTimes {
    /// Loads every saved time.
    pub(crate) fn load(db: &StateDb) -> Self {
        let times = db.query("SELECT prompt, ms FROM turn_times", [], |row| {
            let ms: i64 = row.get(1)?;
            let took = Duration::from_millis(u64::try_from(ms).unwrap_or_default());
            Ok((row.get(0)?, took))
        });
        Self {
            times: times.into_iter().collect(),
            db: db.clone(),
        }
    }

    /// How long the turn that `prompt` started took, if rho-ui ran it.
    pub(crate) fn get(&self, prompt: &str) -> Option<Duration> {
        self.times.get(&key(prompt)).copied()
    }

    /// Records that the turn `prompt` started took `took`. Best effort on
    /// disk: a lost write only loses one turn's time.
    pub(crate) fn record(&mut self, prompt: &str, took: Duration) {
        let prompt = key(prompt);
        self.times.insert(prompt, took);
        let ms = i64::try_from(took.as_millis()).unwrap_or(i64::MAX);
        self.db.execute(
            "INSERT OR REPLACE INTO turn_times (prompt, ms) VALUES (?1, ?2)",
            (prompt, ms),
        );
    }
}

/// A message ID's 64-bit FNV-1a hash, as SQLite's signed integer. Saved
/// keys depend on it, so it must never change: std's hashers promise no
/// stable output.
fn key(message: &str) -> i64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = message.bytes().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    });
    i64::from_ne_bytes(hash.to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_time_survives_a_reload() {
        let db = StateDb::in_memory();
        let mut times = TurnTimes::load(&db);
        times.record("u1", Duration::from_millis(9_400));
        times.record("u2", Duration::from_secs(337));
        times.record("u1", Duration::from_millis(9_500));

        let reloaded = TurnTimes::load(&db);
        assert_eq!(reloaded.get("u1"), Some(Duration::from_millis(9_500)));
        assert_eq!(reloaded.get("u2"), Some(Duration::from_secs(337)));
        assert_eq!(reloaded.get("u3"), None);
    }

    #[test]
    fn the_key_is_fnv_1a() {
        // The published 64-bit FNV-1a test vectors.
        let fnv = |hash: u64| i64::from_ne_bytes(hash.to_ne_bytes());
        assert_eq!(key(""), fnv(0xcbf2_9ce4_8422_2325));
        assert_eq!(key("a"), fnv(0xaf63_dc4c_8601_ec8c));
        assert_eq!(key("foobar"), fnv(0x8594_4171_f739_67e8));
    }
}
