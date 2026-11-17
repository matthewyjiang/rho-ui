//! One WebSocket to `rho-ui serve`, as two halves: what sends bytes, and
//! what receives messages. In a browser it is the browser's own
//! `WebSocket`; elsewhere, tokio-tungstenite on the I/O runtime. Either
//! offers the server's token as its subprotocol (`wire::token_protocol`).

use futures::future::poll_fn;

pub(crate) use self::imp::{open, Receive, Send};

/// A message from the server.
#[derive(Debug)]
pub(crate) enum Frame {
    Binary(Vec<u8>),
    Text(String),
}

impl Send {
    /// Sends `bytes` as one binary message, and waits until it is flushed:
    /// a caller that sends nothing after still gets it to the server.
    ///
    /// # Errors
    ///
    /// Fails once the WebSocket has closed.
    pub(crate) async fn send(&mut self, bytes: Vec<u8>) -> Result<(), Closed> {
        poll_fn(|cx| self.poll_send(cx, &bytes)).await?;
        poll_fn(|cx| self.poll_flush(cx)).await
    }

    /// Closes the WebSocket, as a close frame the server sees; the server
    /// closes its side in turn.
    pub(crate) async fn close(&mut self) {
        let _ = poll_fn(|cx| self.poll_close(cx)).await;
    }
}

impl Receive {
    /// The next message; `None` once the WebSocket has closed.
    pub(crate) async fn recv(&mut self) -> Option<Frame> {
        poll_fn(|cx| self.poll_recv(cx)).await
    }
}

/// The WebSocket closed.
#[derive(Debug)]
pub(crate) struct Closed;

#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::{
        pin::Pin,
        task::{ready, Context, Poll},
    };

    use futures::{
        stream::{SplitSink, SplitStream},
        Sink, SinkExt as _, Stream, StreamExt as _,
    };
    use tokio::net::TcpStream;
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{
            client::IntoClientRequest as _,
            http::{header, HeaderValue},
            Message,
        },
        MaybeTlsStream, WebSocketStream,
    };

    use super::{Closed, Frame};
    use crate::server::wire::token_protocol;

    type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

    pub(crate) struct Send(SplitSink<Socket, Message>);

    pub(crate) struct Receive(SplitStream<Socket>);

    /// Opens the WebSocket at `url`, offering `token`, with the `Origin` of
    /// a page the server served, which it checks. Call it on the I/O
    /// runtime.
    ///
    /// # Errors
    ///
    /// Fails, in words for people, if the server can't be reached or
    /// refuses.
    pub(crate) async fn open(url: &str, token: &str) -> Result<(Send, Receive), String> {
        let mut request = url
            .into_client_request()
            .map_err(|error| error.to_string())?;
        let origin = request
            .uri()
            .authority()
            .map(|authority| format!("http://{authority}"))
            .ok_or_else(|| format!("{url} names no server"))?;
        let headers = request.headers_mut();
        let header =
            |value: String| HeaderValue::from_str(&value).map_err(|error| error.to_string());
        headers.insert(header::ORIGIN, header(origin)?);
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            header(token_protocol(token))?,
        );
        let (socket, _) = connect_async(request)
            .await
            .map_err(|error| error.to_string())?;
        let (send, receive) = socket.split();
        Ok((Send(send), Receive(receive)))
    }

    impl Send {
        /// Sends `text` as one text message.
        pub(crate) async fn send_text(&mut self, text: &str) -> Result<(), Closed> {
            self.0
                .send(Message::Text(text.into()))
                .await
                .map_err(|_| Closed)
        }

        pub(crate) fn poll_send(
            &mut self,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<Result<(), Closed>> {
            let mut sink = Pin::new(&mut self.0);
            ready!(sink.as_mut().poll_ready(cx)).map_err(|_| Closed)?;
            sink.as_mut()
                .start_send(Message::Binary(bytes.to_vec().into()))
                .map_err(|_| Closed)?;
            // Queued; a flush still under way finishes with the next send
            // or flush, as `AsyncWrite` lets a write leave it to them.
            match sink.poll_flush(cx) {
                Poll::Ready(Err(_)) => Poll::Ready(Err(Closed)),
                _ => Poll::Ready(Ok(())),
            }
        }

        pub(crate) fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Closed>> {
            Pin::new(&mut self.0).poll_flush(cx).map_err(|_| Closed)
        }

        pub(crate) fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Closed>> {
            Pin::new(&mut self.0).poll_close(cx).map_err(|_| Closed)
        }
    }

    impl Receive {
        pub(crate) fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<Frame>> {
            loop {
                return match ready!(Pin::new(&mut self.0).poll_next(cx)) {
                    Some(Ok(Message::Binary(bytes))) => {
                        Poll::Ready(Some(Frame::Binary(bytes.into())))
                    }
                    Some(Ok(Message::Text(text))) => {
                        Poll::Ready(Some(Frame::Text(text.to_string())))
                    }
                    Some(Ok(Message::Close(_)) | Err(_)) | None => Poll::Ready(None),
                    // Pings and pongs, which tungstenite answers itself.
                    Some(Ok(_)) => continue,
                };
            }
        }
    }
}

#[cfg(target_family = "wasm")]
mod imp {
    use std::{
        cell::Cell,
        rc::Rc,
        task::{Context, Poll},
    };

    use futures::{
        channel::{mpsc, oneshot},
        Stream as _,
    };
    use js_sys::{ArrayBuffer, Uint8Array};
    use wasm_bindgen::{closure::Closure, JsCast as _};
    use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

    use super::{Closed, Frame};
    use crate::server::wire::token_protocol;

    /// Sends on the browser's WebSocket, which queues what it can't send
    /// yet, so a send never waits. Dropping it closes the WebSocket.
    pub(crate) struct Send(WebSocket);

    /// What the browser's WebSocket received, in order.
    pub(crate) struct Receive {
        messages: mpsc::UnboundedReceiver<Frame>,
        _hooks: Hooks,
    }

    /// The socket's callbacks, which it calls until they are unhooked:
    /// dropping this unhooks them, as the browser throws when it calls one
    /// Rust has dropped, and the socket may yet close or fail after.
    struct Hooks {
        socket: WebSocket,
        on_message: Closure<dyn FnMut(MessageEvent)>,
        on_close: Closure<dyn FnMut(CloseEvent)>,
        on_open: Closure<dyn FnMut(Event)>,
        on_error: Closure<dyn FnMut(Event)>,
    }

    impl Drop for Hooks {
        fn drop(&mut self) {
            self.socket.set_onmessage(None);
            self.socket.set_onclose(None);
            self.socket.set_onopen(None);
            self.socket.set_onerror(None);
        }
    }

    /// Opens the browser's WebSocket at `url`, offering `token`.
    ///
    /// # Errors
    ///
    /// Fails if the browser can't open it, or the server refuses.
    pub(crate) async fn open(url: &str, token: &str) -> Result<(Send, Receive), String> {
        let socket = WebSocket::new_with_str(url, &token_protocol(token))
            .map_err(|error| format!("{error:?}"))?;
        socket.set_binary_type(BinaryType::Arraybuffer);
        let (sender, messages) = mpsc::unbounded();
        let received = sender.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let data = event.data();
            let frame = if let Some(text) = data.as_string() {
                Frame::Text(text)
            } else if let Ok(buffer) = data.dyn_into::<ArrayBuffer>() {
                Frame::Binary(Uint8Array::new(&buffer).to_vec())
            } else {
                return;
            };
            let _ = received.unbounded_send(frame);
        });
        let on_close = Closure::<dyn FnMut(CloseEvent)>::new(move |_: CloseEvent| {
            sender.close_channel();
        });
        let (opened, open) = oneshot::channel::<bool>();
        let opened = Rc::new(Cell::new(Some(opened)));
        let settle = move |ok| {
            let opened = Rc::clone(&opened);
            Closure::<dyn FnMut(Event)>::new(move |_: Event| {
                if let Some(opened) = opened.take() {
                    let _ = opened.send(ok);
                }
            })
        };
        let hooks = Hooks {
            socket: socket.clone(),
            on_message,
            on_close,
            on_open: settle(true),
            on_error: settle(false),
        };
        socket.set_onmessage(Some(hooks.on_message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(hooks.on_close.as_ref().unchecked_ref()));
        socket.set_onopen(Some(hooks.on_open.as_ref().unchecked_ref()));
        socket.set_onerror(Some(hooks.on_error.as_ref().unchecked_ref()));
        if !open.await.unwrap_or(false) {
            return Err(format!("the server refused {url}, or couldn't be reached"));
        }
        Ok((
            Send(socket),
            Receive {
                messages,
                _hooks: hooks,
            },
        ))
    }

    impl Send {
        /// Sends `text` as one text message. The browser queues it, so
        /// this is ready at once; off the browser a send waits.
        pub(crate) fn send_text(&mut self, text: &str) -> std::future::Ready<Result<(), Closed>> {
            std::future::ready(self.0.send_with_str(text).map_err(|_| Closed))
        }

        pub(crate) fn poll_send(
            &mut self,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<Result<(), Closed>> {
            Poll::Ready(self.0.send_with_u8_array(bytes).map_err(|_| Closed))
        }

        #[expect(
            clippy::unused_self,
            reason = "the shape `WsStream` calls off the browser too"
        )]
        pub(crate) fn poll_flush(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Closed>> {
            Poll::Ready(Ok(()))
        }

        pub(crate) fn poll_close(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Closed>> {
            Poll::Ready(self.0.close().map_err(|_| Closed))
        }
    }

    impl Drop for Send {
        fn drop(&mut self) {
            let _ = self.0.close();
        }
    }

    impl Receive {
        pub(crate) fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<Frame>> {
            std::pin::Pin::new(&mut self.messages).poll_next(cx)
        }
    }
}
