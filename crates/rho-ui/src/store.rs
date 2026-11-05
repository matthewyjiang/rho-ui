//! Where rho-ui keeps its small text files, and how it writes them.

use std::path::{Path, PathBuf};

/// `$<var>/rho-ui`, or `~/<fallback>/rho-ui` when the variable is unset or
/// not absolute, as the XDG base directory spec says.
pub(crate) fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))?;
    Some(base.join("rho-ui"))
}

/// Writes `text` to a temporary file next to `path` and renames it over
/// `path`, so a crash never leaves half a file. Best effort: these files
/// only hold preferences, and losing a write loses one change.
pub(crate) fn write_atomic(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temporary = path.with_extension("tmp");
    if std::fs::write(&temporary, text).is_ok() {
        let _ = std::fs::rename(&temporary, path);
    }
}
