//! The user's login shell on a host, under a pseudo-terminal sshd makes
//! there, in a session channel of its own on the host's shared
//! connection: the right panel's terminal on a remote machine. It asks
//! the user's key agent for nothing, as the connection signed in already.
//!
//! russh's session loop hands each channel what arrives for it through a
//! bounded queue, and waits while that queue is full, so a shell that
//! prints faster than its reader takes it slows the whole connection,
//! the daemon's channel and every script on it too, to the reader's
//! pace. The reader must keep reading [`ShellOutput`], as a terminal that
//! draws what it reads does.

use std::{path::Path, pin::pin, sync::Arc};

use russh::{client::Msg, ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Pty};
use tokio::sync::{mpsc, oneshot};

use super::{connection::Connection, quote_bytes, script::CloseOnDrop, SshLink};
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

/// What the shell's channel asks sshd for with a reply, in the order sshd
/// answers, as each refusal words it.
const ASKED: [&str; 2] = [
    "a pseudo-terminal (its sshd_config may say PermitTTY no)",
    "to run the login shell",
];

/// A login shell on a remote machine, from [`SshLink::open_shell`]: what
/// it prints, where to type, and how it ended. The shell lives while any
/// [`ShellInput`] does and its output is read; dropping the last input,
/// or the output, closes its channel, and sshd hangs up on it.
#[derive(Debug)]
pub struct RemoteShell {
    /// What it prints.
    pub output: ShellOutput,
    /// What it reads, and the size of its terminal.
    pub input: ShellInput,
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

    /// The next bytes the shell printed, if they came; doesn't wait.
    pub fn try_recv(&mut self) -> Option<Vec<u8>> {
        self.0.try_recv().ok()
    }
}

/// Types into a [`RemoteShell`] and sizes its terminal, in the order
/// called. Clones go to the same shell, which lives while any does.
#[derive(Clone, Debug)]
pub struct ShellInput(mpsc::UnboundedSender<Request>);

impl ShellInput {
    /// Sends `bytes` to the shell, as typed. Does nothing once it is gone.
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.0.send(Request::Write(bytes));
    }

    /// Tells the shell its terminal is now `columns` by `rows`, as
    /// `window-change`; sshd resizes the pseudo-terminal, which signals
    /// the program in it. Does nothing once the shell is gone.
    pub fn resize(&self, columns: u16, rows: u16) {
        let _ = self.0.send(Request::Resize { columns, rows });
    }
}

/// How a [`RemoteShell`] ended.
#[derive(Debug)]
pub struct ShellEnded(oneshot::Receiver<Result<(), DaemonError>>);

impl ShellEnded {
    /// Waits for the shell to end: it exited, a signal killed it, or its
    /// last [`ShellInput`] or its [`ShellOutput`] went.
    ///
    /// # Errors
    ///
    /// Says why if it couldn't start: the connection is gone, or sshd
    /// refused a session, a pseudo-terminal, or the shell. Or if the
    /// channel closed with no word of the shell's end, as when the
    /// connection died.
    pub async fn wait(self) -> Result<(), DaemonError> {
        self.0.await.unwrap_or(Err(DaemonError::Closed))
    }
}

/// What a [`ShellInput`] asks of the shell.
#[derive(Debug)]
enum Request {
    Write(Vec<u8>),
    Resize { columns: u16, rows: u16 },
}

impl SshLink {
    /// Starts the user's login shell in `dir` on the machine, under a
    /// pseudo-terminal there of `columns` by `rows` that is a `term`, with
    /// `COLORTERM=truecolor` if sshd accepts it (its `AcceptEnv` must name
    /// it). Returns at once: the shell starts on the I/O runtime, and
    /// [`RemoteShell::ended`] says if it couldn't. It runs only on the
    /// connection the daemon connection opened: once that is gone this
    /// fails rather than connect again, which could ask the user's key
    /// agent for another approval. The connection stays open while the
    /// shell runs.
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
        let term = term.to_owned();
        let (printed, output) = mpsc::channel(OUTPUT_QUEUE);
        let (input, mut requests) = mpsc::unbounded_channel();
        let (end, ended) = oneshot::channel();
        // Held while the shell runs, so the connection is.
        let lease = Arc::clone(&self.lease);
        io().spawn(async move {
            let ended = run(
                lease.connection(),
                command,
                &term,
                (columns, rows),
                &printed,
                &mut requests,
            )
            .await;
            let _ = end.send(ended);
        });
        RemoteShell {
            output: ShellOutput(output),
            input: ShellInput(input),
            ended: ShellEnded(ended),
        }
    }
}

/// Opens a session channel on `connection` and asks for a pseudo-terminal
/// that is a `term` of `columns` by `rows`, and to run `command`; then
/// forwards what it prints to `output`, and `requests` to it, at once, so
/// a write waiting on the host's window never stops the reading. Runs on
/// the I/O runtime; returns when the shell ends, and closes the channel.
///
/// # Errors
///
/// As [`ShellEnded::wait`].
async fn run(
    connection: &Connection,
    command: Vec<u8>,
    term: &str,
    (columns, rows): (u16, u16),
    output: &mpsc::Sender<Vec<u8>>,
    requests: &mut mpsc::UnboundedReceiver<Request>,
) -> Result<(), DaemonError> {
    let host = connection.host();
    if connection.is_closed() {
        return Err(DaemonError::Unreachable(format!(
            "no ssh connection to {host}"
        )));
    }
    let (mut read, write) = connection.session().await?.split();
    let write = CloseOnDrop::new(write);
    let channel = write.channel();
    let ask = async move {
        channel
            .request_pty(
                true,
                term,
                u32::from(columns),
                u32::from(rows),
                0,
                0,
                &[(Pty::IUTF8, 1)],
            )
            .await?;
        channel.set_env(false, "COLORTERM", "truecolor").await?;
        channel.exec(true, command).await
    };
    if ask.await.is_err() {
        return Err(DaemonError::Unreachable(format!(
            "the connection to {host} closed"
        )));
    }
    let mut receiving = pin!(receive(&mut read, output, host));
    tokio::select! {
        ended = &mut receiving => ended,
        served = serve(channel, requests) => match served {
            Served::InputGone => Ok(()),
            // How the channel ended says why.
            Served::ChannelGone => receiving.await,
        },
    }
}

/// Forwards what the shell prints to `output` until it ends.
///
/// # Errors
///
/// If sshd refuses something [`ASKED`] lists, or the channel closes with
/// no word of the shell's end, as when the connection died.
async fn receive(
    read: &mut ChannelReadHalf,
    output: &mpsc::Sender<Vec<u8>>,
    host: &str,
) -> Result<(), DaemonError> {
    let mut asked = ASKED.iter();
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
                asked.next();
            }
            ChannelMsg::Failure => {
                let what = asked.next().copied().unwrap_or("a request");
                return Err(DaemonError::Unreachable(format!("{host} refused {what}")));
            }
            ChannelMsg::ExitStatus { .. } | ChannelMsg::ExitSignal { .. } => return Ok(()),
            _ => {}
        }
    }
    Err(DaemonError::Unreachable(format!(
        "the connection to {host} closed"
    )))
}

/// How [`serve`] ended.
enum Served {
    /// Every [`ShellInput`] went.
    InputGone,
    /// The channel closed.
    ChannelGone,
}

/// Sends `requests` to the shell's channel in order, until every
/// [`ShellInput`] or the channel is gone.
async fn serve(
    channel: &ChannelWriteHalf<Msg>,
    requests: &mut mpsc::UnboundedReceiver<Request>,
) -> Served {
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
            return Served::ChannelGone;
        }
    }
    Served::InputGone
}
