//! Running a POSIX `sh` script on a host, in a session channel of its
//! own on the host's shared connection.

use std::{pin::pin, sync::Arc, time::Duration};

use russh::{ChannelMsg, ChannelReadHalf, Sig};

use tokio::{sync::OwnedSemaphorePermit, time::Instant};

use super::connection::{Connection, SCRIPTS_AT_ONCE};
use crate::{
    link::{Ending, Output, ScriptError},
    DaemonError,
};

/// Runs `input` with `sh -s` on the connection's host, within `deadline`
/// if any, which covers its wait for a slot among the connection's
/// scripts too, and returns what it printed. It runs only on a live
/// connection: once that is gone this fails rather than connect again,
/// which could ask the user's key agent for another approval. Runs on the
/// I/O runtime; dropping the future closes the channel, which stops the
/// script.
///
/// # Errors
///
/// Fails if the connection is gone, the host refuses a session or `sh`,
/// the script exits with an error, or `deadline` passes: while it waits
/// for a slot ([`ScriptError::NoSlot`]), or while it runs
/// ([`ScriptError::TimedOut`]).
pub(super) async fn run(
    connection: &Arc<Connection>,
    input: &[u8],
    deadline: Option<Duration>,
) -> Result<Vec<u8>, ScriptError> {
    let output = match deadline {
        Some(deadline) => {
            let until = Instant::now() + deadline;
            let slot = tokio::time::timeout_at(until, connection.script_slot())
                .await
                .map_err(|_| ScriptError::NoSlot {
                    waited: deadline,
                    at_once: SCRIPTS_AT_ONCE,
                })?;
            tokio::time::timeout_at(until, sh(connection, slot, input))
                .await
                .map_err(|_| ScriptError::TimedOut(deadline))?
        }
        None => sh(connection, connection.script_slot().await, input).await,
    }
    .map_err(ScriptError::Session)?;
    output.result()
}

/// Runs `sh -s` in a new session channel, which holds `slot`, from
/// [`Connection::script_slot`], feeds it `input`, and collects what it
/// prints until the channel closes, or sshd refuses to run `sh`.
/// The input goes while the output comes, so neither side waits on a full
/// window. Dropping the future closes the channel, so a deadline around
/// this covers sending the input too, and channels don't pile up against
/// sshd's `MaxSessions`.
///
/// # Errors
///
/// Fails if the connection is gone, or the host refuses a session.
pub(super) async fn sh(
    connection: &Arc<Connection>,
    slot: OwnedSemaphorePermit,
    input: &[u8],
) -> Result<Output, DaemonError> {
    let (mut read, write) = connection.session(slot).await?;
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
            ChannelMsg::ExitStatus { exit_status } => {
                output.ending = Ending::Exited(exit_status.into());
            }
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

#[cfg(all(test, unix))]
mod tests {
    use std::pin::pin;

    use super::*;
    use crate::fake_sshd::FakeSshd;

    /// A script whose deadline passes while the connection's slots are
    /// all taken says it never started, and names the budget, rather than
    /// that it ran too long.
    #[test]
    fn a_script_that_gets_no_slot_by_its_deadline_says_so() {
        let home = tempfile::tempdir().expect("a temporary directory");
        let sshd = FakeSshd::start(home.path());
        smol::block_on(async {
            let ssh = sshd.target().link().await.expect("the fake sshd answers");
            // Held unconfirmed, each of these keeps its slot.
            sshd.hold_sessions();
            let script = |deadline| ssh.script(b"true".to_vec(), deadline);
            let mut running = pin!(futures::future::join_all(
                (0..SCRIPTS_AT_ONCE).map(|_| script(None))
            ));
            assert!(futures::poll!(&mut running).is_pending());
            sshd.sessions_asked(SCRIPTS_AT_ONCE).await;

            let deadline = Duration::from_millis(50);
            match script(Some(deadline)).await {
                Err(ScriptError::NoSlot { waited, at_once }) => {
                    assert_eq!((waited, at_once), (deadline, SCRIPTS_AT_ONCE));
                }
                other => panic!("not for want of a slot: {other:?}"),
            }
        });
    }
}
