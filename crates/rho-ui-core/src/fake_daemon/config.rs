//! The fake daemon's agent settings: [`agent_schema`], their values, and
//! the writes `rho/config/set` makes. Each write publishes the whole new
//! state to a watch, which rho-protocol's server turns into
//! `rho/config/changed` for every client that read the settings.

use std::{collections::BTreeMap, sync::Arc};

use rho_protocol::{
    Applies, ConfigCategory, ConfigGetResponse, ConfigNamespace, ConfigRevision, ConfigSchema,
    ConfigSetParams, ConfigSetResponse, ConfigWatch, ConfigWriter, ErrorCode, IntegerUnit,
    ModelRuntime, ModelSelection, RpcError, SettingChange, SettingChoice, SettingEntry, SettingKey,
    SettingKind, SettingSpec, SettingValue, ValueSource,
};
use tokio::sync::watch;

/// One setting of [`agent_schema`]: `place` is its category and section,
/// `text` its label and help.
fn spec(
    key: &str,
    kind: SettingKind,
    default: SettingValue,
    (category, section): (&str, Option<&str>),
    (label, help): (&str, &str),
    applies: Applies,
) -> SettingSpec {
    SettingSpec {
        key: SettingKey::new(key),
        kind,
        default: Some(default),
        category: category.to_owned(),
        section: section.map(str::to_owned),
        label: label.to_owned(),
        help: help.to_owned(),
        applies,
        overridable_per_project: true,
    }
}

/// rho's own settings as the fake describes them: two pages, with every
/// kind of setting a page shows.
pub(super) fn agent_schema() -> ConfigSchema {
    let choice = |value: &str, label: &str| SettingChoice {
        value: value.to_owned(),
        label: label.to_owned(),
        help: None,
    };
    let general = ("general", None);
    let tools = ("tools", None);
    ConfigSchema {
        categories: vec![
            ConfigCategory {
                id: "general".to_owned(),
                label: "General".to_owned(),
            },
            ConfigCategory {
                id: "tools".to_owned(),
                label: "Tools".to_owned(),
            },
        ],
        settings: vec![
            spec(
                "general.auto_compact",
                SettingKind::Bool,
                SettingValue::Bool(true),
                general,
                (
                    "Auto-compact",
                    "Compact the conversation when the context window fills.",
                ),
                Applies::NextTurn,
            ),
            spec(
                "general.permission_mode",
                SettingKind::Choice {
                    choices: vec![
                        choice("auto", "Auto"),
                        choice("plan", "Plan"),
                        choice("supervised", "Supervised"),
                    ],
                },
                SettingValue::Text("auto".to_owned()),
                general,
                ("Permission mode", "Which tool calls run without asking."),
                Applies::NextSession,
            ),
            spec(
                "general.title_model",
                SettingKind::Model {
                    runtimes: vec![ModelRuntime::Rho],
                },
                SettingValue::Model(ModelSelection::Rho {
                    provider: "fake".to_owned(),
                    model: "small".to_owned(),
                }),
                ("general", Some("Models")),
                ("Title model", "The model that names sessions."),
                Applies::Immediately,
            ),
            spec(
                "tools.shell_timeout",
                SettingKind::Integer {
                    min: 1,
                    max: Some(3600),
                    unit: Some(IntegerUnit::Seconds),
                },
                SettingValue::Integer(120),
                tools,
                ("Shell timeout", "How long a shell command may run."),
                Applies::NextTurn,
            ),
            spec(
                "tools.shell",
                SettingKind::Text,
                SettingValue::Text("bash".to_owned()),
                tools,
                ("Shell", "The shell commands run in."),
                Applies::Restart,
            ),
        ],
        file_only: Vec::new(),
    }
}

/// The fake's agent settings and who wrote them.
#[derive(Debug)]
pub(super) struct FakeConfig {
    schema: ConfigSchema,
    /// The current state, which every client that read the settings
    /// follows.
    state: watch::Sender<Arc<ConfigGetResponse>>,
    /// Who made each write, in order: write `n` made revision `rn`.
    writers: Vec<Option<ConfigWriter>>,
}

impl FakeConfig {
    /// Every setting at its default, at revision `r0`.
    pub(super) fn new() -> Self {
        let schema = agent_schema();
        let values = schema
            .settings
            .iter()
            .map(|spec| {
                let entry = SettingEntry {
                    value: spec.default.clone(),
                    source: ValueSource::Default,
                };
                (spec.key.clone(), entry)
            })
            .collect();
        let (state, _) = watch::channel(Arc::new(ConfigGetResponse {
            revision: ConfigRevision::new("r0"),
            values,
            errors: Vec::new(),
        }));
        Self {
            schema,
            state,
            writers: Vec::new(),
        }
    }

    pub(super) fn schema(&self) -> ConfigSchema {
        self.schema.clone()
    }

    /// The current state, and every later one, for `rho/config/get`.
    pub(super) fn watch(&self, namespace: &ConfigNamespace) -> Result<ConfigWatch, RpcError> {
        if *namespace != ConfigNamespace::Agent {
            return Err(only_agent());
        }
        Ok(ConfigWatch::new(self.state.subscribe()))
    }

    pub(super) fn value(&self, key: &str) -> Option<SettingValue> {
        self.state
            .borrow()
            .values
            .get(&SettingKey::new(key))?
            .value
            .clone()
    }

    /// Answers `rho/config/set`: every change, or none.
    pub(super) fn set(&mut self, params: ConfigSetParams) -> Result<ConfigSetResponse, RpcError> {
        if params.namespace != ConfigNamespace::Agent {
            return Err(only_agent());
        }
        if !self.is_good(&params.revision, params.writer.as_ref()) {
            return Err(RpcError::new(
                ErrorCode::ConfigConflict,
                "the settings changed since they were read",
            ));
        }
        let mut values = self.state.borrow().values.clone();
        for change in params.changes {
            let Some(spec) = self
                .schema
                .settings
                .iter()
                .find(|spec| spec.key == *change.key())
            else {
                return Err(RpcError::new(
                    ErrorCode::InvalidParams,
                    format!("there is no setting {}", change.key()),
                ));
            };
            let entry = match change {
                SettingChange::Set { value, .. } => SettingEntry {
                    value: Some(value),
                    source: ValueSource::Global,
                },
                SettingChange::Reset { .. } => SettingEntry {
                    value: spec.default.clone(),
                    source: ValueSource::Default,
                },
            };
            values.insert(spec.key.clone(), entry);
        }
        let revision = self.write(values, params.writer);
        Ok(ConfigSetResponse { revision })
    }

    /// Sets `key` to `value` as a hand edit of the file does.
    pub(super) fn edit(&mut self, key: &str, value: SettingValue) {
        let mut values = self.state.borrow().values.clone();
        let entry = SettingEntry {
            value: Some(value),
            source: ValueSource::Global,
        };
        values.insert(SettingKey::new(key), entry);
        self.write(values, None);
    }

    /// Publishes `values` as the next revision, written by `writer`.
    fn write(
        &mut self,
        values: BTreeMap<SettingKey, SettingEntry>,
        writer: Option<ConfigWriter>,
    ) -> ConfigRevision {
        self.writers.push(writer);
        let revision = ConfigRevision::new(format!("r{}", self.writers.len()));
        self.state.send_replace(Arc::new(ConfigGetResponse {
            revision: revision.clone(),
            values,
            errors: Vec::new(),
        }));
        revision
    }

    /// Whether a write by `writer` against `revision` may land, as rho's
    /// daemon decides: `revision` is the current one, or only `writer` has
    /// written since.
    fn is_good(&self, revision: &ConfigRevision, writer: Option<&ConfigWriter>) -> bool {
        let Some(since) = revision
            .as_str()
            .strip_prefix('r')
            .and_then(|n| n.parse::<usize>().ok())
        else {
            return false;
        };
        self.writers
            .get(since..)
            .is_some_and(|later| later.iter().all(|by| by.is_some() && by.as_ref() == writer))
    }
}

fn only_agent() -> RpcError {
    RpcError::new(
        ErrorCode::InvalidParams,
        "the fake daemon has only the agent's settings",
    )
}
