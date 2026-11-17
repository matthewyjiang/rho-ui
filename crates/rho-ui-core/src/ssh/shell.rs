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

use std::{path::Path, pin::pin, sync::Arc};

use russh::{client::Msg, ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Pty};
use tokio::sync::mpsc;

use super::{
    connection::{self, Connection, Purpose},
    SshLink,
};
use crate::{
    exec::io,
    link::quote_bytes,
    platform,
    remote_shell::{self, Ends, RemoteShell, Request, ShellError},
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
    /// it couldn't. It runs on the host's terminals' connection, which the
    /// first terminal there opens, reached as this link's was; once this
    /// link's connection is gone it fails rather than sign in again, which
    /// could ask the user's key agent for an approval nobody expects. The
    /// terminals' connection stays open while the shell runs, which ends
    /// when it exits or [`RemoteShell::control`] goes. While the first
    /// terminal on a host waits for the key agent's approval, the others
    /// there wait for it too.
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
        let (shell, ends) = remote_shell::channels(OUTPUT_QUEUE);
        let Ends {
            printed,
            mut requests,
            disowned,
            end,
        } = ends;
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
                if daemon.is_closed() {
                    return Err(ShellError::NotStarted(format!(
                        "no ssh connection to {}",
                        daemon.host()
                    )));
                }
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
        shell
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
    let (mut read, write) = connection
        .session()
        .await
        .map_err(|error| ShellError::NotStarted(error.to_string()))?;
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
