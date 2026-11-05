//! The session settings in the composer footer, laid out by what rho lets a
//! session change. The mode (which tool calls run without asking) sits on the
//! left as a menu. On the right, one button names the model and its reasoning
//! and opens a panel: the model list with search, beside the reasoning levels
//! and any other setting the agent offers, such as fast mode.
//!
//! The agent owns the values. [`SessionSettings`] sends a user change itself
//! and tells its owner, a thread or a new thread's draft, what the agent
//! answered; options the agent pushes come in through [`SessionSettings::sync`].

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    label::Label,
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    menu::{DropdownMenu as _, PopupMenuItem},
    popover::Popover,
    separator::Separator,
    switch::Switch,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    rems, Anchor, AnyElement, App, AppContext as _, Context, Entity, EventEmitter, Focusable as _,
    IntoElement, ParentElement as _, SharedString, Styled as _, Subscription, Task, WeakEntity,
    Window,
};
use rho_ui_core::{
    Agent, ConfigCategory, ConfigChoice, ConfigKind, ConfigOption, ConfigValue, SessionId,
};

/// rho's fast mode toggle, which shows as a lightning bolt.
const FAST_MODE_ID: &str = "fast_mode";

/// Whether the user can change settings right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Enabled,
    /// The session is still opening, or a change is on its way.
    Disabled,
}

/// What the agent answered to a change.
pub(crate) enum SettingsEvent {
    /// The agent took it; these are all the options now.
    Changed(Vec<ConfigOption>),
    /// The agent refused it, in these words; the controls show the old
    /// values again.
    Failed(String),
}

/// The settings controls for one session, shared by its thread or draft.
pub(crate) struct SessionSettings {
    agent: Agent,
    session: SessionId,
    /// Whether the owner lets the user change settings: not while a
    /// resumed session replays.
    allowed: Availability,
    /// `set_config` calls in flight. Controls wait for the answer.
    pending: usize,
    /// The options as the agent last sent them.
    synced: Vec<ConfigOption>,
    /// What the controls show: `synced` plus a change on its way.
    shown: Vec<ConfigOption>,
    /// The list for the first model option, kept so its search and scroll
    /// survive a value update.
    models: Option<ModelPicker>,
    /// Whether the settings panel is open.
    open: bool,
}

struct ModelPicker {
    config_id: String,
    /// The choices `list` was built from, to tell when to rebuild it.
    choices: Vec<ConfigChoice>,
    list: Entity<ListState<ModelList>>,
    _subscription: Subscription,
}

impl EventEmitter<SettingsEvent> for SessionSettings {}

impl SessionSettings {
    pub(crate) fn new(
        agent: Agent,
        session: SessionId,
        options: &[ConfigOption],
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut settings = Self {
            agent,
            session,
            allowed: Availability::Enabled,
            pending: 0,
            synced: Vec::new(),
            shown: Vec::new(),
            models: None,
            open: false,
        };
        settings.sync(options, window, cx);
        settings
    }

    pub(crate) fn set_allowed(&mut self, allowed: Availability, cx: &mut Context<'_, Self>) {
        if self.allowed != allowed {
            self.allowed = allowed;
            cx.notify();
        }
    }

    fn availability(&self) -> Availability {
        if self.pending == 0 {
            self.allowed
        } else {
            Availability::Disabled
        }
    }

    /// Makes the controls show `options`, dropping any change still shown as
    /// requested. Does nothing if they already do.
    pub(crate) fn sync(
        &mut self,
        options: &[ConfigOption],
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.synced == options && self.shown == options {
            return;
        }
        cx.notify();
        self.synced = options.to_vec();
        self.shown = options.to_vec();
        let model = options
            .iter()
            .find(|option| option.category() == ConfigCategory::Model);
        let Some((option, current, choices)) = model.and_then(|option| match option.kind() {
            ConfigKind::Select { current, choices } => Some((option, current, choices)),
            ConfigKind::Toggle(_) => None,
        }) else {
            self.models = None;
            return;
        };
        match &self.models {
            Some(picker) if picker.config_id == option.id() && picker.choices == *choices => {
                set_current_model(&picker.list, current, window, cx);
            }
            _ => {
                self.models = Some(new_model_picker(option.id(), current, choices, window, cx));
            }
        }
    }

    /// Sends a change. Until the agent answers, the controls show it and
    /// take no other.
    fn request(
        &mut self,
        id: String,
        value: ConfigValue,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.availability() == Availability::Disabled {
            return;
        }
        if let Some(option) = self.shown.iter_mut().find(|option| option.id() == id) {
            *option = option.clone().with_value(&value);
        }
        if let (Some(picker), ConfigValue::Select(value)) = (&self.models, &value) {
            if picker.config_id == id {
                set_current_model(&picker.list, value, window, cx);
            }
        }
        self.pending += 1;
        cx.notify();

        let agent = self.agent.clone();
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = agent.set_config(&session, &id, value).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending -= 1;
                match result {
                    Ok(options) => {
                        this.sync(&options, window, cx);
                        cx.emit(SettingsEvent::Changed(options));
                    }
                    Err(error) => {
                        let name = this
                            .synced
                            .iter()
                            .find(|option| option.id() == id)
                            .map_or(id.as_str(), ConfigOption::name)
                            .to_lowercase();
                        // Put back what the agent still has.
                        let synced = std::mem::take(&mut this.synced);
                        this.sync(&synced, window, cx);
                        cx.emit(SettingsEvent::Failed(format!(
                            "Couldn't change {name}: {error}"
                        )));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn close_panel(&mut self, cx: &mut Context<'_, Self>) {
        if self.open {
            self.open = false;
            cx.notify();
        }
    }

    /// One menu per mode option, for the left of the composer footer.
    pub(crate) fn render_modes(this: &Entity<Self>, cx: &App) -> impl IntoElement {
        let settings = this.read(cx);
        let availability = settings.availability();
        let weak = this.downgrade();
        h_flex().gap_1().children(
            settings
                .shown
                .iter()
                .filter(|option| option.category() == ConfigCategory::Mode)
                .filter_map(|option| mode_menu(option, availability, &weak)),
        )
    }

    /// The button that names the model and opens the settings panel, for the
    /// right of the composer footer. Nothing when the agent offers nothing
    /// but modes.
    pub(crate) fn render_panel_button(this: &Entity<Self>, cx: &App) -> Option<impl IntoElement> {
        let settings = this.read(cx);
        let panel = settings.panel_options();
        if settings.models.is_none() && panel.is_empty() {
            return None;
        }
        let weak = this.downgrade();
        let list = settings.models.as_ref().map(|picker| picker.list.clone());
        let focus = list.as_ref().map(|list| list.focus_handle(cx));
        let availability = settings.availability();
        let close = weak.clone();
        Some(
            Popover::new("settings-panel")
                .anchor(Anchor::BottomRight)
                .open(settings.open)
                .on_open_change(move |open, _, cx| {
                    let _ = close.update(cx, |this, cx| {
                        this.open = *open;
                        cx.notify();
                    });
                })
                .when_some(focus, |this, focus| this.track_focus(&focus))
                .trigger(
                    Button::new("config-panel")
                        .ghost()
                        .small()
                        .when(settings.is_fast(), |this| {
                            this.icon(Lucide::Zap).tooltip("Fast mode")
                        })
                        .label(settings.summary())
                        .dropdown_caret(true)
                        .disabled(availability == Availability::Disabled),
                )
                .content(move |_, _, cx| {
                    render_panel(list.as_ref(), &panel, availability, &weak, cx)
                }),
        )
    }

    /// The options the panel shows beside the model list.
    fn panel_options(&self) -> Vec<ConfigOption> {
        let model_id = self.models.as_ref().map(|picker| picker.config_id.as_str());
        self.shown
            .iter()
            .filter(|option| {
                option.category() != ConfigCategory::Mode && Some(option.id()) != model_id
            })
            .cloned()
            .collect()
    }

    /// Whether rho's fast mode is on.
    fn is_fast(&self) -> bool {
        self.shown.iter().any(|option| {
            option.id() == FAST_MODE_ID && matches!(option.kind(), ConfigKind::Toggle(true))
        })
    }

    /// "Claude Opus 5.5 · High": the model, its reasoning, and any other
    /// setting that is switched on. Fast mode is the button's icon instead.
    fn summary(&self) -> SharedString {
        let model = self
            .shown
            .iter()
            .find(|option| option.category() == ConfigCategory::Model);
        let thought = self
            .shown
            .iter()
            .filter(|option| option.category() == ConfigCategory::ThoughtLevel);
        let toggles = self.shown.iter().filter(|option| {
            option.category() == ConfigCategory::Other
                && option.id() != FAST_MODE_ID
                && matches!(option.kind(), ConfigKind::Toggle(true))
        });
        let parts: Vec<&str> = model
            .into_iter()
            .chain(thought)
            .filter_map(|option| option.current_choice().map(ConfigChoice::name))
            .chain(toggles.map(ConfigOption::name))
            .collect();
        if parts.is_empty() {
            "Settings".into()
        } else {
            parts.join(" · ").into()
        }
    }
}

/// A ghost button naming the current mode, opening a menu of the others.
fn mode_menu(
    option: &ConfigOption,
    availability: Availability,
    settings: &WeakEntity<SessionSettings>,
) -> Option<AnyElement> {
    let ConfigKind::Select { choices, .. } = option.kind() else {
        return None;
    };
    let current = option.current_choice()?;
    let config_id = option.id().to_owned();
    let current_value = current.value().to_owned();
    let choices = choices.clone();
    let settings = settings.clone();
    Some(
        Button::new(SharedString::from(format!("config-{config_id}")))
            .ghost()
            .small()
            .icon(Lucide::Shield)
            .label(current.name().to_owned())
            .dropdown_caret(true)
            .accessibility_label(option.name().to_owned())
            .disabled(availability == Availability::Disabled)
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                choices.iter().fold(menu, |menu, choice| {
                    let settings = settings.clone();
                    let config_id = config_id.clone();
                    let value = choice.value().to_owned();
                    menu.item(
                        PopupMenuItem::new(choice.name().to_owned())
                            .checked(choice.value() == current_value)
                            .on_click(move |_, window, cx| {
                                let _ = settings.update(cx, |this, cx| {
                                    this.request(
                                        config_id.clone(),
                                        ConfigValue::Select(value.clone()),
                                        window,
                                        cx,
                                    );
                                });
                            }),
                    )
                })
            })
            .into_any_element(),
    )
}

/// The panel's content: models on the left, every other setting on the right.
fn render_panel(
    models: Option<&Entity<ListState<ModelList>>>,
    options: &[ConfigOption],
    availability: Availability,
    settings: &WeakEntity<SessionSettings>,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .items_stretch()
        .gap_3()
        .when_some(models, |this, list| {
            // Ten rows of about two rem each, under the search field.
            this.child(
                v_flex()
                    .w(rems(18.))
                    .h(rems(22.))
                    .child(List::new(list).small().search_placeholder("Search models")),
            )
        })
        .when(models.is_some() && !options.is_empty(), |this| {
            this.child(Separator::vertical())
        })
        .when(!options.is_empty(), |this| {
            this.child(
                v_flex().w(rems(13.)).gap_2().children(
                    options
                        .iter()
                        .map(|option| render_setting(option, availability, settings, cx)),
                ),
            )
        })
}

/// One setting in the panel: a titled list of choices, or a switch.
fn render_setting(
    option: &ConfigOption,
    availability: Availability,
    settings: &WeakEntity<SessionSettings>,
    cx: &App,
) -> AnyElement {
    let config_id = option.id().to_owned();
    let disabled = availability == Availability::Disabled;
    match option.kind() {
        ConfigKind::Select { current, choices } => v_flex()
            .child(
                Label::new(option.name().to_owned())
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .px_3()
                    .pb_1(),
            )
            .children(choices.iter().map(|choice| {
                let settings = settings.clone();
                let config_id = config_id.clone();
                let value = choice.value().to_owned();
                ListItem::new(SharedString::from(format!(
                    "config-{config_id}-{}",
                    choice.value()
                )))
                .rounded(cx.theme().radius)
                .check_icon(IconName::Check)
                .confirmed(choice.value() == current)
                .disabled(disabled)
                .accessibility_label(choice.name().to_owned())
                .child(choice.name().to_owned())
                .on_click(move |_, window, cx| {
                    let _ = settings.update(cx, |this, cx| {
                        this.request(
                            config_id.clone(),
                            ConfigValue::Select(value.clone()),
                            window,
                            cx,
                        );
                    });
                })
            }))
            .into_any_element(),
        ConfigKind::Toggle(on) => {
            let settings = settings.clone();
            h_flex()
                .px_3()
                .py_1()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .when(option.id() == FAST_MODE_ID, |this| {
                            this.child(Icon::new(Lucide::Zap).small())
                        })
                        .child(option.name().to_owned()),
                )
                .child(
                    Switch::new(SharedString::from(format!("config-{config_id}")))
                        .small()
                        .checked(*on)
                        .accessibility_label(option.name().to_owned())
                        .disabled(disabled)
                        .on_change(move |on: &bool, window, cx| {
                            let _ = settings.update(cx, |this, cx| {
                                this.request(
                                    config_id.clone(),
                                    ConfigValue::Toggle(*on),
                                    window,
                                    cx,
                                );
                            });
                        }),
                )
                .into_any_element()
        }
    }
}

fn new_model_picker(
    config_id: &str,
    current: &str,
    choices: &[ConfigChoice],
    window: &mut Window,
    cx: &mut Context<'_, SessionSettings>,
) -> ModelPicker {
    let delegate = ModelList::new(choices.to_vec(), current.to_owned());
    let list = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
    set_current_model(&list, current, window, cx);
    let id = config_id.to_owned();
    let subscription = cx.subscribe_in(
        &list,
        window,
        move |this: &mut SessionSettings, list, event: &ListEvent, window, cx| match event {
            ListEvent::Confirm(ix) => {
                let Some(value) = list.read(cx).delegate().value_at(*ix) else {
                    return;
                };
                let value = ConfigValue::Select(value.to_owned());
                this.close_panel(cx);
                this.request(id.clone(), value, window, cx);
            }
            ListEvent::Cancel => this.close_panel(cx),
            ListEvent::Select(_) => {}
        },
    );
    ModelPicker {
        config_id: config_id.to_owned(),
        choices: choices.to_vec(),
        list,
        _subscription: subscription,
    }
}

/// Marks `value` as the model in use and moves the keyboard selection to it,
/// so arrows start from there.
fn set_current_model(
    list: &Entity<ListState<ModelList>>,
    value: &str,
    window: &mut Window,
    cx: &mut App,
) {
    list.update(cx, |list, cx| {
        value.clone_into(&mut list.delegate_mut().current);
        let row = list.delegate().row_of(value);
        list.set_selected_index(row, window, cx);
        cx.notify();
    });
}

/// The model choices for a searchable [`List`].
struct ModelList {
    choices: Vec<ConfigChoice>,
    /// Indices into `choices` that match the search, in order.
    matches: Vec<usize>,
    /// The value of the model in use.
    current: String,
}

impl ModelList {
    fn new(choices: Vec<ConfigChoice>, current: String) -> Self {
        Self {
            matches: (0..choices.len()).collect(),
            choices,
            current,
        }
    }

    fn choice_at(&self, ix: IndexPath) -> Option<&ConfigChoice> {
        self.choices.get(*self.matches.get(ix.row)?)
    }

    fn value_at(&self, ix: IndexPath) -> Option<&str> {
        self.choice_at(ix).map(ConfigChoice::value)
    }

    /// Where `value` is among the matches.
    fn row_of(&self, value: &str) -> Option<IndexPath> {
        self.matches
            .iter()
            .position(|&ix| self.choices[ix].value() == value)
            .map(|row| IndexPath::default().row(row))
    }
}

impl ListDelegate for ModelList {
    type Item = ListItem;

    /// Matches the label or the value, which for rho is `provider/model`.
    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<'_, ListState<Self>>,
    ) -> Task<()> {
        let query = query.to_lowercase();
        self.matches = self
            .choices
            .iter()
            .enumerate()
            .filter(|(_, choice)| {
                choice.name().to_lowercase().contains(&query)
                    || choice.value().to_lowercase().contains(&query)
            })
            .map(|(ix, _)| ix)
            .collect();
        Task::ready(())
    }

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.matches.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<'_, ListState<Self>>,
    ) -> Option<ListItem> {
        let choice = self.choice_at(ix)?;
        Some(
            ListItem::new(SharedString::from(format!("model-{}", choice.value())))
                .rounded(cx.theme().radius)
                .check_icon(IconName::Check)
                .confirmed(choice.value() == self.current)
                .accessibility_label(choice.name().to_owned())
                .child(Label::new(choice.name().to_owned()).truncate()),
        )
    }

    fn set_selected_index(
        &mut self,
        _: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<'_, ListState<Self>>,
    ) {
        // ListState keeps the selection; rows read it when it renders them.
    }
}
