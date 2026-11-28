//! The plugins group: the user's agent plugins (`rho/plugins/*`), one row
//! per plugin whose install or enabled state differs. A plugin syncs only
//! from git, as the target can fetch it again; a copy from a folder, or a
//! link to one, names a folder on the source's machine. Revert puts back
//! what the target had only when it can install that again: from git, or
//! by linking the same folder.

use std::{fmt::Write as _, path::PathBuf};

use rho_protocol::{Plugin, PluginInstallSource, PluginScope, PluginSource, PluginTarget};
use serde::{Deserialize, Serialize};

use super::{apply::Unwritten, Action, Change, Copy, GroupKind, Planned, RevertRow, Row, RowKey};
use crate::{Daemon, Enablement, IfExists, PluginError};

/// A user plugin as an install can make it again on any machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct PluginCopy {
    origin: Origin,
    enabled: bool,
}

/// Where an install fetches a plugin from. The commit isn't part of it: an
/// install takes the ref's newest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Origin {
    Git {
        url: String,
        git_ref: Option<String>,
        path: Option<String>,
    },
    Link {
        path: PathBuf,
    },
}

impl PluginCopy {
    /// What `rho/plugins/install` takes to install it.
    pub(super) fn install_source(&self) -> Option<PluginInstallSource> {
        self.origin.install_source()
    }
}

/// The host whose links `rho plugins install` reads a ref and a folder
/// from, and the way rho records its repositories.
const GITHUB: &str = "https://github.com/";

impl Origin {
    /// What `rho/plugins/install` takes to install from here again: a
    /// remote of its default branch's root, or a GitHub link naming the ref
    /// and folder. `None` for a ref or folder of another host, which no
    /// install can name.
    fn install_source(&self) -> Option<PluginInstallSource> {
        match self {
            Self::Link { path } => Some(PluginInstallSource::Link { path: path.clone() }),
            Self::Git { url, git_ref, path } => {
                let url = match (git_ref, path) {
                    (None, None) => url.clone(),
                    (Some(git_ref), path) if url.starts_with(GITHUB) => {
                        let mut link = format!("{url}/tree/{}", encode(git_ref));
                        for segment in path.iter().flat_map(|path| path.split('/')) {
                            link.push('/');
                            link.push_str(&encode(segment));
                        }
                        link
                    }
                    _ => return None,
                };
                Some(PluginInstallSource::Git { url })
            }
        }
    }
}

/// Percent-encodes one URL path segment: everything but RFC 3986's
/// unreserved characters, as rho decodes a GitHub link's segments.
fn encode(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            // Writing to a `String` can't fail.
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// `plugin` as an install makes it again from git; `None` for one not
/// from git, or from a ref or folder no install can name.
fn git_copy(plugin: &Plugin) -> Option<PluginCopy> {
    let PluginSource::Git {
        url, git_ref, path, ..
    } = &plugin.source
    else {
        return None;
    };
    let copy = PluginCopy {
        origin: Origin::Git {
            url: url.clone(),
            git_ref: git_ref.clone(),
            path: path.clone(),
        },
        enabled: plugin.enabled,
    };
    copy.origin.install_source().map(|_| copy)
}

/// `plugin` of the source on `machine`, as an install makes it again on
/// the target, or why none can: only a git install syncs.
fn from_source(plugin: &Plugin, machine: &str) -> Result<PluginCopy, String> {
    git_copy(plugin).ok_or_else(|| match &plugin.source {
        PluginSource::Git { url, .. } => format!("From a ref of {url} no install can name"),
        PluginSource::Link { .. } => format!("Linked from a folder on {machine}"),
        PluginSource::Install => format!("Copied from a folder on {machine}"),
        PluginSource::Local => format!("Put in place by hand on {machine}"),
        _ => "Installed in a way rho-ui doesn't know".to_owned(),
    })
}

/// `plugin` of the target as Revert can install it again: from git, or a
/// link to the same folder. `None` for a copy of a folder.
fn restorable(plugin: &Plugin) -> Option<PluginCopy> {
    match &plugin.source {
        PluginSource::Link { target } => Some(PluginCopy {
            origin: Origin::Link {
                path: target.clone(),
            },
            enabled: plugin.enabled,
        }),
        _ => git_copy(plugin),
    }
}

/// Where a git install came from, for people: `host/repo/folder @ ref`.
fn shown_git(url: &str, git_ref: Option<&String>, path: Option<&String>) -> String {
    let mut shown = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .to_owned();
    if let Some(path) = path {
        shown.push('/');
        shown.push_str(path);
    }
    if let Some(git_ref) = git_ref {
        shown.push_str(" @ ");
        shown.push_str(git_ref);
    }
    shown
}

/// Whether a plugin is enabled, for people.
fn shown_state(enabled: Enablement) -> &'static str {
    match enabled {
        Enablement::Enabled => "enabled",
        Enablement::Disabled => "disabled",
    }
}

fn shown_copy(copy: &PluginCopy) -> String {
    let origin = match &copy.origin {
        Origin::Git { url, git_ref, path } => shown_git(url, git_ref.as_ref(), path.as_ref()),
        Origin::Link { path } => format!("linked to {}", path.display()),
    };
    format!("{origin} · {}", shown_state(copy.enabled.into()))
}

fn shown_plugin(plugin: &Plugin) -> String {
    let origin = match &plugin.source {
        PluginSource::Git {
            url, git_ref, path, ..
        } => shown_git(url, git_ref.as_ref(), path.as_ref()),
        PluginSource::Link { target } => format!("linked to {}", target.display()),
        PluginSource::Install => "copied from a folder".to_owned(),
        PluginSource::Local => "put in place by hand".to_owned(),
        _ => "installed some other way".to_owned(),
    };
    format!("{origin} · {}", shown_state(plugin.enabled.into()))
}

/// Whether `a` and `b` are the same install: the same source, but for the
/// commit of a git one, and the same enabled state.
fn same(a: &Plugin, b: &Plugin) -> bool {
    let source = |plugin: &Plugin| match &plugin.source {
        PluginSource::Git {
            url, git_ref, path, ..
        } => PluginSource::Git {
            url: url.clone(),
            git_ref: git_ref.clone(),
            path: path.clone(),
            commit: String::new(),
        },
        other => other.clone(),
    };
    a.enabled == b.enabled && source(a) == source(b)
}

/// The user's plugins of `plugins`.
fn user(plugins: &[Plugin]) -> impl Iterator<Item = &Plugin> {
    plugins
        .iter()
        .filter(|plugin| plugin.scope == PluginScope::User)
}

/// The plugins whose install or enabled state differs: the source's, by
/// name, then those only the target has.
pub(super) fn rows(from: &[Plugin], to: &[Plugin], source: &str, target: &str) -> Vec<Row> {
    let theirs = |name: &str| user(to).find(|plugin| plugin.name == name);
    let ours = user(from).filter_map(|plugin| {
        let there = theirs(&plugin.name);
        if there.is_some_and(|there| same(plugin, there)) {
            return None;
        }
        let action = Action::of(Some(plugin), there);
        let copy = match from_source(plugin, source) {
            Err(origin) => {
                let why =
                    format!("{origin}: install it on {target} from git, or link a folder there");
                return Some(row(plugin, there, action, Copy::Cannot(why)));
            }
            Ok(copy) => copy,
        };
        let before = match there {
            None => None,
            Some(there) => {
                let Some(theirs) = restorable(there) else {
                    let why = format!(
                        "{target}'s copy came from a folder: Revert couldn't bring it back"
                    );
                    return Some(row(plugin, Some(there), action, Copy::Cannot(why)));
                };
                Some(theirs)
            }
        };
        let change = Change::Plugin {
            name: plugin.name.clone(),
            before,
            after: Some(copy),
        };
        Some(row(plugin, there, action, Copy::Write(change)))
    });
    let only_theirs = user(to)
        .filter(|plugin| !user(from).any(|ours| ours.name == plugin.name))
        .map(|plugin| {
            let copy = match restorable(plugin) {
                Some(copy) => Copy::Write(Change::Plugin {
                    name: plugin.name.clone(),
                    before: Some(copy),
                    after: None,
                }),
                None => Copy::Cannot(format!(
                    "Copied from a folder on {target}: Revert couldn't bring it back"
                )),
            };
            Row {
                source: None,
                ..row(plugin, Some(plugin), Action::Remove, copy)
            }
        });
    ours.chain(only_theirs).collect()
}

/// The row of plugin `plugin`, which the target has as `there`.
fn row(plugin: &Plugin, there: Option<&Plugin>, action: Action, copy: Copy) -> Row {
    Row {
        key: RowKey {
            kind: GroupKind::Plugins,
            id: plugin.name.clone(),
        },
        label: plugin.name.clone(),
        source: Some(shown_plugin(plugin)),
        target: there.map(shown_plugin),
        action,
        one_machine: false,
        restart: false,
        note: None,
        copy,
    }
}

/// Writes one plugin row: installs it, changes whether it is enabled, or
/// removes it. An install starts enabled, so a disabled one is disabled
/// after; if that fails, what is left is to disable it.
pub(super) async fn write(
    target: &Daemon,
    name: &str,
    before: Option<&PluginCopy>,
    after: Option<&PluginCopy>,
) -> Result<(), Unwritten> {
    let plugin = || PluginTarget::new(name.to_owned(), PluginScope::User);
    let failed = |error: PluginError| Unwritten {
        reason: error.to_string(),
        rest: None,
    };
    match (before, after) {
        (_, None) => target
            .remove_plugin(plugin(), None)
            .await
            .map(drop)
            .map_err(failed),
        (Some(before), Some(after)) if before.origin == after.origin => target
            .set_plugin_enabled(plugin(), after.enabled.into(), None)
            .await
            .map(drop)
            .map_err(failed),
        (before, Some(after)) => {
            let Some(source) = after.install_source() else {
                return Err(Unwritten {
                    reason: "No install can name where this plugin came from".to_owned(),
                    rest: None,
                });
            };
            let if_exists = match before {
                Some(_) => IfExists::Replace,
                None => IfExists::Refuse,
            };
            target
                .install_plugin(source, PluginScope::User, if_exists, None)
                .await
                .map_err(failed)?;
            if after.enabled {
                return Ok(());
            }
            let disabled = target
                .set_plugin_enabled(plugin(), Enablement::Disabled, None)
                .await;
            disabled.map(drop).map_err(|error| Unwritten {
                reason: error.to_string(),
                rest: Some(Box::new(left_to_disable(name, after))),
            })
        }
    }
}

/// What is left of installing `after` as plugin `name` once the install
/// landed, enabled, but disabling it didn't.
pub(super) fn left_to_disable(name: &str, after: &PluginCopy) -> Change {
    Change::Plugin {
        name: name.to_owned(),
        before: Some(PluginCopy {
            enabled: true,
            ..after.clone()
        }),
        after: Some(after.clone()),
    }
}

/// What the target has of a plugin now, as Revert finds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Found {
    Absent,
    Copy(PluginCopy),
    /// A copy of a folder, which Revert can't replace.
    Other,
}

impl Found {
    /// The user's plugin `name` of `plugins`.
    pub(super) fn of(name: &str, plugins: &[Plugin]) -> Self {
        match user(plugins).find(|plugin| plugin.name == name) {
            None => Self::Absent,
            Some(plugin) => restorable(plugin).map_or(Self::Other, Self::Copy),
        }
    }
}

/// `item`'s row of a revert: unchanged while the target still has what the
/// sync installed, or still lacks what it removed.
pub(super) fn revert_row(
    item: &Planned,
    name: &str,
    before: Option<&PluginCopy>,
    after: Option<&PluginCopy>,
    found: &Found,
) -> RevertRow {
    let show =
        |copy: Option<&PluginCopy>| copy.map_or_else(|| "Not installed".to_owned(), shown_copy);
    let now = match found {
        Found::Other => {
            return RevertRow::blocked(
                item,
                [show(before), show(after)],
                "copied from a folder".to_owned(),
                "It's a copy of a folder now, which Revert won't replace".to_owned(),
            );
        }
        Found::Copy(copy) => Some(copy),
        Found::Absent => None,
    };
    let back = Change::Plugin {
        name: name.to_owned(),
        before: now.cloned(),
        after: before.cloned(),
    };
    RevertRow::of(item, [before, after, now], show, back)
}
