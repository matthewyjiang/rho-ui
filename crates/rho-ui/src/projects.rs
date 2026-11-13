//! The projects a new session can start in. The user adds them, by typing a
//! path or with the folder chooser, and removes them; rho-ui never adds one
//! on its own. A project is a folder, and for a git repository it is the
//! main checkout: adding a linked worktree or a subfolder adds the
//! repository it belongs to (see [`crate::repo::Repo::root`]).
//!
//! The list lives in the state database's `projects` table (see
//! [`StateDb`]), in the order the projects were added.

use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::{Path, PathBuf},
};

use crate::store::StateDb;

/// The user's projects.
#[derive(Debug)]
pub(crate) struct Projects {
    roots: Vec<PathBuf>,
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
        let roots = db.query("SELECT path FROM projects ORDER BY rowid", [], |row| {
            Ok(PathBuf::from(OsString::from_vec(row.get(0)?)))
        });
        Self {
            roots,
            db: db.clone(),
        }
    }

    /// For tests: these projects, kept in memory.
    #[cfg(test)]
    pub(crate) fn of<P: Into<PathBuf>>(roots: impl IntoIterator<Item = P>) -> Self {
        let mut projects = Self::default();
        for root in roots {
            projects.add(root.into());
        }
        projects
    }

    /// Every project, in the order added.
    pub(crate) fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Adds `root`, a project's folder, unless it is in the list already.
    /// Returns whether the list changed.
    pub(crate) fn add(&mut self, root: PathBuf) -> bool {
        if self.roots.contains(&root) {
            return false;
        }
        self.db.execute(
            "INSERT OR IGNORE INTO projects (path) VALUES (?1)",
            [bytes(&root)],
        );
        self.roots.push(root);
        true
    }

    /// Takes `root` off the list; its folder stays where it is. Returns
    /// whether the list changed.
    pub(crate) fn remove(&mut self, root: &Path) -> bool {
        let before = self.roots.len();
        self.roots.retain(|known| known != root);
        let changed = self.roots.len() != before;
        if changed {
            self.db
                .execute("DELETE FROM projects WHERE path = ?1", [bytes(root)]);
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
            projects.add(root.into());
        }
        projects.remove(Path::new("/w/a"));
        // Not UTF-8: a path is bytes.
        projects.add(PathBuf::from(OsString::from_vec(b"/w/\xff".to_vec())));
        assert_eq!(Projects::load(&db).roots(), projects.roots());
    }

    #[test]
    fn adding_twice_or_removing_a_stranger_changes_nothing() {
        let mut projects = Projects::of(["/w/a"]);
        assert!(!projects.add("/w/a".into()));
        assert!(projects.add("/w/b".into()));
        assert!(!projects.remove(Path::new("/w/c")));
        assert!(projects.remove(Path::new("/w/a")));
        assert_eq!(projects.roots(), [PathBuf::from("/w/b")]);
    }
}
