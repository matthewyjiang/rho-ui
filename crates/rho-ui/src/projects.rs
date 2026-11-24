//! The projects a new session can start in, each a folder on one machine.
//! The user adds them, by typing a path or with the folder chooser, and
//! removes them. rho-ui also finds the folders in each machine's clone
//! folder (see [`crate::cloning::find_projects`]): those are projects for
//! as long as they are there, so removing one doesn't take it off the
//! list. A project is a folder,
//! and for a git repository on this machine it is the main checkout: adding
//! a linked worktree or a subfolder adds the repository it belongs to (see
//! [`crate::repo::Repo::root`]).
//!
//! The added projects live in the state database's `projects` table (see
//! [`StateDb`]), in the order added. Found ones aren't saved: the clone
//! folder is read again as each machine connects and each draft opens.

use std::path::{Path, PathBuf};

use rho_ui_core::platform;

use crate::{
    machines::{MachineId, Place},
    store::StateDb,
};

/// The user's projects.
#[derive(Debug)]
pub(crate) struct Projects {
    /// Those the user added, in the order added.
    roots: Vec<Place>,
    /// Those found in each machine's clone folder, as last read, sorted
    /// by machine and path.
    found: Vec<Place>,
    /// The machines whose clone folder has been read.
    read: Vec<MachineId>,
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
                let path = platform::path_from_bytes(&row.get::<_, Vec<u8>>(1)?);
                Ok(Place::new(machine, path))
            },
        );
        Self {
            roots,
            found: Vec::new(),
            read: Vec::new(),
            db: db.clone(),
        }
    }

    /// For tests: these projects on this machine, kept in memory.
    #[cfg(test)]
    pub(crate) fn of<P: Into<std::path::PathBuf>>(roots: impl IntoIterator<Item = P>) -> Self {
        let mut projects = Self::default();
        for root in roots {
            projects.add(Place::local(root));
        }
        projects
    }

    /// Every project: those added, in the order added, then those found
    /// and not added.
    pub(crate) fn roots(&self) -> impl Iterator<Item = &Place> {
        let found = self.found.iter().filter(|root| !self.roots.contains(root));
        self.roots.iter().chain(found)
    }

    /// Whether `root` is in its machine's clone folder, so it stays on the
    /// list until its folder goes.
    pub(crate) fn is_found(&self, root: &Place) -> bool {
        self.found.contains(root)
    }

    /// Whether `root` may be on the list: it is, or its machine's clone
    /// folder, where it may be, hasn't been read yet.
    pub(crate) fn may_list(&self, root: &Place) -> bool {
        !self.read.contains(&root.machine) || self.roots().any(|known| known == root)
    }

    /// Takes `found` as the projects in `machine`'s clone folder, in place
    /// of those last found there. Returns whether the list changed.
    pub(crate) fn set_found(&mut self, machine: &MachineId, found: Vec<PathBuf>) -> bool {
        if !self.read.contains(machine) {
            self.read.push(machine.clone());
        }
        let mut all: Vec<Place> = self
            .found
            .iter()
            .filter(|root| root.machine != *machine)
            .cloned()
            .chain(
                found
                    .into_iter()
                    .map(|path| Place::new(machine.clone(), path)),
            )
            .collect();
        all.sort();
        let changed = all != self.found;
        self.found = all;
        changed
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

/// A path as the database keeps it: [`platform::path_to_bytes`], which on
/// Unix needn't be UTF-8.
fn bytes(path: &Path) -> Vec<u8> {
    platform::path_to_bytes(path.as_os_str())
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
        // Not UTF-8: on Unix a path is bytes.
        projects.add(Place::local(platform::path_from_bytes(b"/w/\xff")));
        // The same folder on another machine is another project.
        projects.add(Place::new(MachineId::remote("workstation"), "/w/c"));
        assert_eq!(roots(&Projects::load(&db)), roots(&projects));
    }

    #[test]
    fn adding_twice_or_removing_a_stranger_changes_nothing() {
        let mut projects = Projects::of(["/w/a"]);
        assert!(!projects.add(Place::local("/w/a")));
        assert!(projects.add(Place::local("/w/b")));
        assert!(!projects.remove(&Place::local("/w/c")));
        assert!(projects.remove(&Place::local("/w/a")));
        assert_eq!(roots(&projects), [Place::local("/w/b")]);
    }

    /// A project found in a clone folder lists once, after those added,
    /// stays when removed while its folder is there, goes when a later
    /// read of its machine misses it, and isn't saved.
    #[test]
    fn found_projects_follow_their_folder() {
        let db = StateDb::in_memory();
        let mut projects = Projects::load(&db);
        let there = MachineId::remote("workstation");
        projects.add(Place::local("/p/b"));
        // Until its machine's clone folder is read, any project may be.
        assert!(projects.may_list(&Place::local("/p/a")));
        assert!(projects.set_found(&MachineId::local(), vec!["/p/a".into(), "/p/b".into()]));
        assert!(projects.set_found(&there, vec!["/p/c".into()]));
        let listed = [
            Place::local("/p/b"),
            Place::local("/p/a"),
            Place::new(there.clone(), "/p/c"),
        ];
        assert_eq!(roots(&projects), listed);
        projects.remove(&Place::local("/p/b"));
        projects.remove(&Place::local("/p/a"));
        assert_eq!(
            roots(&projects),
            [listed[1].clone(), listed[0].clone(), listed[2].clone()]
        );
        assert!(
            !projects.set_found(&there, vec!["/p/c".into()]),
            "the same read"
        );
        assert!(projects.set_found(&MachineId::local(), Vec::new()));
        assert_eq!(roots(&projects), [Place::new(there, "/p/c")]);
        assert!(!projects.may_list(&Place::local("/p/a")), "it was read");
        assert_eq!(roots(&Projects::load(&db)), []);
    }

    fn roots(projects: &Projects) -> Vec<Place> {
        projects.roots().cloned().collect()
    }
}
