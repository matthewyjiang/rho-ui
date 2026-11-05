//! The session settings in the thread header: one Select per choice option
//! (model, mode, thought level) and one Switch per toggle (fast mode).
//!
//! The agent owns the values. A user change is sent to the agent, and the
//! options it returns replace these controls through [`ConfigBar::sync`].

use gpui_kit::component::{
    h_flex,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    switch::Switch,
    Disableable as _, IndexPath, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    rems, AppContext as _, Context, Entity, IntoElement, ParentElement as _, SharedString,
    Styled as _, Subscription, Window,
};
use rho_ui_core::{ConfigChoice, ConfigKind, ConfigOption, ConfigValue};

use crate::thread_view::ThreadView;

/// Lists longer than this get a search field. The dropdown is 20rem tall and
/// rows are about 2rem, so ten rows fit without scrolling.
const SEARCH_THRESHOLD: usize = 10;

/// Whether the user can change settings right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Enabled,
    /// The session is still opening, or a change is on its way.
    Disabled,
}

/// One choice in a settings Select.
#[derive(Clone)]
pub(crate) struct Choice {
    value: SharedString,
    title: SharedString,
}

impl Choice {
    pub(crate) fn new(value: SharedString, title: SharedString) -> Self {
        Self { value, title }
    }
}

impl SelectItem for Choice {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &SharedString {
        &self.value
    }
}

type ChoiceSelect = Entity<SelectState<SearchableVec<Choice>>>;

enum Control {
    Select {
        state: ChoiceSelect,
        /// The choices `state` was built from, to tell when to rebuild it.
        choices: Vec<ConfigChoice>,
        subscription: Subscription,
    },
    Toggle(bool),
}

struct Picker {
    id: String,
    name: SharedString,
    control: Control,
}

/// The controls for a thread's settings. Owned by its [`ThreadView`].
#[derive(Default)]
pub(crate) struct ConfigBar {
    pickers: Vec<Picker>,
    /// The options the controls show.
    synced: Vec<ConfigOption>,
}

impl ConfigBar {
    pub(crate) fn is_synced(&self, options: &[ConfigOption]) -> bool {
        self.synced == options
    }

    /// Makes the controls show `options`. Reuses a Select whose choices did
    /// not change, so an open menu or search survives a value update.
    pub(crate) fn sync(
        &mut self,
        options: &[ConfigOption],
        window: &mut Window,
        cx: &mut Context<'_, ThreadView>,
    ) {
        let mut old = std::mem::take(&mut self.pickers);
        for option in options {
            let reused = old
                .iter()
                .position(|picker| picker.id == option.id())
                .map(|ix| old.remove(ix));
            let control = match (option.kind(), reused.map(|picker| picker.control)) {
                (
                    ConfigKind::Select { current, choices },
                    Some(Control::Select {
                        state,
                        choices: built_from,
                        subscription,
                    }),
                ) if built_from == *choices => {
                    let current = SharedString::from(current.clone());
                    state.update(cx, |state, cx| {
                        state.set_selected_value(&current, window, cx);
                    });
                    Control::Select {
                        state,
                        choices: built_from,
                        subscription,
                    }
                }
                (ConfigKind::Select { current, choices }, _) => {
                    new_select(option.id(), current, choices, window, cx)
                }
                (ConfigKind::Toggle(on), _) => Control::Toggle(*on),
            };
            self.pickers.push(Picker {
                id: option.id().to_owned(),
                name: option.name().to_owned().into(),
                control,
            });
        }
        self.synced = options.to_vec();
    }

    /// Shows a requested toggle value until the agent answers. A Select
    /// already shows what the user picked.
    pub(crate) fn show_requested(&mut self, id: &str, value: &ConfigValue) {
        let ConfigValue::Toggle(on) = value else {
            return;
        };
        if let Some(Picker {
            control: Control::Toggle(shown),
            ..
        }) = self.pickers.iter_mut().find(|picker| picker.id == id)
        {
            *shown = *on;
        }
    }

    pub(crate) fn name_of(&self, id: &str) -> Option<SharedString> {
        self.pickers
            .iter()
            .find(|picker| picker.id == id)
            .map(|picker| picker.name.clone())
    }

    pub(crate) fn render(
        &self,
        availability: Availability,
        cx: &mut Context<'_, ThreadView>,
    ) -> impl IntoElement {
        let disabled = availability == Availability::Disabled;
        h_flex()
            .flex_shrink_0()
            .gap_2()
            .children(self.pickers.iter().map(|picker| match &picker.control {
                Control::Select { state, .. } => {
                    let is_model = picker.id == "model";
                    Select::new(state)
                        .id(SharedString::from(format!("config-{}", picker.id)))
                        .small()
                        .accessibility_label(picker.name.clone())
                        .search_placeholder(format!("Search {}", picker.name.to_lowercase()))
                        .disabled(disabled)
                        // The model name says what it is; short values such
                        // as "high" need their setting's name.
                        .when(!is_model, |this| {
                            this.title_prefix(format!("{}: ", picker.name))
                        })
                        // Room for a typical "provider/model-name" id, or a
                        // "Name: value" pair, at text_sm.
                        .w(if is_model { rems(16.) } else { rems(11.) })
                        .menu_width(if is_model { rems(22.) } else { rems(14.) })
                        .into_any_element()
                }
                Control::Toggle(on) => {
                    let id = picker.id.clone();
                    Switch::new(SharedString::from(format!("config-{}", picker.id)))
                        .small()
                        .checked(*on)
                        .label(picker.name.clone())
                        .disabled(disabled)
                        .on_change(cx.listener(move |this, on: &bool, window, cx| {
                            this.request_config(id.clone(), ConfigValue::Toggle(*on), window, cx);
                        }))
                        .into_any_element()
                }
            }))
    }
}

fn new_select(
    config_id: &str,
    current: &str,
    choices: &[ConfigChoice],
    window: &mut Window,
    cx: &mut Context<'_, ThreadView>,
) -> Control {
    let items: Vec<Choice> = choices
        .iter()
        .map(|choice| Choice {
            value: choice.value().to_owned().into(),
            title: choice.name().to_owned().into(),
        })
        .collect();
    let selected = choices
        .iter()
        .position(|choice| choice.value() == current)
        .map(|ix| IndexPath::default().row(ix));
    let searchable = choices.len() > SEARCH_THRESHOLD;
    let state = cx.new(|cx| {
        SelectState::new(SearchableVec::new(items), selected, window, cx).searchable(searchable)
    });
    let config_id = config_id.to_owned();
    let subscription = cx.subscribe_in(
        &state,
        window,
        move |this: &mut ThreadView, _, event: &SelectEvent<SearchableVec<Choice>>, window, cx| {
            if let SelectEvent::Confirm(Some(value)) = event {
                this.request_config(
                    config_id.clone(),
                    ConfigValue::Select(value.to_string()),
                    window,
                    cx,
                );
            }
        },
    );
    Control::Select {
        state,
        choices: choices.to_vec(),
        subscription,
    }
}
