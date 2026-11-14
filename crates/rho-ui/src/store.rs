//! Where rho-ui keeps what it saves: the settings the user chooses, in a
//! text file in the config directory (see [`crate::settings`]), and
//! everything it remembers between runs, in one SQLite database,
//! [`StateDb`].

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    rc::Rc,
};

use rusqlite::{Connection, Params, Row, TransactionBehavior};

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
/// `path`, so a crash never leaves half a file. Best effort: the file only
/// holds preferences, and losing a write loses one change.
pub(crate) fn write_atomic(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temporary = path.with_extension("tmp");
    if std::fs::write(&temporary, text).is_ok() {
        let _ = std::fs::rename(&temporary, path);
    }
}

/// Bump on any change to [`SCHEMA`]. A database of another version starts
/// over empty: rho-ui has no release, so there is nothing to migrate.
const SCHEMA_VERSION: i32 = 2;

/// Every table. Each belongs to the module that reads and writes it.
/// Tables keyed by text are `WITHOUT ROWID`, so the key is stored once
/// rather than in the table and again in its index, which halved the file
/// with text-keyed turn times.
const SCHEMA: &str = "
    -- Settled and snoozed sessions, by machine (crate::machines::MachineId)
    -- and session; see crate::shelf. A row with neither mark nor ownership
    -- is deleted.
    CREATE TABLE shelves (
        machine TEXT NOT NULL,
        session_id TEXT NOT NULL,
        owned INTEGER NOT NULL,
        settled_at INTEGER,
        snoozed_until INTEGER,
        PRIMARY KEY (machine, session_id),
        CHECK (settled_at IS NULL OR snoozed_until IS NULL)
    ) STRICT, WITHOUT ROWID;
    -- The user's projects, in the order added (rowid); see crate::projects.
    -- The path's bytes, as a path needn't be UTF-8.
    CREATE TABLE projects (
        machine TEXT NOT NULL,
        path BLOB NOT NULL,
        UNIQUE (machine, path)
    ) STRICT;
    -- How long each turn took, by a hash of its prompt's message ID, which
    -- is the rowid; see crate::turn_times.
    CREATE TABLE turn_times (prompt INTEGER PRIMARY KEY, ms INTEGER NOT NULL) STRICT;
    -- One-off values, by Key.
    CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT, WITHOUT ROWID;
";

/// How the file is kept, set on every connection. Sizes measured with rows
/// shaped like rho's (UUID sessions, 46-character prompt IDs); times with
/// one-row writes on an `NVMe` disk.
///
/// - `page_size`, before the first table: with 1 KiB pages the file is
///   6 KiB empty and 116 KiB with 300 sessions and 5,000 turns, against 24
///   and 132 KiB with SQLite's 4 KiB default, and about the same (445 KiB)
///   at 1,000 sessions and 20,000 turns. Each write adds one page to the
///   WAL, so it shrinks by the same factor.
/// - `synchronous = NORMAL`: a commit stops waiting on fsync, which took
///   writes from 5 ms to 0.02 ms at the median on the UI thread. A power
///   cut can lose the last writes; the file stays whole.
/// - `wal_autocheckpoint`: the WAL peaks at 108 KiB, 100 one-page writes,
///   where SQLite's default of 1,000 lets it reach about 1 MiB. The
///   checkpoint every 100 writes takes about 10 ms.
const PRAGMAS: &str = "
    PRAGMA page_size = 1024;
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    PRAGMA wal_autocheckpoint = 100;
";

/// A value in the `kv` table. One enum, so no two modules share a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    /// `open` or `collapsed`; see [`crate::sidebar::SidebarLayout`].
    SidebarLayout,
    /// The machines and projects the sidebar hides; see
    /// [`crate::session_list::Filter`].
    SidebarFilter,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Key::SidebarLayout => "sidebar-layout",
            Key::SidebarFilter => "sidebar-filter",
        }
    }
}

/// rho-ui's state database, `$XDG_STATE_HOME/rho-ui/state.sqlite3` (by
/// default `~/.local/state/rho-ui/state.sqlite3`), or one in memory when
/// that can't be opened, and in tests. Owners load their rows once and
/// write each change as it happens. Writes are best effort: a failed one
/// loses one change and says so on stderr.
///
/// Cloning shares the one connection. GPUI runs everything that touches it
/// on the main thread.
#[derive(Clone)]
pub(crate) struct StateDb {
    connection: Rc<Connection>,
}

impl std::fmt::Debug for StateDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateDb")
            .field("path", &self.connection.path())
            .finish()
    }
}

impl StateDb {
    /// Opens the database in the XDG state directory, creating it if need
    /// be. When it can't, rho-ui runs on one in memory, remembering nothing
    /// past this run, and says why on stderr.
    #[expect(
        clippy::print_stderr,
        reason = "rho-ui has no log; stderr reaches the terminal or the journal"
    )]
    pub(crate) fn open() -> Self {
        let Some(dir) = xdg_dir("XDG_STATE_HOME", ".local/state") else {
            eprintln!("rho-ui: no state directory (set HOME); nothing will be remembered");
            return Self::in_memory();
        };
        let path = dir.join("state.sqlite3");
        Self::open_at(&path).unwrap_or_else(|error| {
            eprintln!(
                "rho-ui: can't open {}: {error}; nothing will be remembered",
                path.display()
            );
            Self::in_memory()
        })
    }

    /// Opens the database at `path`, making its directory if need be.
    fn open_at(path: &Path) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            // A directory that can't be made fails the open, which says so.
            let _ = std::fs::create_dir_all(dir);
        }
        Connection::open(path).and_then(Self::new)
    }

    /// A database in memory, gone when rho-ui quits.
    pub(crate) fn in_memory() -> Self {
        Connection::open_in_memory()
            .and_then(Self::new)
            .expect("SQLite opens a database in memory")
    }

    /// Sets up `connection`: [`PRAGMAS`], with WAL so a second rho-ui's
    /// reads don't wait on writes, and the schema, made afresh when the
    /// version differs.
    fn new(mut connection: Connection) -> rusqlite::Result<Self> {
        connection.execute_batch(PRAGMAS)?;
        // Immediate: a second rho-ui starting at the same moment waits here,
        // then finds the schema made.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i32 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            let tables = transaction
                .prepare(
                    "SELECT name FROM sqlite_schema \
                     WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                )?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            // Writing to a String can't fail.
            let mut batch = String::new();
            for table in &tables {
                let _ = write!(batch, "DROP TABLE \"{table}\";");
            }
            let _ = write!(batch, "{SCHEMA} PRAGMA user_version = {SCHEMA_VERSION};");
            transaction.execute_batch(&batch)?;
        }
        transaction.commit()?;
        Ok(Self {
            connection: Rc::new(connection),
        })
    }

    /// Runs one write. Best effort; see [`StateDb`].
    #[expect(
        clippy::print_stderr,
        reason = "rho-ui has no log; stderr reaches the terminal or the journal"
    )]
    pub(crate) fn execute(&self, sql: &str, params: impl Params) {
        let written = self
            .connection
            .prepare_cached(sql)
            .and_then(|mut statement| statement.execute(params));
        if let Err(error) = written {
            eprintln!("rho-ui: couldn't save state: {error}");
        }
    }

    /// Every row `sql` returns, made by `row`. Empty, and said on stderr,
    /// if the query fails.
    #[expect(
        clippy::print_stderr,
        reason = "rho-ui has no log; stderr reaches the terminal or the journal"
    )]
    pub(crate) fn query<T>(
        &self,
        sql: &str,
        params: impl Params,
        row: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Vec<T> {
        let rows = self
            .connection
            .prepare_cached(sql)
            .and_then(|mut statement| {
                statement
                    .query_map(params, row)?
                    .collect::<rusqlite::Result<Vec<T>>>()
            });
        rows.unwrap_or_else(|error| {
            eprintln!("rho-ui: couldn't read state: {error}");
            Vec::new()
        })
    }

    /// The value at `key`, if it was ever set.
    pub(crate) fn get(&self, key: Key) -> Option<String> {
        self.query("SELECT value FROM kv WHERE key = ?1", [key.name()], |row| {
            row.get(0)
        })
        .pop()
    }

    pub(crate) fn set(&self, key: Key, value: &str) {
        self.execute(
            "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
            (key.name(), value),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_survives_reopening_the_file() {
        let dir = std::env::temp_dir().join(format!("rho-ui-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.sqlite3");
        let db = StateDb::open_at(&path).expect("the database opens");
        db.set(Key::SidebarLayout, "collapsed");
        // A second connection, as a second rho-ui would have, sees it.
        let other = StateDb::open_at(&path).expect("the database opens again");
        assert_eq!(other.get(Key::SidebarLayout).as_deref(), Some("collapsed"));
        let mode = db.query("PRAGMA journal_mode", [], |row| row.get::<_, String>(0));
        assert_eq!(mode, ["wal"]);
        drop((db, other));
        let reopened = StateDb::open_at(&path).expect("the database opens after closing");
        assert_eq!(
            reopened.get(Key::SidebarLayout).as_deref(),
            Some("collapsed")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn another_schema_version_starts_over() {
        let connection = Connection::open_in_memory().expect("SQLite opens in memory");
        connection
            .execute_batch("CREATE TABLE old (x); PRAGMA user_version = 99;")
            .expect("an old schema");
        let db = StateDb::new(connection).expect("the schema applies");
        db.set(Key::SidebarLayout, "collapsed");
        assert_eq!(db.get(Key::SidebarLayout).as_deref(), Some("collapsed"));
        let old = db.query(
            "SELECT name FROM sqlite_schema WHERE name = 'old'",
            [],
            |row| row.get::<_, String>(0),
        );
        assert!(old.is_empty(), "the old tables are gone");
    }
}
