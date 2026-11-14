//! The projects a new session can start in, each a folder on one machine.
//! The user adds them, by typing a path or with the folder chooser, and
//! removes them; rho-ui never adds one on its own. A project is a folder,
//! and for a git repository on this machine it is the main checkout: adding
//! a linked worktree or a subfolder adds the repository it belongs to (see
//! [`crate::repo::Repo::root`]).
//!
//! The list lives in the state database's `projects` table (see
//! [`StateDb`]), in the order the projects were added.

use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::{Path, PathBuf},
};

use crate::{
    machines::{MachineId, Place},
    store::StateDb,
};

/// The user's projects.
#[derive(Debug)]
pub(crate) struct Projects {
    roots: Vec<Place>,
    db: StateDb,
}

/// For tests: no projects, kept in memory.
#[cfg(test)]
impl Default for Projects {
    fn default() -> Self {
        Self::load(&StateDb::in_memory())
    }
}

impl Projects {
    /// Loads the saved projects. Rows keep their insertion order in
    /// `rowid`, as SQLite gives a new row one past the largest.
    pub(crate) fn load(db: &StateDb) -> Self {
        let roots = db.query(
            "SELECT machine, path FROM projects ORDER BY rowid",
            [],
            |row| {
                let machine = MachineId::from(row.get::<_, String>(0)?.as_str());
                let path = PathBuf::from(OsString::from_vec(row.get(1)?));
                Ok(Place::new(machine, path))
            },
        );
        Self {
            roots,
            db: db.clone(),
        }
    }

    /// For tests: these projects on this machine, kept in memory.
    #[cfg(test)]
    pub(crate) fn of<P: Into<PathBuf>>(roots: impl IntoIterator<Item = P>) -> Self {
        let mut projects = Self::default();
        for root in roots {
            projects.add(Place::local(root));
        }
        projects
    }

    /// Every project, in the order added.
    pub(crate) fn roots(&self) -> &[Place] {
        &self.roots
    }

    /// Adds `root`, a project's folder, unless it is in the list already.
    /// Returns whether the list changed.
    pub(crate) fn add(&mut self, root: Place) -> bool {
        if self.roots.contains(&root) {
            return false;
        }
        self.db.execute(
            "INSERT OR IGNORE INTO projects (machine, path) VALUES (?1, ?2)",
            (root.machine.as_str(), bytes(&root.path)),
        );
        self.roots.push(root);
        true
    }

    /// Takes `root` off the list; its folder stays where it is. Returns
    /// whether the list changed.
    pub(crate) fn remove(&mut self, root: &Place) -> bool {
        let before = self.roots.len();
        self.roots.retain(|known| known != root);
        let changed = self.roots.len() != before;
        if changed {
            self.db.execute(
                "DELETE FROM projects WHERE machine = ?1 AND path = ?2",
                (root.machine.as_str(), bytes(&root.path)),
            );
        }
        changed
    }
}

/// A path as the database keeps it: its bytes, which needn't be UTF-8.
fn bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_survive_a_reload_in_the_order_added() {
        let db = StateDb::in_memory();
        let mut projects = Projects::load(&db);
        for root in ["/w/c", "/w/a", "/w/b"] {
            projects.add(Place::local(root));
        }
        projects.remove(&Place::local("/w/a"));
        // Not UTF-8: a path is bytes.
        projects.add(Place::local(OsString::from_vec(b"/w/\xff".to_vec())));
        // The same folder on another machine is another project.
        projects.add(Place::new(MachineId::remote("workstation"), "/w/c"));
        assert_eq!(Projects::load(&db).roots(), projects.roots());
    }

    #[test]
    fn adding_twice_or_removing_a_stranger_changes_nothing() {
        let mut projects = Projects::of(["/w/a"]);
        assert!(!projects.add(Place::local("/w/a")));
        assert!(projects.add(Place::local("/w/b")));
        assert!(!projects.remove(&Place::local("/w/c")));
        assert!(projects.remove(&Place::local("/w/a")));
        assert_eq!(projects.roots(), [Place::local("/w/b")]);
    }
}
