//! The pickers of the rho page's model settings: a searchable list of the
//! models the machine's daemon lists (`rho/model/list`), as far as each
//! setting's runtimes allow, kept current as the daemon's list and the
//! setting's value change.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    select::{SearchableVec, SelectEvent, SelectItem, SelectState},
    IndexPath,
};
use gpui_kit::{
    div, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Subscription, Window,
};
use rho_ui_core::{
    agent_settings::{
        ListedModels, ModelList, ModelRuntime, ModelSelection, SettingKey, SettingKind,
        SettingValue,
    },
    AgentSettings,
};

use super::{SettingRef, SettingsView};
use crate::{machines::MachineId, rho_settings::Loaded};

/// The picker of one model setting.
pub(super) struct ModelPicker {
    key: SettingKey,
    /// What the picker lists, to tell when the daemon's list changed.
    items: Vec<ModelItem>,
    state: ModelSelect,
    _subscription: Subscription,
}

pub(super) type ModelSelect = Entity<SelectState<SearchableVec<ModelItem>>>;

/// One model a model setting may take, as its picker lists it.
#[derive(Clone, PartialEq)]
pub(super) struct ModelItem {
    title: SharedString,
    value: ModelSelection,
}

impl SelectItem for ModelItem {
    type Value = ModelSelection;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &ModelSelection {
        &self.value
    }

    /// The label, in a row tests find by the model's name.
    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let id = SharedString::from(format!("setting-model-{}", selection_name(&self.value)));
        div().id(id).test_support().child(self.title.clone())
    }

    /// Matches the label, or the model's name as rho writes it.
    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.title.to_lowercase().contains(&query)
            || selection_name(&self.value).to_lowercase().contains(&query)
    }
}

impl SettingsView {
    /// Gives each model setting of `machine` a picker of the models its
    /// daemon lists, showing the setting's value, and drops the pickers of
    /// settings that are gone. None while the daemon can't list the models.
    pub(super) fn sync_model_pickers(
        &mut self,
        machine: &MachineId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let wanted = match self.rho.settings.read(cx).loaded(machine) {
            Some(Loaded::Ready(settings)) => model_settings(settings),
            _ => Vec::new(),
        };
        self.rho
            .models
            .retain(|picker| wanted.iter().any(|(key, ..)| *key == picker.key));
        for (key, items, current) in wanted {
            if let Some(picker) = self.rho.models.iter_mut().find(|picker| picker.key == key) {
                let state = picker.state.clone();
                let relisted = picker.items != items;
                if relisted {
                    picker.items.clone_from(&items);
                    state.update(cx, |state, cx| {
                        state.set_items(SearchableVec::new(items), window, cx);
                    });
                }
                // Selecting the value again also clears a search the new
                // list made stale. An unset setting keeps it, since Kit
                // 0.9.1 clears the query only through `set_selected_value`.
                if relisted || state.read(cx).selected_value() != current.as_ref() {
                    state.update(cx, |state, cx| match &current {
                        Some(value) => state.set_selected_value(value, window, cx),
                        None => state.set_selected_index(None, window, cx),
                    });
                }
                continue;
            }
            let selected = current
                .as_ref()
                .and_then(|value| items.iter().position(|item| item.value == *value))
                .map(|row| IndexPath::default().row(row));
            let listed = SearchableVec::new(items.clone());
            let state =
                cx.new(|cx| SelectState::new(listed, selected, window, cx).searchable(true));
            let setting = SettingRef {
                settings: self.rho.settings.clone(),
                machine: machine.clone(),
                key: key.clone(),
            };
            let subscription = cx.subscribe_in(
                &state,
                window,
                move |_, _, event: &SelectEvent<SearchableVec<ModelItem>>, _, cx| {
                    if let SelectEvent::Confirm(Some(model)) = event {
                        setting.set(SettingValue::Model(model.clone()), cx);
                    }
                },
            );
            self.rho.models.push(ModelPicker {
                key,
                items,
                state,
                _subscription: subscription,
            });
        }
    }

    /// The picker of model setting `key`, once [`Self::sync_model_pickers`]
    /// made it.
    pub(super) fn model_picker_for(&self, key: &SettingKey) -> Option<ModelSelect> {
        self.rho
            .models
            .iter()
            .find(|picker| picker.key == *key)
            .map(|picker| picker.state.clone())
    }
}

/// A model as rho names it: `provider/model`, or `claude-code/model`.
pub(super) fn selection_name(model: &ModelSelection) -> String {
    match model {
        ModelSelection::Rho { provider, model } => format!("{provider}/{model}"),
        ModelSelection::ClaudeCli { model } => {
            format!("claude-code/{}", model.as_deref().unwrap_or("default"))
        }
    }
}

/// The model settings of `settings`, each with the models its picker
/// lists and the value it holds. None when the daemon can't list the
/// models.
fn model_settings(
    settings: &AgentSettings,
) -> Vec<(SettingKey, Vec<ModelItem>, Option<ModelSelection>)> {
    let ListedModels::Listed(models) = settings.models() else {
        return Vec::new();
    };
    settings
        .categories()
        .iter()
        .flat_map(|category| settings.settings(&category.id))
        .filter_map(|spec| {
            let SettingKind::Model { runtimes } = &spec.kind else {
                return None;
            };
            let current = match settings.value(&spec.key) {
                Some(SettingValue::Model(model)) => Some(model.clone()),
                _ => None,
            };
            let items = model_items(runtimes, models, current.as_ref());
            Some((spec.key.clone(), items, current))
        })
        .collect()
}

/// The models a setting on `runtimes` may take, in the daemon's order: rho's
/// own, then Claude Code's. The setting's `current` model comes last when
/// the list lacks it, as one of a provider since signed out of.
fn model_items(
    runtimes: &[ModelRuntime],
    models: &ModelList,
    current: Option<&ModelSelection>,
) -> Vec<ModelItem> {
    let mut items = Vec::new();
    if runtimes.contains(&ModelRuntime::Rho) {
        items.extend(models.models.iter().map(|choice| ModelItem {
            title: choice.label.clone().into(),
            value: ModelSelection::Rho {
                provider: choice.model.provider.clone(),
                model: choice.model.model.clone(),
            },
        }));
    }
    if runtimes.contains(&ModelRuntime::ClaudeCli) {
        items.extend(models.claude_cli.iter().map(|model| ModelItem {
            title: model.label.clone().into(),
            value: ModelSelection::ClaudeCli {
                model: model.model.clone(),
            },
        }));
    }
    if let Some(current) = current {
        if !items.iter().any(|item| item.value == *current) {
            items.push(ModelItem {
                title: selection_name(current).into(),
                value: current.clone(),
            });
        }
    }
    items
}
