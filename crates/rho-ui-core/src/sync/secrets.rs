//! Comparing two machines' secrets without holding them: the source daemon
//! seals its values to a one-time key the target issues
//! (`rho/sync/recipient`, `rho/sync/export`), and the target says, item by
//! item, whether it holds the same (`rho/sync/compare`). rho-ui relays the
//! sealed bundle and never sees a value.

use std::collections::HashMap;

use rho_protocol::{
    AuthKind, AuthStatus, CompareState, McpSecretKind, McpSecretSlot, SecretItem,
    SyncExportResponse,
};

use super::Side;
use crate::Daemon;

/// How the target holds one secret, against the source's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Held {
    Same,
    Differs,
    /// The target has no value for it.
    Absent,
    /// The source has no saved value to send: an environment variable
    /// holds it.
    Missing,
    /// The target answered something this rho-ui doesn't know.
    Unknown,
}

/// What the target holds of the source's secrets, as compared when the two
/// machines were read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Secrets {
    compared: Result<HashMap<SecretItem, Held>, String>,
}

impl Secrets {
    /// No secret compared, as for two machines that have none.
    pub fn none() -> Self {
        Self {
            compared: Ok(HashMap::new()),
        }
    }

    /// How the target holds `item`, or why it can't say.
    pub(super) fn held(&self, item: &SecretItem) -> Result<Held, &str> {
        match &self.compared {
            Ok(held) => Ok(held.get(item).copied().unwrap_or(Held::Unknown)),
            Err(why) => Err(why),
        }
    }
}

/// The secret item of MCP server `server`'s secret `slot`; `None` for a
/// kind this rho-ui doesn't know.
pub(super) fn mcp_item(server: &str, slot: &McpSecretSlot) -> Option<SecretItem> {
    match slot.kind {
        McpSecretKind::Header => Some(SecretItem::mcp_header(server, &slot.name)),
        McpSecretKind::Env => Some(SecretItem::mcp_env(server, &slot.name)),
        _ => None,
    }
}

/// The secrets of `from` a sync could copy: its signed-in API keys, and its
/// MCP servers' secrets.
fn copyable(from: &Side) -> Vec<SecretItem> {
    let keys = from
        .auths
        .read()
        .into_iter()
        .flatten()
        .filter_map(|profile| {
            let saved = profile.kind == AuthKind::ApiKey && profile.status == AuthStatus::SignedIn;
            saved.then(|| SecretItem::api_key(&profile.auth))
        });
    let servers = from.mcp.read().into_iter().flat_map(|read| &read.servers);
    let slots = servers.flat_map(|server| {
        let identity = &server.identity;
        server
            .secrets
            .iter()
            .filter_map(move |slot| mcp_item(identity, slot))
    });
    keys.chain(slots).collect()
}

/// Compares the secrets of `from`, which `source` serves, with what
/// `target`, the machine `to`, holds.
pub async fn compare(source: &Daemon, from: &Side, target: &Daemon, to: &Side) -> Secrets {
    let items = copyable(from);
    if items.is_empty() {
        return Secrets::none();
    }
    Secrets {
        compared: compared(source, target, items, &to.label).await,
    }
}

async fn compared(
    source: &Daemon,
    target: &Daemon,
    items: Vec<SecretItem>,
    label: &str,
) -> Result<HashMap<SecretItem, Held>, String> {
    let export = seal(source, target, items, label).await?;
    let mut held: HashMap<SecretItem, Held> = export
        .missing
        .into_iter()
        .map(|item| (item, Held::Missing))
        .collect();
    if export.sealed.items.is_empty() {
        return Ok(held);
    }
    let compared = target
        .sync_compare(export.sealed)
        .await
        .map_err(|error| format!("Couldn't compare it with {label}'s: {error}"))?;
    held.extend(compared.into_iter().map(|compared| {
        let state = match compared.state {
            CompareState::Same => Held::Same,
            CompareState::Differs => Held::Differs,
            CompareState::Absent => Held::Absent,
            _ => Held::Unknown,
        };
        (compared.item, state)
    }));
    Ok(held)
}

/// Seals `source`'s values of `items` to a fresh one-time key of `target`,
/// the machine `label`; one key opens one bundle.
pub(super) async fn seal(
    source: &Daemon,
    target: &Daemon,
    items: Vec<SecretItem>,
    label: &str,
) -> Result<SyncExportResponse, String> {
    let recipient = target
        .sync_recipient()
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("Rho on {label} is too old to take this"))?;
    source
        .sync_export(recipient, items)
        .await
        .map_err(|error| error.to_string())
}
