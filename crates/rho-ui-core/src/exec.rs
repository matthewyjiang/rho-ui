//! Where rho-ui-core's I/O runs, and its one timer. Natively, on the I/O
//! runtime ([`io`]): one Tokio worker for the daemon sockets, the ssh
//! connections that carry remote ones, and rho-ui-serve's WebSockets,
//! because `rho-protocol`'s client, russh and tokio-tungstenite need it.
//! In a browser, on the page's executor, which its WebSockets and
//! `setTimeout` need. Either way the futures callers get back are plain
//! ones that GPUI's executors can await.

use std::time::Duration;

#[cfg(not(target_family = "wasm"))]
pub(crate) use self::imp::io;
pub(crate) use self::imp::{on_io, spawn_io, within};

/// The deadline of a [`within`] passed.
#[derive(Debug)]
pub(crate) struct Elapsed;

#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::{future::Future, sync::OnceLock};

    use tokio::{runtime::Runtime, task::JoinHandle};

    use super::{Duration, Elapsed};
    use crate::DaemonError;

    /// The runtime the connection's socket I/O runs on, made on first use
    /// and kept for the life of the process. One worker: the work is the
    /// daemon sockets' reads and writes, `LocalDaemon` waiting on
    /// `rho daemon start`, and for a daemon on another machine, the ssh
    /// connection or WebSocket that carries its socket and the scripts run
    /// there.
    pub(crate) fn io() -> &'static Runtime {
        static IO: OnceLock<Runtime> = OnceLock::new();
        IO.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("rho-ui-daemon-io")
                .enable_io()
                // The handshake's deadline, those on a remote machine's
                // scripts, and the ssh connection's keepalives; see `ssh.rs`.
                .enable_time()
                .build()
                .expect("the OS lets this process start a thread and an I/O driver")
        })
    }

    /// Runs `future` on the I/O runtime and waits for it. Dropping the
    /// future this returns stops the task, and drops what the task holds,
    /// such as a half-open connection or a script's ssh channel, so a
    /// caller that stops waiting leaves nothing running.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Closed`] if the task panicked.
    pub(crate) async fn on_io<F>(future: F) -> Result<F::Output, DaemonError>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let mut task = AbortOnDrop(io().spawn(future));
        (&mut task.0).await.map_err(|_| DaemonError::Closed)
    }

    /// Runs `future`, a connection's I/O, on the I/O runtime until it ends.
    pub(crate) fn spawn_io<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        io().spawn(future);
    }

    /// A task on the I/O runtime that stops when this is dropped.
    struct AbortOnDrop<T>(JoinHandle<T>);

    impl<T> Drop for AbortOnDrop<T> {
        fn drop(&mut self) {
            // Does nothing to a task that has finished.
            self.0.abort();
        }
    }

    /// Waits for `future` for at most `limit`, on Tokio's timer: call it on
    /// the I/O runtime.
    ///
    /// # Errors
    ///
    /// [`Elapsed`] if `limit` passes first, which drops `future`.
    pub(crate) async fn within<F: Future>(
        limit: Duration,
        future: F,
    ) -> Result<F::Output, Elapsed> {
        tokio::time::timeout(limit, future)
            .await
            .map_err(|_| Elapsed)
    }
}

#[cfg(target_family = "wasm")]
mod imp {
    use std::future::Future;

    use futures::{
        channel::oneshot,
        future::{select, AbortHandle, Abortable, Either},
    };

    use super::{Duration, Elapsed};
    use crate::DaemonError;

    /// Runs `future` on the page's executor and returns a future that
    /// waits for it and is `Send`, for GPUI's executors, whatever `future`
    /// holds. Dropping that stops the task, as natively.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Closed`] if the task stopped without an answer.
    pub(crate) fn on_io<F>(future: F) -> impl Future<Output = Result<F::Output, DaemonError>> + Send
    where
        F: Future + 'static,
        F::Output: Send + 'static,
    {
        /// Stops the task when the waiting future is dropped.
        struct AbortOnDrop(AbortHandle);
        impl Drop for AbortOnDrop {
            fn drop(&mut self) {
                self.0.abort();
            }
        }

        let (answer, answered) = oneshot::channel();
        let (abort, registration) = AbortHandle::new_pair();
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(output) = Abortable::new(future, registration).await {
                let _ = answer.send(output);
            }
        });
        let stop = AbortOnDrop(abort);
        async move {
            let answered = answered.await.map_err(|_| DaemonError::Closed);
            drop(stop);
            answered
        }
    }

    /// Runs `future`, a connection's I/O, on the page's executor until it
    /// ends.
    pub(crate) fn spawn_io<F>(future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        wasm_bindgen_futures::spawn_local(future);
    }

    /// Waits for `future` for at most `limit`, on the page's `setTimeout`.
    ///
    /// # Errors
    ///
    /// [`Elapsed`] if `limit` passes first, which drops `future`.
    pub(crate) async fn within<F: Future>(
        limit: Duration,
        future: F,
    ) -> Result<F::Output, Elapsed> {
        let millis = i32::try_from(limit.as_millis()).unwrap_or(i32::MAX);
        let sleep = js_sys::Promise::new(&mut |resolve, _| {
            let window = web_sys::window().expect("rho-ui runs in a browser window");
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
        });
        let sleep = wasm_bindgen_futures::JsFuture::from(sleep);
        match select(std::pin::pin!(future), sleep).await {
            Either::Left((output, _)) => Ok(output),
            Either::Right(_) => Err(Elapsed),
        }
    }
}
