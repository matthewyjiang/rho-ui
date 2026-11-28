//! The secrets settings sync copies without ever holding them: API keys and
//! MCP servers' header and environment values, sealed by the source daemon
//! to a one-time key the target issues (`rho/sync/recipient|export|
//! compare|import_keys`), and the MCP servers they belong to
//! (`rho/mcp/config/*`). The target journals what each write replaced, so
//! the sync can be reverted there (`rho/sync/status|revert|discard`).

use rho_protocol::{
    methods, ErrorCode, ImportedSecret, McpConfigGetParams, McpConfigGetResponse,
    McpConfigSetParams, RevertedItem, SealRecipient, SealedSecrets, SecretComparison, SecretItem,
    SyncCompareParams, SyncDiscardParams, SyncExportParams, SyncExportResponse, SyncId,
    SyncImportKeysParams, SyncItem, SyncRecipientParams, SyncRevertParams, SyncStatusParams,
    SyncStatusResponse,
};

use super::{Daemon, DaemonError};

impl Daemon {
    /// A one-time key another daemon can seal secrets to for this one,
    /// good for one bundle. `None` from a daemon older than sealed sync.
    ///
    /// # Errors
    ///
    /// Fails if the daemon refuses, or the connection is gone.
    pub async fn sync_recipient(&self) -> Result<Option<SealRecipient>, DaemonError> {
        let params = SyncRecipientParams::default();
        match self.client.request::<methods::SyncRecipient>(&params).await {
            Ok(issued) => Ok(Some(issued.recipient)),
            Err(error) if error.error_code() == Some(ErrorCode::MethodNotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Seals this daemon's values of `items` to `recipient`; the items it
    /// has no saved value for, as a key an environment variable holds, come
    /// back as missing.
    ///
    /// # Errors
    ///
    /// Fails if the daemon refuses, as a credential store it can't reach,
    /// or the connection is gone.
    pub async fn sync_export(
        &self,
        recipient: SealRecipient,
        items: Vec<SecretItem>,
    ) -> Result<SyncExportResponse, DaemonError> {
        let params = SyncExportParams::new(recipient, items);
        Ok(self.client.request::<methods::SyncExport>(&params).await?)
    }

    /// Whether this daemon holds the same values as `sealed`, sealed to a
    /// recipient it issued, item by item.
    ///
    /// # Errors
    ///
    /// Fails if the recipient is gone, the daemon refuses, or the
    /// connection is gone.
    pub async fn sync_compare(
        &self,
        sealed: SealedSecrets,
    ) -> Result<Vec<SecretComparison>, DaemonError> {
        let params = SyncCompareParams::new(sealed);
        let compared = self.client.request::<methods::SyncCompare>(&params).await?;
        Ok(compared.items)
    }

    /// Saves the API keys `sealed` holds, journaling what each replaced
    /// under `sync`; each item with what became of it.
    ///
    /// # Errors
    ///
    /// Fails if the recipient is gone, the daemon refuses, or the
    /// connection is gone. Nothing was saved then.
    pub async fn sync_import_keys(
        &self,
        sealed: SealedSecrets,
        sync: SyncId,
    ) -> Result<Vec<ImportedSecret>, DaemonError> {
        let params = SyncImportKeysParams::new(sealed, sync);
        let imported = self
            .client
            .request::<methods::SyncImportKeys>(&params)
            .await?;
        Ok(imported.items)
    }

    /// Whether each item `sync` changed, and not reverted yet, changed since;
    /// `None` when the daemon keeps no journal of `sync`.
    ///
    /// # Errors
    ///
    /// Fails if the daemon refuses, or the connection is gone.
    pub async fn sync_status(
        &self,
        sync: SyncId,
    ) -> Result<Option<SyncStatusResponse>, DaemonError> {
        let params = SyncStatusParams::new(sync);
        match self.client.request::<methods::SyncStatus>(&params).await {
            Ok(status) => Ok(Some(status)),
            Err(error) if gone(error.error_code()) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Restores what `sync` replaced of `items`, and of `overwrite` even
    /// where it changed since; each item with what became of it. `None`
    /// when the daemon keeps no journal of `sync`.
    ///
    /// # Errors
    ///
    /// Fails if the daemon refuses, or the connection is gone.
    pub async fn sync_revert(
        &self,
        sync: SyncId,
        items: Vec<SyncItem>,
        overwrite: Vec<SyncItem>,
    ) -> Result<Option<Vec<RevertedItem>>, DaemonError> {
        let params = SyncRevertParams::new(sync, items).with_overwrite(overwrite);
        match self.client.request::<methods::SyncRevert>(&params).await {
            Ok(reverted) => Ok(Some(reverted.items)),
            Err(error) if gone(error.error_code()) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Forgets `sync`'s journal, keeping what it wrote; one the daemon no
    /// longer keeps is forgotten already.
    ///
    /// # Errors
    ///
    /// Fails if the daemon refuses, or the connection is gone.
    pub async fn sync_discard(&self, sync: SyncId) -> Result<(), DaemonError> {
        let params = SyncDiscardParams::new(sync);
        match self.client.request::<methods::SyncDiscard>(&params).await {
            Ok(_) => Ok(()),
            Err(error) if gone(error.error_code()) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// The MCP servers of the daemon's `config.toml`, their secrets named
    /// without their values. `None` from a daemon older than reading them.
    ///
    /// # Errors
    ///
    /// Fails if the daemon can't read the file, or the connection is gone.
    pub async fn mcp_config(&self) -> Result<Option<McpConfigGetResponse>, DaemonError> {
        let params = McpConfigGetParams::default();
        match self.client.request::<methods::McpConfigGet>(&params).await {
            Ok(read) => Ok(Some(read)),
            Err(error) if error.error_code() == Some(ErrorCode::MethodNotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Writes MCP server changes, all of them or none.
    ///
    /// # Errors
    ///
    /// [`DaemonError::Conflict`] if the servers changed since the revision
    /// `params` sends. Also fails if the recipient its bundle was sealed to
    /// is gone, the daemon refuses a change, or the file wouldn't load with
    /// them. Nothing was written then.
    pub async fn set_mcp_config(&self, params: McpConfigSetParams) -> Result<(), DaemonError> {
        self.client
            .request::<methods::McpConfigSet>(&params)
            .await?;
        Ok(())
    }
}

/// Whether `code` says the daemon keeps no journal of the sync named.
fn gone(code: Option<ErrorCode>) -> bool {
    code == Some(ErrorCode::SyncNotFound)
}
