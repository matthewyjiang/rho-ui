//! The fake daemon's `config.toml` tables (`rho/config/table/*`) and the
//! user's hooks file (`rho/hooks/file/*`): each its own revision, a digest
//! of what it holds, which a write must send back, as rho's daemon checks
//! them.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
};

use rho_protocol::{
    ConfigRevision, ConfigTable, ConfigTableGetResponse, ConfigTableSetParams, ErrorCode,
    HooksFileGetResponse, HooksFileSetParams, RpcError, TableChange, TableEntry,
};
use serde_json::Value;

/// The revision of `content`: equal content, equal revision.
fn revision(content: &impl Hash) -> ConfigRevision {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    ConfigRevision::new(format!("{:x}", hasher.finish()))
}

/// One table's entries, in order.
#[derive(Debug, Default)]
struct Table {
    entries: Vec<TableEntry>,
}

impl Table {
    fn revision(&self) -> ConfigRevision {
        let content: Vec<(&str, String)> = self
            .entries
            .iter()
            .map(|entry| (entry.key.as_str(), entry.value.to_string()))
            .collect();
        revision(&content)
    }
}

/// The fake's tables and hooks file.
#[derive(Debug, Default)]
pub(super) struct FakeConfigFiles {
    tables: HashMap<ConfigTable, Table>,
    hooks: Option<String>,
    /// The programs under `/nonexistent/` are there after all.
    programs_present: bool,
}

impl FakeConfigFiles {
    fn table(&mut self, table: ConfigTable) -> &mut Table {
        self.tables.entry(table).or_default()
    }

    /// Answers `rho/config/table/get`.
    pub(super) fn get_table(&mut self, table: ConfigTable) -> ConfigTableGetResponse {
        let read = self.table(table);
        ConfigTableGetResponse::new(read.revision(), read.entries.clone())
    }

    /// Answers `rho/config/table/set`: every change, or none. A favorite
    /// that isn't `provider/model` is refused, as rho refuses it.
    pub(super) fn set_table(
        &mut self,
        params: ConfigTableSetParams,
    ) -> Result<ConfigRevision, RpcError> {
        let table = params.table;
        let read = self.table(table);
        if params.revision != read.revision() {
            return Err(RpcError::new(
                ErrorCode::ConfigConflict,
                "the table changed since it was read",
            ));
        }
        let mut entries = read.entries.clone();
        for change in params.changes {
            match change {
                TableChange::Put { key, .. }
                    if table == ConfigTable::FavoriteModels && !key.contains('/') =>
                {
                    return Err(RpcError::new(
                        ErrorCode::InvalidParams,
                        format!("favorite {key} isn't provider/model"),
                    ));
                }
                TableChange::Put { key, value } => {
                    match entries.iter_mut().find(|entry| entry.key == key) {
                        Some(entry) => entry.value = value,
                        None => entries.push(TableEntry::new(key, value)),
                    }
                }
                TableChange::Remove { key } => {
                    let before = entries.len();
                    entries.retain(|entry| entry.key != key);
                    if entries.len() == before {
                        return Err(RpcError::new(
                            ErrorCode::InvalidParams,
                            format!("there is no entry {key}"),
                        ));
                    }
                }
            }
        }
        let read = self.table(table);
        read.entries = entries;
        Ok(read.revision())
    }

    /// Puts `key` of `table` as `value`, or removes it when `None`, as a
    /// hand edit of `config.toml` does.
    pub(super) fn edit_table(&mut self, table: ConfigTable, key: &str, value: Option<Value>) {
        let entries = &mut self.table(table).entries;
        match (entries.iter_mut().find(|entry| entry.key == key), value) {
            (Some(entry), Some(value)) => entry.value = value,
            (None, Some(value)) => entries.push(TableEntry::new(key.to_owned(), value)),
            (_, None) => entries.retain(|entry| entry.key != key),
        }
    }

    /// `key` of `table`, if it has it.
    pub(super) fn entry(&mut self, table: ConfigTable, key: &str) -> Option<Value> {
        self.table(table)
            .entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.clone())
    }

    /// Answers `rho/hooks/file/get`.
    pub(super) fn get_hooks(&self) -> HooksFileGetResponse {
        HooksFileGetResponse::new(
            PathBuf::from("/home/fake/.rho/hooks.toml"),
            revision(&self.hooks),
            self.hooks.clone(),
        )
    }

    /// Answers `rho/hooks/file/set`. A hook whose program is under
    /// `/nonexistent/` is missing here, until [`Self::add_programs`], so
    /// the file is refused, as rho refuses a program named by a path the
    /// machine lacks; as there, the text is checked before the revision.
    pub(super) fn set_hooks(
        &mut self,
        params: HooksFileSetParams,
    ) -> Result<ConfigRevision, RpcError> {
        if params
            .text
            .as_deref()
            .is_some_and(|text| !self.programs_present && text.contains("/nonexistent/"))
        {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "a hook's program under /nonexistent/ doesn't exist on this machine",
            ));
        }
        if params.revision != revision(&self.hooks) {
            return Err(RpcError::new(
                ErrorCode::ConfigConflict,
                "the hooks file changed since it was read",
            ));
        }
        self.hooks = params.text;
        Ok(revision(&self.hooks))
    }

    /// Replaces the hooks file with `text`, or deletes it, as a hand edit
    /// does.
    pub(super) fn edit_hooks(&mut self, text: Option<String>) {
        self.hooks = text;
    }

    /// Puts the programs under `/nonexistent/` in place.
    pub(super) fn add_programs(&mut self) {
        self.programs_present = true;
    }

    /// The hooks file's text.
    pub(super) fn hooks(&self) -> Option<String> {
        self.hooks.clone()
    }
}
