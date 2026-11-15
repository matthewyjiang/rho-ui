//! What a terminal runs ([`Program`]): a shell under a pseudo-terminal
//! here, or the user's login shell on a remote machine, under a
//! pseudo-terminal sshd makes there, in a channel on the ssh connection
//! the machine's daemon rides ([`SshLink::open_shell`]; rho-ui-core's
//! `ssh/shell.rs` says how it runs).
//!
//! Here, threads of its own read the pseudo-terminal, write to it, and
//! wait for the shell to exit, as each of those blocks; for a remote shell
//! rho-ui-core's I/O runtime does. Either way the terminal only parses
//! what was read and queues what to write, and reads all the time: a
//! remote shell's output that waits unread holds up the whole ssh
//! connection.

use std::{
    io::{Read, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use futures::{
    channel::{mpsc, oneshot},
    future::LocalBoxFuture,
    stream::{self, BoxStream},
    FutureExt as _, SinkExt as _, StreamExt as _,
};
use gpui_kit::SharedString;
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use rho_ui_core::{RemoteShell, ShellControl, ShellError, ShellInput, SshLink};

/// The terminal type the shell is told. Ghostty's own, `xterm-ghostty`,
/// isn't installed on most machines, a remote one least of all.
pub(super) const TERM: &str = "xterm-256color";

/// The most bytes one read of the pseudo-terminal takes.
const READ_BYTES: usize = 64 * 1024;

/// The most reads of the program's output, each up to [`READ_BYTES`], that
/// wait for the UI thread. Past it the reader thread waits, and so does a
/// program printing faster than the screen takes it, as with any terminal.
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
    /// Starts `command` under a pseudo-terminal here of `grid`, columns by
    /// rows.
    pub(super) fn local(mut command: CommandBuilder, grid: (u16, u16)) -> Result<Self, String> {
        let pair = native_pty_system()
            .openpty(pty_size(grid))
            .map_err(|error| error.to_string())?;
        command.env("TERM", TERM);
        command.env("COLORTERM", "truecolor");
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| error.to_string())?;
        drop(pair.slave);
        let exited = Arc::new(AtomicBool::new(false));
        // Hangs up on the shell, dropped when a step below fails.
        let pty = LocalPty {
            master: pair.master,
            killer: child.clone_killer(),
            exited: Arc::clone(&exited),
        };
        let exit = wait_for(child, exited).map_err(|error| error.to_string())?;
        let reader = pty
            .master
            .try_clone_reader()
            .map_err(|error| error.to_string())?;
        let output = read_from(reader).map_err(|error| error.to_string())?;
        let writer = pty
            .master
            .take_writer()
            .map_err(|error| error.to_string())?;
        let input = write_to(writer).map_err(|error| error.to_string())?;
        Ok(Self {
            grid,
            output: output.boxed(),
            input: Input::Local(input),
            control: Control::Local(pty),
            // The waiter thread ends only when the shell does.
            ended: exit.map(|_| Ok::<_, SharedString>(())).boxed_local(),
        })
    }

    /// Starts the user's login shell in `dir` on `ssh`'s machine, under a
    /// pseudo-terminal there of `grid`, columns by rows.
    pub(super) fn remote(ssh: &SshLink, dir: &Path, grid: (u16, u16)) -> Self {
        let RemoteShell {
            output,
            input,
            control,
            ended,
        } = ssh.open_shell(dir, TERM, grid.0, grid.1);
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
    Local(std::sync::mpsc::Sender<Vec<u8>>),
    /// A remote shell's channel.
    Remote(ShellInput),
}

impl Input {
    /// Queues `bytes` for the program. Does nothing once it is gone.
    pub(super) fn write(&self, bytes: Vec<u8>) {
        match self {
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
    Local(LocalPty),
    /// A remote shell's channel, which closes when this goes, and sshd
    /// hangs up on the shell.
    Remote(ShellControl),
}

impl Control {
    /// Tells the program its terminal is now `grid`, columns by rows.
    pub(super) fn resize(&self, grid: (u16, u16)) {
        match self {
            Control::Local(pty) => {
                let _ = pty.master.resize(pty_size(grid));
            }
            Control::Remote(shell) => shell.resize(grid.0, grid.1),
        }
    }

    /// The name the system gives the foreground process, which only a
    /// pseudo-terminal here can tell.
    pub(super) fn foreground_program(&self) -> Option<String> {
        match self {
            Control::Local(pty) => foreground_program(&*pty.master),
            Control::Remote(_) => None,
        }
    }
}

/// The pseudo-terminal of a shell here. Dropping it hangs up on the shell,
/// if it hasn't exited, once and without waiting, which ends the shells
/// rho-ui starts. One known to have exited is left alone: the waiter
/// thread reaped it, so its process ID may be another process's by now.
pub(super) struct LocalPty {
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Whether the waiter thread saw the shell exit, and reaped it.
    exited: Arc<AtomicBool>,
}

impl Drop for LocalPty {
    fn drop(&mut self) {
        if !self.exited.load(Ordering::Acquire) {
            let _ = self.killer.kill();
        }
    }
}

/// Reads what the program prints on a thread of its own, as a read
/// blocks. The thread waits while [`OUTPUT_QUEUE`] reads are queued, and
/// ends at the end of the output or when the receiver is dropped.
fn read_from(mut reader: Box<dyn Read + Send>) -> std::io::Result<mpsc::Receiver<Vec<u8>>> {
    let (mut sender, output) = mpsc::channel::<Vec<u8>>(OUTPUT_QUEUE);
    std::thread::Builder::new()
        .name("rho-ui-terminal-read".into())
        .spawn(move || {
            let mut buffer = vec![0; READ_BYTES];
            while let Ok(read @ 1..) = reader.read(&mut buffer) {
                let chunk = buffer[..read].to_vec();
                if futures::executor::block_on(sender.send(chunk)).is_err() {
                    return;
                }
            }
        })?;
    Ok(output)
}

/// Writes to the program, in order, what is sent on the returned sender,
/// on a thread of its own, as a write blocks while the program doesn't
/// read. The thread ends when every sender is dropped, or the program is
/// gone.
fn write_to(
    mut writer: Box<dyn Write + Send>,
) -> std::io::Result<std::sync::mpsc::Sender<Vec<u8>>> {
    let (input, queued) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name("rho-ui-terminal-write".into())
        .spawn(move || {
            for bytes in queued {
                if writer.write_all(&bytes).is_err() {
                    return;
                }
            }
        })?;
    Ok(input)
}

/// Waits for `child` to exit on a thread of its own, and reaps it: sets
/// `exited`, then completes the returned receiver.
fn wait_for(
    mut child: Box<dyn Child + Send + Sync>,
    exited: Arc<AtomicBool>,
) -> std::io::Result<oneshot::Receiver<()>> {
    let (done, exit) = oneshot::channel();
    std::thread::Builder::new()
        .name("rho-ui-terminal-wait".into())
        .spawn(move || {
            // An error means there is no child left to wait for.
            let _ = child.wait();
            exited.store(true, Ordering::Release);
            let _ = done.send(());
        })?;
    Ok(exit)
}

/// The name the system gives the foreground process of `pty`, such as
/// `vim`, or the shell's while it waits for a command. Linux keeps it in
/// `/proc`; elsewhere this reads nothing.
#[cfg(target_os = "linux")]
fn foreground_program(pty: &(dyn MasterPty + Send)) -> Option<String> {
    let pid = pty.process_group_leader()?;
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(name.trim_end().to_owned()).filter(|name| !name.is_empty())
}

#[cfg(not(target_os = "linux"))]
fn foreground_program(_: &(dyn MasterPty + Send)) -> Option<String> {
    None
}

fn pty_size((cols, rows): (u16, u16)) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}
