//! Sessions whose turn finished since they were last on screen, which the
//! sidebar marks Done until they are opened. [`crate::session_list`] decides
//! when a session joins or leaves; this keeps the set in the state
//! database's `unseen` table (see [`StateDb`]), so Done outlives a restart.

use std::collections::BTreeSet;

use rho_ui_core::SessionId;

use crate::{
    machines::{MachineId, SessionKey},
    store::StateDb,
};

#[derive(Debug)]
pub(crate) struct Unseen {
    keys: BTreeSet<SessionKey>,
    db: StateDb,
}

/// For tests: an empty set, kept in memory.
#[cfg(test)]
impl Default for Unseen {
    fn default() -> Self {
        Self::load(&StateDb::in_memory())
    }
}

impl Unseen {
    pub(crate) fn load(db: &StateDb) -> Self {
        let keys = db.query("SELECT machine, session_id FROM unseen", [], |row| {
            let machine = MachineId::from(row.get::<_, String>(0)?.as_str());
            let id = SessionId::from(row.get::<_, String>(1)?.as_str());
            Ok(SessionKey::new(machine, id))
        });
        Self {
            keys: keys.into_iter().collect(),
            db: db.clone(),
        }
    }

    pub(crate) fn contains(&self, id: &SessionKey) -> bool {
        self.keys.contains(id)
    }

    /// Adds `id`, saving it if it is new.
    pub(crate) fn insert(&mut self, id: &SessionKey) {
        if self.keys.insert(id.clone()) {
            self.db.execute(
                "INSERT OR IGNORE INTO unseen (machine, session_id) VALUES (?1, ?2)",
                (id.machine.as_str(), id.id.as_str()),
            );
        }
    }

    /// Removes `id`. Returns whether it was there.
    pub(crate) fn remove(&mut self, id: &SessionKey) -> bool {
        let removed = self.keys.remove(id);
        if removed {
            self.db.execute(
                "DELETE FROM unseen WHERE machine = ?1 AND session_id = ?2",
                (id.machine.as_str(), id.id.as_str()),
            );
        }
        removed
    }
}
