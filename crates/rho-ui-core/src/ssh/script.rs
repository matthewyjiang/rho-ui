//! Running a POSIX `sh` script on a host, in a session channel of its
//! own on the host's shared connection.

use std::time::Duration;

use russh::{client::Msg, ChannelMsg, ChannelReadHalf, ChannelWriteHalf};

use super::connection::Connection;
use crate::daemon::{io, DaemonError};

/// How long [`run`] waits for a script that has a deadline: a tripwire
/// for a script stuck on a hung disk. rho-ui's longest, reading the git
/// repositories of every workspace, took 1.3 s for 133 worktrees (about
/// 10 ms each), so this leaves room for thousands.
pub(super) const SCRIPT_DEADLINE: Duration = Duration::from_secs(60);

/// What a script printed, and how it ended.
#[derive(Debug)]
pub(super) struct Output {
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) ending: Ending,
}

/// How a script ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Ending {
    /// It exited with this status.
    Exited(u32),
    /// A signal killed it; its name.
    Killed(String),
    /// sshd wouldn't run `sh`.
    Refused,
    /// The channel closed with no word of how, as when the connection
    /// died.
    Cut,
}

impl Output {
    /// Why the script failed, in its own words if it wrote any; `None`
    /// when it exited 0. [`Ending::Cut`] is the caller's to word.
    pub(super) fn failure(&self) -> Option<String> {
        let said = String::from_utf8_lossy(&self.stderr);
        let said = said.trim();
        let status = match &self.ending {
            Ending::Exited(0) => return None,
            Ending::Exited(code) => format!("it exited with status {code}"),
            Ending::Killed(signal) => format!("it was killed by signal {signal}"),
            Ending::Refused => "sshd refused to run sh".to_owned(),
            Ending::Cut => "the connection closed while it ran".to_owned(),
        };
        Some(if said.is_empty() {
            status
        } else {
            said.to_owned()
        })
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
/// Fails if the connection is gone, the host refuses a session, the
/// script exits with an error, or it runs past `deadline`.
pub(super) async fn run(
    connection: &Connection,
    input: &[u8],
    deadline: Option<Duration>,
) -> Result<Vec<u8>, DaemonError> {
    let host = connection.host();
    if connection.is_closed() {
        return Err(DaemonError::Unreachable(format!(
            "no ssh connection to {host}"
        )));
    }
    let ran = sh(connection, input);
    let output = match deadline {
        Some(deadline) => tokio::time::timeout(deadline, ran).await.map_err(|_| {
            DaemonError::Unreachable(format!(
                "a script on {host} ran past {} s",
                deadline.as_secs()
            ))
        })?,
        None => ran.await,
    }?;
    match output.failure() {
        None => Ok(output.stdout),
        Some(_) if output.ending == Ending::Cut => Err(DaemonError::Unreachable(format!(
            "the connection to {host} closed while a script ran"
        ))),
        Some(why) => Err(DaemonError::Unreachable(format!(
            "a script on {host} failed: {why}"
        ))),
    }
}

/// Runs `sh -s` in a new session channel, feeds it `input`, and collects
/// what it prints until the channel closes. The input goes while the
/// output comes, so neither side waits on a full window. Dropping the
/// future closes the channel, so a deadline around this covers sending
/// the input too, and channels don't pile up against sshd's
/// `MaxSessions`.
///
/// # Errors
///
/// Fails if the host refuses a session.
pub(super) async fn sh(connection: &Connection, input: &[u8]) -> Result<Output, DaemonError> {
    let (mut read, write) = connection.session().await?.split();
    let write = CloseOnDrop(Some(write));
    let channel = write.channel();
    let send = async {
        channel.exec(true, "sh -s").await?;
        channel.data(input).await?;
        channel.eof().await
    };
    // A failed send means the channel closed, and how it ended says why.
    let (_, output) = tokio::join!(send, receive(&mut read));
    Ok(output)
}

/// Reads a session channel until it closes.
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
                output.ending = Ending::Killed(format!("{signal_name:?}"));
            }
            ChannelMsg::Failure => output.ending = Ending::Refused,
            _ => {}
        }
    }
    output
}

/// A channel's writing half that closes the channel when dropped, as when
/// a script's future is given up. Closing one already closed does
/// nothing.
struct CloseOnDrop(Option<ChannelWriteHalf<Msg>>);

impl CloseOnDrop {
    fn channel(&self) -> &ChannelWriteHalf<Msg> {
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
