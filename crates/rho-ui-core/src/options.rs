//! A session's settings as the UI shows them: generic options built from the
//! daemon's `SessionOptions`, and the change a pick turns back into.

use rho_protocol::{
    ModelRef, PermissionMode, ReasoningLevel, SessionChange, SessionCommand, SessionOptions,
};

const MODE_ID: &str = "mode";
const MODEL_ID: &str = "model";
const REASONING_ID: &str = "thought_level";
/// The ID of the fast mode toggle, so a host can give it its own control.
pub const FAST_MODE_ID: &str = "fast_mode";

/// Every permission mode this UI can show: its wire value and its label.
const MODES: [(PermissionMode, &str, &str); 5] = [
    (PermissionMode::Bypass, "bypass", "Bypass"),
    (PermissionMode::Auto, "auto", "Auto"),
    (PermissionMode::AllowEdits, "allow_edits", "Allow edits"),
    (PermissionMode::Plan, "plan", "Plan"),
    (PermissionMode::Supervised, "supervised", "Supervised"),
];

/// Every reasoning level this UI can show: its wire value and its label.
const REASONING_LEVELS: [(ReasoningLevel, &str, &str); 7] = [
    (ReasoningLevel::Off, "off", "Off"),
    (ReasoningLevel::Minimal, "minimal", "Minimal"),
    (ReasoningLevel::Low, "low", "Low"),
    (ReasoningLevel::Medium, "medium", "Medium"),
    (ReasoningLevel::High, "high", "High"),
    (ReasoningLevel::Xhigh, "xhigh", "Extra high"),
    (ReasoningLevel::Max, "max", "Max"),
];

/// A value to set with [`crate::Daemon::set_config`].
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigValue {
    /// A [`ConfigChoice::value`] for a [`ConfigKind::Select`] option.
    Select(String),
    /// The new state of a [`ConfigKind::Toggle`] option.
    Toggle(bool),
}

/// One choice in a [`ConfigKind::Select`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigChoice {
    value: String,
    name: String,
}

impl ConfigChoice {
    /// Pass to [`crate::Daemon::set_config`] as [`ConfigValue::Select`].
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Label to show.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The shape of a [`ConfigOption`] and its current value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigKind {
    /// Pick one of several values.
    Select {
        /// The selected [`ConfigChoice::value`].
        current: String,
        /// Every choice, in the daemon's order.
        choices: Vec<ConfigChoice>,
    },
    /// On or off.
    Toggle(bool),
}

/// What a [`ConfigOption`] sets, so a host can give it its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigCategory {
    /// The permission mode: which tool calls run without asking.
    Mode,
    /// Which model answers.
    Model,
    /// How much the model reasons per turn.
    ThoughtLevel,
    /// A model setting with no place of its own, such as fast mode.
    Other,
}

/// A per-session setting rho exposes, such as mode or model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigOption {
    id: &'static str,
    name: &'static str,
    category: ConfigCategory,
    kind: ConfigKind,
}

impl ConfigOption {
    /// Stable ID: `mode`, `model`, `thought_level`, or [`FAST_MODE_ID`].
    pub fn id(&self) -> &str {
        self.id
    }

    /// Label to show, such as "Model".
    pub fn name(&self) -> &str {
        self.name
    }

    /// What it sets.
    pub fn category(&self) -> ConfigCategory {
        self.category
    }

    /// Its shape and current value.
    pub fn kind(&self) -> &ConfigKind {
        &self.kind
    }

    /// The choice a [`ConfigKind::Select`] has selected.
    pub fn current_choice(&self) -> Option<&ConfigChoice> {
        match &self.kind {
            ConfigKind::Select { current, choices } => {
                choices.iter().find(|choice| choice.value == *current)
            }
            ConfigKind::Toggle(_) => None,
        }
    }

    /// What it is set to, as [`crate::Daemon::set_config`] takes it.
    pub fn value(&self) -> ConfigValue {
        match &self.kind {
            ConfigKind::Select { current, .. } => ConfigValue::Select(current.clone()),
            ConfigKind::Toggle(on) => ConfigValue::Toggle(*on),
        }
    }

    /// This option holding `value`, to show a change before the daemon
    /// confirms it. A value of the other kind changes nothing.
    #[must_use]
    pub fn with_value(mut self, value: &ConfigValue) -> Self {
        match (&mut self.kind, value) {
            (ConfigKind::Select { current, .. }, ConfigValue::Select(value)) => {
                current.clone_from(value);
            }
            (ConfigKind::Toggle(on), ConfigValue::Toggle(value)) => *on = *value,
            _ => {}
        }
        self
    }
}

/// A slash command or skill the session offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInfo {
    name: String,
    description: String,
}

impl CommandInfo {
    /// The command's name, such as `compact`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// One line on what it does.
    pub fn description(&self) -> &str {
        &self.description
    }
}

pub(crate) fn commands(list: Vec<SessionCommand>) -> Vec<CommandInfo> {
    list.into_iter()
        .map(|command| CommandInfo {
            name: command.name,
            description: command.description,
        })
        .collect()
}

/// `provider/model`: how a model is named in a [`ConfigChoice::value`]. A
/// provider id holds no `/`, so [`model_ref`] splits at the first one.
fn model_value(model: &ModelRef) -> String {
    format!("{}/{}", model.provider, model.model)
}

fn model_ref(value: &str) -> Option<ModelRef> {
    let (provider, model) = value.split_once('/')?;
    Some(ModelRef {
        provider: provider.to_owned(),
        model: model.to_owned(),
    })
}

/// A select over the rows of `table` the daemon lists in `available`. A
/// value this UI has no row for, such as one a newer daemon added, is left
/// out; the option itself is dropped when the current value has none.
fn select<T: Copy + PartialEq>(
    id: &'static str,
    name: &'static str,
    category: ConfigCategory,
    table: &[(T, &str, &str)],
    current: T,
    available: &[T],
) -> Option<ConfigOption> {
    let row = |value: T| table.iter().find(|(known, _, _)| *known == value);
    let (_, current, _) = row(current)?;
    let choices = available
        .iter()
        .filter_map(|value| row(*value))
        .map(|(_, value, name)| ConfigChoice {
            value: (*value).to_owned(),
            name: (*name).to_owned(),
        })
        .collect();
    Some(ConfigOption {
        id,
        name,
        category,
        kind: ConfigKind::Select {
            current: (*current).to_owned(),
            choices,
        },
    })
}

/// The settings in `options`, in the order the UI lays them out.
pub(crate) fn config_options(options: &SessionOptions) -> Vec<ConfigOption> {
    let mode = select(
        MODE_ID,
        "Mode",
        ConfigCategory::Mode,
        &MODES,
        options.permission_mode.current,
        &options.permission_mode.available,
    );
    let model = ConfigOption {
        id: MODEL_ID,
        name: "Model",
        category: ConfigCategory::Model,
        kind: ConfigKind::Select {
            current: model_value(&options.model.current),
            choices: options
                .model
                .available
                .iter()
                .map(|choice| ConfigChoice {
                    value: model_value(&choice.model),
                    name: choice.label.clone(),
                })
                .collect(),
        },
    };
    let reasoning = options.reasoning.as_ref().and_then(|reasoning| {
        select(
            REASONING_ID,
            "Reasoning",
            ConfigCategory::ThoughtLevel,
            &REASONING_LEVELS,
            reasoning.current,
            &reasoning.available,
        )
    });
    let fast_mode = options.fast_mode.as_ref().map(|fast| ConfigOption {
        id: FAST_MODE_ID,
        name: "Fast mode",
        category: ConfigCategory::Other,
        kind: ConfigKind::Toggle(fast.enabled),
    });
    mode.into_iter()
        .chain([model])
        .chain(reasoning)
        .chain(fast_mode)
        .collect()
}

/// The change that sets option `config_id` to `value`, or `None` when no
/// option of that ID takes such a value.
pub(crate) fn change(config_id: &str, value: &ConfigValue) -> Option<SessionChange> {
    fn known<T: Copy>(table: &[(T, &str, &str)], value: &str) -> Option<T> {
        table
            .iter()
            .find(|(_, known, _)| *known == value)
            .map(|(value, _, _)| *value)
    }
    match (config_id, value) {
        (MODE_ID, ConfigValue::Select(value)) => {
            let mode = known(&MODES, value)?;
            Some(SessionChange::PermissionMode { mode })
        }
        (MODEL_ID, ConfigValue::Select(value)) => Some(SessionChange::Model {
            model: model_ref(value)?,
            auth: None,
            setup: false,
        }),
        (REASONING_ID, ConfigValue::Select(value)) => {
            let level = known(&REASONING_LEVELS, value)?;
            Some(SessionChange::Reasoning { level })
        }
        (FAST_MODE_ID, ConfigValue::Toggle(enabled)) => {
            Some(SessionChange::FastMode { enabled: *enabled })
        }
        _ => None,
    }
}

/// The label for `mode`, for an approval that offers it.
pub(crate) fn mode_label(mode: PermissionMode) -> Option<(&'static str, &'static str)> {
    MODES
        .iter()
        .find(|(known, _, _)| *known == mode)
        .map(|(_, value, name)| (*value, *name))
}
