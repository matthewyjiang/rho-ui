//! The parts of rho's configuration no setting edits, read and written
//! whole entries at a time for settings sync: provider profiles, model
//! aliases and favorites (`rho/config/table/*`), and the user's hooks file
//! (`rho/hooks/file/*`). Each has its own revision, which a write sends
//! back; a stale one is refused as [`DaemonError::Conflict`].

use std::path::PathBuf;

use rho_protocol::{
    methods, ConfigRevision, ConfigTable, ConfigTableGetParams, ConfigTableSetParams, ErrorCode,
    HooksFileGetParams, HooksFileSetParams, TableChange, TableEntry,
};

use super::{Daemon, DaemonError};

/// One table as [`Daemon::config_table`] read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRead {
    /// The table's own revision, which a write sends back.
    pub revision: ConfigRevision,
    /// Provider profiles and aliases by key; favorites in the file's order.
    pub entries: Vec<TableEntry>,
}

/// The user's hooks file as [`Daemon::hooks_file`] read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HooksFileRead {
    /// Where it is, or would be, on the daemon's machine.
    pub path: PathBuf,
    /// The file's revision, which a write sends back.
    pub revision: ConfigRevision,
    /// Its whole text; `None` when there is no file.
    pub text: Option<String>,
}

impl Daemon {
    /// `table`'s entries and revision. `None` from a daemon older than
    /// tables.
    ///
    /// # Errors
    ///
    /// Fails if the daemon can't read `config.toml`, or the connection is
    /// gone.
    pub async fn config_table(&self, table: ConfigTable) -> Result<Option<TableRead>, DaemonError> {
        let params = ConfigTableGetParams::new(table);
        match self
            .client
            .request::<methods::ConfigTableGet>(&params)
            .await
        {
            Ok(read) => Ok(Some(TableRead {
                revision: read.revision,
                entries: read.entries,
            })),
            Err(error) if error.error_code() == Some(ErrorCode::MethodNotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Writes `changes` to `table`, all of them or none, against
    /// `revision`; returns the table's new revision.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Conflict`] if the table changed since `revision`.
    /// Also fails if the daemon refuses an entry, or the file wouldn't load
    /// with it. Nothing was written then.
    pub async fn set_config_table(
        &self,
        table: ConfigTable,
        revision: ConfigRevision,
        changes: Vec<TableChange>,
    ) -> Result<ConfigRevision, DaemonError> {
        let params = ConfigTableSetParams::new(table, revision, changes);
        let set = self
            .client
            .request::<methods::ConfigTableSet>(&params)
            .await?;
        Ok(set.revision)
    }

    /// The user's hooks file. `None` from a daemon older than reading it.
    ///
    /// # Errors
    ///
    /// Fails if the daemon can't read the file, or the connection is gone.
    pub async fn hooks_file(&self) -> Result<Option<HooksFileRead>, DaemonError> {
        let params = HooksFileGetParams::new();
        match self.client.request::<methods::HooksFileGet>(&params).await {
            Ok(read) => Ok(Some(HooksFileRead {
                path: read.path,
                revision: read.revision,
                text: read.text,
            })),
            Err(error) if error.error_code() == Some(ErrorCode::MethodNotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Replaces the user's hooks file with `text`, or deletes it when
    /// `None`, against `revision`; returns the file's new revision.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Conflict`] if the file changed since `revision`. Also
    /// fails if the text doesn't load, as when a hook names a program by a
    /// path the machine lacks. Nothing was written then.
    pub async fn set_hooks_file(
        &self,
        revision: ConfigRevision,
        text: Option<String>,
    ) -> Result<ConfigRevision, DaemonError> {
        let params = HooksFileSetParams::new(revision, text);
        let set = self
            .client
            .request::<methods::HooksFileSet>(&params)
            .await?;
        Ok(set.revision)
    }
}
