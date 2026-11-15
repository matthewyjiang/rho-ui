//! Reaching a rho daemon on another machine over ssh, with an ssh client
//! in this process (russh), so Windows reaches Unix machines as Linux and
//! macOS do.
//!
//! One ssh connection per host carries everything ([`connection`]):
//! [`connect`] opens it, or takes the open one, runs a setup script on
//! the machine that starts its daemon and reports where it listens and
//! the login environment ([`setup`]), then opens a `direct-streamlocal`
//! channel to the daemon's socket, as `ssh -W` does, and speaks rho's
//! protocol over it. Every script after that runs in a session channel
//! of its own on the same connection ([`script`]). A new connection costs
//! a key signature, which an agent such as 1Password's asks the user to
//! approve, so the user approves once per host while the app uses the
//! host and the link stays up; a connection nothing uses closes 10
//! minutes later.
//!
//! The connection follows the user's ssh config as `ssh -G` resolves it
//! ([`config`]), checks the host's key against `known_hosts` and
//! `RevokedHostKeys` without ever adding to them ([`host_key`]), and
//! signs in with the key agent's keys and certificates, or key files
//! that need no passphrase and their certificates ([`auth`]). The files
//! are read on Tokio's blocking threads, not the I/O runtime's one worker.

mod auth;
mod config;
mod connection;
mod host_key;
mod script;
mod setup;

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use rho_protocol::{Client, ClientError, ClientHello, HelloResponse, Notifications};
use russh::{client::Msg, ChannelStream};

pub(crate) use config::Resolved;
#[cfg(feature = "test-support")]
pub(crate) use config::{AgentKeys, IdentityAgent};
use connection::Lease;
use script::ScriptError;

use crate::{
    daemon::{on_io, DaemonError, Machine, HANDSHAKE_DEADLINE},
    platform,
};

/// A rho daemon on another machine, reached over ssh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshTarget {
    host: String,
    rho: String,
    /// How to reach the host, when not as the user's ssh config says: a
    /// test's own server.
    route: Option<Box<Resolved>>,
}

impl SshTarget {
    /// The daemon on `host`, any destination ssh accepts (`user@host`, or a
    /// `Host` alias from `~/.ssh/config`). It runs the program `rho` there;
    /// change it with [`Self::with_rho`].
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            rho: "rho".to_owned(),
            route: None,
        }
    }

    /// The daemon on `host`, reached as `route` says rather than as the
    /// user's ssh config does: `fake_sshd`'s server.
    #[cfg(feature = "test-support")]
    pub(crate) fn routed(host: String, route: Resolved) -> Self {
        Self {
            host,
            rho: "rho".to_owned(),
            route: Some(Box::new(route)),
        }
    }

    /// Runs `rho` on the machine to start its daemon: a name looked up on
    /// the PATH of a non-interactive ssh command, or a path, where a
    /// leading `~/` means that machine's home.
    #[must_use]
    pub fn with_rho(mut self, rho: impl Into<String>) -> Self {
        self.rho = rho.into();
        self
    }

    /// The `rho` program it runs on the machine; see [`Self::with_rho`].
    pub(crate) fn rho(&self) -> &str {
        &self.rho
    }

    /// The link to the machine, opening its connection if none is open,
    /// for scripts to run there whether or not its daemon answers.
    ///
    /// # Errors
    ///
    /// Fails if ssh can't reach the machine.
    pub(crate) async fn link(&self) -> Result<SshLink, DaemonError> {
        let target = self.clone();
        on_io(async move { target.open_link().await }).await?
    }

    /// [`Self::link`], on the I/O runtime.
    async fn open_link(&self) -> Result<SshLink, DaemonError> {
        let host = &self.host;
        if host.is_empty() || host.starts_with('-') {
            return Err(DaemonError::Unreachable(format!(
                "{host:?} is not an ssh destination"
            )));
        }
        let lease = connection::shared(host, self.route.as_deref()).await?;
        Ok(SshLink {
            lease: Arc::new(lease),
        })
    }
}

/// Connects to the daemon `target` names, starting it there first if none
/// runs, and completes the handshake as `hello`. Runs on the I/O runtime.
pub(crate) async fn connect(
    target: &SshTarget,
    hello: ClientHello,
) -> Result<(Client, Notifications, HelloResponse, Machine), DaemonError> {
    let link = target.open_link().await?;
    let connection = link.lease.connection();
    let setup = setup::run(connection, &target.rho).await?;
    let stream = connection.daemon(&setup.socket).await?;
    let (client, notifications, hello) =
        handshake(connection.host(), &setup.socket, stream, hello).await?;
    let machine = Machine {
        environment: setup.environment,
        home: Some(setup.home),
        ssh: Some(link),
    };
    Ok((client, notifications, hello, machine))
}

/// Completes rho's handshake as `hello` over `stream`, a channel to the
/// daemon at `socket` on `host`, within [`HANDSHAKE_DEADLINE`]. The
/// channel lives as long as the client.
///
/// # Errors
///
/// Fails if the daemon doesn't answer; a daemon that answers with another
/// protocol version says which side to update.
async fn handshake(
    host: &str,
    socket: &str,
    stream: ChannelStream<Msg>,
    hello: ClientHello,
) -> Result<(Client, Notifications, HelloResponse), DaemonError> {
    match tokio::time::timeout(HANDSHAKE_DEADLINE, Client::handshake(stream, hello)).await {
        Err(_) => Err(DaemonError::Unreachable(format!(
            "the Rho daemon on {host} at {socket} didn't answer the handshake within {} s",
            HANDSHAKE_DEADLINE.as_secs()
        ))),
        Ok(Ok(connected)) => Ok(connected),
        // The daemon answered; its message names the side to update.
        Ok(Err(error @ (ClientError::VersionMismatch(_) | ClientError::Rpc { .. }))) => {
            Err(error.into())
        }
        Ok(Err(error)) => Err(DaemonError::Unreachable(format!(
            "couldn't reach the Rho daemon on {host} at {socket}: {error}"
        ))),
    }
}

/// Runs `work`, which reads files, on Tokio's blocking threads, so the
/// I/O runtime's one worker, which every daemon socket shares, never
/// waits on the disk. Call it on the I/O runtime.
///
/// # Errors
///
/// [`DaemonError::Closed`] if `work` panicked.
async fn read_files<T, F>(work: F) -> Result<T, DaemonError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| DaemonError::Closed)
}

/// The ssh connection to one remote machine, shared by its daemon
/// connection and every script run there. The connection stays open
/// while any link to it does, and 10 minutes after. Links are equal when
/// they share a connection.
#[derive(Clone)]
pub struct SshLink {
    lease: Arc<Lease>,
}

impl PartialEq for SshLink {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(self.lease.connection(), other.lease.connection())
    }
}

impl Eq for SshLink {}

impl std::fmt::Debug for SshLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshLink")
            .field("host", &self.lease.connection().host())
            .finish_non_exhaustive()
    }
}

impl SshLink {
    /// Runs the POSIX `sh` script `script` on the machine, with `args` as
    /// its positional parameters, and returns what it wrote to stdout. It
    /// runs only on the connection the daemon connection opened: once that
    /// is gone this fails rather than connect again, which could ask the
    /// user's key agent for another approval.
    ///
    /// # Errors
    ///
    /// Fails if the connection is gone, the machine refuses a session, the
    /// script exits with an error, or it runs past 60 s.
    pub async fn run_script(
        &self,
        script: &str,
        args: &[impl AsRef<OsStr>],
    ) -> Result<Vec<u8>, DaemonError> {
        self.spawn_script(script, args, Some(script::SCRIPT_DEADLINE))
            .await
    }

    /// [`Self::run_script`] with no deadline, for a script whose time the
    /// network and the work set, such as a `git clone`: it ends when the
    /// script does, the connection does, or the caller drops the future,
    /// which closes the script's channel.
    ///
    /// # Errors
    ///
    /// Fails if the connection is gone, the machine refuses a session, or
    /// the script exits with an error.
    pub async fn run_long_script(
        &self,
        script: &str,
        args: &[impl AsRef<OsStr>],
    ) -> Result<Vec<u8>, DaemonError> {
        self.spawn_script(script, args, None).await
    }

    /// Runs [`script::run`] on the I/O runtime, with `args` set as the
    /// script's positional parameters, and words how it failed. Dropping
    /// the future stops the script.
    async fn spawn_script(
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
        let lease = Arc::clone(&self.lease);
        let ran = on_io(async move { script::run(lease.connection(), &input, deadline).await });
        let host = self.lease.connection().host();
        ran.await?.map_err(|error| {
            let why = match error {
                ScriptError::Session(error) => return error,
                ScriptError::TimedOut(deadline) => {
                    format!("a script on {host} ran past {} s", deadline.as_secs())
                }
                ScriptError::Cut => format!("the connection to {host} closed while a script ran"),
                ScriptError::Refused => {
                    format!("a script on {host} failed: sshd refused to run sh")
                }
                ScriptError::Failed(why) => format!("a script on {host} failed: {why}"),
            };
            DaemonError::Unreachable(why)
        })
    }

    /// The program and arguments of an `ssh` that runs the user's login
    /// shell in `dir` on the machine, in a terminal of its own (`-tt`), for
    /// a terminal emulator to run under a pseudo-terminal here. The local
    /// OpenSSH program makes a connection of its own for it, in
    /// `BatchMode`, so an agent such as 1Password's may ask to approve it;
    /// a pseudo-terminal channel on the shared connection is to replace it.
    ///
    /// sshd hands the command line, `cd -- 'dir' && exec "$SHELL" -l`, to
    /// the user's login shell to parse, so this assumes a POSIX one, such
    /// as sh, bash or zsh, unlike the setup script, which `sh` reads. fish
    /// reads `\\` and `\'` inside single quotes, so there a `dir` holding a
    /// backslash goes wrong.
    pub fn terminal_command(&self, dir: &Path) -> (PathBuf, Vec<OsString>) {
        let mut remote = b"cd -- ".to_vec();
        remote.extend(quote_bytes(&platform::path_to_bytes(dir.as_os_str())));
        remote.extend(br#" && exec "$SHELL" -l"#);
        let mut args: Vec<OsString> = [
            "BatchMode=yes",
            // Nor the user's key agent, nor X11, go to the machine.
            "ForwardAgent=no",
            "ForwardX11=no",
        ]
        .iter()
        .flat_map(|&option| [OsString::from("-o"), OsString::from(option)])
        .collect();
        args.extend([
            "-tt".into(),
            self.lease.connection().host().into(),
            platform::path_from_bytes(&remote).into_os_string(),
        ]);
        (config::ssh_program(), args)
    }
}

/// `text` as one single-quoted POSIX shell word.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// [`quote`] for bytes, such as a path that needn't be UTF-8.
fn quote_bytes(bytes: &[u8]) -> Vec<u8> {
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
