//! The login shell on a rho-ui-serve's machine, as [`super::wire::SHELL`]
//! runs it: the right panel's terminal in a browser. A task on the I/O
//! runtime, or the page's executor, opens the WebSocket, then forwards
//! what the shell prints to [`crate::ShellOutput`], and what is typed and
//! each resize to the server, side by side.

use std::path::Path;

use futures::StreamExt as _;

use super::{
    percent_encode,
    wire::{self, ShellControl},
    ws::{self, Frame},
    ServerLink,
};
use crate::{
    exec::spawn_io,
    platform,
    remote_shell::{self, Ends, RemoteShell, Request, ShellError},
};

/// Reads of the shell's output that wait for the terminal: as many as
/// the local terminal's queue holds. The terminal, not the queue, sets the
/// pace: each read is said back to the server once the terminal took it,
/// and the server sends no more than `wire::SHELL_WINDOW` ahead.
const OUTPUT_QUEUE: usize = 16;

/// What goes to the server: what is typed, or a [`ShellControl`].
enum Outgoing {
    Typed(Vec<u8>),
    Control(ShellControl),
}

impl ServerLink {
    /// Starts the login shell in `dir` on the server's machine, under a
    /// pseudo-terminal there of `columns` by `rows` that is a `term`.
    /// Returns at once: [`RemoteShell::ended`] says if it couldn't start.
    /// It ends when it exits, the WebSocket closes, or
    /// [`RemoteShell::control`] goes.
    ///
    /// # Panics
    ///
    /// Never: a [`ShellControl`] always encodes as JSON.
    pub fn open_shell(&self, dir: &Path, term: &str, columns: u16, rows: u16) -> RemoteShell {
        let dir = String::from_utf8_lossy(&platform::path_to_bytes(dir.as_os_str())).into_owned();
        let query = format!(
            "?dir={}&term={}&columns={columns}&rows={rows}",
            percent_encode(&dir),
            percent_encode(term)
        );
        let url = self.url(wire::SHELL, &query);
        let token = self.target.token.clone();
        let address = self.address().to_owned();
        let (shell, ends) = remote_shell::channels(OUTPUT_QUEUE);
        spawn_io(async move {
            let Ends {
                printed,
                mut requests,
                disowned,
                end,
            } = ends;
            let ran = async {
                let (mut send, mut receive) = ws::open(&url, &token).await.map_err(|why| {
                    ShellError::NotStarted(format!("couldn't reach {address}: {why}"))
                })?;
                // What the terminal took, said back to the server so it
                // sends more (`wire::SHELL_WINDOW`).
                let (took, acks) = futures::channel::mpsc::unbounded();
                let output = async move {
                    while let Some(frame) = receive.recv().await {
                        if let Frame::Binary(bytes) = frame {
                            let len = bytes.len();
                            if printed.send(bytes).await.is_err() {
                                // The reader went, and the terminal with it.
                                return;
                            }
                            let _ = took.unbounded_send(ShellControl::Took { bytes: len });
                        }
                    }
                };
                let typed = futures::stream::poll_fn(|cx| requests.poll_recv(cx)).map(|request| {
                    match request {
                        Request::Write(bytes) => Outgoing::Typed(bytes),
                        Request::Resize { columns, rows } => {
                            Outgoing::Control(ShellControl::Resize { columns, rows })
                        }
                    }
                });
                let mut outgoing = futures::stream::select(typed, acks.map(Outgoing::Control));
                let input = async move {
                    while let Some(message) = outgoing.next().await {
                        let delivered = match message {
                            Outgoing::Typed(bytes) => send.send(bytes).await,
                            Outgoing::Control(control) => {
                                let json = serde_json::to_string(&control)
                                    .expect("a ShellControl is plain JSON");
                                send.send_text(&json).await
                            }
                        };
                        if delivered.is_err() {
                            return;
                        }
                    }
                };
                futures::pin_mut!(output, input);
                // The shell's output ending is how it ends; input that
                // stops first only means the terminal stopped typing.
                match futures::future::select(output, input).await {
                    futures::future::Either::Left(((), _)) => {}
                    futures::future::Either::Right(((), output)) => output.await,
                }
                Ok(())
            };
            futures::pin_mut!(ran, disowned);
            let ended = match futures::future::select(ran, disowned).await {
                futures::future::Either::Left((ended, _)) => ended,
                // The terminal went: dropping `ran` closes the WebSocket,
                // and the server hangs up on the shell.
                futures::future::Either::Right(_) => Ok(()),
            };
            let _ = end.send(ended);
        });
        shell
    }
}
