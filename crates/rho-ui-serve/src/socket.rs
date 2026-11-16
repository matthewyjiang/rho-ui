//! `wire::SOCKET`: relays a Unix socket, the rho daemon's, as ssh's
//! `direct-streamlocal` channel does for a remote machine.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::UnixStream,
};

/// How much of what the socket sends one message carries at most: 64 KiB,
/// as one read of a Unix socket returns at most on Linux.
const CHUNK: usize = 64 * 1024;

#[derive(Deserialize)]
pub(crate) struct Target {
    path: String,
}

/// Connects to the socket before upgrading, so a socket that isn't there
/// fails the WebSocket's handshake with why.
pub(crate) async fn open(Query(target): Query<Target>, upgrade: WebSocketUpgrade) -> Response {
    match UnixStream::connect(&target.path).await {
        Ok(stream) => upgrade.on_upgrade(move |socket| relay(socket, stream)),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("rho-ui-serve couldn't connect to {}: {error}", target.path),
        )
            .into_response(),
    }
}

/// Relays bytes each way until either side closes or fails, then closes
/// both.
async fn relay(socket: WebSocket, stream: UnixStream) {
    let (mut send, mut receive) = socket.split();
    let (mut read, mut write) = stream.into_split();
    let inbound = async {
        while let Some(Ok(message)) = receive.next().await {
            match message {
                Message::Binary(bytes) => {
                    if write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = write.shutdown().await;
    };
    let outbound = async {
        let mut buffer = vec![0; CHUNK];
        loop {
            match read.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let chunk = buffer[..read].to_vec();
                    if send.send(Message::Binary(chunk.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = send.send(Message::Close(None)).await;
    };
    // The daemon answers what it has read after the client stops writing,
    // and the client still reads it, so each side runs to its own end.
    tokio::join!(inbound, outbound);
}
