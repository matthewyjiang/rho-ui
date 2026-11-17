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
use rho_ui_core::server::wire::{SettingsFile, StateSnapshot, StateWrite};
use rusqlite::Connection;

use crate::{
    settings::{self, Settings},
    store::{self, StateDb},
};

/// How long a write waits for the desktop app's to the same database to
/// finish: a write and its checkpoint take about 10 ms (`store.rs`), so
/// this only stops one held up for good.
const BUSY: std::time::Duration = std::time::Duration::from_secs(1);

/// The state this machine's rho-ui keeps, as the server shares it.
pub(crate) struct Shared {
    db: Mutex<Connection>,
    /// The settings file, if there is a config folder.
    settings: Option<PathBuf>,
}

impl Shared {
    /// Opens this user's state database, as the desktop app finds it, and
    /// finds the settings file.
    ///
    /// # Errors
    ///
    /// Says why if there is no state folder, or the database won't open.
    pub(crate) fn open() -> Result<Self, String> {
        let path = StateDb::file().ok_or("there is no state folder (set HOME)")?;
        let connection = StateDb::connect(&path)
            .and_then(|connection| connection.busy_timeout(BUSY).map(|()| connection))
            .map_err(|error| format!("couldn't open {}: {error}", path.display()))?;
        Ok(Self {
            db: Mutex::new(connection),
            settings: Settings::file(),
        })
    }
}

/// Sends the state, then applies what the page writes, until it closes;
/// if the state couldn't be opened, refuses with why.
pub(crate) fn open(shared: &Result<Arc<Shared>, String>, upgrade: WebSocketUpgrade) -> Response {
    match shared {
        Ok(shared) => {
            let shared = Arc::clone(shared);
            upgrade.on_upgrade(move |socket| share(shared, socket))
        }
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
    let settings = match &shared.settings {
        Some(path) => settings::read(path),
        None => SettingsFile::Unreadable {
            why: "there is no config folder (set HOME)".to_owned(),
        },
    };
    // A file that couldn't be read is never written over, as the desktop
    // app doesn't.
    let settings_path = shared
        .settings
        .as_ref()
        .filter(|_| !matches!(settings, SettingsFile::Unreadable { .. }));
    let snapshot =
        serde_json::to_string(&StateSnapshot { settings }).expect("a StateSnapshot is plain JSON");
    let image = store::image(&mut shared.db.lock().expect("the state's lock"));
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
                let written = shared
                    .db
                    .lock()
                    .expect("the state's lock")
                    .execute_batch(&sql);
                if let Err(error) = written {
                    eprintln!("rho-ui serve: couldn't save the page's state: {error}");
                }
            }
            Ok(StateWrite::Settings { text }) => {
                let Some(path) = settings_path else {
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
