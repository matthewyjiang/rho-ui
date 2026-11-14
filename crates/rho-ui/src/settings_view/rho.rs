//! rho's own settings: a page for each category the chosen machine's daemon
//! describes, drawn from its schema alone, so a setting rho adds shows up
//! with no change here. Each item is the setting's label, its help and when
//! a change applies, and a control for its kind: a switch, a dropdown of
//! choices, a number or text box (applied on Enter or when it loses the
//! keyboard), or, for a model, its name, which rho's terminal UI changes.
//! A setting the user's file sets can be reset to its default from the
//! page's reset button.
//!
//! Every machine has its own `config.toml`. With more than one machine, a
//! picker heads each page; this machine shows first. One that can't be
//! reached says so instead of showing settings.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    input::{Input, InputEvent, InputState, NumberInput},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage},
    switch::Switch,
    v_flex, ActiveTheme as _, Sizable as _,
};
use gpui_kit::{
    div, rems, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    ParentElement as _, SharedString, Styled as _, Subscription, WeakEntity, Window,
};
use rho_ui_core::{
    agent_settings::{
        Applies, IntegerUnit, ModelSelection, SettingChange, SettingKey, SettingKind, SettingSpec,
        SettingValue,
    },
    AgentSettings,
};

use super::{page_header, SettingsView, CONTROL_REMS};
use crate::{
    machines::{MachineId, MachineStatus, Machines},
    rho_settings::{Loaded, RhoSettings},
};

/// The rho pages' own state: which machine they show, and the text boxes
/// of its text and number settings.
pub(super) struct RhoFields {
    view: WeakEntity<SettingsView>,
    machines: Entity<Machines>,
    settings: Entity<RhoSettings>,
    /// The machine picked; this one until the user picks another.
    machine: MachineId,
    inputs: Vec<SettingInput>,
    /// The machine whose settings `inputs` hold.
    inputs_for: Option<MachineId>,
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
        machines: Entity<Machines>,
        settings: Entity<RhoSettings>,
        window: &mut Window,
        cx: &mut Context<'_, SettingsView>,
    ) -> Self {
        let subscriptions = [
            // A change from this page, another client, or a hand edit.
            cx.observe_in(&settings, window, |this, _, window, cx| {
                this.sync_rho_inputs(window, cx);
                cx.notify();
            }),
            // A machine connecting, failing, or leaving the list.
            cx.observe(&machines, |_, _, cx| cx.notify()),
        ];
        Self {
            // Not `entity()`: the view is still being made.
            view: cx.weak_entity(),
            machines,
            settings,
            machine: MachineId::local(),
            inputs: Vec::new(),
            inputs_for: None,
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
    /// A problem in the file, which rho works around.
    Warning(String),
    /// A change of the user's that didn't land.
    Problem(SharedString),
}

impl SettingsView {
    /// The machine whose settings show: the one picked, while it is still
    /// on the list.
    fn rho_machine(&self, cx: &App) -> MachineId {
        let picked = &self.rho.machine;
        if self.rho.machines.read(cx).get(picked).is_some() {
            picked.clone()
        } else {
            MachineId::local()
        }
    }

    /// Shows `machine`'s settings.
    fn pick_machine(&mut self, machine: MachineId, cx: &mut Context<'_, Self>) {
        if self.rho.machine != machine {
            self.rho.machine = machine;
            cx.notify();
        }
    }

    /// Gives each text and number setting of the shown machine a text box,
    /// showing its value unless the user is typing in it, and drops the
    /// boxes of settings that are gone. Called as the settings change, and
    /// as the page draws a machine it hasn't yet.
    pub(super) fn sync_rho_inputs(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let machine = self.rho_machine(cx);
        if self.rho.inputs_for.as_ref() != Some(&machine) {
            self.rho.inputs.clear();
            self.rho.inputs_for = Some(machine.clone());
        }
        let wanted = match self.rho.settings.read(cx).loaded(&machine) {
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
    /// it changes something; otherwise the box shows the value again.
    fn commit_rho_input(
        &mut self,
        key: &SettingKey,
        text: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let machine = self.rho_machine(cx);
        let change = match self.rho.settings.read(cx).loaded(&machine) {
            Some(Loaded::Ready(settings)) => typed_change(settings, key, text.trim()),
            _ => None,
        };
        match change {
            Some(change) => self
                .rho
                .settings
                .update(cx, |settings, cx| settings.change(&machine, change, cx)),
            None => self.sync_rho_inputs(window, cx),
        }
    }

    /// rho's settings pages for the shown machine: one per category of its
    /// schema, or until they can show, one page that says why.
    pub(super) fn rho_pages(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Vec<SettingPage> {
        let machine = self.rho_machine(cx);
        // The boxes follow the machine picked since the last draw.
        if self.rho.inputs_for.as_ref() != Some(&machine) {
            self.sync_rho_inputs(window, cx);
        }
        let machines = self.rho.machines.read(cx);
        let label = machines.label(&machine);
        let picker = machines
            .several()
            .then(|| machine_picker(machines, &machine, self.rho.view.clone()));
        let unreachable = match machines.status(&machine) {
            None | Some(MachineStatus::Connecting) => Some(format!("Connecting to {label}…")),
            Some(MachineStatus::Failed(why)) => Some(format!("Can't reach {label}: {why}")),
            Some(MachineStatus::Ready { .. }) => None,
        };
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
                Err(format!("Couldn't read rho's settings on {label}: {why}"))
            }
            (None, Some(Loaded::Reading) | None) => {
                Err(format!("Reading rho's settings on {label}…"))
            }
        };
        let settings = match settings {
            Ok(settings) => settings,
            Err(why) => {
                notes.push(Note::Status(why));
                let header = header_group(picker, notes);
                return vec![rho_page("rho").groups(header)];
            }
        };

        let pages: Vec<(SharedString, Vec<&SettingSpec>)> = settings
            .categories()
            .iter()
            .map(|category| {
                let shown: Vec<&SettingSpec> = settings
                    .settings(&category.id)
                    .filter(|spec| has_control(&spec.kind))
                    .collect();
                (SharedString::from(category.label.clone()), shown)
            })
            .filter(|(_, shown)| !shown.is_empty())
            .collect();
        notes.extend(file_notes(settings, &pages));
        let header = header_group(picker, notes);
        pages
            .into_iter()
            .map(|(title, shown)| {
                let items = shown.into_iter().filter_map(|spec| {
                    let setting = SettingRef {
                        settings: self.rho.settings.clone(),
                        machine: machine.clone(),
                        key: spec.key.clone(),
                    };
                    self.setting_item(spec, settings, setting)
                });
                rho_page(title)
                    .groups(header.clone())
                    .groups(section_groups(items))
            })
            .collect()
    }

    /// The item for `spec`: its label, what it does and when a change
    /// applies, and the control for its kind; `None` for a kind this app
    /// doesn't know.
    fn setting_item(
        &self,
        spec: &SettingSpec,
        settings: &AgentSettings,
        setting: SettingRef,
    ) -> Option<(Option<String>, SettingItem)> {
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
            // rho's terminal UI picks models from a session's list, which
            // Settings doesn't have, so the model only shows here.
            SettingKind::Model { .. } => {
                let id = SharedString::from(format!("setting-{}", spec.key));
                SettingField::render(move |_, _, cx| {
                    div()
                        .id(id.clone())
                        .test_support()
                        .w(rems(CONTROL_REMS))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(model_name(setting.value(cx).as_ref()))
                })
            }
            _ => return None,
        }
        .on_reset(move |cx| dirty.is_set(cx), move |_, cx| reset.reset(cx));
        let item =
            SettingItem::new(spec.label.clone(), field).description(description(spec, settings));
        Some((spec.section.clone(), item))
    }

    /// The text box of setting `key`, once [`Self::sync_rho_inputs`] made
    /// it.
    fn input_for(&self, key: &SettingKey) -> Option<Entity<InputState>> {
        self.rho
            .inputs
            .iter()
            .find(|input| input.key == *key)
            .map(|input| input.state.clone())
    }
}

/// What a page says of the problems in the file that no item on `pages`
/// names, as an item names its own.
fn file_notes(settings: &AgentSettings, pages: &[(SharedString, Vec<&SettingSpec>)]) -> Vec<Note> {
    let on_a_page = |key: &SettingKey| {
        pages
            .iter()
            .any(|(_, shown)| shown.iter().any(|spec| spec.key == *key))
    };
    settings
        .errors()
        .iter()
        .filter(|error| !error.key.as_ref().is_some_and(on_a_page))
        .map(|error| {
            let place = error
                .key
                .as_ref()
                .map(|key| format!(" ({key})"))
                .unwrap_or_default();
            Note::Warning(format!(
                "config.toml doesn't load{place}: {}. rho uses the last values that did.",
                error.message
            ))
        })
        .collect()
}

/// A rho settings page titled `title`.
fn rho_page(title: impl Into<SharedString>) -> SettingPage {
    SettingPage::new(title)
        .icon(Lucide::SlidersHorizontal)
        .header_style(&page_header())
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

/// `items` in groups by section: those without one first, untitled, then
/// each section under its heading, in the order the schema first names it.
fn section_groups(items: impl Iterator<Item = (Option<String>, SettingItem)>) -> Vec<SettingGroup> {
    let mut sections: Vec<(Option<String>, Vec<SettingItem>)> = vec![(None, Vec::new())];
    for (section, item) in items {
        match sections.iter_mut().find(|(name, _)| *name == section) {
            Some((_, group)) => group.push(item),
            None => sections.push((section, vec![item])),
        }
    }
    sections
        .into_iter()
        .filter(|(_, items)| !items.is_empty())
        .map(|(section, items)| {
            let group = SettingGroup::new().items(items);
            match section {
                Some(title) => group.title(title),
                None => group,
            }
        })
        .collect()
}

/// What heads every rho page: the machine picker, and what to say about
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

/// The picker of the machine whose settings show, among `machines`.
fn machine_picker(
    machines: &Machines,
    shown: &MachineId,
    view: WeakEntity<SettingsView>,
) -> SettingItem {
    let options: Vec<(SharedString, SharedString)> = machines
        .iter()
        .map(|machine| {
            let id = SharedString::from(machine.id.as_str().to_owned());
            (id, machine.label.clone())
        })
        .collect();
    let shown = SharedString::from(shown.as_str().to_owned());
    SettingItem::new(
        "Machine",
        SettingField::dropdown(
            options,
            move |_: &App| shown.clone(),
            move |id: SharedString, cx: &mut App| {
                let _ = view.update(cx, |this, cx| {
                    this.pick_machine(MachineId::from(&*id), cx);
                });
            },
        )
        .w(rems(CONTROL_REMS))
        .justify_between(),
    )
    .description("Each machine's rho has its own config.toml.")
}

/// `spec`'s help, when a change applies, where to change a model, and the
/// file's problem with its value, if any.
fn description(spec: &SettingSpec, settings: &AgentSettings) -> String {
    let mut text = spec.help.clone();
    let model = matches!(spec.kind, SettingKind::Model { .. })
        .then_some("Change it with /config in rho's terminal UI.");
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
        Applies::Restart => Some("Applies once rho restarts."),
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
        Some(SettingValue::Model(ModelSelection::Rho { provider, model })) => {
            format!("{provider}/{model}").into()
        }
        Some(SettingValue::Model(ModelSelection::ClaudeCli { model })) => {
            format!("claude-code/{}", model.as_deref().unwrap_or("default")).into()
        }
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

/// What typing `text` in setting `key`'s box changes: nothing when the
/// setting holds it already or doesn't take it. Empty resets the setting.
fn typed_change(settings: &AgentSettings, key: &SettingKey, text: &str) -> Option<SettingChange> {
    let spec = settings.spec(key)?;
    if text.is_empty() {
        return settings
            .is_set(key)
            .then(|| SettingChange::Reset { key: key.clone() });
    }
    let value = match &spec.kind {
        SettingKind::Integer { min, max, .. } => {
            let number: u64 = text.parse().ok()?;
            SettingValue::Integer(number.max(*min).min(max.unwrap_or(u64::MAX)))
        }
        SettingKind::Text => SettingValue::Text(text.to_owned()),
        _ => return None,
    };
    (settings.value(key) != Some(&value)).then(|| SettingChange::Set {
        key: key.clone(),
        value,
    })
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
