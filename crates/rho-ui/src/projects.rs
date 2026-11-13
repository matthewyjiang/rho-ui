//! The projects a new session can start in. The user adds them, by typing a
//! path or with the folder chooser, and removes them; rho-ui never adds one
//! on its own. A project is a folder, and for a git repository it is the
//! main checkout: adding a linked worktree or a subfolder adds the
//! repository it belongs to (see [`crate::repo::Repo::root`]).
//!
//! The list lives in one small text file, one absolute path per line, in
//! the order the projects were added.

use std::path::{Path, PathBuf};

use crate::store;

/// The user's projects.
#[derive(Debug, Default)]
pub(crate) struct Projects {
    roots: Vec<PathBuf>,
    /// Where to save; `None` keeps changes in memory only (tests).
    file: Option<PathBuf>,
}

impl Projects {
    /// Loads `$XDG_STATE_HOME/rho-ui/projects`, or `~/.local/state/...`. A
    /// missing or unreadable file starts empty.
    pub(crate) fn load() -> Self {
        let file = store::xdg_dir("XDG_STATE_HOME", ".local/state").map(|dir| dir.join("projects"));
        let roots = file
            .as_ref()
            .and_then(|file| std::fs::read_to_string(file).ok())
            .map(|text| parse(&text))
            .unwrap_or_default();
        Self { roots, file }
    }

    /// For tests: these projects, kept in memory.
    #[cfg(test)]
    pub(crate) fn of<P: Into<PathBuf>>(roots: impl IntoIterator<Item = P>) -> Self {
        Self {
            roots: roots.into_iter().map(Into::into).collect(),
            file: None,
        }
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
        self.roots.push(root);
        self.save();
        true
    }

    /// Takes `root` off the list; its folder stays where it is. Returns
    /// whether the list changed.
    pub(crate) fn remove(&mut self, root: &Path) -> bool {
        let before = self.roots.len();
        self.roots.retain(|known| known != root);
        let changed = self.roots.len() != before;
        if changed {
            self.save();
        }
        changed
    }

    fn save(&self) {
        if let Some(file) = &self.file {
            store::write_atomic(file, &render(&self.roots));
        }
    }
}

/// The absolute paths in `text`, one per line, without repeats.
fn parse(text: &str) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for line in text.lines().map(str::trim) {
        let root = PathBuf::from(line);
        if root.is_absolute() && !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

/// One path per line. A path that isn't UTF-8 can't be written in a text
/// file, so it lasts until rho-ui quits.
fn render(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .filter_map(|root| root.to_str())
        .flat_map(|root| [root, "\n"])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_round_trips_and_skips_what_is_no_project() {
        let roots = parse("/w/a\n\n  /w/b  \nrelative\n/w/a\n");
        assert_eq!(roots, [PathBuf::from("/w/a"), PathBuf::from("/w/b")]);
        assert_eq!(parse(&render(&roots)), roots);
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
