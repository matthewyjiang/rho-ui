//! The desktop app's state, shared with the page through `rho-ui serve`
//! ([`super::wire::STATE`]): the page loads the settings and the state
//! database as they are on the server's machine, then sends each change
//! back, as the desktop app writes its own.

use futures::{
    channel::{mpsc, oneshot},
    StreamExt as _,
};

use super::{
    wire::{self, SettingsFile, StateSnapshot, StateWrite},
    ws::{self, Frame},
    ServerTarget,
};
use crate::{
    exec::{on_io, spawn_io},
    DaemonError,
};

/// The state on the server's machine, as it was when opened, and where to
/// send changes to it.
#[derive(Debug)]
pub struct SharedState {
    /// The settings file.
    pub settings: SettingsFile,
    /// The state database's image, for SQLite to deserialize, in rollback
    /// journal mode, as an image in memory can't use a WAL.
    pub database: Vec<u8>,
    /// Sends changes to the server, in order.
    pub writer: StateWriter,
    /// Resolves once the server has applied every change and closed.
    pub done: StateDone,
}

/// Sends changes to the shared state. Clones send to the same server, in
/// the order called. Once the last goes, the connection closes, after the
/// server applied what was sent.
#[derive(Clone, Debug)]
pub struct StateWriter(mpsc::UnboundedSender<StateWrite>);

impl StateWriter {
    /// Sends `write`. A change made once the connection went is lost.
    pub fn write(&self, write: StateWrite) {
        let _ = self.0.unbounded_send(write);
    }
}

/// When the server is done with the shared state; see [`SharedState::done`].
/// A browser holds its writer as long as the page is open, so there it
/// resolves only when the connection went.
#[derive(Debug)]
pub struct StateDone(oneshot::Receiver<()>);

impl StateDone {
    /// Waits until the server has applied every change and closed, or the
    /// connection went.
    pub async fn wait(self) {
        let _ = self.0.await;
    }
}

impl ServerTarget {
    /// Loads the state on the server's machine, and keeps a connection to
    /// send changes back on. It is the server's own state, whatever
    /// machine this target names.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Unreachable`] if the server can't be reached or says
    /// something else.
    ///
    /// # Panics
    ///
    /// Never: a [`StateWrite`] always encodes as JSON.
    pub async fn open_state(&self) -> Result<SharedState, DaemonError> {
        let server = self.link();
        let url = server.url(wire::STATE, &[]);
        let token = self.token.clone();
        on_io(async move {
            let unreachable = |why: String| {
                DaemonError::Unreachable(format!(
                    "couldn't load rho-ui's state through {}: {why}",
                    server.name()
                ))
            };
            let (mut send, mut receive) = ws::open(&url, &token).await.map_err(unreachable)?;
            let Some(Frame::Text(snapshot)) = receive.recv().await else {
                return Err(unreachable("it sent no settings".to_owned()));
            };
            let snapshot: StateSnapshot = serde_json::from_str(&snapshot)
                .map_err(|error| unreachable(format!("its settings didn't read: {error}")))?;
            let Some(Frame::Binary(database)) = receive.recv().await else {
                return Err(unreachable("it sent no database".to_owned()));
            };
            let (writer, mut queued) = mpsc::unbounded();
            let (finished, done) = oneshot::channel();
            spawn_io(async move {
                while let Some(write) = queued.next().await {
                    let json = serde_json::to_string(&write).expect("a StateWrite is plain JSON");
                    if send.send_text(&json).await.is_err() {
                        return;
                    }
                }
                // The server closes once it applied what came before.
                send.close().await;
                while receive.recv().await.is_some() {}
                let _ = finished.send(());
            });
            Ok(SharedState {
                settings: snapshot.settings,
                database,
                writer: StateWriter(writer),
                done: StateDone(done),
            })
        })
        .await?
    }
}
