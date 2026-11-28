//! The MCP servers of rho's `config.toml`, read and written whole server
//! at a time through `rho/mcp/config/*`. A server's header and environment
//! values are secrets: a put keeps the target's own value where it is the
//! same, and takes the rest sealed from the source.

use rho_protocol::{
    ConfigRevision, McpConfigSetParams, McpSecret, McpServerChange, McpServerEntry, SecretItem,
    SecretSource, SyncId,
};
use serde_json::Value;

use super::secrets::{self, Held, Secrets};
use super::{Action, Change, Copy, GroupKind, McpServer, Part, Planned, Row, RowKey, Side};
use crate::{Daemon, DaemonError};

impl McpServer {
    fn of(entry: &McpServerEntry) -> Self {
        Self {
            value: Box::new(entry.value.clone()),
            secrets: entry.secrets.clone(),
        }
    }
}

/// The servers that differ, the source's first in its order, then those
/// only the target has. A server differs in its entry, in which secrets it
/// has, or in a secret's value.
pub(super) fn rows(source: &Side, target: &Side, secrets: &Secrets) -> Vec<Row> {
    let from = match &source.mcp {
        Part::Read(from) => &from.servers,
        Part::Failed(why) => {
            return vec![Row::unread(
                GroupKind::Mcp,
                "mcp",
                "MCP servers",
                source,
                why,
            )];
        }
        Part::Lacks => return Vec::new(),
    };
    let to = target.take(&target.mcp);
    let theirs = |identity: &str| {
        let servers = to.as_ref().ok().map(|to| &to.servers)?;
        servers.iter().find(|server| server.identity == identity)
    };
    let ours = from.iter().filter_map(|entry| {
        let server = McpServer::of(entry);
        let there = theirs(&entry.identity).map(McpServer::of);
        let copy = match &to {
            Ok(_) => copy(entry, &server, there.as_ref(), secrets)?,
            Err(why) => Copy::Cannot(why.clone()),
        };
        // A write over the same entry changes only secrets' values.
        let values_only = matches!(copy, Copy::Write(_)) && there.as_ref() == Some(&server);
        let mut row = row(&entry.identity, Some(&server), there.as_ref(), copy);
        // The target may refuse it too, and with it the other servers.
        row.note = entry
            .error
            .as_ref()
            .map(|error| format!("Rho on {} doesn't load it: {error}", source.label));
        if values_only {
            row.target = row
                .target
                .map(|shown| format!("{shown} · other secret values"));
        }
        Some(row)
    });
    let only_theirs = to.iter().flat_map(|to| &to.servers).filter_map(|entry| {
        if from.iter().any(|ours| ours.identity == entry.identity) {
            return None;
        }
        let there = McpServer::of(entry);
        let change = Change::Mcp {
            identity: entry.identity.clone(),
            before: Some(there.clone()),
            after: None,
            keep: Vec::new(),
        };
        Some(row(
            &entry.identity,
            None,
            Some(&there),
            Copy::Write(change),
        ))
    });
    ours.chain(only_theirs).collect()
}

/// What syncing server `entry`, read as `ours`, writes over `there`, the
/// target's; `None` when the target holds the same, its secrets' values
/// too.
fn copy(
    entry: &McpServerEntry,
    ours: &McpServer,
    there: Option<&McpServer>,
    secrets: &Secrets,
) -> Option<Copy> {
    let mut keep = Vec::new();
    for slot in &entry.secrets {
        let Some(item) = secrets::mcp_item(&entry.identity, slot) else {
            let why = format!("Its secret {} is of a kind rho-ui doesn't know", slot.name);
            return Some(Copy::Cannot(why));
        };
        match secrets.held(&item) {
            Ok(Held::Same) => keep.push(slot.clone()),
            Ok(Held::Differs | Held::Absent) => {}
            Ok(Held::Missing | Held::Unknown) => {
                let why = format!("Couldn't compare its secret {}", slot.name);
                return Some(Copy::Cannot(why));
            }
            Err(why) => return Some(Copy::Cannot(why.to_owned())),
        }
    }
    let change = Change::Mcp {
        identity: entry.identity.clone(),
        before: there.cloned(),
        after: Some(ours.clone()),
        keep,
    };
    (!change.is_noop()).then_some(Copy::Write(change))
}

fn row(identity: &str, from: Option<&McpServer>, to: Option<&McpServer>, copy: Copy) -> Row {
    Row {
        key: RowKey {
            kind: GroupKind::Mcp,
            id: identity.to_owned(),
        },
        label: identity.to_owned(),
        source: from.map(shown),
        target: to.map(shown),
        action: Action::of(from, to),
        one_machine: false,
        restart: false,
        note: None,
        offer: None,
        copy,
    }
}

/// A server for people: where it is, and the names of its secrets.
pub(super) fn shown(server: &McpServer) -> String {
    let field = |name| server.value.get(name).and_then(Value::as_str);
    let at = match (field("url"), field("command")) {
        (Some(url), _) => url.to_owned(),
        (None, Some(command)) => {
            let args = server.value.get("args").and_then(Value::as_array);
            let args = args.into_iter().flatten().filter_map(Value::as_str);
            std::iter::once(command)
                .chain(args)
                .collect::<Vec<_>>()
                .join(" ")
        }
        (None, None) => "configured".to_owned(),
    };
    if server.secrets.is_empty() {
        return at;
    }
    let names: Vec<&str> = server.secrets.iter().map(|s| s.name.as_str()).collect();
    format!("{at} · {}", names.join(", "))
}

/// Server `identity` as `side` holds it now, or why it can't say.
pub(super) fn value(side: &Side, identity: &str) -> Result<Option<McpServer>, String> {
    let read = side.take(&side.mcp)?;
    Ok(read
        .servers
        .iter()
        .find(|server| server.identity == identity)
        .map(McpServer::of))
}

/// Writes the servers `items` change to `target`, the machine `label`, in
/// one request against `revision`, journaled under `sync`; the secrets it
/// doesn't keep are sealed from `source`.
pub(super) async fn write(
    source: Option<&Daemon>,
    target: &Daemon,
    label: &str,
    revision: ConfigRevision,
    items: &[Planned],
    sync: &SyncId,
) -> Result<(), DaemonError> {
    let mut changes = Vec::new();
    let mut sealed: Vec<SecretItem> = Vec::new();
    for item in items {
        let Change::Mcp {
            identity,
            after,
            keep,
            ..
        } = &item.change
        else {
            continue;
        };
        let Some(server) = after else {
            changes.push(McpServerChange::Remove {
                identity: identity.clone(),
            });
            continue;
        };
        let secrets = server.secrets.iter().map(|slot| {
            let kept = keep.contains(slot);
            if !kept {
                sealed.extend(secrets::mcp_item(identity, slot));
            }
            let source = if kept {
                SecretSource::Keep
            } else {
                SecretSource::Sealed
            };
            McpSecret::new(slot.clone(), source)
        });
        changes.push(McpServerChange::Put {
            identity: identity.clone(),
            value: (*server.value).clone(),
            secrets: secrets.collect(),
        });
    }
    let mut params = McpConfigSetParams::new(revision, changes).with_sync(sync.clone());
    if !sealed.is_empty() {
        let source = source.ok_or_else(|| {
            DaemonError::Refused("Its secrets come from the source, which isn't here".to_owned())
        })?;
        let export = secrets::seal(source, target, sealed, label)
            .await
            .map_err(DaemonError::Refused)?;
        if !export.missing.is_empty() {
            let why = "The source no longer has all of its secrets".to_owned();
            return Err(DaemonError::Refused(why));
        }
        params = params.with_sealed(export.sealed);
    }
    target.set_mcp_config(params).await
}
