//! rho's own settings: one page with a group for each category the chosen
//! machine's daemon describes, drawn from its schema alone, so a setting rho adds shows up
//! with no change here. Each item is the setting's label, its help and when
//! a change applies, and a control for its kind: a switch, a dropdown of
//! choices, a number or text box (applied on Enter or when it loses the
//! keyboard), or, for a model, a searchable picker of the models the
//! daemon lists ([`models`]). A daemon that can't list them shows the
//! model's name alone.
//! A setting the user's file sets can be reset to its default from the
//! page's reset button.
//!
//! Every machine has its own `config.toml`. With more than one machine, a
//! picker heads the page; this machine shows first. One that can't be
//! reached says so instead of showing settings.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    input::{Input, InputEvent, InputState, NumberInput},
    select::Select,
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    switch::Switch,
    v_flex, ActiveTheme as _, Sizable as _,
};
use gpui_kit::{
    div, rems, App, AppContext as _, Context, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, ParentElement as _, SharedString, Styled as _, Subscription, Window,
};
use rho_ui_core::{
    agent_settings::{
        Applies, ConfigCategory, IntegerUnit, ListedModels, SettingChange, SettingKey, SettingKind,
        SettingSpec, SettingValue,
    },
    AgentSettings,
};

use super::{
    machine_picker, page_header, unreachable_note, PickedMachine, SettingsView, CONTROL_REMS,
};
use crate::{
    machines::{MachineId, Machines},
    rho_settings::{Loaded, RhoSettings},
};
use models::{selection_name, ModelPicker};

mod models;

/// The rho pages' own state: which machine they show, the text boxes of
/// its text and number settings, and the pickers of its model settings.
pub(super) struct RhoFields {
    settings: Entity<RhoSettings>,
    machine: PickedMachine,
    inputs: Vec<SettingInput>,
    models: Vec<ModelPicker>,
    /// The machine whose settings `inputs` and `models` hold.
    controls_for: Option<MachineId>,
    _subscriptions: [Subscription; 2],
}

/// The text box of one text or number setting.
struct SettingInput {
    key: SettingKey,
    state: Entity<InputState>,
    _subscription: Subscription,
}

/// What a text box holds.
#[derive(Clone, Copy)]
enum TextKind {
    Integer { min: u64, max: Option<u64> },
    Text,
}

impl RhoFields {
    pub(super) fn new(
        machines: &Entity<Machines>,
        settings: Entity<RhoSettings>,
        window: &mut Window,
        cx: &mut Context<'_, SettingsView>,
    ) -> Self {
        let subscriptions = [
            // A change from this page, another client, or a hand edit.
            cx.observe_in(&settings, window, |this, _, window, cx| {
                this.sync_rho_controls(window, cx);
                cx.notify();
            }),
            // A machine connecting, failing, or leaving the list.
            cx.observe(machines, |_, _, cx| cx.notify()),
        ];
        Self {
            settings,
            machine: PickedMachine::new(),
            inputs: Vec::new(),
            models: Vec::new(),
            controls_for: None,
            _subscriptions: subscriptions,
        }
    }
}

/// One setting on one machine, for its control's closures.
#[derive(Clone)]
struct SettingRef {
    settings: Entity<RhoSettings>,
    machine: MachineId,
    key: SettingKey,
}

impl SettingRef {
    fn read<R>(&self, cx: &App, read: impl FnOnce(&AgentSettings) -> R) -> Option<R> {
        match self.settings.read(cx).loaded(&self.machine)? {
            Loaded::Ready(settings) => Some(read(settings)),
            Loaded::Reading | Loaded::Failed(_) => None,
        }
    }

    fn value(&self, cx: &App) -> Option<SettingValue> {
        self.read(cx, |settings| settings.value(&self.key).cloned())
            .flatten()
    }

    fn is_set(&self, cx: &App) -> bool {
        self.read(cx, |settings| settings.is_set(&self.key))
            .unwrap_or_default()
    }

    fn set(&self, value: SettingValue, cx: &mut App) {
        let key = self.key.clone();
        self.change(SettingChange::Set { key, value }, cx);
    }

    fn reset(&self, cx: &mut App) {
        let key = self.key.clone();
        self.change(SettingChange::Reset { key }, cx);
    }

    fn change(&self, change: SettingChange, cx: &mut App) {
        self.settings.update(cx, |settings, cx| {
            settings.change(&self.machine, change, cx);
        });
    }
}

/// Something a page says about the machine's settings, and how loudly.
enum Note {
    /// Why there are no settings to show yet.
    Status(String),
    /// A problem rho works around: one in the file, or models it
    /// couldn't list.
    Warning(String),
    /// A change of the user's that didn't land as made.
    Problem(SharedString),
}

impl SettingsView {
    /// The machine whose settings show.
    fn rho_machine(&self, cx: &App) -> MachineId {
        self.rho.machine.shown(self.machine_list.read(cx))
    }

    /// Shows `machine`'s settings.
    fn pick_machine(&mut self, machine: MachineId, cx: &mut Context<'_, Self>) {
        if self.rho.machine.pick(machine) {
            cx.notify();
        }
    }

    /// Gives each text and number setting of the shown machine a text box,
    /// and each model setting a picker, showing its value, and drops the
    /// controls of settings that are gone. Called as the settings change,
    /// and as the page draws a machine it hasn't yet.
    pub(super) fn sync_rho_controls(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let machine = self.rho_machine(cx);
        if self.rho.controls_for.as_ref() != Some(&machine) {
            self.rho.inputs.clear();
            self.rho.models.clear();
            self.rho.controls_for = Some(machine.clone());
        }
        self.sync_text_boxes(&machine, window, cx);
        self.sync_model_pickers(&machine, window, cx);
    }

    /// The text boxes of [`Self::sync_rho_controls`]: each shows its
    /// setting's value unless the user is typing in it.
    fn sync_text_boxes(
        &mut self,
        machine: &MachineId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let wanted = match self.rho.settings.read(cx).loaded(machine) {
            Some(Loaded::Ready(settings)) => text_settings(settings),
            _ => Vec::new(),
        };
        self.rho
            .inputs
            .retain(|input| wanted.iter().any(|(key, ..)| *key == input.key));
        for (key, kind, text) in wanted {
            if let Some(input) = self.rho.inputs.iter().find(|input| input.key == key) {
                let state = input.state.clone();
                let typing = state.focus_handle(cx).is_focused(window);
                if !typing && state.read(cx).value() != text {
                    state.update(cx, |state, cx| state.set_value(text, window, cx));
                }
                continue;
            }
            let state = cx.new(|cx| {
                let input = InputState::new(window, cx).default_value(text);
                match kind {
                    TextKind::Integer { min, max } => {
                        let input = input.step(1.).min(as_f64(min));
                        match max {
                            Some(max) => input.max(as_f64(max)),
                            None => input,
                        }
                    }
                    TextKind::Text => input,
                }
            });
            let typed = key.clone();
            let subscription = cx.subscribe_in(
                &state,
                window,
                move |this, input, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        let text = input.read(cx).value();
                        this.commit_rho_input(&typed, &text, window, cx);
                    }
                },
            );
            self.rho.inputs.push(SettingInput {
                key,
                state,
                _subscription: subscription,
            });
        }
    }

    /// Writes what setting `key`'s box holds, if the setting takes it and
    /// it changes something; otherwise the box shows the value again. The
    /// page says why when the setting doesn't take it, or takes it clamped
    /// to its bounds.
    fn commit_rho_input(
        &mut self,
        key: &SettingKey,
        text: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let machine = self.rho_machine(cx);
        let typed = match self.rho.settings.read(cx).loaded(&machine) {
            Some(Loaded::Ready(settings)) => typed_change(settings, key, text.trim()),
            _ => Typed::Nothing,
        };
        match typed {
            Typed::Change(change, clamped) => self.rho.settings.update(cx, |settings, cx| {
                settings.change(&machine, change, cx);
                if let Some(why) = clamped {
                    settings.set_problem(&machine, why.into(), cx);
                }
            }),
            Typed::Nothing => self.sync_rho_controls(window, cx),
            Typed::Refused(why) => {
                self.rho.settings.update(cx, |settings, cx| {
                    settings.set_problem(&machine, why.into(), cx);
                });
                self.sync_rho_controls(window, cx);
            }
        }
    }

    /// rho's settings page for the shown machine: the machine picker and
    /// notes, then a group per category of its schema, or until they can
    /// show, what stops them.
    pub(super) fn rho_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> SettingPage {
        let machine = self.rho_machine(cx);
        // The controls follow the machine picked since the last draw.
        if self.rho.controls_for.as_ref() != Some(&machine) {
            self.sync_rho_controls(window, cx);
        }
        let view = cx.entity().downgrade();
        let machines = self.machine_list.read(cx);
        let label = machines.label(&machine);
        let picker = machines.several().then(|| {
            machine_picker(
                machines,
                &machine,
                "Each machine's Rho has its own config.toml.",
                view,
                Self::pick_machine,
            )
        });
        let unreachable = unreachable_note(machines, &machine);
        let rho_settings = self.rho.settings.read(cx);
        let mut notes: Vec<Note> = rho_settings
            .problem(&machine)
            .map(|problem| Note::Problem(problem.clone()))
            .into_iter()
            .collect();
        let settings = match (unreachable, rho_settings.loaded(&machine)) {
            (Some(why), _) => Err(why),
            (None, Some(Loaded::Ready(settings))) => Ok(settings),
            (None, Some(Loaded::Failed(why))) => {
                Err(format!("Couldn't read Rho's settings on {label}: {why}"))
            }
            (None, Some(Loaded::Reading) | None) => {
                Err(format!("Reading Rho's settings on {label}…"))
            }
        };
        let settings = match settings {
            Ok(settings) => settings,
            Err(why) => {
                notes.push(Note::Status(why));
                return rho_page().groups(header_group(picker, notes));
            }
        };

        let categories: Vec<(&ConfigCategory, Vec<&SettingSpec>)> = settings
            .categories()
            .iter()
            .map(|category| {
                let shown: Vec<&SettingSpec> = settings
                    .settings(&category.id)
                    .filter(|spec| has_control(&spec.kind))
                    .collect();
                (category, shown)
            })
            .filter(|(_, shown)| !shown.is_empty())
            .collect();
        notes.extend(file_notes(settings, &categories));
        if let ListedModels::Failed(why) = settings.models() {
            notes.push(Note::Warning(format!(
                "Couldn't list the models Rho runs, so model settings only show their value: {why}"
            )));
        }
        let groups = categories
            .into_iter()
            .map(|(category, shown)| self.category_group(category, shown, settings, &machine))
            .collect::<Vec<_>>();
        rho_page()
            .groups(header_group(picker, notes))
            .groups(groups)
    }

    /// The group of `category`'s settings `shown`: those outside a section
    /// first, then each section under its heading, in the order the schema
    /// first names it.
    fn category_group(
        &self,
        category: &ConfigCategory,
        shown: Vec<&SettingSpec>,
        settings: &AgentSettings,
        machine: &MachineId,
    ) -> SettingGroup {
        let mut sections: Vec<(Option<&str>, Vec<&SettingSpec>)> = vec![(None, Vec::new())];
        for spec in shown {
            let section = spec.section.as_deref();
            match sections.iter_mut().find(|(name, _)| *name == section) {
                Some((_, specs)) => specs.push(spec),
                None => sections.push((section, vec![spec])),
            }
        }
        let mut group = SettingGroup::new().title(category.label.clone());
        for (section, specs) in sections {
            if let Some(title) = section {
                group = group.item(section_heading(title, &specs));
            }
            group = group.items(specs.into_iter().filter_map(|spec| {
                let setting = SettingRef {
                    settings: self.rho.settings.clone(),
                    machine: machine.clone(),
                    key: spec.key.clone(),
                };
                self.setting_item(spec, settings, setting)
            }));
        }
        group
    }

    /// The item for `spec`: its label, what it does and when a change
    /// applies, and the control for its kind; `None` for a kind this app
    /// doesn't know.
    fn setting_item(
        &self,
        spec: &SettingSpec,
        settings: &AgentSettings,
        setting: SettingRef,
    ) -> Option<SettingItem> {
        let reset = setting.clone();
        let dirty = setting.clone();
        let field = match &spec.kind {
            SettingKind::Bool => {
                let id = SharedString::from(format!("setting-{}", spec.key));
                let label = SharedString::from(spec.label.clone());
                SettingField::render(move |options, _, cx| {
                    let on = matches!(setting.value(cx), Some(SettingValue::Bool(true)));
                    let setting = setting.clone();
                    Switch::new(id.clone())
                        .accessibility_label(label.clone())
                        .checked(on)
                        .with_size(options.size())
                        .on_click(move |on: &bool, _, cx| setting.set(SettingValue::Bool(*on), cx))
                })
            }
            SettingKind::Choice { choices } => {
                let options: Vec<(SharedString, SharedString)> = choices
                    .iter()
                    .map(|choice| (choice.value.clone().into(), choice.label.clone().into()))
                    .collect();
                let current = setting.clone();
                SettingField::dropdown(
                    options,
                    move |cx: &App| match current.value(cx) {
                        Some(SettingValue::Text(value)) => value.into(),
                        _ => SharedString::default(),
                    },
                    move |value: SharedString, cx: &mut App| {
                        setting.set(SettingValue::Text(value.to_string()), cx);
                    },
                )
                .w(rems(CONTROL_REMS))
                .justify_between()
            }
            SettingKind::Integer { unit, .. } => {
                let state = self.input_for(&spec.key)?;
                let label = SharedString::from(spec.label.clone());
                let unit = (*unit).and_then(unit_label);
                SettingField::render(move |options, _, _| {
                    let input = NumberInput::new(&state)
                        .accessibility_label(label.clone())
                        .with_size(options.size())
                        .w(rems(CONTROL_REMS));
                    match unit {
                        Some(unit) => input.suffix(unit),
                        None => input,
                    }
                })
            }
            SettingKind::Text => {
                let state = self.input_for(&spec.key)?;
                let label = SharedString::from(spec.label.clone());
                SettingField::render(move |options, _, _| {
                    Input::new(&state)
                        .aria_label(label.clone())
                        .with_size(options.size())
                        .w(rems(CONTROL_REMS))
                })
            }
            SettingKind::Model { .. } => {
                let id = SharedString::from(format!("setting-{}", spec.key));
                match self.model_picker_for(&spec.key) {
                    Some(state) => {
                        let label = SharedString::from(spec.label.clone());
                        SettingField::render(move |options, _, _| {
                            Select::new(&state)
                                .id(id.clone())
                                .with_size(options.size())
                                .accessibility_label(label.clone())
                                .placeholder("Not set")
                                .w(rems(CONTROL_REMS))
                                .menu_width(rems(20.))
                        })
                    }
                    // A daemon that can't list the models: the model only
                    // shows here.
                    None => SettingField::render(move |_, _, cx| {
                        div()
                            .id(id.clone())
                            .test_support()
                            .w(rems(CONTROL_REMS))
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(model_name(setting.value(cx).as_ref()))
                    }),
                }
            }
            _ => return None,
        }
        .on_reset(move |cx| dirty.is_set(cx), move |_, cx| reset.reset(cx));
        Some(SettingItem::new(spec.label.clone(), field).description(description(spec, settings)))
    }

    /// The text box of setting `key`, once [`Self::sync_rho_controls`]
    /// made it.
    fn input_for(&self, key: &SettingKey) -> Option<Entity<InputState>> {
        self.rho
            .inputs
            .iter()
            .find(|input| input.key == *key)
            .map(|input| input.state.clone())
    }
}

/// What the page says of the problems in the file that no item shown in
/// `categories` names, as an item names its own.
fn file_notes(
    settings: &AgentSettings,
    categories: &[(&ConfigCategory, Vec<&SettingSpec>)],
) -> Vec<Note> {
    let shown = |key: &SettingKey| {
        categories
            .iter()
            .any(|(_, shown)| shown.iter().any(|spec| spec.key == *key))
    };
    settings
        .errors()
        .iter()
        .filter(|error| !error.key.as_ref().is_some_and(shown))
        .map(|error| {
            let place = error.key.as_ref().map_or_default(|key| format!(" ({key})"));
            Note::Warning(format!(
                "config.toml doesn't load{place}: {}. Rho uses the last values that did.",
                error.message
            ))
        })
        .collect()
}

/// The rho settings page, before its groups.
fn rho_page() -> SettingPage {
    SettingPage::new("Rho")
        .icon(Lucide::Bot)
        .default_open(true)
        .header_style(&page_header())
}

/// The heading of section `title` within its category's group. It shows
/// in a search that finds any of its settings `specs`.
fn section_heading(title: &str, specs: &[&SettingSpec]) -> SettingItem {
    let title = SharedString::from(title.to_owned());
    let keywords = specs
        .iter()
        .flat_map(|spec| [spec.label.clone(), spec.help.clone()]);
    SettingItem::render(move |_, _, cx| {
        div()
            .pt_2()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().muted_foreground)
            .child(title.clone())
    })
    .keywords(keywords)
}

/// Whether this app has a control for settings of `kind`.
fn has_control(kind: &SettingKind) -> bool {
    matches!(
        kind,
        SettingKind::Bool
            | SettingKind::Choice { .. }
            | SettingKind::Integer { .. }
            | SettingKind::Text
            | SettingKind::Model { .. }
    )
}

/// What heads the rho page: the machine picker, and what to say about
/// the machine's settings. Nothing when there is neither.
fn header_group(picker: Option<SettingItem>, notes: Vec<Note>) -> Option<SettingGroup> {
    if picker.is_none() && notes.is_empty() {
        return None;
    }
    let mut group = SettingGroup::new();
    if let Some(picker) = picker {
        group = group.item(picker);
    }
    if !notes.is_empty() {
        let notes = std::rc::Rc::new(notes);
        group = group.item(SettingItem::render(move |_, _, cx| {
            let theme = cx.theme();
            v_flex()
                .id("rho-settings-notes")
                .test_support()
                .w_full()
                .gap_1()
                .text_sm()
                .children(notes.iter().map(|note| {
                    let (text, color) = match note {
                        Note::Status(text) => {
                            (SharedString::from(text.clone()), theme.muted_foreground)
                        }
                        Note::Warning(text) => (SharedString::from(text.clone()), theme.warning),
                        Note::Problem(text) => (text.clone(), theme.danger),
                    };
                    div().text_color(color).child(text)
                }))
        }));
    }
    Some(group)
}

/// `spec`'s help, when a change applies, where to change a model on a rho
/// too old to list the models, and the file's problem with its value, if
/// any.
fn description(spec: &SettingSpec, settings: &AgentSettings) -> String {
    let mut text = spec.help.clone();
    let model = (matches!(spec.kind, SettingKind::Model { .. })
        && matches!(settings.models(), ListedModels::Unsupported))
    .then_some("Change it with /config in Rho's terminal UI.");
    for sentence in [applies(spec.applies), model].into_iter().flatten() {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(sentence);
    }
    for error in settings
        .errors()
        .iter()
        .filter(|error| error.key.as_ref() == Some(&spec.key))
    {
        text.push_str("\n\nThe file's value isn't used: ");
        text.push_str(&error.message);
    }
    text
}

/// When a change takes effect, as a description ends; nothing for at once.
fn applies(applies: Applies) -> Option<&'static str> {
    match applies {
        Applies::NextTurn => Some("Applies from the next turn."),
        Applies::NextSession => Some("Applies to new sessions."),
        Applies::Restart => Some("Applies once Rho restarts."),
        // `Immediately`, or a time a newer daemon names.
        _ => None,
    }
}

/// What a number box's suffix calls `unit`.
fn unit_label(unit: IntegerUnit) -> Option<&'static str> {
    match unit {
        IntegerUnit::Bytes => Some("bytes"),
        IntegerUnit::Lines => Some("lines"),
        IntegerUnit::Percent => Some("%"),
        IntegerUnit::Seconds => Some("seconds"),
        // A unit a newer daemon names.
        _ => None,
    }
}

/// A model setting's value as rho names it.
fn model_name(value: Option<&SettingValue>) -> SharedString {
    match value {
        Some(SettingValue::Model(model)) => selection_name(model).into(),
        Some(SettingValue::Text(text)) => text.clone().into(),
        _ => "Not set".into(),
    }
}

/// The text and number settings of `settings`, each with what its box
/// shows.
fn text_settings(settings: &AgentSettings) -> Vec<(SettingKey, TextKind, SharedString)> {
    settings
        .categories()
        .iter()
        .flat_map(|category| settings.settings(&category.id))
        .filter_map(|spec| {
            let kind = match &spec.kind {
                SettingKind::Integer { min, max, .. } => TextKind::Integer {
                    min: *min,
                    max: *max,
                },
                SettingKind::Text => TextKind::Text,
                _ => return None,
            };
            let text = match settings.value(&spec.key) {
                Some(SettingValue::Integer(number)) => number.to_string().into(),
                Some(SettingValue::Text(text)) => text.clone().into(),
                _ => SharedString::default(),
            };
            Some((spec.key.clone(), kind, text))
        })
        .collect()
}

/// What typing in a setting's box does.
enum Typed {
    /// Nothing: the setting holds the value already, or has no box.
    Nothing,
    /// Makes the change, and says why when it isn't what was typed: a
    /// number past the setting's bounds is clamped to them.
    Change(SettingChange, Option<String>),
    /// Nothing changes, and why: the setting doesn't take the text, or
    /// clamping it gives the value the setting holds already.
    Refused(String),
}

/// What typing `text` in setting `key`'s box does. Empty resets the
/// setting.
fn typed_change(settings: &AgentSettings, key: &SettingKey, text: &str) -> Typed {
    let Some(spec) = settings.spec(key) else {
        return Typed::Nothing;
    };
    if text.is_empty() {
        if settings.is_set(key) {
            return Typed::Change(SettingChange::Reset { key: key.clone() }, None);
        }
        return Typed::Nothing;
    }
    let (value, clamped) = match &spec.kind {
        SettingKind::Integer { min, max, .. } => {
            let bounds = match max {
                Some(max) => format!("from {min} to {max}"),
                None => format!("of at least {min}"),
            };
            let Ok(number) = text.parse::<u64>() else {
                return Typed::Refused(format!(
                    "{} takes a whole number {bounds}, not \"{text}\".",
                    spec.label
                ));
            };
            let kept = number.max(*min).min(max.unwrap_or(u64::MAX));
            let clamped = (kept != number).then(|| {
                format!(
                    "{} takes a number {bounds}, so {number} became {kept}.",
                    spec.label
                )
            });
            (SettingValue::Integer(kept), clamped)
        }
        SettingKind::Text => (SettingValue::Text(text.to_owned()), None),
        _ => return Typed::Nothing,
    };
    if settings.value(key) == Some(&value) {
        // Clamped to what it holds already, the page still says why.
        return clamped.map_or(Typed::Nothing, Typed::Refused);
    }
    Typed::Change(
        SettingChange::Set {
            key: key.clone(),
            value,
        },
        clamped,
    )
}

/// A setting's bound as the number box takes it.
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "GPUI Kit's number box bounds are f64; a bound past 2^53 only rounds"
)]
fn as_f64(number: u64) -> f64 {
    number as f64
}
