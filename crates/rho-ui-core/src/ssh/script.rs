//! Running a POSIX `sh` script on a host, in a session channel of its
//! own on the host's shared connection.

use std::{pin::pin, time::Duration};

use russh::{client::Msg, ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Sig};

use super::connection::Connection;
use crate::daemon::{io, DaemonError};

/// How long a script that has a deadline may run: a tripwire for a
/// script stuck on a hung disk. rho-ui's longest, reading the git
/// repositories of every workspace, took 1.3 s for 133 worktrees (about
/// 10 ms each), so this leaves room for thousands.
pub(super) const SCRIPT_DEADLINE: Duration = Duration::from_secs(60);

/// Why a script didn't succeed. Callers word it for what the script did.
#[derive(Debug)]
pub(super) enum ScriptError {
    /// It ran past its deadline, this one.
    TimedOut(Duration),
    /// The channel closed with no word of how the script ended, as when
    /// the connection died.
    Cut,
    /// sshd wouldn't run `sh`.
    Refused,
    /// It exited with an error, or a signal killed it: why, in its own
    /// words if it wrote any to stderr.
    Failed(String),
    /// It got no session channel: the connection is gone, or the host
    /// refused one.
    Session(DaemonError),
}

/// What a script printed, and how it ended.
#[derive(Debug)]
struct Output {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    ending: Ending,
}

/// How a script ended.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ending {
    /// It exited with this status.
    Exited(u32),
    /// A signal killed it; its name, as `SIGTERM`.
    Killed(String),
    /// sshd wouldn't run `sh`.
    Refused,
    /// The channel closed with no word of how, as when the connection
    /// died.
    Cut,
}

impl Output {
    /// What it printed, if it exited 0; else why not.
    fn result(self) -> Result<Vec<u8>, ScriptError> {
        let status = match self.ending {
            Ending::Exited(0) => return Ok(self.stdout),
            Ending::Refused => return Err(ScriptError::Refused),
            Ending::Cut => return Err(ScriptError::Cut),
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

/// Runs `input` with `sh -s` on the connection's host, within `deadline`
/// if any, and returns what it printed. It runs only on a live
/// connection: once that is gone this fails rather than connect again,
/// which could ask the user's key agent for another approval. Runs on the
/// I/O runtime; dropping the future closes the channel, which stops the
/// script.
///
/// # Errors
///
/// Fails if the connection is gone, the host refuses a session or `sh`,
/// the script exits with an error, or it runs past `deadline`.
pub(super) async fn run(
    connection: &Connection,
    input: &[u8],
    deadline: Option<Duration>,
) -> Result<Vec<u8>, ScriptError> {
    if connection.is_closed() {
        return Err(ScriptError::Session(DaemonError::Unreachable(format!(
            "no ssh connection to {}",
            connection.host()
        ))));
    }
    let ran = sh(connection, input);
    let output = match deadline {
        Some(deadline) => tokio::time::timeout(deadline, ran)
            .await
            .map_err(|_| ScriptError::TimedOut(deadline))?,
        None => ran.await,
    }?;
    output.result()
}

/// Runs `sh -s` in a new session channel, feeds it `input`, and collects
/// what it prints until the channel closes, or sshd refuses to run `sh`.
/// The input goes while the output comes, so neither side waits on a full
/// window. Dropping the future closes the channel, so a deadline around
/// this covers sending the input too, and channels don't pile up against
/// sshd's `MaxSessions`.
///
/// # Errors
///
/// Fails if the host refuses a session.
async fn sh(connection: &Connection, input: &[u8]) -> Result<Output, ScriptError> {
    let (mut read, write) = connection
        .session()
        .await
        .map_err(ScriptError::Session)?
        .split();
    let write = CloseOnDrop::new(write);
    let channel = write.channel();
    let mut send = pin!(async {
        channel.exec(true, "sh -s").await?;
        channel.data(input).await?;
        channel.eof().await
    });
    let mut receiving = pin!(receive(&mut read));
    let output = tokio::select! {
        output = &mut receiving => output,
        // A failed send means the channel closed, and how it ended says
        // why; a refused `sh` reads none of the input, so the output is
        // what ends this, not the send.
        _ = &mut send => receiving.await,
    };
    Ok(output)
}

/// Reads a session channel until it closes, or sshd refuses to run `sh`,
/// which leaves the channel open.
async fn receive(read: &mut ChannelReadHalf) -> Output {
    let mut output = Output {
        stdout: Vec::new(),
        stderr: Vec::new(),
        ending: Ending::Cut,
    };
    while let Some(message) = read.wait().await {
        match message {
            ChannelMsg::Data { data } => output.stdout.extend_from_slice(&data),
            // Extended data of type 1 is stderr (RFC 4254 § 5.2).
            ChannelMsg::ExtendedData { data, ext: 1 } => output.stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => output.ending = Ending::Exited(exit_status),
            ChannelMsg::ExitSignal { signal_name, .. } => {
                output.ending = Ending::Killed(signal(&signal_name));
            }
            // The only request on the channel is `exec`'s.
            ChannelMsg::Failure => {
                output.ending = Ending::Refused;
                break;
            }
            _ => {}
        }
    }
    output
}

/// `signal`'s name, as `SIGTERM`.
fn signal(signal: &Sig) -> String {
    let name = match signal {
        Sig::ABRT => "ABRT",
        Sig::ALRM => "ALRM",
        Sig::FPE => "FPE",
        Sig::HUP => "HUP",
        Sig::ILL => "ILL",
        Sig::INT => "INT",
        Sig::KILL => "KILL",
        Sig::PIPE => "PIPE",
        Sig::QUIT => "QUIT",
        Sig::SEGV => "SEGV",
        Sig::TERM => "TERM",
        Sig::USR1 => "USR1",
        Sig::Custom(name) => name.as_str(),
    };
    format!("SIG{name}")
}

/// A channel's writing half that closes the channel when dropped, as when
/// a script's future is given up, a shell ends, or sshd refused either.
/// Closing one already closed does nothing.
pub(super) struct CloseOnDrop(Option<ChannelWriteHalf<Msg>>);

impl CloseOnDrop {
    pub(super) fn new(channel: ChannelWriteHalf<Msg>) -> Self {
        Self(Some(channel))
    }

    pub(super) fn channel(&self) -> &ChannelWriteHalf<Msg> {
        self.0.as_ref().expect("only dropping takes the channel")
    }
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if let Some(channel) = self.0.take() {
            // On the I/O runtime, as the future may drop on any thread.
            io().spawn(async move {
                let _ = channel.close().await;
            });
        }
    }
}
