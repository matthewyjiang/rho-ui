//! rho's own settings, the agent's `config.toml`, as its daemon describes
//! them: each setting's kind, label, help and page from `rho/config/schema`,
//! and its value from `rho/config/get`, kept current by
//! `rho/config/changed`. A settings screen draws itself from this, so a
//! setting rho adds shows up with no change here.
//!
//! ```ignore
//! let mut settings = daemon.agent_settings().await?;
//! let change = SettingChange::Set { key, value: SettingValue::Bool(true) };
//! settings.change(&change);
//! daemon.set_agent_settings(settings.revision().clone(), vec![change]).await?;
//! // Later, from the event stream:
//! settings.apply(changed);
//! ```

use std::collections::BTreeMap;

pub use rho_protocol::{
    Applies, ConfigCategory, ConfigError, ConfigRevision, IntegerUnit, ModelRuntime,
    ModelSelection, SettingChange, SettingChoice, SettingKey, SettingKind, SettingSpec,
    SettingValue, ValueSource,
};
use rho_protocol::{ConfigChangedParams, ConfigGetResponse, ConfigSchema, SettingEntry};

/// One daemon's agent settings: what each one is, its value and where that
/// comes from, and what keeps the file from loading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSettings {
    schema: ConfigSchema,
    revision: ConfigRevision,
    values: BTreeMap<SettingKey, SettingEntry>,
    errors: Vec<ConfigError>,
}

/// A change to a daemon's agent settings, by any client or a hand edit of
/// the file: [`crate::DaemonEvent::AgentSettingsChanged`]. Apply it with
/// [`AgentSettings::apply`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSettingsChanged(pub(crate) ConfigChangedParams);

impl AgentSettings {
    pub(crate) fn new(schema: ConfigSchema, read: ConfigGetResponse) -> Self {
        Self {
            schema,
            revision: read.revision,
            values: read.values,
            errors: read.errors,
        }
    }

    /// The pages the settings sit on, in the daemon's order.
    pub fn categories(&self) -> &[ConfigCategory] {
        &self.schema.categories
    }

    /// The settings on page `category`, in the daemon's order.
    pub fn settings<'a>(&'a self, category: &'a str) -> impl Iterator<Item = &'a SettingSpec> {
        self.schema
            .settings
            .iter()
            .filter(move |spec| spec.category == category)
    }

    /// The value `key` has now: `None` for an optional setting that is
    /// unset.
    pub fn value(&self, key: &SettingKey) -> Option<&SettingValue> {
        match self.values.get(key) {
            Some(entry) => entry.value.as_ref(),
            // The daemon lists every setting it describes; one it left out
            // has its default.
            None => self.spec(key)?.default.as_ref(),
        }
    }

    /// Whether the user's file sets `key`, so a reset would change it.
    pub fn is_set(&self, key: &SettingKey) -> bool {
        self.values
            .get(key)
            .is_some_and(|entry| entry.source == ValueSource::Global)
    }

    /// What keeps the file from loading. While there is any, the values are
    /// the last ones that loaded, or the defaults.
    pub fn errors(&self) -> &[ConfigError] {
        &self.errors
    }

    /// The state the values are of: send it with the next
    /// [`crate::Daemon::set_agent_settings`].
    pub fn revision(&self) -> &ConfigRevision {
        &self.revision
    }

    /// Takes in a change the daemon sent: the entries it names, and the
    /// whole list of problems.
    pub fn apply(&mut self, changed: AgentSettingsChanged) {
        let AgentSettingsChanged(changed) = changed;
        self.revision = changed.revision;
        self.values.extend(changed.values);
        self.errors = changed.errors;
    }

    /// Shows `change` before the daemon confirms it, so a control follows a
    /// click at once. The daemon's own [`AgentSettingsChanged`] follows; if
    /// it refuses the write instead, read the settings again.
    pub fn change(&mut self, change: &SettingChange) {
        let entry = match change {
            SettingChange::Set { value, .. } => SettingEntry {
                value: Some(value.clone()),
                source: ValueSource::Global,
            },
            SettingChange::Reset { key } => SettingEntry {
                value: self.spec(key).and_then(|spec| spec.default.clone()),
                source: ValueSource::Default,
            },
        };
        self.values.insert(change.key().clone(), entry);
    }

    /// What setting `key` is, if the daemon describes it.
    pub fn spec(&self, key: &SettingKey) -> Option<&SettingSpec> {
        self.schema.settings.iter().find(|spec| spec.key == *key)
    }
}
