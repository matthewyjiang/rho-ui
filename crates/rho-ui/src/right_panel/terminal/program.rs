//! What a terminal runs ([`Program`]): a shell under a pseudo-terminal
//! here (`local.rs`, off the browser), or the user's login shell on a
//! machine reached through a link, under a pseudo-terminal there
//! ([`Link::open_shell`]: rho-ui-core's `ssh/shell.rs` and
//! `server/shell.rs` say how it runs).
//!
//! Here, threads of its own read the pseudo-terminal, write to it, and
//! wait for the shell to exit, as each of those blocks; for a remote shell
//! rho-ui-core's I/O runtime does. Either way the terminal only parses
//! what was read and queues what to write, and reads all the time: a
//! remote shell's output that waits unread holds up the whole ssh
//! connection.

#[cfg(not(target_family = "wasm"))]
mod local;

use std::path::Path;

use futures::{
    future::LocalBoxFuture,
    stream::{self, BoxStream},
    FutureExt as _, StreamExt as _,
};
use gpui_kit::SharedString;
use rho_ui_core::{Link, RemoteShell, ShellControl, ShellError, ShellInput};

#[cfg(not(target_family = "wasm"))]
use self::local::LocalPty;

/// The terminal type the shell is told. Ghostty's own, `xterm-ghostty`,
/// isn't installed on most machines, a remote one least of all.
pub(super) const TERM: &str = "xterm-256color";

/// The most reads of the program's output, each up to `local.rs`'s
/// `READ_BYTES`, that wait for the UI thread. Past it the reader thread
/// waits, and so does a program printing faster than the screen takes it,
/// as with any terminal.
/// That holds about 1 MiB. The screen, not the queue, sets the pace: in
/// a debug build 40 MB of `yes` took 9.6 s with a queue of 1, 9.3 s with
/// 16 and 8.8 s with 64. A remote shell's queue is rho-ui-core's; this
/// many of its packets at most go in one frame.
pub(super) const OUTPUT_QUEUE: usize = 16;

/// What a terminal runs: a shell under a pseudo-terminal here, or the
/// login shell on a remote machine, under one there.
pub(super) struct Program {
    /// The columns and rows of the terminal it started in.
    pub(super) grid: (u16, u16),
    /// What it prints. Dropping it hangs up on a remote shell.
    pub(super) output: BoxStream<'static, Vec<u8>>,
    /// Where what is typed goes.
    pub(super) input: Input,
    pub(super) control: Control,
    /// Resolves when the shell ends: with why, for a remote one that
    /// couldn't start or whose connection went.
    pub(super) ended: LocalBoxFuture<'static, Result<(), SharedString>>,
}

impl Program {
    /// Starts the user's login shell in `dir` on `link`'s machine, under a
    /// pseudo-terminal there of `grid`, columns by rows.
    pub(super) fn remote(link: &Link, dir: &Path, grid: (u16, u16)) -> Self {
        let RemoteShell {
            output,
            input,
            control,
            ended,
        } = link.open_shell(dir, TERM, grid.0, grid.1);
        let output = stream::unfold(output, |mut output| async move {
            let bytes = output.recv().await?;
            Some((bytes, output))
        });
        Self {
            grid,
            output: output.boxed(),
            input: Input::Remote(input),
            control: Control::Remote(control),
            ended: async move {
                ended.wait().await.map_err(|error| {
                    SharedString::from(match error {
                        ShellError::NotStarted(why) => format!("Couldn't start: {why}"),
                        ShellError::Stopped(why) => format!("The shell stopped: {why}"),
                    })
                })
            }
            .boxed_local(),
        }
    }
}

/// Where a terminal's input goes, in order: what is typed, and the
/// terminal's replies to the program's queries. Clones go to the same
/// program, and don't keep it running.
#[derive(Clone)]
pub(super) enum Input {
    /// The writer thread of the pseudo-terminal here.
    #[cfg(not(target_family = "wasm"))]
    Local(std::sync::mpsc::Sender<Vec<u8>>),
    /// A remote shell's channel.
    Remote(ShellInput),
}

impl Input {
    /// Queues `bytes` for the program. Does nothing once it is gone.
    pub(super) fn write(&self, bytes: Vec<u8>) {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Input::Local(writer) => {
                let _ = writer.send(bytes);
            }
            Input::Remote(shell) => shell.write(bytes),
        }
    }
}

/// The rest of a terminal's hold on its program: its size, the name of
/// what runs in it, and its life. Dropped, the program is hung up on at
/// once.
pub(super) enum Control {
    /// The pseudo-terminal here.
    #[cfg(not(target_family = "wasm"))]
    Local(LocalPty),
    /// A remote shell's channel, which closes when this goes, and sshd
    /// hangs up on the shell.
    Remote(ShellControl),
}

impl Control {
    /// Tells the program its terminal is now `grid`, columns by rows.
    pub(super) fn resize(&self, grid: (u16, u16)) {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Control::Local(pty) => pty.resize(grid),
            Control::Remote(shell) => shell.resize(grid.0, grid.1),
        }
    }

    /// The name the system gives the foreground process, which only a
    /// pseudo-terminal here can tell.
    pub(super) fn foreground_program(&self) -> Option<String> {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Control::Local(pty) => pty.foreground_program(),
            Control::Remote(_) => None,
        }
    }
}
