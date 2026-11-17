//! `wire::SHELL`: the user's login shell under a pseudo-terminal here, as
//! sshd runs one for the desktop app's terminal on a remote machine. It
//! gets `TERM` from the client and `COLORTERM=truecolor`, as there. On a
//! remote machine it is that shell, over ssh, as the desktop app opens it.
//!
//! A pseudo-terminal's reads, writes and wait block, so a thread of its
//! own does each; the WebSocket's task only forwards between them.
//!
//! Either way the relay sends at most [`SHELL_WINDOW`] ahead of what the
//! client took, and stops reading the shell while the client lags. Over
//! ssh that holds up the machine's other terminals too, which share a
//! connection, as a desktop terminal that stopped reading would
//! (`rho_ui_core`'s `ssh/shell.rs`).

use std::{
    io::{Read, Write},
    path::PathBuf,
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query,
    },
    response::Response,
};
use futures::{SinkExt as _, StreamExt as _};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use rho_ui_core::{
    server::wire::{ShellControl, ShellEnd, SHELL_WINDOW},
    Link, RemoteShell, ShellEnded, ShellError, ShellInput, ShellOutput, SshLink,
};
use serde::Deserialize;
use tokio::sync::{mpsc, Semaphore};

/// How much one read of the pseudo-terminal takes at most: 64 KiB, as the
/// desktop app's terminal reads.
const READ_BYTES: usize = 64 * 1024;

/// Reads waiting to be sent: few, so a shell printing faster than the
/// window lets out waits, rather than this queueing all it prints.
const OUTPUT_QUEUE: usize = 16;

/// The shell's terminal and folder, and the machine it is on
/// ([`super::At`]), from the query.
#[derive(Deserialize)]
pub(crate) struct ShellQuery {
    dir: PathBuf,
    term: String,
    columns: u16,
    rows: u16,
    machine: Option<String>,
}

/// Starts the shell, then relays it. One that can't start here, or on a
/// remote machine that can't be reached, says why in a [`ShellEnd`].
pub(crate) async fn open(Query(query): Query<ShellQuery>, upgrade: WebSocketUpgrade) -> Response {
    let shell = match &query.machine {
        None => Shell::start(&query).map_err(|error| {
            format!(
                "rho-ui serve couldn't start a shell in {}: {error}",
                query.dir.display()
            )
        }),
        Some(host) => super::ssh(host)
            .await
            .map(|link| Shell::remote(link, &query))
            .map_err(|error| error.to_string()),
    };
    upgrade.on_upgrade(move |mut socket| async move {
        match shell {
            Ok(shell) => shell.relay(socket).await,
            Err(why) => {
                let _ = socket.send(end_message(&ShellEnd::NotStarted(why))).await;
                let _ = socket.send(Message::Close(None)).await;
            }
        }
    })
}

fn end_message(end: &ShellEnd) -> Message {
    let json = serde_json::to_string(end).expect("a ShellEnd is plain JSON");
    Message::Text(json.into())
}

/// A shell, under a pseudo-terminal here or on a remote machine, as the
/// relay talks to it.
struct Shell {
    output: Output,
    keys: Keys,
    /// The shell here, to hang up on once the relay is done. One on a
    /// remote machine hangs up as its control goes, with [`Keys`].
    child: Option<Box<dyn Child + Send + Sync>>,
}

/// What the shell prints.
enum Output {
    /// From the thread that reads the pseudo-terminal here.
    Here(mpsc::Receiver<Vec<u8>>),
    There {
        output: ShellOutput,
        ended: ShellEnded,
    },
}

impl Output {
    /// The next bytes it printed; `None` once it is done printing.
    async fn recv(&mut self) -> Option<Vec<u8>> {
        match self {
            Self::Here(output) => output.recv().await,
            Self::There { output, .. } => output.recv().await,
        }
    }

    /// Once it is done printing, why it ended, if not by exiting: a remote
    /// shell that couldn't start, or whose connection went.
    async fn end(self) -> Option<ShellEnd> {
        match self {
            Self::Here(_) => None,
            Self::There { ended, .. } => match ended.wait().await {
                Ok(()) => None,
                Err(ShellError::NotStarted(why)) => Some(ShellEnd::NotStarted(why)),
                Err(ShellError::Stopped(why)) => Some(ShellEnd::Stopped(why)),
            },
        }
    }
}

/// Types into the shell and sizes its terminal. Only the task that reads
/// the client holds it: a pseudo-terminal is no `Sync` to share.
enum Keys {
    Here {
        master: Box<dyn MasterPty + Send>,
        /// To the thread that writes to the pseudo-terminal.
        input: std::sync::mpsc::Sender<Vec<u8>>,
    },
    There {
        input: ShellInput,
        /// Dropping it hangs up on the shell.
        control: rho_ui_core::ShellControl,
        /// The ssh link to the machine, held while the shell runs.
        _link: SshLink,
    },
}

impl Keys {
    /// Types `bytes`; `false` once the shell is gone.
    fn type_in(&self, bytes: Vec<u8>) -> bool {
        match self {
            Self::Here { input, .. } => input.send(bytes).is_ok(),
            Self::There { input, .. } => {
                input.write(bytes);
                true
            }
        }
    }

    fn resize(&self, columns: u16, rows: u16) {
        match self {
            Self::Here { master, .. } => {
                let _ = master.resize(size(columns, rows));
            }
            Self::There { control, .. } => control.resize(columns, rows),
        }
    }
}

impl Shell {
    /// Runs `$SHELL -l` (else `/bin/sh -l`) in the query's folder, under
    /// a pseudo-terminal of its size.
    fn start(query: &ShellQuery) -> Result<Self, String> {
        let pair = native_pty_system()
            .openpty(size(query.columns, query.rows))
            .map_err(|error| error.to_string())?;
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
        let mut command = CommandBuilder::new(shell);
        command.arg("-l");
        command.cwd(&query.dir);
        command.env("TERM", &query.term);
        command.env("COLORTERM", "truecolor");
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| error.to_string())?;
        // The shell holds the terminal's other end now: dropping this one
        // lets a read see the end once the shell and its children exit.
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| error.to_string())?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| error.to_string())?;
        let output = read_from(reader).map_err(|error| error.to_string())?;
        let input = write_to(writer).map_err(|error| error.to_string())?;
        Ok(Self {
            output: Output::Here(output),
            keys: Keys::Here {
                master: pair.master,
                input,
            },
            child: Some(child),
        })
    }

    /// The login shell on the remote machine `link` reaches, in the
    /// query's folder, under a pseudo-terminal of its size there. One that
    /// can't start there ends its output at once, and says why.
    fn remote(link: SshLink, query: &ShellQuery) -> Self {
        let RemoteShell {
            output,
            input,
            control,
            ended,
        } = Link::Ssh(link.clone()).open_shell(&query.dir, &query.term, query.columns, query.rows);
        Self {
            output: Output::There { output, ended },
            keys: Keys::There {
                input,
                control,
                _link: link,
            },
            child: None,
        }
    }

    /// Sends what the shell prints until it is done printing, then why it
    /// ended if not by exiting, and closes, at most [`SHELL_WINDOW`] bytes
    /// ahead of what the client took; types what comes in, and resizes on
    /// a [`ShellControl::Resize`]. The client closing first hangs up on the
    /// shell.
    async fn relay(self, socket: WebSocket) {
        let Self {
            mut output,
            keys,
            child,
        } = self;
        let (mut send, mut receive) = socket.split();
        // A byte of the window each: taken as output goes, given back as
        // the client says it took it.
        let window = &Semaphore::new(SHELL_WINDOW);
        let printed = async move {
            while let Some(bytes) = output.recv().await {
                let len = u32::try_from(bytes.len()).expect("a read is at most READ_BYTES");
                let Ok(credit) = window.acquire_many(len).await else {
                    return;
                };
                credit.forget();
                if send.send(Message::Binary(bytes.into())).await.is_err() {
                    return;
                }
            }
            if let Some(end) = output.end().await {
                let _ = send.send(end_message(&end)).await;
            }
            let _ = send.send(Message::Close(None)).await;
        };
        let typed = async move {
            while let Some(Ok(message)) = receive.next().await {
                match message {
                    Message::Binary(bytes) => {
                        if !keys.type_in(bytes.to_vec()) {
                            return;
                        }
                    }
                    Message::Text(text) => match serde_json::from_str(&text) {
                        Ok(ShellControl::Resize { columns, rows }) => keys.resize(columns, rows),
                        // No more than the window back, which a client
                        // that miscounts can't grow past.
                        Ok(ShellControl::Took { bytes }) => {
                            let room = SHELL_WINDOW - window.available_permits();
                            window.add_permits(bytes.min(room));
                        }
                        Err(_) => {}
                    },
                    Message::Close(_) => return,
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
        };
        tokio::select! {
            () = printed => {}
            () = typed => {}
        }
        // Done either way: hang up on a shell still there. A kill and a
        // wait both block, a kill up to 200 ms for the shell to go, so a
        // thread of its own reaps it.
        if let Some(mut child) = child {
            std::thread::spawn(move || {
                let _ = child.kill();
                child.wait()
            });
        }
    }
}

/// Reads what the shell prints on a thread of its own, until the end of
/// its output or the receiver goes.
fn read_from(mut reader: Box<dyn Read + Send>) -> std::io::Result<mpsc::Receiver<Vec<u8>>> {
    let (sender, output) = mpsc::channel(OUTPUT_QUEUE);
    std::thread::Builder::new()
        .name("rho-ui-shell-read".into())
        .spawn(move || {
            let mut buffer = vec![0; READ_BYTES];
            while let Ok(read @ 1..) = reader.read(&mut buffer) {
                if sender.blocking_send(buffer[..read].to_vec()).is_err() {
                    return;
                }
            }
        })?;
    Ok(output)
}

/// Types into the shell, in order, what is sent on the returned sender,
/// on a thread of its own, until every sender goes or the shell does.
fn write_to(
    mut writer: Box<dyn Write + Send>,
) -> std::io::Result<std::sync::mpsc::Sender<Vec<u8>>> {
    let (input, queued) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name("rho-ui-shell-write".into())
        .spawn(move || {
            for bytes in queued {
                if writer.write_all(&bytes).is_err() {
                    return;
                }
            }
        })?;
    Ok(input)
}

fn size(columns: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols: columns,
        pixel_width: 0,
        pixel_height: 0,
    }
}
