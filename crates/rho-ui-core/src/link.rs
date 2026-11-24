//! A machine rho-ui reaches through something, not directly: over ssh from
//! the desktop app ([`SshLink`]), or through `rho-ui serve` from a browser
//! ([`ServerLink`]). Either way it runs the same POSIX `sh` scripts there,
//! read by the same parsers, and reaches the daemon's socket there, which
//! [`crate::setup`] starts and finds.

use std::{ffi::OsStr, path::Path, time::Duration};

#[cfg(not(target_family = "wasm"))]
use crate::ssh::SshLink;
use crate::{daemon::DaemonError, platform, remote_shell::RemoteShell, server::ServerLink};

/// How long a script that has a deadline may run: a tripwire for a
/// script stuck on a hung disk. rho-ui's longest, reading the git
/// repositories of every workspace, took 1.3 s for 133 worktrees (about
/// 10 ms each), so this leaves room for thousands.
pub(crate) const SCRIPT_DEADLINE: Duration = Duration::from_secs(60);

/// How rho-ui reaches a machine it runs scripts on. Links are equal when
/// they share a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// Over ssh, from the desktop app.
    #[cfg(not(target_family = "wasm"))]
    Ssh(SshLink),
    /// Through `rho-ui serve`, from a browser.
    Server(ServerLink),
}

impl Link {
    /// The machine, as messages name it: its ssh host, or the server's
    /// address.
    pub fn name(&self) -> &str {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Self::Ssh(ssh) => ssh.host(),
            Self::Server(server) => server.name(),
        }
    }

    /// Runs the POSIX `sh` script `script` on the machine, with `args` as
    /// its positional parameters, and returns what it wrote to stdout.
    ///
    /// # Errors
    ///
    /// Fails if the machine can't be reached, refuses the script, the
    /// script exits with an error, or it runs past 60 s.
    pub async fn run_script(
        &self,
        script: &str,
        args: &[impl AsRef<OsStr>],
    ) -> Result<Vec<u8>, DaemonError> {
        self.run_with_args(script, args, Some(SCRIPT_DEADLINE))
            .await
    }

    /// [`Self::run_script`] with no deadline, for a script whose time the
    /// network and the work set, such as a `git clone`: it ends when the
    /// script does, the link does, or the caller drops the future, which
    /// stops the script.
    ///
    /// # Errors
    ///
    /// Fails if the machine can't be reached, refuses the script, or the
    /// script exits with an error.
    pub async fn run_long_script(
        &self,
        script: &str,
        args: &[impl AsRef<OsStr>],
    ) -> Result<Vec<u8>, DaemonError> {
        self.run_with_args(script, args, None).await
    }

    /// Runs `script` with `args` set as its positional parameters, and
    /// words how it failed.
    async fn run_with_args(
        &self,
        script: &str,
        args: &[impl AsRef<OsStr>],
        deadline: Option<Duration>,
    ) -> Result<Vec<u8>, DaemonError> {
        let mut input = b"set --".to_vec();
        for arg in args {
            input.push(b' ');
            input.extend(quote_bytes(&platform::path_to_bytes(arg.as_ref())));
        }
        input.push(b'\n');
        input.extend(script.as_bytes());
        let name = self.name();
        self.script(input, deadline).await.map_err(|error| {
            let why = match error {
                ScriptError::Session(error) => return error,
                ScriptError::TimedOut(deadline) => {
                    format!("a script on {name} ran past {} s", deadline.as_secs())
                }
                ScriptError::NoSlot { waited, at_once } => {
                    format!("a script didn't run: {}", no_slot(name, waited, at_once))
                }
                ScriptError::Cut => format!("the connection to {name} closed while a script ran"),
                ScriptError::Refused => {
                    format!("a script on {name} failed: sshd refused to run sh")
                }
                ScriptError::Failed(why) => format!("a script on {name} failed: {why}"),
            };
            DaemonError::Unreachable(why)
        })
    }

    /// Starts the user's login shell in `dir` on the machine, under a
    /// pseudo-terminal there of `columns` by `rows` that is a `term`: the
    /// right panel's terminal. Returns at once; [`RemoteShell::ended`]
    /// says if it couldn't start.
    pub fn open_shell(&self, dir: &Path, term: &str, columns: u16, rows: u16) -> RemoteShell {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Self::Ssh(ssh) => ssh.open_shell(dir, term, columns, rows),
            Self::Server(server) => server.open_shell(dir, term, columns, rows),
        }
    }

    /// Runs `input` with `sh -s` on the machine, within `deadline` if any,
    /// and returns what it printed. Dropping the future stops the script.
    ///
    /// # Errors
    ///
    /// As [`ScriptError`] says.
    pub(crate) async fn script(
        &self,
        input: Vec<u8>,
        deadline: Option<Duration>,
    ) -> Result<Vec<u8>, ScriptError> {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Self::Ssh(ssh) => ssh.script(input, deadline).await,
            Self::Server(server) => server.script(input, deadline).await,
        }
    }
}

/// Why a script didn't succeed. Callers word it for what the script did.
#[derive(Debug)]
pub(crate) enum ScriptError {
    /// It ran past its deadline, this one.
    TimedOut(Duration),
    /// It never started: its deadline, `waited`, passed while the machine
    /// was running `at_once` others, the most its link runs at once.
    #[cfg_attr(
        target_family = "wasm",
        expect(dead_code, reason = "only an ssh link's scripts wait for a slot")
    )]
    NoSlot { waited: Duration, at_once: usize },
    /// The link closed with no word of how the script ended, as when the
    /// connection died.
    Cut,
    /// sshd wouldn't run `sh`.
    Refused,
    /// It exited with an error, a signal killed it, or `sh` didn't start:
    /// why, in its own words if it wrote any to stderr.
    Failed(String),
    /// The machine couldn't be asked: the connection is gone, or the host
    /// refused a session.
    Session(DaemonError),
}

/// Words a [`ScriptError::NoSlot`] on the machine `name`.
pub(crate) fn no_slot(name: &str, waited: Duration, at_once: usize) -> String {
    format!(
        "{name} was still running {at_once} other scripts after {} s, the most rho-ui runs \
         there at once",
        waited.as_secs()
    )
}

/// What a script printed, and how it ended.
#[derive(Debug)]
pub(crate) struct Output {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) ending: Ending,
}

/// How a script ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// It exited with this status.
    Exited(i64),
    /// A signal killed it; which, as `SIGTERM`.
    Killed(String),
    /// `sh` didn't start; why, in words for people.
    NotStarted(String),
    /// sshd wouldn't run `sh`.
    #[cfg_attr(
        target_family = "wasm",
        expect(dead_code, reason = "only an ssh script ends so")
    )]
    Refused,
    /// The link closed with no word of how, as when the connection died.
    #[cfg_attr(
        target_family = "wasm",
        expect(dead_code, reason = "only an ssh script ends so")
    )]
    Cut,
}

impl Output {
    /// What it printed, if it exited 0; else why not.
    pub(crate) fn result(self) -> Result<Vec<u8>, ScriptError> {
        let status = match self.ending {
            Ending::Exited(0) => return Ok(self.stdout),
            Ending::Refused => return Err(ScriptError::Refused),
            Ending::Cut => return Err(ScriptError::Cut),
            Ending::NotStarted(why) => return Err(ScriptError::Failed(why)),
            Ending::Exited(code) => format!("it exited with status {code}"),
            Ending::Killed(signal) => format!("it was killed by {signal}"),
        };
        let said = String::from_utf8_lossy(&self.stderr);
        Err(ScriptError::Failed(match said.trim() {
            "" => status,
            said => said.to_owned(),
        }))
    }
}

/// `text` as one single-quoted POSIX shell word.
pub(crate) fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// [`quote`] for bytes, such as a path that needn't be UTF-8.
pub(crate) fn quote_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut quoted = vec![b'\''];
    for &byte in bytes {
        if byte == b'\'' {
            quoted.extend(br"'\''");
        } else {
            quoted.push(byte);
        }
    }
    quoted.push(b'\'');
    quoted
}
