//! The fake daemon's sealed secrets (`rho/sync/*`), its sync journal, and
//! the MCP servers of its `config.toml` (`rho/mcp/config/*`).
//!
//! Nothing is encrypted: a bundle's ciphertext is the JSON of its values.
//! As in rho, only the daemon that issued a bundle's recipient opens it, and
//! only once, so two fakes in one test exchange bundles as two daemons do.

use std::{
    collections::BTreeMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::atomic::{AtomicU64, Ordering},
};

use rho_protocol::{
    AuthKind, CompareState, ConfigRevision, ErrorCode, ImportOutcome, ImportedSecret,
    McpConfigGetResponse, McpConfigSetParams, McpSecret, McpSecretKind, McpSecretSlot,
    McpServerChange, McpServerEntry, RevertOutcome, RevertedItem, RpcError, SealRecipient,
    SealSuite, SealedSecrets, SecretComparison, SecretItem, SecretSource, SyncCompareResponse,
    SyncExportParams, SyncExportResponse, SyncId, SyncImportKeysParams, SyncImportKeysResponse,
    SyncItem, SyncItemState, SyncItemStatus, SyncRevertParams, SyncRevertResponse,
    SyncStatusResponse,
};
use serde_json::Value;

use super::{auth::FakeAuth, unix_now};

/// The one suite the fake seals with, as rho's daemon.
const SUITE: SealSuite = SealSuite::X25519HkdfSha256ChaCha20Poly1305;

/// How long a recipient lasts, in seconds, as rho's: five minutes. The fake
/// reports it and never expires one.
const RECIPIENT_LIFETIME: u64 = 5 * 60;

/// How long the journal lasts after its sync's last write, in seconds, as
/// rho's: seven days. The fake reports it and never expires one.
const JOURNAL_LIFETIME: u64 = 7 * 24 * 60 * 60;

/// The recipients every fake in the process has issued, so no two share a
/// public key, as no two daemons do.
static RECIPIENTS: AtomicU64 = AtomicU64::new(0);

/// The revision of `content`: equal content, equal revision.
fn revision(content: &impl Hash) -> ConfigRevision {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    ConfigRevision::new(format!("{:x}", hasher.finish()))
}

/// A refusal of the request's params.
fn invalid(message: impl Into<String>) -> RpcError {
    RpcError::new(ErrorCode::InvalidParams, message)
}

/// One server of `[mcp.servers]`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Server {
    /// The entry without its `headers` and `env` maps.
    value: Value,
    /// Those maps' values, by slot, in order.
    secrets: Vec<(McpSecretSlot, String)>,
}

/// A server of `[mcp.servers]` as its revision hashes it: identity, value
/// and secrets.
type RevisionPart<'a> = (&'a str, String, &'a [(McpSecretSlot, String)]);

impl Server {
    /// The value in `slot`, if the server has one.
    fn secret(&self, slot: &McpSecretSlot) -> Option<&str> {
        self.secrets
            .iter()
            .find(|(held, _)| held == slot)
            .map(|(_, value)| value.as_str())
    }
}

/// One item the journal's sync changed: what the daemon held before the
/// sync's first write of it, and what its last write left.
#[derive(Debug)]
enum Journaled {
    /// The saved API key of `auth`; `before` is none when the sync saved
    /// the first.
    Key {
        auth: String,
        before: Option<String>,
        written: String,
    },
    /// MCP server `identity`, each side none when the server was absent.
    Server {
        identity: String,
        before: Option<Server>,
        written: Option<Server>,
    },
}

impl Journaled {
    /// The item, as the protocol names it.
    fn item(&self) -> SyncItem {
        match self {
            Self::Key { auth, .. } => SyncItem::api_key(auth.clone()),
            Self::Server { identity, .. } => SyncItem::mcp_server(identity.clone()),
        }
    }

    /// Takes what `later`, a later write of the same item, left, keeping
    /// the first `before`.
    fn rewrite(&mut self, later: Self) {
        match (self, later) {
            (Self::Key { written, .. }, Self::Key { written: later, .. }) => *written = later,
            (Self::Server { written, .. }, Self::Server { written: later, .. }) => {
                *written = later;
            }
            // Equal items are of one kind.
            (Self::Key { .. } | Self::Server { .. }, _) => {}
        }
    }
}

/// The last sync's journal.
#[derive(Debug)]
struct Journal {
    sync: SyncId,
    /// What it changed and didn't revert yet, in the order it first wrote
    /// them.
    items: Vec<Journaled>,
    /// Unix seconds of its last write.
    written_at: u64,
}

/// The fake's live recipients, `[mcp.servers]`, and sync journal.
#[derive(Debug, Default)]
pub(super) struct FakeSecrets {
    /// The public keys of the recipients issued and not yet used.
    recipients: Vec<String>,
    /// `[mcp.servers]`, by identity.
    servers: BTreeMap<String, Server>,
    /// The last sync's journal, until it is fully reverted or discarded.
    journal: Option<Journal>,
}

impl FakeSecrets {
    /// Answers `rho/sync/recipient`: a key no fake in the process issued
    /// before, live on this one until a bundle sealed to it is opened.
    pub(super) fn recipient(&mut self) -> SealRecipient {
        let issued = RECIPIENTS.fetch_add(1, Ordering::Relaxed);
        let public_key = format!("fake-recipient-{issued}");
        self.recipients.push(public_key.clone());
        SealRecipient::new(SUITE, public_key, unix_now() + RECIPIENT_LIFETIME)
    }

    /// Answers `rho/sync/export`: this daemon's values of the items,
    /// "sealed" to the recipient. An item with no value saved, such as a key
    /// that comes from the environment, is missing; a profile that takes no
    /// API key, or an item of a kind the fake doesn't know, is refused.
    pub(super) fn export(
        &self,
        auth: &FakeAuth,
        params: SyncExportParams,
    ) -> Result<SyncExportResponse, RpcError> {
        if params.recipient.suite != SUITE {
            return Err(invalid("a suite the fake doesn't know"));
        }
        let mut items = Vec::new();
        let mut values = Vec::new();
        let mut missing = Vec::new();
        for item in params.items {
            exportable(&item)?;
            match self.value(auth, &item) {
                Some(value) => {
                    items.push(item);
                    values.push(value);
                }
                None => missing.push(item),
            }
        }
        let ciphertext = serde_json::to_string(&values).expect("strings are JSON");
        let sealed = SealedSecrets::new(
            SUITE,
            params.recipient.public_key,
            items,
            "fake".to_owned(),
            ciphertext,
        );
        Ok(SyncExportResponse::new(sealed, missing))
    }

    /// This daemon's value of `item`, if it holds one.
    fn value(&self, auth: &FakeAuth, item: &SecretItem) -> Option<String> {
        let (server, kind, name) = match item {
            SecretItem::ApiKey { auth: id, .. } => return auth.saved_key(id).map(str::to_owned),
            SecretItem::McpHeader { server, name, .. } => (server, McpSecretKind::Header, name),
            SecretItem::McpEnv { server, name, .. } => (server, McpSecretKind::Env, name),
            _ => return None,
        };
        let slot = McpSecretSlot::new(kind, name.as_str());
        self.servers.get(server)?.secret(&slot).map(str::to_owned)
    }

    /// Opens `sealed`, using its recipient up whether or not the rest
    /// opens: its items with their values, in order. A suite or item kind
    /// the fake doesn't know is refused first, keeping the recipient.
    fn open(&mut self, sealed: &SealedSecrets) -> Result<Vec<(SecretItem, String)>, RpcError> {
        if sealed.suite != SUITE
            || sealed
                .items
                .iter()
                .any(|item| matches!(item, SecretItem::Unknown))
        {
            return Err(invalid("a suite or item kind the fake doesn't know"));
        }
        let Some(at) = self
            .recipients
            .iter()
            .position(|key| *key == sealed.recipient)
        else {
            return Err(RpcError::new(
                ErrorCode::SealRecipientGone,
                "no live recipient matches the bundle",
            ));
        };
        self.recipients.swap_remove(at);
        let values: Vec<String> = serde_json::from_str(&sealed.ciphertext)
            .map_err(|_| invalid("the bundle doesn't open"))?;
        if values.len() != sealed.items.len() {
            return Err(invalid("the bundle doesn't open"));
        }
        Ok(sealed.items.iter().cloned().zip(values).collect())
    }

    /// Answers `rho/sync/compare`: how each sealed value compares with this
    /// daemon's.
    pub(super) fn compare(
        &mut self,
        auth: &FakeAuth,
        sealed: &SealedSecrets,
    ) -> Result<SyncCompareResponse, RpcError> {
        let opened = self.open(sealed)?;
        let items = opened
            .into_iter()
            .map(|(item, value)| {
                let state = match self.value(auth, &item) {
                    Some(held) if held == value => CompareState::Same,
                    Some(_) => CompareState::Differs,
                    None => CompareState::Absent,
                };
                SecretComparison::new(item, state)
            })
            .collect();
        Ok(SyncCompareResponse::new(items))
    }

    /// Answers `rho/sync/import_keys`: saves each sealed key, journaling
    /// what it replaced under the request's sync.
    pub(super) fn import_keys(
        &mut self,
        auth: &mut FakeAuth,
        params: &SyncImportKeysParams,
    ) -> Result<SyncImportKeysResponse, RpcError> {
        let opened = self.open(&params.sealed)?;
        let mut items = Vec::new();
        for (item, key) in opened {
            let outcome = self.import_key(auth, &params.sync, &item, key);
            items.push(ImportedSecret::new(item, outcome));
        }
        Ok(SyncImportKeysResponse::new(items))
    }

    /// Saves `key` as `item`'s, if it names a profile that takes an API
    /// key, and journals it under `sync`.
    fn import_key(
        &mut self,
        auth: &mut FakeAuth,
        sync: &SyncId,
        item: &SecretItem,
        key: String,
    ) -> ImportOutcome {
        let SecretItem::ApiKey { auth: id, .. } = item else {
            return ImportOutcome::Failed {
                message: "only API keys import".to_owned(),
            };
        };
        if FakeAuth::kind(id) != Some(AuthKind::ApiKey) {
            return ImportOutcome::Failed {
                message: format!("{id} takes no API key here"),
            };
        }
        let before = auth.saved_key(id).map(str::to_owned);
        auth.set_key(id, Some(key.clone()));
        self.record(
            sync,
            Journaled::Key {
                auth: id.clone(),
                before,
                written: key,
            },
        );
        ImportOutcome::Saved
    }

    /// Journals `entry` under `sync`, replacing another sync's journal. An
    /// item already journaled keeps its first `before`.
    fn record(&mut self, sync: &SyncId, entry: Journaled) {
        let now = unix_now();
        if self
            .journal
            .as_ref()
            .is_some_and(|journal| journal.sync != *sync)
        {
            self.journal = None;
        }
        let journal = self.journal.get_or_insert_with(|| Journal {
            sync: sync.clone(),
            items: Vec::new(),
            written_at: now,
        });
        journal.written_at = now;
        let item = entry.item();
        match journal.items.iter_mut().find(|held| held.item() == item) {
            Some(held) => held.rewrite(entry),
            None => journal.items.push(entry),
        }
    }

    /// The revision of `[mcp.servers]`, which covers the secret values too.
    fn mcp_revision(&self) -> ConfigRevision {
        let content: Vec<RevisionPart<'_>> = self
            .servers
            .iter()
            .map(|(identity, server)| {
                (
                    identity.as_str(),
                    server.value.to_string(),
                    server.secrets.as_slice(),
                )
            })
            .collect();
        revision(&content)
    }

    /// Answers `rho/mcp/config/get`: the servers, their secrets named
    /// without their values.
    pub(super) fn mcp_config(&self) -> McpConfigGetResponse {
        let servers = self
            .servers
            .iter()
            .map(|(identity, server)| {
                let slots = server.secrets.iter().map(|(slot, _)| slot.clone());
                McpServerEntry::new(
                    identity.clone(),
                    server.value.clone(),
                    slots.collect(),
                    None,
                )
            })
            .collect();
        McpConfigGetResponse::new(self.mcp_revision(), servers)
    }

    /// Answers `rho/mcp/config/set`: every change, or none. The revision is
    /// checked before the bundle opens, so a stale write keeps the
    /// recipient; the bundle must carry exactly the sealed secrets.
    pub(super) fn set_mcp(
        &mut self,
        params: McpConfigSetParams,
    ) -> Result<ConfigRevision, RpcError> {
        if params.revision != self.mcp_revision() {
            return Err(RpcError::new(
                ErrorCode::ConfigConflict,
                "[mcp.servers] changed since it was read",
            ));
        }
        let holds_secrets = |change: &McpServerChange| {
            matches!(change, McpServerChange::Put { value, .. }
                if value.get("headers").is_some() || value.get("env").is_some())
        };
        if params.changes.iter().any(holds_secrets) {
            return Err(invalid("a put's value holds headers or env"));
        }
        let mut sealed = match &params.sealed {
            Some(bundle) => self.open(bundle)?,
            None => Vec::new(),
        };
        let mut servers = self.servers.clone();
        let mut written = Vec::new();
        for change in params.changes {
            let identity = change.identity().to_owned();
            let before = servers.get(&identity).cloned();
            match change {
                McpServerChange::Put { value, secrets, .. } => {
                    let server = put(&identity, before.as_ref(), value, secrets, &mut sealed)?;
                    servers.insert(identity.clone(), server);
                }
                McpServerChange::Remove { .. } => {
                    if servers.remove(&identity).is_none() {
                        return Err(invalid(format!("there is no server {identity}")));
                    }
                }
            }
            let after = servers.get(&identity).cloned();
            written.push(Journaled::Server {
                identity,
                before,
                written: after,
            });
        }
        if !sealed.is_empty() {
            return Err(invalid("the bundle carries secrets no put takes"));
        }
        self.servers = servers;
        if let Some(sync) = &params.sync {
            for entry in written {
                self.record(sync, entry);
            }
        }
        Ok(self.mcp_revision())
    }

    /// The journal, if it is `sync`'s.
    fn journal_of(&self, sync: &SyncId) -> Result<&Journal, RpcError> {
        self.journal
            .as_ref()
            .filter(|journal| journal.sync == *sync)
            .ok_or_else(|| sync_not_found(sync))
    }

    /// Whether the daemon still holds what `entry`'s sync wrote.
    fn unchanged(&self, auth: &FakeAuth, entry: &Journaled) -> bool {
        match entry {
            Journaled::Key {
                auth: id, written, ..
            } => auth.saved_key(id) == Some(written.as_str()),
            Journaled::Server {
                identity, written, ..
            } => self.servers.get(identity) == written.as_ref(),
        }
    }

    /// Answers `rho/sync/status`: whether each journaled item changed since
    /// the sync wrote it.
    pub(super) fn status(
        &self,
        auth: &FakeAuth,
        sync: &SyncId,
    ) -> Result<SyncStatusResponse, RpcError> {
        let journal = self.journal_of(sync)?;
        let items = journal
            .items
            .iter()
            .map(|entry| {
                let state = if self.unchanged(auth, entry) {
                    SyncItemState::Unchanged
                } else {
                    SyncItemState::Changed
                };
                SyncItemStatus::new(entry.item(), state)
            })
            .collect();
        Ok(SyncStatusResponse::new(
            journal.written_at + JOURNAL_LIFETIME,
            items,
        ))
    }

    /// Answers `rho/sync/revert`: restores each item, unless it changed
    /// since the sync and isn't in `overwrite`. A journal left empty is
    /// dropped.
    pub(super) fn revert(
        &mut self,
        auth: &mut FakeAuth,
        params: SyncRevertParams,
    ) -> Result<SyncRevertResponse, RpcError> {
        let mut journal = match self.journal.take() {
            Some(journal) if journal.sync == params.sync => journal,
            other => {
                self.journal = other;
                return Err(sync_not_found(&params.sync));
            }
        };
        let mut items = Vec::new();
        for item in params.items {
            let outcome = self.revert_item(auth, &mut journal, &item, &params.overwrite);
            items.push(RevertedItem::new(item, outcome));
        }
        if !journal.items.is_empty() {
            self.journal = Some(journal);
        }
        Ok(SyncRevertResponse::new(items))
    }

    /// Restores `item` from `journal`, unless it changed since the sync and
    /// `overwrite` lacks it.
    fn revert_item(
        &mut self,
        auth: &mut FakeAuth,
        journal: &mut Journal,
        item: &SyncItem,
        overwrite: &[SyncItem],
    ) -> RevertOutcome {
        let Some(at) = journal.items.iter().position(|entry| entry.item() == *item) else {
            return RevertOutcome::Failed {
                message: "the sync didn't change it, or it is reverted".to_owned(),
            };
        };
        if !overwrite.contains(item)
            && journal
                .items
                .get(at)
                .is_some_and(|entry| !self.unchanged(auth, entry))
        {
            return RevertOutcome::Changed;
        }
        match journal.items.remove(at) {
            Journaled::Key {
                auth: id, before, ..
            } => auth.set_key(&id, before),
            Journaled::Server {
                identity, before, ..
            } => self.edit_server(identity, before),
        }
        RevertOutcome::Restored
    }

    /// Answers `rho/sync/discard`: forgets the journal, keeping what the
    /// sync wrote.
    pub(super) fn discard(&mut self, sync: &SyncId) -> Result<(), RpcError> {
        self.journal_of(sync)?;
        self.journal = None;
        Ok(())
    }

    /// Puts server `identity` as `value` with `secrets`, each a slot and its
    /// value, or removes it when `None`, as a hand edit of `config.toml`
    /// does.
    pub(super) fn edit_mcp_server(
        &mut self,
        identity: &str,
        server: Option<(Value, Vec<(McpSecretSlot, String)>)>,
    ) {
        let server = server.map(|(value, secrets)| Server { value, secrets });
        self.edit_server(identity.to_owned(), server);
    }

    /// Puts server `identity` as `server`, or removes it when `None`.
    fn edit_server(&mut self, identity: String, server: Option<Server>) {
        match server {
            Some(server) => {
                self.servers.insert(identity, server);
            }
            None => {
                self.servers.remove(&identity);
            }
        }
    }

    /// Server `identity` now: its value, and its secrets with their values.
    pub(super) fn mcp_server(
        &self,
        identity: &str,
    ) -> Option<(Value, Vec<(McpSecretSlot, String)>)> {
        self.servers
            .get(identity)
            .map(|server| (server.value.clone(), server.secrets.clone()))
    }
}

/// Refuses what an export can't seal: a profile that takes no API key, or
/// an item of a kind the fake doesn't know.
fn exportable(item: &SecretItem) -> Result<(), RpcError> {
    match item {
        SecretItem::ApiKey { auth, .. } if FakeAuth::kind(auth) != Some(AuthKind::ApiKey) => {
            Err(invalid(format!("{auth} takes no API key")))
        }
        SecretItem::ApiKey { .. } | SecretItem::McpHeader { .. } | SecretItem::McpEnv { .. } => {
            Ok(())
        }
        _ => Err(invalid("an item of a kind the fake doesn't know")),
    }
}

/// The server a put writes: `value`, with each secret from the server's
/// own value (`before`) or the matching item of `sealed`, which it takes.
fn put(
    identity: &str,
    before: Option<&Server>,
    value: Value,
    secrets: Vec<McpSecret>,
    sealed: &mut Vec<(SecretItem, String)>,
) -> Result<Server, RpcError> {
    let mut filled = Vec::new();
    for secret in secrets {
        let held = match secret.source {
            SecretSource::Keep => before
                .and_then(|server| server.secret(&secret.slot))
                .map(str::to_owned)
                .ok_or_else(|| {
                    invalid(format!("{identity} holds no {} to keep", secret.slot.name))
                })?,
            SecretSource::Sealed => take_sealed(identity, &secret.slot, sealed)?,
        };
        filled.push((secret.slot, held));
    }
    Ok(Server {
        value,
        secrets: filled,
    })
}

/// Takes the value of `slot` of server `identity` out of `sealed`.
fn take_sealed(
    identity: &str,
    slot: &McpSecretSlot,
    sealed: &mut Vec<(SecretItem, String)>,
) -> Result<String, RpcError> {
    let item = match slot.kind {
        McpSecretKind::Header => SecretItem::mcp_header(identity, slot.name.as_str()),
        McpSecretKind::Env => SecretItem::mcp_env(identity, slot.name.as_str()),
        _ => return Err(invalid("a secret kind the fake doesn't know")),
    };
    let at = sealed
        .iter()
        .position(|(held, _)| *held == item)
        .ok_or_else(|| invalid(format!("the bundle lacks {identity}'s {}", slot.name)))?;
    Ok(sealed.swap_remove(at).1)
}

/// The refusal of a request naming a sync the journal isn't of.
fn sync_not_found(sync: &SyncId) -> RpcError {
    RpcError::new(
        ErrorCode::SyncNotFound,
        format!("no journal for sync {sync}"),
    )
}
