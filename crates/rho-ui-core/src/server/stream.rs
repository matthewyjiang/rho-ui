//! The daemon's socket as `rho-ui serve` relays it, as the byte stream
//! rho-protocol's client reads and writes: each write goes as one binary
//! message, and reads take the messages' bytes in order.

use std::{
    io,
    pin::Pin,
    task::{ready, Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::ws::{Frame, Receive, Send};

/// A WebSocket to [`super::wire::SOCKET`], as a byte stream.
pub(crate) struct WsStream {
    send: Send,
    receive: Receive,
    /// What the last message held that a read hasn't taken yet.
    unread: Vec<u8>,
    taken: usize,
}

impl WsStream {
    pub(crate) fn new(send: Send, receive: Receive) -> Self {
        Self {
            send,
            receive,
            unread: Vec::new(),
            taken: 0,
        }
    }
}

fn closed() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "rho-ui serve closed the WebSocket",
    )
}

impl AsyncRead for WsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        while this.taken == this.unread.len() {
            match ready!(this.receive.poll_recv(cx)) {
                Some(Frame::Binary(bytes)) => {
                    this.unread = bytes;
                    this.taken = 0;
                }
                // The relay sends only bytes.
                Some(Frame::Text(_)) => {}
                // The end of the stream.
                None => return Poll::Ready(Ok(())),
            }
        }
        let take = buf.remaining().min(this.unread.len() - this.taken);
        buf.put_slice(&this.unread[this.taken..this.taken + take]);
        this.taken += take;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for WsStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let sent = ready!(self.get_mut().send.poll_send(cx, buf));
        Poll::Ready(sent.map(|()| buf.len()).map_err(|_| closed()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let flushed = ready!(self.get_mut().send.poll_flush(cx));
        Poll::Ready(flushed.map_err(|_| closed()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let closed_ = ready!(self.get_mut().send.poll_close(cx));
        Poll::Ready(closed_.map_err(|_| closed()))
    }
}
