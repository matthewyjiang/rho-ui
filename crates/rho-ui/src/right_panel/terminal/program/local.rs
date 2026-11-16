//! A shell under a pseudo-terminal here, which a browser can't make.

use std::{
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use futures::{
    channel::{mpsc, oneshot},
    FutureExt as _, SinkExt as _, StreamExt as _,
};
use gpui_kit::SharedString;
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};

use super::{Control, Input, Program, OUTPUT_QUEUE, TERM};

/// The most bytes one read of the pseudo-terminal takes.
const READ_BYTES: usize = 64 * 1024;

impl Program {
    /// Starts `command` under a pseudo-terminal here of `grid`, columns by
    /// rows.
    pub(in crate::right_panel::terminal) fn local(
        mut command: CommandBuilder,
        grid: (u16, u16),
    ) -> Result<Self, String> {
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
}

/// The pseudo-terminal of a shell here. Dropping it hangs up on the shell,
/// if it hasn't exited, once and without waiting, which ends the shells
/// rho-ui starts. One known to have exited is left alone: the waiter
/// thread reaped it, so its process ID may be another process's by now.
pub(in crate::right_panel::terminal) struct LocalPty {
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Whether the waiter thread saw the shell exit, and reaped it.
    exited: Arc<AtomicBool>,
}

impl LocalPty {
    /// Tells the program its terminal is now `grid`, columns by rows.
    pub(super) fn resize(&self, grid: (u16, u16)) {
        let _ = self.master.resize(pty_size(grid));
    }

    /// The name the system gives the foreground process.
    pub(super) fn foreground_program(&self) -> Option<String> {
        foreground_program(&*self.master)
    }
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
