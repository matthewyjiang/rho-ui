//! `wire::STATE`: shares the desktop app's settings file and state database
//! on this machine with the page, which loads them as they are and sends
//! each change back. An open desktop app and an open page are then as two
//! desktop apps: each loads at start and writes row by row, and the last
//! write to a row wins.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures::{SinkExt as _, StreamExt as _};
use rho_ui_core::server::wire::{StateSnapshot, StateWrite};
use rusqlite::Connection;

use crate::{
    settings::Settings,
    store::{self, StateDb},
};

/// How long a write waits for the desktop app's to the same database to
/// finish: a write and its checkpoint take about 10 ms (`store.rs`), so
/// this only stops one held up for good.
const BUSY: std::time::Duration = std::time::Duration::from_secs(1);

/// The state this machine's rho-ui keeps, as the server shares it.
pub(crate) struct Shared {
    /// The state database, or why it couldn't be opened.
    db: Result<Mutex<Connection>, String>,
    /// The settings file, if there is a config folder.
    settings: Option<PathBuf>,
}

impl Shared {
    /// Opens this user's settings file and state database, as the desktop
    /// app finds them.
    pub(crate) fn open() -> Self {
        let db = StateDb::file()
            .ok_or_else(|| "there is no state folder (set HOME)".to_owned())
            .and_then(|path| {
                let connection = StateDb::connect(&path)
                    .and_then(|connection| connection.busy_timeout(BUSY).map(|()| connection))
                    .map_err(|error| format!("couldn't open {}: {error}", path.display()))?;
                Ok(Mutex::new(connection))
            });
        Self {
            db,
            settings: Settings::file(),
        }
    }
}

/// Sends the state, then applies what the page writes, until it closes.
pub(crate) fn open(shared: Arc<Shared>, upgrade: WebSocketUpgrade) -> Response {
    match &shared.db {
        Ok(_) => upgrade.on_upgrade(move |socket| share(shared, socket)),
        Err(why) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("rho-ui serve: {why}"),
        )
            .into_response(),
    }
}

#[expect(
    clippy::print_stderr,
    reason = "a server says what went wrong on its terminal"
)]
async fn share(shared: Arc<Shared>, mut socket: WebSocket) {
    let Ok(db) = &shared.db else { return };
    let settings = shared
        .settings
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    let snapshot =
        serde_json::to_string(&StateSnapshot { settings }).expect("a StateSnapshot is plain JSON");
    let image = store::image(&mut db.lock().expect("the state's lock"));
    let image = match image {
        Ok(image) => image,
        Err(error) => {
            eprintln!("rho-ui serve: couldn't read the state database: {error}");
            return;
        }
    };
    if socket.send(Message::Text(snapshot.into())).await.is_err()
        || socket.send(Message::Binary(image.into())).await.is_err()
    {
        return;
    }
    while let Some(Ok(message)) = socket.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        match serde_json::from_str(&text) {
            Ok(StateWrite::Sql { sql }) => {
                let written = db.lock().expect("the state's lock").execute_batch(&sql);
                if let Err(error) = written {
                    eprintln!("rho-ui serve: couldn't save the page's state: {error}");
                }
            }
            Ok(StateWrite::Settings { text }) => {
                let Some(path) = &shared.settings else {
                    continue;
                };
                if let Err(error) = store::write_atomic(path, &text) {
                    eprintln!("rho-ui serve: couldn't save {}: {error}", path.display());
                }
            }
            Err(error) => eprintln!("rho-ui serve: the page sent a bad write: {error}"),
        }
    }
    // Everything the page sent before it closed is applied.
    let _ = socket.close().await;
}
