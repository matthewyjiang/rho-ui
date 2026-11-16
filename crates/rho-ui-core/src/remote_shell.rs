//! A login shell on a machine rho-ui reaches through a link, under a
//! pseudo-terminal there: the right panel's terminal on a remote machine,
//! or in a browser on this one. The handles here are the same whether ssh
//! (`ssh/shell.rs`) or rho-ui-serve (`server/shell.rs`) runs it; a task of
//! the link's holds the other [`Ends`], and forwards between them and the
//! shell.

use std::convert::Infallible;

use tokio::sync::{mpsc, oneshot};

/// A login shell on a machine rho-ui reaches through a link: what
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

/// What a [`RemoteShell`] prints, as the link sends it. Keep reading it:
/// over ssh, while it is full, the whole connection waits (see
/// `ssh/shell.rs`).
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
/// queued, and the machine hangs up on the shell, as on a terminal closed.
#[derive(Debug)]
pub struct ShellControl {
    requests: mpsc::UnboundedSender<Request>,
    /// Never sent: the shell's task hangs up when it is dropped.
    _owner: oneshot::Sender<Infallible>,
}

impl ShellControl {
    /// Tells the shell its terminal is now `columns` by `rows`; the machine
    /// resizes the pseudo-terminal, which signals the program in it. Does
    /// nothing once the shell is gone.
    pub fn resize(&self, columns: u16, rows: u16) {
        let _ = self.requests.send(Request::Resize { columns, rows });
    }
}

/// Why a [`RemoteShell`] ended other than by exiting. Each holds why, in
/// words for people.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ShellError {
    /// It never ran: there is no connection, the machine has as many
    /// shells open as its link allows, or it refused a session, the
    /// pseudo-terminal or the shell.
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
    /// Waits for the shell to end: it exited, a signal killed it, the
    /// machine closed its channel, or its [`ShellControl`] or
    /// [`ShellOutput`] went.
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
pub(crate) enum Request {
    Write(Vec<u8>),
    Resize { columns: u16, rows: u16 },
}

/// The link's ends of a [`RemoteShell`]'s handles.
pub(crate) struct Ends {
    /// Where what the shell prints goes; closed once the reader went.
    pub(crate) printed: mpsc::Sender<Vec<u8>>,
    /// What is typed, and resizes, in order.
    pub(crate) requests: mpsc::UnboundedReceiver<Request>,
    /// Resolves once the [`ShellControl`] went: hang up then.
    pub(crate) disowned: oneshot::Receiver<Infallible>,
    /// Says how the shell ended.
    pub(crate) end: oneshot::Sender<Result<(), ShellError>>,
}

/// A shell's handles, with room for `queue` reads of its output, and the
/// ends a link's task holds.
pub(crate) fn channels(queue: usize) -> (RemoteShell, Ends) {
    let (printed, output) = mpsc::channel(queue);
    let (input, requests) = mpsc::unbounded_channel();
    let (owner, disowned) = oneshot::channel();
    let (end, ended) = oneshot::channel();
    let shell = RemoteShell {
        output: ShellOutput(output),
        input: ShellInput(input.clone()),
        control: ShellControl {
            requests: input,
            _owner: owner,
        },
        ended: ShellEnded(ended),
    };
    let ends = Ends {
        printed,
        requests,
        disowned,
        end,
    };
    (shell, ends)
}
