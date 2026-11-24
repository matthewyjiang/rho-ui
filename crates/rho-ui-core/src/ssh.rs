//! Reaching a rho daemon on another machine over ssh, with an ssh client
//! in this process (russh), so Windows reaches Unix machines as Linux and
//! macOS do.
//!
//! One ssh connection per host carries everything ([`connection`]):
//! [`connect`] opens it, or takes the open one, runs a setup script on
//! the machine that starts its daemon and reports where it listens and
//! the login environment ([`crate::setup`]), then opens a `direct-streamlocal`
//! channel to the daemon's socket, as `ssh -W` does, and speaks rho's
//! protocol over it. Every script after that runs in a session channel
//! of its own on the same connection ([`script`]), 5 at once, half the
//! sessions sshd allows a connection by default; the rest wait their
//! turn. The terminals' login
//! shells, under a pseudo-terminal there ([`shell`]), run on a second
//! connection, made when the first terminal opens, so they never take the sessions
//! scripts need; [`MAX_SHELLS`] at most. A new connection costs a key
//! signature, which an agent such as 1Password's asks the user to
//! approve, so the user approves once per host, and once more for its
//! terminals, while the app uses the host and the link stays up; a
//! connection nothing uses closes 10 minutes later.
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
mod shell;

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use rho_protocol::{Client, ClientHello, HelloResponse, Notifications};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub(crate) use config::Resolved;
#[cfg(feature = "test-support")]
pub(crate) use config::{AgentKeys, IdentityAgent};
pub use connection::MAX_SHELLS;
use connection::{Lease, Purpose};

use crate::{
    daemon::{handshake, DaemonError, Machine},
    exec::on_io,
    link::{Link, ScriptError},
    server::wire,
    setup,
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
    pub async fn link(&self) -> Result<SshLink, DaemonError> {
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
        let lease = connection::shared(host, self.route.as_deref(), Purpose::Daemon).await?;
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
    let connection = Arc::clone(link.lease.connection());
    let link = Link::Ssh(link);
    let setup = setup::run(&link, &target.rho).await?;
    let stream = connection.daemon(&setup.socket).await?;
    let (client, notifications, hello) = handshake(
        stream,
        hello,
        &format!("on {} at {}", connection.host(), setup.socket),
    )
    .await?;
    let machine = Machine {
        environment: setup.environment,
        home: Some(setup.home),
        link: Some(link),
    };
    Ok((client, notifications, hello, machine))
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
/// connection and every script run there; its terminals open one of their
/// own through it ([`Self::open_shell`]). The connection stays open while
/// any link to it does, and 10 minutes after. Links are equal when they
/// share a connection.
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
    /// The host, as the user named it.
    pub(crate) fn host(&self) -> &str {
        self.lease.connection().host()
    }

    /// Runs `input` with `sh -s` on the host, on the I/O runtime, within
    /// `deadline` if any. It runs only on the connection the daemon
    /// connection opened: once that is gone this fails rather than connect
    /// again, which could ask the user's key agent for another approval.
    /// Dropping the future stops the script.
    pub(crate) async fn script(
        &self,
        input: Vec<u8>,
        deadline: Option<Duration>,
    ) -> Result<Vec<u8>, ScriptError> {
        let lease = Arc::clone(&self.lease);
        on_io(async move { script::run(lease.connection(), &input, deadline).await })
            .await
            .map_err(ScriptError::Session)?
    }

    /// Runs `input` with `sh -s` on the host, as `rho-ui serve` runs a
    /// browser's script there ([`wire::SCRIPT`]), and returns what it
    /// printed and how it ended, in the server's words; `None` if the
    /// connection went with no word of how. Dropping the future stops the
    /// script.
    pub async fn relay_script(&self, input: Vec<u8>) -> Option<(Vec<u8>, wire::ScriptEnd)> {
        let lease = Arc::clone(&self.lease);
        let ran = on_io(async move { script::sh(lease.connection(), &input).await })
            .await
            .and_then(|ran| ran);
        crate::server::script_end(ran, self.host())
    }

    /// A stream to the Unix socket at `path` on the host, such as the rho
    /// daemon's, as `rho-ui serve` relays one for a browser
    /// ([`wire::SOCKET`]). It keeps the host's connection while open, as a
    /// link does, so the scripts after it never sign in again.
    ///
    /// # Errors
    ///
    /// Fails if sshd won't forward to the socket, or nothing listens there.
    pub async fn open_socket(
        &self,
        path: &str,
    ) -> Result<impl AsyncRead + AsyncWrite + Send + Unpin + 'static, DaemonError> {
        let lease = Arc::clone(&self.lease);
        let path = path.to_owned();
        let stream = on_io(async move { lease.connection().daemon(&path).await }).await??;
        Ok(Leased {
            stream,
            _lease: Arc::clone(&self.lease),
        })
    }
}

/// A stream that keeps its host's connection while open
/// ([`SshLink::open_socket`]).
struct Leased<S> {
    stream: S,
    _lease: Arc<Lease>,
}

impl<S: AsyncRead + Unpin> AsyncRead for Leased<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Leased<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}
