//! `wire::SCRIPT`: runs a POSIX `sh` script as `sh -s` in the home
//! directory, as an ssh server runs the scripts rho-ui sends a remote
//! machine.

use std::process::{ExitStatus, Stdio};

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Response,
};
use futures::{SinkExt as _, StreamExt as _};
use rho_ui_core::{
    platform,
    server::wire::{Ending, ScriptEnd},
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    process::{Child, Command},
};

/// How much of stdout one message carries at most. The client gathers it
/// all, so this only bounds a message: 64 KiB, as a pipe holds on Linux.
const CHUNK: usize = 64 * 1024;

pub(crate) async fn open(upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(run)
}

/// Reads the script's input, runs it, and sends what it prints and how it
/// ended. The client closing the WebSocket first kills the script.
async fn run(socket: WebSocket) {
    let (mut send, mut receive) = socket.split();
    let Some(Ok(Message::Binary(input))) = receive.next().await else {
        return;
    };
    let end = match start() {
        Ok(child) => {
            let ran = finish(child, &input, &mut send);
            // The client sends nothing more: whatever comes, or the end,
            // means it went, and dropping `ran` kills the script.
            tokio::select! {
                end = ran => end,
                _ = receive.next() => return,
            }
        }
        Err(error) => Some(ScriptEnd {
            ending: Ending::NotStarted(format!("couldn't run sh: {error}")),
            stderr: String::new(),
        }),
    };
    if let Some(end) = end {
        let json = serde_json::to_string(&end).expect("a ScriptEnd is plain JSON");
        let _ = send.send(Message::Text(json.into())).await;
        let _ = send.send(Message::Close(None)).await;
    }
}

fn start() -> std::io::Result<Child> {
    Command::new("sh")
        .arg("-s")
        .current_dir(platform::home_dir().unwrap_or_else(|| "/".into()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

/// Feeds `child` its input and sends what it prints to `send`, each at
/// once, so neither waits on a full pipe, then says how it ended; `None`
/// if the client went.
async fn finish(
    mut child: Child,
    input: &[u8],
    send: &mut (impl futures::Sink<Message> + Unpin),
) -> Option<ScriptEnd> {
    let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        unreachable!("`start` pipes all three");
    };
    let feed = async move {
        // A script that exits before reading all its input breaks the
        // pipe; how it exited says what happened.
        let _ = stdin.write_all(input).await;
    };
    let errors = async move {
        let mut said = Vec::new();
        let _ = stderr.read_to_end(&mut said).await;
        said
    };
    let relay = async {
        let mut buffer = vec![0; CHUNK];
        loop {
            match stdout.read(&mut buffer).await {
                Ok(0) | Err(_) => return true,
                Ok(read) => {
                    let chunk = buffer[..read].to_vec();
                    if send.send(Message::Binary(chunk.into())).await.is_err() {
                        return false;
                    }
                }
            }
        }
    };
    let ((), said, relayed) = tokio::join!(feed, errors, relay);
    if !relayed {
        return None;
    }
    let ending = match child.wait().await {
        Ok(status) => ending(status),
        Err(error) => Ending::NotStarted(format!("couldn't wait for sh: {error}")),
    };
    Some(ScriptEnd {
        ending,
        stderr: String::from_utf8_lossy(&said).into_owned(),
    })
}

fn ending(status: ExitStatus) -> Ending {
    if let Some(code) = status.code() {
        return Ending::Exited(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return Ending::Killed(format!("signal {signal}"));
        }
    }
    Ending::Killed("a signal".to_owned())
}
