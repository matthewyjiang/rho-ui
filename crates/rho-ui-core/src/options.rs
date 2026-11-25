//! A session's typed settings built from the daemon's `SessionOptions`, and
//! the typed changes sent back with [`crate::Daemon::set_option`]; and what
//! the sessions it starts open with, changed with
//! [`crate::Daemon::set_started_session_default`].

use std::fmt;

use rho_protocol::SessionCommand;

/// Which tool calls a session runs without asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PermissionMode {
    /// Run every tool call without asking.
    Bypass,
    /// Decide automatically which tool calls need approval.
    Auto,
    /// Allow file edits without asking; ask for other tool calls.
    AllowEdits,
    /// Plan changes without modifying files or running side-effecting tools.
    Plan,
    /// Ask before tool calls that change state.
    Supervised,
}

impl PermissionMode {
    /// Label to show in controls.
    pub fn label(self) -> &'static str {
        match self {
            Self::Bypass => "Bypass",
            Self::Auto => "Auto",
            Self::AllowEdits => "Allow edits",
            Self::Plan => "Plan",
            Self::Supervised => "Supervised",
        }
    }

    pub(crate) fn slug(self) -> &'static str {
        match self {
            Self::Bypass => "bypass",
            Self::Auto => "auto",
            Self::AllowEdits => "allow_edits",
            Self::Plan => "plan",
            Self::Supervised => "supervised",
        }
    }

    pub(crate) fn from_wire(mode: rho_protocol::PermissionMode) -> Option<Self> {
        match mode {
            rho_protocol::PermissionMode::Bypass => Some(Self::Bypass),
            rho_protocol::PermissionMode::Auto => Some(Self::Auto),
            rho_protocol::PermissionMode::AllowEdits => Some(Self::AllowEdits),
            rho_protocol::PermissionMode::Plan => Some(Self::Plan),
            rho_protocol::PermissionMode::Supervised => Some(Self::Supervised),
            _ => None,
        }
    }

    pub(crate) fn to_wire(self) -> rho_protocol::PermissionMode {
        match self {
            Self::Bypass => rho_protocol::PermissionMode::Bypass,
            Self::Auto => rho_protocol::PermissionMode::Auto,
            Self::AllowEdits => rho_protocol::PermissionMode::AllowEdits,
            Self::Plan => rho_protocol::PermissionMode::Plan,
            Self::Supervised => rho_protocol::PermissionMode::Supervised,
        }
    }
}

/// How much the model reasons on each turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReasoningLevel {
    /// No extended reasoning.
    Off,
    /// Minimal reasoning budget.
    Minimal,
    /// Low reasoning budget.
    Low,
    /// Medium reasoning budget.
    Medium,
    /// High reasoning budget.
    High,
    /// Extra-high reasoning budget.
    Xhigh,
    /// Maximum reasoning budget.
    Max,
}

impl ReasoningLevel {
    /// Label to show in controls.
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Minimal => "Minimal",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Xhigh => "Extra high",
            Self::Max => "Max",
        }
    }

    pub(crate) fn from_wire(level: rho_protocol::ReasoningLevel) -> Option<Self> {
        match level {
            rho_protocol::ReasoningLevel::Off => Some(Self::Off),
            rho_protocol::ReasoningLevel::Minimal => Some(Self::Minimal),
            rho_protocol::ReasoningLevel::Low => Some(Self::Low),
            rho_protocol::ReasoningLevel::Medium => Some(Self::Medium),
            rho_protocol::ReasoningLevel::High => Some(Self::High),
            rho_protocol::ReasoningLevel::Xhigh => Some(Self::Xhigh),
            rho_protocol::ReasoningLevel::Max => Some(Self::Max),
            _ => None,
        }
    }

    pub(crate) fn to_wire(self) -> rho_protocol::ReasoningLevel {
        match self {
            Self::Off => rho_protocol::ReasoningLevel::Off,
            Self::Minimal => rho_protocol::ReasoningLevel::Minimal,
            Self::Low => rho_protocol::ReasoningLevel::Low,
            Self::Medium => rho_protocol::ReasoningLevel::Medium,
            Self::High => rho_protocol::ReasoningLevel::High,
            Self::Xhigh => rho_protocol::ReasoningLevel::Xhigh,
            Self::Max => rho_protocol::ReasoningLevel::Max,
        }
    }
}

/// A setting with its current value and the choices the session allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choices<T> {
    current: T,
    available: Vec<T>,
}

impl<T: Copy> Choices<T> {
    /// The value in use now.
    pub fn current(&self) -> T {
        self.current
    }

    /// Every choice the session allows, in the daemon's order.
    pub fn available(&self) -> &[T] {
        &self.available
    }
}

/// One model a session can run on, named by its provider and model ID.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelRef {
    provider: String,
    model: String,
}

impl ModelRef {
    /// Names a model by its provider ID and the provider's model ID.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }

    /// The provider ID, such as `anthropic`.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// The model ID within its provider.
    pub fn model(&self) -> &str {
        &self.model
    }

    fn from_wire(model: &rho_protocol::ModelRef) -> Self {
        Self {
            provider: model.provider.clone(),
            model: model.model.clone(),
        }
    }

    fn into_wire(self) -> rho_protocol::ModelRef {
        rho_protocol::ModelRef {
            provider: self.provider,
            model: self.model,
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

/// One model a session can switch to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    model: ModelRef,
    label: String,
}

impl ModelChoice {
    /// The model's provider and model ID, for [`OptionChange::Model`].
    pub fn model(&self) -> &ModelRef {
        &self.model
    }

    /// Short display label, unique within [`ModelOptions::available`].
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// The session's model, its billing auth profile, and the models it can
/// switch to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOptions {
    current: ModelRef,
    auth: String,
    available: Vec<ModelChoice>,
}

impl ModelOptions {
    /// The model answering turns now.
    pub fn current(&self) -> &ModelRef {
        &self.current
    }

    /// The entry in [`Self::available`] matching [`Self::current`].
    pub fn current_choice(&self) -> Option<&ModelChoice> {
        self.available
            .iter()
            .find(|choice| choice.model == self.current)
    }

    /// The auth profile [`Self::current`] runs on, such as `openai-codex`,
    /// which names how its use is billed.
    pub fn auth(&self) -> &str {
        &self.auth
    }

    /// Every model the session can switch to, favorites first.
    pub fn available(&self) -> &[ModelChoice] {
        &self.available
    }
}

/// A session's configurable settings: permission mode, model, reasoning, and
/// fast mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionOptions {
    permission_mode: Option<Choices<PermissionMode>>,
    model: ModelOptions,
    reasoning: Option<Choices<ReasoningLevel>>,
    fast_mode: Option<bool>,
}

impl SessionOptions {
    /// The permission mode and the modes the session allows, or `None` when
    /// the current mode is one a newer daemon added.
    pub fn permission_mode(&self) -> Option<&Choices<PermissionMode>> {
        self.permission_mode.as_ref()
    }

    /// The current model, its billing auth profile, and the models on offer.
    pub fn model(&self) -> &ModelOptions {
        &self.model
    }

    /// The reasoning level and the levels the current model allows, or `None`
    /// when the model has no configurable reasoning (or uses a level a newer
    /// daemon added).
    pub fn reasoning(&self) -> Option<&Choices<ReasoningLevel>> {
        self.reasoning.as_ref()
    }

    /// Fast mode's current state, or `None` when the current model has none.
    pub fn fast_mode(&self) -> Option<bool> {
        self.fast_mode
    }

    /// Shows `change` in place, before the daemon confirms it. A change for a
    /// setting this session does not offer leaves it as it was.
    pub fn apply_change(&mut self, change: &OptionChange) {
        match change {
            OptionChange::PermissionMode(mode) => {
                if let Some(modes) = &mut self.permission_mode {
                    modes.current = *mode;
                }
            }
            OptionChange::Model(model) => {
                self.model.current.clone_from(model);
            }
            OptionChange::Reasoning(level) => {
                if let Some(reasoning) = &mut self.reasoning {
                    reasoning.current = *level;
                }
            }
            OptionChange::FastMode(enabled) => {
                if let Some(fast) = &mut self.fast_mode {
                    *fast = *enabled;
                }
            }
        }
    }

    pub(crate) fn from_wire(options: &rho_protocol::SessionOptions) -> Self {
        let permission_mode = choices(
            options.permission_mode.current,
            &options.permission_mode.available,
            PermissionMode::from_wire,
        );
        let model = ModelOptions {
            current: ModelRef::from_wire(&options.model.current),
            auth: options.model.auth.clone(),
            available: options
                .model
                .available
                .iter()
                .map(|choice| ModelChoice {
                    model: ModelRef::from_wire(&choice.model),
                    label: choice.label.clone(),
                })
                .collect(),
        };
        let reasoning = options.reasoning.as_ref().and_then(|reasoning| {
            choices(
                reasoning.current,
                &reasoning.available,
                ReasoningLevel::from_wire,
            )
        });
        let fast_mode = options.fast_mode.as_ref().map(|fast| fast.enabled);
        Self {
            permission_mode,
            model,
            reasoning,
            fast_mode,
        }
    }
}

/// One change to a session's settings, sent with [`crate::Daemon::set_option`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OptionChange {
    /// Gate later turns' tool calls with this mode.
    PermissionMode(PermissionMode),
    /// Switch the session's model.
    Model(ModelRef),
    /// Spend this much reasoning on later turns.
    Reasoning(ReasoningLevel),
    /// Turn fast mode on or off.
    FastMode(bool),
}

impl OptionChange {
    pub(crate) fn into_wire(self) -> rho_protocol::SessionChange {
        match self {
            Self::PermissionMode(mode) => rho_protocol::SessionChange::PermissionMode {
                mode: mode.to_wire(),
            },
            Self::Model(model) => rho_protocol::SessionChange::Model {
                model: model.into_wire(),
                auth: None,
                setup: false,
            },
            Self::Reasoning(level) => rho_protocol::SessionChange::Reasoning {
                level: level.to_wire(),
            },
            Self::FastMode(enabled) => rho_protocol::SessionChange::FastMode { enabled },
        }
    }
}

/// What a person chose for the sessions a session starts through session
/// control (the `sessions` tool's `new`), as the daemon last reported it.
/// The sessions those start take it on too. The session's own settings
/// ([`SessionOptions`]) are apart from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StartedSessionDefaults {
    mode: StartedMode,
    model: Option<ModelRef>,
}

impl StartedSessionDefaults {
    /// A choice of `mode` and `model`; `None` leaves the model to the
    /// config.
    pub fn new(mode: StartedMode, model: Option<ModelRef>) -> Self {
        Self { mode, model }
    }

    /// The permission mode they open in.
    pub fn mode(&self) -> StartedMode {
        self.mode
    }

    /// The model they open on, unless their agent names its own; `None`
    /// when not chosen, so they take the config's.
    pub fn model(&self) -> Option<&ModelRef> {
        self.model.as_ref()
    }

    /// Shows `change` in place, before the daemon reports it. A mode
    /// replaces an unknown one; a model leaves the mode as it is.
    pub fn apply_change(&mut self, change: &StartedSessionChange) {
        match change {
            StartedSessionChange::PermissionMode(mode) => self.mode = StartedMode::from(*mode),
            StartedSessionChange::Model(model) => self.model.clone_from(model),
        }
    }

    pub(crate) fn from_wire(defaults: &rho_protocol::StartedSessionDefaults) -> Self {
        let mode = match defaults.permission_mode {
            None => StartedMode::Default,
            Some(rho_protocol::PermissionMode::Unknown) => StartedMode::Unknown,
            // A mode the protocol crate knows but this client doesn't.
            Some(mode) => {
                PermissionMode::from_wire(mode).map_or(StartedMode::Unknown, StartedMode::Chosen)
            }
        };
        Self {
            mode,
            model: defaults.model.as_ref().map(ModelRef::from_wire),
        }
    }
}

/// The permission mode the sessions a session starts open in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StartedMode {
    /// Not chosen: they take the config's.
    #[default]
    Default,
    /// This mode.
    Chosen(PermissionMode),
    /// A mode this client can't name. The daemon sends `unknown` for a
    /// saved choice it can't read at all, with no model, under which it
    /// starts no session and refuses a model until a mode replaces it, and
    /// for a saved mode name it doesn't know, which keeps its model and
    /// takes a new one. A mode a newer daemon knows and this client doesn't
    /// reads as unknown too, and sessions start under it. The client can't
    /// tell them apart.
    Unknown,
}

impl From<Option<PermissionMode>> for StartedMode {
    fn from(mode: Option<PermissionMode>) -> Self {
        mode.map_or(Self::Default, Self::Chosen)
    }
}

/// One change to what the sessions a session starts open with, sent with
/// [`crate::Daemon::set_started_session_default`]. `None` clears the choice,
/// so they take the config's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartedSessionChange {
    /// Open them in this mode, one of the session's
    /// [`SessionOptions::permission_mode`] choices.
    PermissionMode(Option<PermissionMode>),
    /// Open them on this model, one of [`ModelOptions::available`].
    Model(Option<ModelRef>),
}

impl StartedSessionChange {
    pub(crate) fn into_wire(self) -> rho_protocol::SessionChange {
        match self {
            Self::PermissionMode(mode) => {
                rho_protocol::SessionChange::StartedSessionPermissionMode {
                    mode: mode.map(PermissionMode::to_wire),
                }
            }
            Self::Model(model) => rho_protocol::SessionChange::StartedSessionModel {
                model: model.map(ModelRef::into_wire),
            },
        }
    }
}

/// A slash command or skill the session offers. rho runs both itself: a
/// prompt names one as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInfo {
    name: String,
    kind: CommandKind,
    description: String,
    input_hint: Option<String>,
}

impl CommandInfo {
    /// The name without its sigil, such as `compact` or `prompt:review`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether a prompt names it as `/name` or as `$name`.
    pub fn kind(&self) -> CommandKind {
        self.kind
    }

    /// One line on what it does.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// What to type after the name, such as `<task>` or `[instructions]`;
    /// `None` for a command that takes nothing.
    pub fn input_hint(&self) -> Option<&str> {
        self.input_hint.as_deref()
    }
}

/// How a prompt runs a [`CommandInfo`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandKind {
    /// The prompt starts with `/name`, then the command's input.
    Slash,
    /// `$name` anywhere in the prompt loads the skill before the turn.
    Skill,
}

impl CommandKind {
    /// What a prompt puts before the name: `/` or `$`.
    pub fn sigil(self) -> char {
        match self {
            Self::Slash => '/',
            Self::Skill => '$',
        }
    }
}

/// The commands in `list` this UI knows how to offer, in the daemon's menu
/// order. A kind a newer daemon added is left out.
pub(crate) fn commands(list: Vec<SessionCommand>) -> Vec<CommandInfo> {
    list.into_iter()
        .filter_map(|command| {
            let kind = match command.kind {
                rho_protocol::CommandKind::Slash => CommandKind::Slash,
                rho_protocol::CommandKind::Skill => CommandKind::Skill,
                _ => return None,
            };
            Some(CommandInfo {
                name: command.name,
                kind,
                description: command.description,
                input_hint: command.input_hint,
            })
        })
        .collect()
}

/// Choices over values this UI knows. A value a newer daemon added is left
/// out; the option itself is dropped when the current value is unknown.
fn choices<W: Copy, T: Copy>(
    current: W,
    available: &[W],
    from_wire: fn(W) -> Option<T>,
) -> Option<Choices<T>> {
    let current = from_wire(current)?;
    let available = available.iter().copied().filter_map(from_wire).collect();
    Some(Choices { current, available })
}

/// The slug and label for `mode`, for a plan approval that offers it.
pub(crate) fn mode_label(
    mode: rho_protocol::PermissionMode,
) -> Option<(&'static str, &'static str)> {
    let mode = PermissionMode::from_wire(mode)?;
    Some((mode.slug(), mode.label()))
}
