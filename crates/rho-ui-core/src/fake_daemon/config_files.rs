//! The fake daemon's `config.toml` tables (`rho/config/table/*`) and the
//! user's hooks file (`rho/hooks/file/*`): each its own revision, which a
//! write must send back, as rho's daemon checks them.

use std::{collections::BTreeMap, path::PathBuf};

use rho_protocol::{
    ConfigRevision, ConfigTable, ConfigTableGetResponse, ConfigTableSetParams, ErrorCode,
    HooksFileGetResponse, HooksFileSetParams, RpcError, TableChange, TableEntry,
};
use serde_json::Value;

/// One table: its entries in order, and how many writes it has had.
#[derive(Debug, Default)]
struct Table {
    entries: Vec<TableEntry>,
    writes: usize,
}

impl Table {
    fn revision(&self, table: ConfigTable) -> ConfigRevision {
        ConfigRevision::new(format!("{table:?}-{}", self.writes))
    }
}

/// The fake's tables and hooks file.
#[derive(Debug, Default)]
pub(super) struct FakeConfigFiles {
    tables: BTreeMap<String, Table>,
    hooks: Option<String>,
    hooks_writes: usize,
}

impl FakeConfigFiles {
    fn table(&mut self, table: ConfigTable) -> &mut Table {
        self.tables.entry(format!("{table:?}")).or_default()
    }

    /// Answers `rho/config/table/get`.
    pub(super) fn get_table(&mut self, table: ConfigTable) -> ConfigTableGetResponse {
        let read = self.table(table);
        ConfigTableGetResponse::new(read.revision(table), read.entries.clone())
    }

    /// Answers `rho/config/table/set`: every change, or none. A favorite
    /// that isn't `provider/model` is refused, as rho refuses it.
    pub(super) fn set_table(
        &mut self,
        params: ConfigTableSetParams,
    ) -> Result<ConfigRevision, RpcError> {
        let table = params.table;
        let read = self.table(table);
        if params.revision != read.revision(table) {
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
        Ok(self.write_table(table, entries))
    }

    /// Puts `key` of `table` as `value`, or removes it when `None`, as a
    /// hand edit of `config.toml` does.
    pub(super) fn edit_table(&mut self, table: ConfigTable, key: &str, value: Option<Value>) {
        let mut entries = self.table(table).entries.clone();
        entries.retain(|entry| entry.key != key);
        if let Some(value) = value {
            entries.push(TableEntry::new(key.to_owned(), value));
        }
        self.write_table(table, entries);
    }

    /// `key` of `table`, if it has it.
    pub(super) fn entry(&mut self, table: ConfigTable, key: &str) -> Option<Value> {
        self.table(table)
            .entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.clone())
    }

    fn write_table(&mut self, table: ConfigTable, entries: Vec<TableEntry>) -> ConfigRevision {
        let read = self.table(table);
        read.entries = entries;
        read.writes += 1;
        read.revision(table)
    }

    fn hooks_revision(&self) -> ConfigRevision {
        ConfigRevision::new(format!("hooks-{}", self.hooks_writes))
    }

    /// Answers `rho/hooks/file/get`.
    pub(super) fn get_hooks(&self) -> HooksFileGetResponse {
        HooksFileGetResponse::new(
            PathBuf::from("/home/fake/.rho/hooks.toml"),
            self.hooks_revision(),
            self.hooks.clone(),
        )
    }

    /// Answers `rho/hooks/file/set`. A hook whose program is under
    /// `/nonexistent/` is missing here, so the file is refused, as rho
    /// refuses a program named by a path the machine lacks.
    pub(super) fn set_hooks(
        &mut self,
        params: HooksFileSetParams,
    ) -> Result<ConfigRevision, RpcError> {
        if params.revision != self.hooks_revision() {
            return Err(RpcError::new(
                ErrorCode::ConfigConflict,
                "the hooks file changed since it was read",
            ));
        }
        if params
            .text
            .as_deref()
            .is_some_and(|text| text.contains("/nonexistent/"))
        {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "a hook's program under /nonexistent/ doesn't exist on this machine",
            ));
        }
        self.edit_hooks(params.text);
        Ok(self.hooks_revision())
    }

    /// Replaces the hooks file with `text`, or deletes it, as a hand edit
    /// does.
    pub(super) fn edit_hooks(&mut self, text: Option<String>) {
        self.hooks = text;
        self.hooks_writes += 1;
    }

    /// The hooks file's text.
    pub(super) fn hooks(&self) -> Option<String> {
        self.hooks.clone()
    }
}
