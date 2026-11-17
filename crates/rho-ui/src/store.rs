//! Where rho-ui keeps what it saves: the settings the user chooses, in a
//! text file in the config directory (see [`crate::settings`]), and
//! everything it remembers between runs, in one SQLite database,
//! [`StateDb`].

use std::{fmt::Write as _, io, path::Path, rc::Rc};

#[cfg(not(target_family = "wasm"))]
use rho_ui_core::platform::{self, AppDir};
use rusqlite::{Connection, Params, Row, TransactionBehavior};

/// Writes `text` to a temporary file next to `path` and renames it over
/// `path`, so a crash never leaves half a file.
pub(crate) fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

/// Bump on any change to [`SCHEMA`]. A database of another version starts
/// over empty: rho-ui has no release, so there is nothing to migrate.
const SCHEMA_VERSION: i32 = 7;

/// Every table. Each belongs to the module that reads and writes it.
/// Tables keyed by text are `WITHOUT ROWID`, so the key is stored once
/// rather than in the table and again in its index, which halved the file
/// with text-keyed turn times.
const SCHEMA: &str = "
    -- Pinned, settled and snoozed sessions, by machine
    -- (crate::machines::MachineId) and session; see crate::shelf. A row
    -- with neither mark nor ownership is deleted; it holds one mark at most.
    CREATE TABLE shelves (
        machine TEXT NOT NULL,
        session_id TEXT NOT NULL,
        owned INTEGER NOT NULL,
        settled_at INTEGER,
        snoozed_until INTEGER,
        pinned_at INTEGER,
        PRIMARY KEY (machine, session_id),
        CHECK ((settled_at IS NOT NULL) + (snoozed_until IS NOT NULL)
            + (pinned_at IS NOT NULL) <= 1)
    ) STRICT, WITHOUT ROWID;
    -- The user's projects, in the order added (rowid); see crate::projects.
    -- The path's bytes, as a path needn't be UTF-8.
    CREATE TABLE projects (
        machine TEXT NOT NULL,
        path BLOB NOT NULL,
        UNIQUE (machine, path)
    ) STRICT;
    -- The sidebar filter's hidden machines, and the projects it keeps to
    -- (the path's bytes); see crate::sidebar::SavedFilter.
    CREATE TABLE filter_machines (machine TEXT PRIMARY KEY) STRICT, WITHOUT ROWID;
    CREATE TABLE filter_projects (
        machine TEXT NOT NULL,
        path BLOB NOT NULL,
        PRIMARY KEY (machine, path)
    ) STRICT, WITHOUT ROWID;
    -- How long each turn took, by a hash of its prompt's message ID, which
    -- is the rowid; see crate::turn_times.
    CREATE TABLE turn_times (prompt INTEGER PRIMARY KEY, ms INTEGER NOT NULL) STRICT;
    -- The background work rho-ui saw each session run, by machine and
    -- session; see crate::background_log. kind and work_id name the work
    -- as the daemon does, and subagent_session is a subagent's own
    -- session. ended orders the session's ended work as rho-ui saw it
    -- end, and is NULL while the work runs.
    CREATE TABLE background (
        machine TEXT NOT NULL,
        session_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        work_id TEXT NOT NULL,
        subagent_session TEXT,
        title TEXT NOT NULL,
        status TEXT NOT NULL,
        exit_code INTEGER,
        ended INTEGER,
        PRIMARY KEY (machine, session_id, kind, work_id),
        CHECK ((status = 'running') = (ended IS NULL))
    ) STRICT, WITHOUT ROWID;
    -- The pull requests linked by hand to a session, each a Pull request
    -- tab beside its branch's; by machine and session, in the order linked
    -- (rowid). See crate::pull_requests::PrLinks.
    CREATE TABLE pr_links (
        machine TEXT NOT NULL,
        session_id TEXT NOT NULL,
        host TEXT NOT NULL,
        owner TEXT NOT NULL,
        name TEXT NOT NULL,
        number INTEGER NOT NULL,
        UNIQUE (machine, session_id, host, owner, name, number)
    ) STRICT;
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
    /// The open sidebar's width in rems; see
    /// [`crate::sidebar::SidebarLayout`].
    SidebarWidth,
    /// `shown` or `hidden`; see [`crate::right_panel::PanelLayout`].
    RightPanel,
    /// The right panel's width in rems; see
    /// [`crate::right_panel::PanelLayout`].
    RightPanelWidth,
    /// `local` or `new-worktree`; see
    /// [`crate::project_picker::LastCheckout`].
    Checkout,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Key::SidebarLayout => "sidebar-layout",
            Key::SidebarWidth => "sidebar-width",
            Key::RightPanel => "right-panel",
            Key::RightPanelWidth => "right-panel-width",
            Key::Checkout => "checkout",
        }
    }
}

/// rho-ui's state database, `state.sqlite3` in its state folder
/// ([`AppDir::State`]: by default `~/.local/state/rho-ui`), or one in memory when
/// that can't be opened, and in tests. Owners load their rows once and
/// write each change as it happens. Writes are best effort: a failed one
/// loses one change and says so on stderr.
///
/// In a browser it is a copy, in memory, of the one on the server's
/// machine, as `rho-ui serve` shares it ([`Self::from_image`]), which sends
/// each write on to that one.
///
/// Cloning shares the one connection. GPUI runs everything that touches it
/// on the main thread.
#[derive(Clone)]
pub(crate) struct StateDb {
    connection: Rc<Connection>,
    /// Where each write goes besides, as SQL with its values in it.
    forward: Option<Rc<dyn Fn(String)>>,
}

impl std::fmt::Debug for StateDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateDb")
            .field("path", &self.connection.path())
            .field("forwards", &self.forward.is_some())
            .finish()
    }
}

impl StateDb {
    /// Opens the database in the state folder, creating it if need
    /// be. When it can't, rho-ui runs on one in memory, remembering nothing
    /// past this run, and says why on stderr. A browser has no folder for
    /// it, and runs on one in memory.
    #[cfg(not(target_family = "wasm"))]
    #[expect(
        clippy::print_stderr,
        reason = "rho-ui has no log; stderr reaches the terminal or the journal"
    )]
    pub(crate) fn open() -> Self {
        let Some(path) = Self::file() else {
            eprintln!("rho-ui: no state directory (set HOME); nothing will be remembered");
            return Self::in_memory();
        };
        Self::open_at(&path).unwrap_or_else(|error| {
            eprintln!(
                "rho-ui: can't open {}: {error}; nothing will be remembered",
                path.display()
            );
            Self::in_memory()
        })
    }

    /// Where the database is: `state.sqlite3` in the state folder.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn file() -> Option<std::path::PathBuf> {
        platform::app_dir(AppDir::State).map(|dir| dir.join("state.sqlite3"))
    }

    /// Opens the database at `path`, making its directory if need be.
    #[cfg(not(target_family = "wasm"))]
    fn open_at(path: &Path) -> rusqlite::Result<Self> {
        Self::connect(path).map(Self::wrap)
    }

    /// A connection to the database at `path`, set up as [`Self::open`]
    /// does: `rho-ui serve`'s, which shares it with a browser.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn connect(path: &Path) -> rusqlite::Result<Connection> {
        if let Some(dir) = path.parent() {
            // A directory that can't be made fails the open, which says so.
            let _ = std::fs::create_dir_all(dir);
        }
        let mut connection = Connection::open(path)?;
        set_up(&mut connection)?;
        Ok(connection)
    }

    /// A copy in memory of the database whose `image` [`image`] made,
    /// which sends each write to `forward` too, as SQL with its values in
    /// it: a browser's, whose writes go to the server's.
    #[cfg_attr(
        not(any(target_family = "wasm", test)),
        expect(dead_code, reason = "only a browser shares the database")
    )]
    pub(crate) fn from_image(
        image: &[u8],
        forward: impl Fn(String) + 'static,
    ) -> rusqlite::Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        connection.deserialize_read_exact(rusqlite::MAIN_DB, image, image.len(), false)?;
        set_up(&mut connection)?;
        Ok(Self {
            forward: Some(Rc::new(forward)),
            ..Self::wrap(connection)
        })
    }

    /// A database in memory, gone when rho-ui quits.
    pub(crate) fn in_memory() -> Self {
        let mut connection =
            Connection::open_in_memory().expect("SQLite opens a database in memory");
        set_up(&mut connection).expect("SQLite sets up a database in memory");
        Self::wrap(connection)
    }

    fn wrap(connection: Connection) -> Self {
        Self {
            connection: Rc::new(connection),
            forward: None,
        }
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
            .and_then(|mut statement| {
                statement.execute(params)?;
                // The statement with its values in it, as SQLite writes them:
                // integers, text quoted, blobs in hex. The schema has no REAL,
                // which this would round to 15 digits (see the test
                // `no_column_holds_a_real_which_a_shared_copy_would_round`).
                Ok(statement.expanded_sql())
            });
        match (written, &self.forward) {
            (Ok(Some(sql)), Some(forward)) => forward(sql),
            (Ok(_), _) => {}
            (Err(error), _) => eprintln!("rho-ui: couldn't save state: {error}"),
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

/// Sets up `connection`: [`PRAGMAS`], with WAL so a second rho-ui's reads
/// don't wait on writes, and the schema, made afresh when the version
/// differs.
fn set_up(connection: &mut Connection) -> rusqlite::Result<()> {
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
    Ok(())
}

/// `connection`'s database as one image, as SQLite serializes it, for
/// [`StateDb::from_image`] to load: what `rho-ui serve` sends a browser.
/// The image says rollback journal, not WAL, as one in memory can't use
/// a WAL: bytes 18 and 19 of the header are the write and read versions,
/// 1 for rollback and 2 for WAL.
#[cfg(any(unix, test))]
pub(crate) fn image(connection: &mut Connection) -> rusqlite::Result<Vec<u8>> {
    let mut image = connection.serialize(rusqlite::MAIN_DB)?.to_vec();
    if let Some(versions) = image.get_mut(18..20) {
        versions.copy_from_slice(&[1, 1]);
    }
    Ok(image)
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
        let mut connection = Connection::open_in_memory().expect("SQLite opens in memory");
        connection
            .execute_batch("CREATE TABLE old (x); PRAGMA user_version = 99;")
            .expect("an old schema");
        set_up(&mut connection).expect("the schema applies");
        let db = StateDb::wrap(connection);
        db.set(Key::SidebarLayout, "collapsed");
        assert_eq!(db.get(Key::SidebarLayout).as_deref(), Some("collapsed"));
        let old = db.query(
            "SELECT name FROM sqlite_schema WHERE name = 'old'",
            [],
            |row| row.get::<_, String>(0),
        );
        assert!(old.is_empty(), "the old tables are gone");
    }

    /// A browser's copy sends its writes on as SQL that SQLite writes out
    /// (`StateDb::execute`), which rounds a REAL to 15 digits: the server's
    /// database would then hold another number than the page's.
    #[test]
    fn no_column_holds_a_real_which_a_shared_copy_would_round() {
        let db = StateDb::in_memory();
        let reals = db.query(
            "SELECT m.name || '.' || c.name FROM sqlite_schema m, pragma_table_info(m.name) c \
             WHERE m.type = 'table' AND (upper(c.type) LIKE '%REAL%' \
             OR upper(c.type) LIKE '%FLOA%' OR upper(c.type) LIKE '%DOUB%')",
            [],
            |row| row.get::<_, String>(0),
        );
        assert!(reals.is_empty(), "REAL columns: {reals:?}");
    }

    /// A browser's copy starts as the server's database is, writes in its
    /// WAL included, and each write it makes, sent on and applied there,
    /// leaves both with the same rows: quotes, newlines and bytes that
    /// aren't UTF-8 too.
    #[test]
    fn a_shared_copy_starts_as_the_database_and_its_writes_replay_there() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let mut server = StateDb::connect(&dir.path().join("state.sqlite3"))
            .expect("the server's database opens");
        server
            .execute_batch("INSERT INTO kv (key, value) VALUES ('sidebar-layout', 'collapsed')")
            .expect("a row in the WAL");
        let sent = Rc::new(std::cell::RefCell::new(Vec::new()));
        let copy = StateDb::from_image(&image(&mut server).expect("an image"), {
            let sent = Rc::clone(&sent);
            move |sql| sent.borrow_mut().push(sql)
        })
        .expect("the copy loads");
        assert_eq!(copy.get(Key::SidebarLayout).as_deref(), Some("collapsed"));

        copy.set(Key::SidebarLayout, "it's\nopen");
        copy.execute(
            "INSERT INTO projects (machine, path) VALUES (?1, ?2)",
            ("local", b"/w/\xff'a".as_slice()),
        );
        for sql in sent.borrow().iter() {
            server
                .execute_batch(sql)
                .expect("the server applies the write");
        }
        let server = StateDb::wrap(server);
        for db in [&copy, &server] {
            assert_eq!(db.get(Key::SidebarLayout).as_deref(), Some("it's\nopen"));
            let projects = db.query("SELECT machine, path FROM projects", [], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            });
            assert_eq!(projects, [("local".to_owned(), b"/w/\xff'a".to_vec())]);
        }
    }
}
