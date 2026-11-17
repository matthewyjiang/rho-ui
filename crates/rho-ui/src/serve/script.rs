//! `wire::SCRIPT`: runs a POSIX `sh` script as `sh -s` in the home
//! directory, as an ssh server runs the scripts rho-ui sends a remote
//! machine; on a remote machine, over ssh, as the desktop app does.

use std::process::{ExitStatus, Stdio};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query,
    },
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

pub(crate) async fn open(Query(at): Query<super::At>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| run(socket, at.machine))
}

/// Reads the script's input, runs it here or on the remote machine at ssh
/// host `machine`, and sends what it prints and how it ended. The client
/// closing the WebSocket first kills the script.
async fn run(socket: WebSocket, machine: Option<String>) {
    let (mut send, mut receive) = socket.split();
    let Some(Ok(Message::Binary(input))) = receive.next().await else {
        return;
    };
    let ran = async {
        match machine {
            None => run_here(&input, &mut send).await,
            Some(host) => run_there(&host, input.to_vec(), &mut send).await,
        }
    };
    // The client sends nothing more: whatever comes, or the end, means it
    // went, and dropping `ran` stops the script.
    let end = tokio::select! {
        end = ran => end,
        _ = receive.next() => return,
    };
    if let Some(end) = end {
        let json = serde_json::to_string(&end).expect("a ScriptEnd is plain JSON");
        let _ = send.send(Message::Text(json.into())).await;
        let _ = send.send(Message::Close(None)).await;
    }
}

/// Runs `input` here, sending what it prints to `send` as it comes, and
/// says how it ended; `None` if the client went.
async fn run_here(
    input: &[u8],
    send: &mut (impl futures::Sink<Message> + Unpin),
) -> Option<ScriptEnd> {
    match start() {
        Ok(child) => finish(child, input, send).await,
        Err(error) => Some(not_started(format!("couldn't run sh: {error}"))),
    }
}

/// Runs `input` on the remote machine at ssh host `host`, sends what it
/// printed to `send`, and says how it ended; `None` if the connection or
/// the client went. Dropping the future stops the script.
async fn run_there(
    host: &str,
    input: Vec<u8>,
    send: &mut (impl futures::Sink<Message> + Unpin),
) -> Option<ScriptEnd> {
    let link = match super::ssh(host).await {
        Ok(link) => link,
        Err(error) => return Some(not_started(error.to_string())),
    };
    let (stdout, end) = link.relay_script(input).await?;
    for chunk in stdout.chunks(CHUNK) {
        if send
            .send(Message::Binary(chunk.to_vec().into()))
            .await
            .is_err()
        {
            return None;
        }
    }
    Some(end)
}

fn not_started(why: String) -> ScriptEnd {
    ScriptEnd {
        ending: Ending::NotStarted(why),
        stderr: String::new(),
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
        return Ending::Exited(code.into());
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
