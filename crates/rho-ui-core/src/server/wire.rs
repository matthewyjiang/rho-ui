//! What rho-ui and `rho-ui serve` say to each other. Each endpoint is a
//! WebSocket of its own, opened per use, as an ssh session channel is: one
//! per script, daemon connection or shell.

use serde::{Deserialize, Serialize};

/// The WebSocket subprotocol that carries the server's `token`: each
/// WebSocket offers it, and the server names it back when it accepts. A
/// browser's WebSocket takes no header of its own, but offers these.
#[must_use]
pub fn token_protocol(token: &str) -> String {
    format!("rho-ui-token.{token}")
}

/// The query key that names a remote machine: on [`SCRIPT`], [`SOCKET`]
/// and [`SHELL`], `machine=HOST` does the work on ssh host `HOST` instead,
/// which the server reaches as the desktop app on its machine would.
pub const MACHINE: &str = "machine";

/// Runs a POSIX `sh` script, as `sh -s` in the server's home. The client
/// sends the script's input as one binary message; the server answers
/// with what it prints to stdout as binary messages, then one text
/// message, a [`ScriptEnd`] as JSON, and closes. Closing the WebSocket
/// first kills the script.
pub const SCRIPT: &str = "/script";

/// Relays the Unix socket at the query's `path` (`/socket?path=...`):
/// binary messages each way carry its bytes, and either side closing
/// closes both. It is how the client reaches the daemon's socket, as ssh's
/// `direct-streamlocal` does.
pub const SOCKET: &str = "/socket";

/// How a [`SCRIPT`] ended, and what it wrote to stderr.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptEnd {
    /// How it ended.
    pub ending: Ending,
    /// What it wrote to stderr.
    pub stderr: String,
}

/// How a script ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ending {
    /// It exited with this status.
    Exited(i64),
    /// A signal killed it; which, in words for people.
    Killed(String),
    /// `sh` didn't start: why, in words for people.
    NotStarted(String),
}

/// Runs the login shell, `$SHELL -l`, under a pseudo-terminal of the
/// query's size and type, in its folder:
/// `/shell?dir=...&term=...&columns=...&rows=...`. Binary messages from
/// the client are what is typed, and text messages are [`ShellControl`]s
/// as JSON; binary messages from the server are what it prints, at most
/// [`SHELL_WINDOW`] bytes of it ahead of what the client said it took. The
/// server closes when the shell exits, and hangs up on it when the client
/// closes.
pub const SHELL: &str = "/shell";

/// How many bytes of a [`SHELL`]'s output the server sends before the
/// client says it took them. A browser can't hold back a WebSocket's
/// reads, so without it a shell printing faster than the page draws would
/// fill the page's memory. 1 MiB, as the desktop app's terminal queues
/// from a shell here: 16 reads of 64 KiB.
pub const SHELL_WINDOW: usize = 1 << 20;

/// What the client of a [`SHELL`] tells the server, besides what is typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellControl {
    /// The terminal is now this size, in cells.
    Resize {
        /// Its width.
        columns: u16,
        /// Its height.
        rows: u16,
    },
    /// The terminal took this many more bytes of the output, so the server
    /// may send as many more.
    Took {
        /// How many.
        bytes: usize,
    },
}

/// The desktop app's own state on the server's machine, for the page to
/// share: its settings file and its state database. On open the server
/// sends a [`StateSnapshot`] as JSON text, then the database's image, as
/// SQLite serializes it, as one binary message. Text messages from the
/// client are [`StateWrite`]s, which the server applies in order; it
/// closes once the client has closed and it applied them all.
pub const STATE: &str = "/state";

/// What `/state` sends first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// The settings file's text; `None` if there is none yet.
    pub settings: Option<String>,
}

/// A change the page made to the shared state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StateWrite {
    /// One write to the database, as SQL with its values in it.
    Sql {
        /// The statement.
        sql: String,
    },
    /// The settings file's new text.
    Settings {
        /// The text.
        text: String,
    },
}
