//! The user's login shell on a host, under a pseudo-terminal sshd makes
//! there, in a session channel of its own on the host's terminals'
//! connection: the right panel's terminal on a remote machine. The first
//! terminal on a host opens that connection, reached as the daemon's was,
//! which costs one more key signature; the rest share it, as scripts
//! share the daemon's, so scripts and terminals never compete for sshd's
//! sessions.
//!
//! [`SshLink::open_shell`] starts it: a task on the I/O runtime opens the
//! channel, asks for a pseudo-terminal (`pty-req`, with `IUTF8`), sets
//! `COLORTERM=truecolor` and this machine's locale (`LANG` and `LC_*`)
//! where sshd's `AcceptEnv` lets them through, and runs
//! `cd -- DIR && exec "$SHELL" -l`. Then it forwards what the shell
//! prints to [`ShellOutput`], and what [`ShellInput`] and
//! [`ShellControl`] send to the channel, side by side, so a write waiting
//! on the host's window never stops the reading. A resize goes as
//! `window-change`.
//!
//! [`ShellControl`] owns the shell's life, as a local pseudo-terminal's
//! owner does: dropping it, or the [`ShellOutput`], closes the channel at
//! once, without sending the input still queued, and sshd hangs up on
//! the shell. Clones of [`ShellInput`] don't keep it. One machine runs at
//! most [`MAX_SHELLS`](crate::MAX_SHELLS) shells at once, the sessions
//! sshd allows on their connection by default; a shell's slot frees when
//! its task ends.
//!
//! russh's session loop hands each channel what arrives for it through a
//! queue of `channel_buffer_size` messages (100, `client::Config`'s
//! default), each a packet of at most `maximum_packet_size` (32 KiB), and
//! waits while that queue is full (`client/encrypted.rs`, at
//! `CHANNEL_DATA`). It tops the channel's window up as packets arrive,
//! not as they are read, so the window (2 MiB) bounds none of it. A
//! shell's output that its reader doesn't take holds up the whole
//! connection, the daemon's channel and every script on it too, once
//! about 4.1 MiB waits: russh's queue (up to 3.2 MiB) and
//! [`ShellOutput`]'s [`OUTPUT_QUEUE`] packets (1 MiB). The reader must
//! keep reading, as a terminal that draws what it reads does.

use std::{convert::Infallible, path::Path, pin::pin, sync::Arc};

use russh::{client::Msg, ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Pty};
use tokio::sync::{mpsc, oneshot};

use super::{
    connection::{self, Connection, Purpose},
    quote_bytes, SshLink,
};
use crate::{
    daemon::{io, DaemonError},
    platform,
};

/// Packets of output that wait for the reader, each at most 32 KiB
/// (russh's default `maximum_packet_size`, the largest sshd sends this
/// client): 1 MiB, as the local terminal's queue holds. The reader, not
/// the queue, sets the pace: the local terminal drew 40 MB of `yes` in
/// 9.6 s with a queue of 1 read and in 8.8 s with 64.
const OUTPUT_QUEUE: usize = 32;

/// The pseudo-terminal request, as its refusal names it.
const PSEUDO_TERMINAL: &str = "a pseudo-terminal (its sshd_config may say PermitTTY no)";

/// The `exec` request, as its refusal names it.
const LOGIN_SHELL: &str = "to run the login shell";

/// A login shell on a remote machine, from [`SshLink::open_shell`]: what
/// it prints, where to type, its terminal's size and life, and how it
/// ended.
#[derive(Debug)]
pub struct RemoteShell {
    /// What it prints. Dropping it hangs up on the shell.
    pub output: ShellOutput,
    /// What it reads; clones go to the same shell.
    pub input: ShellInput,
    /// The size of its terminal. Dropping it hangs up on the shell.
    pub control: ShellControl,
    /// How it ended.
    pub ended: ShellEnded,
}

/// What a [`RemoteShell`] prints, as sshd sends it. Keep reading it: while
/// it is full, the whole ssh connection waits (see the module docs).
#[derive(Debug)]
pub struct ShellOutput(mpsc::Receiver<Vec<u8>>);

impl ShellOutput {
    /// The next bytes the shell printed; `None` once its channel is gone.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.0.recv().await
    }
}

/// Types into a [`RemoteShell`], in the order called, and in order with
/// its [`ShellControl`]'s resizes. Clones go to the same shell, and don't
/// keep it running.
#[derive(Clone, Debug)]
pub struct ShellInput(mpsc::UnboundedSender<Request>);

impl ShellInput {
    /// Sends `bytes` to the shell, as typed. Does nothing once it is gone.
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.0.send(Request::Write(bytes));
    }
}

/// Sizes a [`RemoteShell`]'s terminal, and owns the shell's life: dropping
/// it closes the shell's channel at once, without sending the input still
/// queued, and sshd hangs up on the shell, as on a terminal closed.
#[derive(Debug)]
pub struct ShellControl {
    requests: mpsc::UnboundedSender<Request>,
    /// Never sent: the shell's task hangs up when it is dropped.
    _owner: oneshot::Sender<Infallible>,
}

impl ShellControl {
    /// Tells the shell its terminal is now `columns` by `rows`, as
    /// `window-change`; sshd resizes the pseudo-terminal, which signals
    /// the program in it. Does nothing once the shell is gone.
    pub fn resize(&self, columns: u16, rows: u16) {
        let _ = self.requests.send(Request::Resize { columns, rows });
    }
}

/// Why a [`RemoteShell`] ended other than by exiting. Each holds why, in
/// words for people.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ShellError {
    /// It never ran: there is no connection, the machine has
    /// [`MAX_SHELLS`](crate::MAX_SHELLS) open, or sshd refused a session,
    /// the pseudo-terminal or the shell.
    #[error("{0}")]
    NotStarted(String),
    /// It ran, then its connection went, or rho-ui lost its task.
    #[error("{0}")]
    Stopped(String),
}

/// How a [`RemoteShell`] ended.
#[derive(Debug)]
pub struct ShellEnded(oneshot::Receiver<Result<(), ShellError>>);

impl ShellEnded {
    /// Waits for the shell to end: it exited, a signal killed it, sshd
    /// closed its channel, or its [`ShellControl`] or [`ShellOutput`]
    /// went.
    ///
    /// # Errors
    ///
    /// [`ShellError::NotStarted`] if it couldn't start, and
    /// [`ShellError::Stopped`] if its connection went while it ran.
    pub async fn wait(self) -> Result<(), ShellError> {
        self.0.await.unwrap_or_else(|_| {
            Err(ShellError::Stopped(
                "the task that ran it in rho-ui stopped unexpectedly".to_owned(),
            ))
        })
    }
}

/// What a [`ShellInput`] or [`ShellControl`] asks of the shell.
#[derive(Debug)]
enum Request {
    Write(Vec<u8>),
    Resize { columns: u16, rows: u16 },
}

/// What the shell's channel sends in its first requests.
struct Start<'a> {
    /// `cd -- DIR && exec "$SHELL" -l`.
    command: Vec<u8>,
    term: &'a str,
    columns: u16,
    rows: u16,
    /// Variables to set, as `LANG` and `LC_*`, best effort.
    env: Vec<(String, String)>,
}

impl SshLink {
    /// Starts the user's login shell in `dir` on the machine, under a
    /// pseudo-terminal there of `columns` by `rows` that is a `term`, with
    /// `COLORTERM=truecolor` and this machine's `LANG` and `LC_*` if sshd
    /// accepts them (its `AcceptEnv` must name them). Returns at once: the
    /// shell starts on the I/O runtime, and [`RemoteShell::ended`] says if
    /// it couldn't. It runs only on the connection the daemon connection
    /// opened: once that is gone this fails rather than connect again,
    /// which could ask the user's key agent for another approval. The
    /// connection stays open while the shell runs, which ends when it
    /// exits or [`RemoteShell::control`] goes.
    ///
    /// sshd hands the command line, `cd -- 'dir' && exec "$SHELL" -l`, to
    /// the user's login shell to parse, so this assumes a POSIX one, such
    /// as sh, bash or zsh, unlike the scripts, which `sh` reads. fish reads
    /// `\\` and `\'` inside single quotes, so there a `dir` holding a
    /// backslash goes wrong.
    pub fn open_shell(&self, dir: &Path, term: &str, columns: u16, rows: u16) -> RemoteShell {
        let mut command = b"cd -- ".to_vec();
        command.extend(quote_bytes(&platform::path_to_bytes(dir.as_os_str())));
        command.extend(br#" && exec "$SHELL" -l"#);
        let mut env = vec![("COLORTERM".to_owned(), "truecolor".to_owned())];
        env.extend(locale());
        let term = term.to_owned();
        let (printed, output) = mpsc::channel(OUTPUT_QUEUE);
        let (input, mut requests) = mpsc::unbounded_channel();
        let (owner, disowned) = oneshot::channel();
        let (end, ended) = oneshot::channel();
        // The host and how the daemon's connection reached it, to reach it
        // again for the terminals.
        let daemon = Arc::clone(self.lease.connection());
        io().spawn(async move {
            let start = Start {
                command,
                term: &term,
                columns,
                rows,
                env,
            };
            let ran = async {
                // Held while the shell runs, so the connection is.
                let lease =
                    connection::shared(daemon.host(), Some(daemon.resolved()), Purpose::Terminals)
                        .await
                        .map_err(|error| ShellError::NotStarted(error.to_string()))?;
                drop(daemon);
                run(lease.connection(), start, &printed, &mut requests).await
            };
            let ended = tokio::select! {
                ended = ran => ended,
                // The terminal went. Dropping `ran` drops the channel's
                // writing half, which closes it, and the shell's slot.
                _ = disowned => Ok(()),
                () = printed.closed() => Ok(()),
            };
            let _ = end.send(ended);
        });
        RemoteShell {
            output: ShellOutput(output),
            input: ShellInput(input.clone()),
            control: ShellControl {
                requests: input,
                _owner: owner,
            },
            ended: ShellEnded(ended),
        }
    }
}

/// This machine's locale, `LANG` and `LC_*`, to send as the
/// `SendEnv LANG LC_*` in the `ssh_config` Debian and macOS ship does,
/// which the `ssh -tt` this replaced followed. A variable that isn't
/// UTF-8 stays here: russh sends text.
fn locale() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| name == "LANG" || name.starts_with("LC_"))
        .collect()
}

/// The requests the shell's channel sent asking for a reply, in order, as
/// a refusal of each names it. sshd answers a channel's requests in the
/// order they came, and those sent with no reply asked not at all. Every
/// request's `want_reply` comes from here, so the list follows the flags
/// sent: a request that comes to ask for a reply takes [`Self::reply`],
/// and names itself.
#[derive(Debug, Default)]
struct Asked(Vec<&'static str>);

impl Asked {
    /// A `want_reply` that asks for none.
    const NO_REPLY: bool = false;

    /// A `want_reply` that asks for one, noting the request as `what`.
    fn reply(&mut self, what: &'static str) -> bool {
        self.0.push(what);
        true
    }
}

/// Takes a shell slot on `connection`, opens a session channel and sends
/// it `start`; then forwards what it prints to `output`, and `requests` to
/// it, at once, so a write waiting on the host's window never stops the
/// reading. Runs on the I/O runtime; returns when the shell ends, and
/// closes the channel. Dropped, it closes the channel too, and frees the
/// slot.
///
/// # Errors
///
/// As [`ShellEnded::wait`].
async fn run(
    connection: &Connection,
    start: Start<'_>,
    output: &mpsc::Sender<Vec<u8>>,
    requests: &mut mpsc::UnboundedReceiver<Request>,
) -> Result<(), ShellError> {
    // Held while this runs.
    let _slot = connection.shell_slot().map_err(ShellError::NotStarted)?;
    let (mut read, write) = connection.session().await.map_err(|error| match error {
        DaemonError::Unreachable(why) => ShellError::NotStarted(why),
        other => ShellError::NotStarted(other.to_string()),
    })?;
    let channel = write.channel();
    let Start {
        command,
        term,
        columns,
        rows,
        env,
    } = start;
    let mut asked = Asked::default();
    let sent: Result<(), russh::Error> = async {
        channel
            .request_pty(
                asked.reply(PSEUDO_TERMINAL),
                term,
                u32::from(columns),
                u32::from(rows),
                0,
                0,
                &[(Pty::IUTF8, 1)],
            )
            .await?;
        // Best effort, as OpenSSH's `SendEnv`: sshd sets those its
        // `AcceptEnv` names and drops the rest without a word.
        for (name, value) in env {
            channel.set_env(Asked::NO_REPLY, name, value).await?;
        }
        channel.exec(asked.reply(LOGIN_SHELL), command).await
    }
    .await;
    if sent.is_err() {
        // A send fails only once russh's session loop is gone.
        return Err(ShellError::NotStarted(format!(
            "the connection to {} closed",
            connection.host()
        )));
    }
    let mut receiving = pin!(receive(&mut read, output, connection, asked));
    tokio::select! {
        ended = &mut receiving => ended,
        // The channel or every request's sender went; how the channel
        // ends says why.
        () = serve(channel, requests) => receiving.await,
    }
}

/// Forwards what the shell prints to `output` until it ends.
///
/// # Errors
///
/// [`ShellError::NotStarted`] if sshd refuses something `asked` lists,
/// and [`ShellError::Stopped`] if the connection closes.
async fn receive(
    read: &mut ChannelReadHalf,
    output: &mpsc::Sender<Vec<u8>>,
    connection: &Connection,
    asked: Asked,
) -> Result<(), ShellError> {
    let host = connection.host();
    let mut answers = asked.0.into_iter();
    while let Some(message) = read.wait().await {
        match message {
            // Under a pseudo-terminal stderr is stdout, so sshd sends no
            // extended data but what the shell's own setup may print.
            ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                if output.send(data.to_vec()).await.is_err() {
                    // The reader went, and the terminal with it.
                    return Ok(());
                }
            }
            ChannelMsg::Success => {
                answers.next();
            }
            ChannelMsg::Failure => {
                let what = answers.next().unwrap_or("a request");
                return Err(ShellError::NotStarted(format!("{host} refused {what}")));
            }
            ChannelMsg::ExitStatus { .. } | ChannelMsg::ExitSignal { .. } => return Ok(()),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    // sshd closed the channel with no word of how the shell ended, or the
    // connection went. russh's session drops its handle's queue before
    // the channels' (its `Session`'s fields drop in that order), so a
    // connection that went reads as closed by the time its channels end.
    if connection.is_closed() {
        Err(ShellError::Stopped(format!(
            "the connection to {host} closed"
        )))
    } else {
        Ok(())
    }
}

/// Sends `requests` to the shell's channel in order, until the channel or
/// every request's sender is gone.
async fn serve(channel: &ChannelWriteHalf<Msg>, requests: &mut mpsc::UnboundedReceiver<Request>) {
    while let Some(request) = requests.recv().await {
        let sent = match request {
            Request::Write(bytes) => channel.data_bytes(bytes).await,
            Request::Resize { columns, rows } => {
                channel
                    .window_change(u32::from(columns), u32::from(rows), 0, 0)
                    .await
            }
        };
        if sent.is_err() {
            return;
        }
    }
}
