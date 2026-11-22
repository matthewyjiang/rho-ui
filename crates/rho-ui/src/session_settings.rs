//! The session settings in the composer footer, laid out by what rho lets a
//! session change. The permission mode sits on the left as a menu. On the
//! right, one button names the model and its reasoning and opens a panel: the
//! model list with search, beside the reasoning levels and fast mode.
//!
//! The daemon owns the values. [`SessionSettings`] sends a user change itself
//! and tells its owner, a session or a new session's draft, what the daemon
//! answered; options the daemon pushes come in through [`SessionSettings::sync`].
//! A change made while a turn runs applies, and is answered, when the turn
//! ends; the controls wait for it.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
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
    rems, Anchor, App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, Subscription, Task, WeakEntity, Window,
};
use rho_ui_core::{
    Choices, Daemon, ModelChoice, ModelRef, OptionChange, PermissionMode, ReasoningLevel,
    SessionId, SessionOptions,
};

use crate::session_list::contains_ignore_case;

/// Whether the user can change settings right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Enabled,
    /// The session is still opening, or a change is on its way.
    Disabled,
}

/// What the daemon answered to a change.
pub(crate) enum SettingsEvent {
    /// The daemon took it; these are all the options now.
    Changed(SessionOptions),
    /// The daemon refused it, in these words; the controls show the old
    /// values again.
    Failed(String),
}

/// The settings controls for one session, shared by its session view or draft.
pub(crate) struct SessionSettings {
    daemon: Daemon,
    session: SessionId,
    /// Whether the owner lets the user change settings: not until a saved
    /// session's attach has answered.
    allowed: Availability,
    /// `set_option` calls in flight. Controls wait for the answer.
    pending: usize,
    /// The options as the daemon last sent them.
    synced: Option<SessionOptions>,
    /// What the controls show: `synced` plus a change on its way.
    shown: Option<SessionOptions>,
    /// The model picker list, kept so its search and scroll survive a value
    /// update.
    models: Option<ModelPicker>,
    /// Whether the settings panel is open.
    open: bool,
}

struct ModelPicker {
    /// The choices `list` was built from, to tell when to rebuild it.
    choices: Vec<ModelChoice>,
    list: Entity<ListState<ModelList>>,
    _subscription: Subscription,
}

impl EventEmitter<SettingsEvent> for SessionSettings {}

impl SessionSettings {
    pub(crate) fn new(
        daemon: Daemon,
        session: SessionId,
        options: Option<&SessionOptions>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut settings = Self {
            daemon,
            session,
            allowed: Availability::Enabled,
            pending: 0,
            synced: None,
            shown: None,
            models: None,
            open: false,
        };
        settings.sync(options, window, cx);
        settings
    }

    /// Sends changes to `daemon` from now on, as once the session's
    /// machine connected again.
    pub(crate) fn set_daemon(&mut self, daemon: Daemon) {
        self.daemon = daemon;
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
        options: Option<&SessionOptions>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.synced.as_ref() == options && self.shown.as_ref() == options {
            return;
        }
        cx.notify();
        self.synced = options.cloned();
        self.shown = options.cloned();
        let Some(options) = options else {
            self.models = None;
            return;
        };
        let model = options.model();
        match &self.models {
            Some(picker) if picker.choices == model.available() => {
                set_current_model(&picker.list, model.current(), window, cx);
            }
            _ => {
                self.models = Some(new_model_picker(
                    model.current(),
                    model.available(),
                    window,
                    cx,
                ));
            }
        }
    }

    /// Sends a change. Until the daemon answers, the controls show it and
    /// take no other.
    fn request(&mut self, change: OptionChange, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.availability() == Availability::Disabled {
            return;
        }
        if let Some(shown) = &mut self.shown {
            shown.apply_change(&change);
        }
        if let (Some(picker), OptionChange::Model(model)) = (&self.models, &change) {
            set_current_model(&picker.list, model, window, cx);
        }
        self.pending += 1;
        cx.notify();

        let daemon = self.daemon.clone();
        let session = self.session.clone();
        let name = setting_name(&change);
        cx.spawn_in(window, async move |this, cx| {
            let result = daemon.set_option(&session, change).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending -= 1;
                match result {
                    Ok(options) => {
                        this.sync(Some(&options), window, cx);
                        cx.emit(SettingsEvent::Changed(options));
                    }
                    Err(error) => {
                        // Put back what the daemon still has.
                        let synced = this.synced.take();
                        this.sync(synced.as_ref(), window, cx);
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

    /// Whether no change is on its way to the daemon, which answers one
    /// made during a turn once the turn ends.
    pub(crate) fn is_settled(&self) -> bool {
        self.pending == 0
    }

    fn close_panel(&mut self, cx: &mut Context<'_, Self>) {
        if self.open {
            self.open = false;
            cx.notify();
        }
    }

    /// The permission mode menu, for the left of the composer footer.
    pub(crate) fn render_mode(this: &Entity<Self>, cx: &App) -> impl IntoElement {
        let settings = this.read(cx);
        let availability = settings.availability();
        let weak = this.downgrade();
        h_flex().gap_1().children(
            settings
                .shown
                .as_ref()
                .and_then(SessionOptions::permission_mode)
                .map(|modes| mode_menu(modes, availability, &weak, cx)),
        )
    }

    /// The button that names the model and opens the settings panel, for the
    /// right of the composer footer. Nothing until the session's options
    /// arrive.
    pub(crate) fn render_panel_button(this: &Entity<Self>, cx: &App) -> Option<impl IntoElement> {
        let settings = this.read(cx);
        let options = settings.shown.clone()?;
        let list = settings.models.as_ref()?.list.clone();
        let focus = list.focus_handle(cx);
        let availability = settings.availability();
        let weak = this.downgrade();
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
                .track_focus(&focus)
                .trigger(
                    Button::new("config-panel")
                        .custom(footer_button(cx))
                        .small()
                        .when(options.fast_mode() == Some(true), |this| {
                            this.icon(Lucide::Zap).tooltip("Fast mode")
                        })
                        .label(summary(&options))
                        .dropdown_caret(true)
                        .disabled(availability == Availability::Disabled),
                )
                .content(move |_, _, cx| render_panel(&list, &options, availability, &weak, cx)),
        )
    }
}

/// "Claude Opus 5.5 · High": the model and its reasoning level. Fast mode is
/// the button's icon instead.
fn summary(options: &SessionOptions) -> SharedString {
    let model = options.model().current_choice().map(ModelChoice::label);
    let reasoning = options
        .reasoning()
        .map(|reasoning| reasoning.current().label());
    let parts: Vec<&str> = model.into_iter().chain(reasoning).collect();
    if parts.is_empty() {
        "Settings".into()
    } else {
        parts.join(" · ").into()
    }
}

fn setting_name(change: &OptionChange) -> &'static str {
    match change {
        OptionChange::PermissionMode(_) => "mode",
        OptionChange::Model(_) => "model",
        OptionChange::Reasoning(_) => "reasoning",
        OptionChange::FastMode(_) => "fast mode",
    }
}

fn reasoning_id(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Off => "off",
        ReasoningLevel::Minimal => "minimal",
        ReasoningLevel::Low => "low",
        ReasoningLevel::Medium => "medium",
        ReasoningLevel::High => "high",
        ReasoningLevel::Xhigh => "xhigh",
        ReasoningLevel::Max => "max",
    }
}

/// A borderless button for the composer footer. Kit's ghost button hovers
/// with `accent`, which the dark theme makes the same color as the
/// composer's `secondary` background, so its hover can't be seen there. This
/// one tints the background with the foreground color instead, which shows
/// on any surface.
fn footer_button(cx: &App) -> ButtonCustomVariant {
    let foreground = cx.theme().foreground;
    ButtonCustomVariant::new(cx)
        .hover(foreground.alpha(0.08))
        .active(foreground.alpha(0.12))
}

/// A footer button naming the current permission mode, opening a menu of the
/// others.
fn mode_menu(
    modes: &Choices<PermissionMode>,
    availability: Availability,
    settings: &WeakEntity<SessionSettings>,
    cx: &App,
) -> impl IntoElement {
    let current = modes.current();
    let available = modes.available().to_vec();
    let settings = settings.clone();
    Button::new("config-mode")
        .custom(footer_button(cx))
        .small()
        .icon(Lucide::Shield)
        .label(current.label())
        .dropdown_caret(true)
        .accessibility_label("Mode")
        .disabled(availability == Availability::Disabled)
        .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
            available.iter().copied().fold(menu, |menu, mode| {
                let settings = settings.clone();
                menu.item(
                    PopupMenuItem::new(mode.label())
                        .checked(mode == current)
                        .on_click(move |_, window, cx| {
                            let _ = settings.update(cx, |this, cx| {
                                this.request(OptionChange::PermissionMode(mode), window, cx);
                            });
                        }),
                )
            })
        })
}

/// The panel's content: models on the left, reasoning and fast mode on the
/// right.
fn render_panel(
    models: &Entity<ListState<ModelList>>,
    options: &SessionOptions,
    availability: Availability,
    settings: &WeakEntity<SessionSettings>,
    cx: &App,
) -> impl IntoElement {
    let reasoning = options.reasoning();
    let fast_mode = options.fast_mode();
    let has_side = reasoning.is_some() || fast_mode.is_some();
    let disabled = availability == Availability::Disabled;
    h_flex()
        .items_stretch()
        .gap_3()
        // Ten rows of about two rem each, under the search field.
        .child(
            v_flex().w(rems(18.)).h(rems(22.)).child(
                List::new(models)
                    .small()
                    .search_placeholder("Search models"),
            ),
        )
        .when(has_side, |this| this.child(Separator::vertical()))
        .when(has_side, |this| {
            this.child(
                v_flex()
                    .w(rems(13.))
                    .gap_2()
                    .when_some(reasoning, |this, reasoning| {
                        this.child(render_reasoning(reasoning, availability, settings, cx))
                    })
                    .when_some(fast_mode, |this, on| {
                        let settings = settings.clone();
                        this.child(
                            h_flex()
                                .px_3()
                                .py_1()
                                .justify_between()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(Icon::new(Lucide::Zap).small())
                                        .child("Fast mode"),
                                )
                                .child(
                                    Switch::new("config-fast_mode")
                                        .small()
                                        .checked(on)
                                        .accessibility_label("Fast mode")
                                        .disabled(disabled)
                                        .on_change(move |on: &bool, window, cx| {
                                            let _ = settings.update(cx, |this, cx| {
                                                this.request(
                                                    OptionChange::FastMode(*on),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }),
                                ),
                        )
                    }),
            )
        })
}

/// The reasoning levels offered by the current model.
fn render_reasoning(
    reasoning: &Choices<ReasoningLevel>,
    availability: Availability,
    settings: &WeakEntity<SessionSettings>,
    cx: &App,
) -> impl IntoElement {
    let current = reasoning.current();
    let disabled = availability == Availability::Disabled;
    v_flex()
        .child(
            Label::new("Reasoning")
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .px_3()
                .pb_1(),
        )
        .children(reasoning.available().iter().copied().map(|level| {
            let settings = settings.clone();
            ListItem::new(SharedString::from(format!(
                "config-reasoning-{}",
                reasoning_id(level)
            )))
            .rounded(cx.theme().radius)
            .check_icon(IconName::Check)
            .confirmed(level == current)
            .disabled(disabled)
            .accessibility_label(level.label())
            .child(level.label())
            .on_click(move |_, window, cx| {
                let _ = settings.update(cx, |this, cx| {
                    this.request(OptionChange::Reasoning(level), window, cx);
                });
            })
        }))
}

fn new_model_picker(
    current: &ModelRef,
    choices: &[ModelChoice],
    window: &mut Window,
    cx: &mut Context<'_, SessionSettings>,
) -> ModelPicker {
    let delegate = ModelList::new(choices.to_vec(), current.clone());
    let list = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
    set_current_model(&list, current, window, cx);
    let subscription = cx.subscribe_in(
        &list,
        window,
        |this: &mut SessionSettings, list, event: &ListEvent, window, cx| match event {
            ListEvent::Confirm(ix) => {
                let Some(model) = list.read(cx).delegate().model_at(*ix).cloned() else {
                    return;
                };
                this.close_panel(cx);
                this.request(OptionChange::Model(model), window, cx);
            }
            ListEvent::Cancel => this.close_panel(cx),
            ListEvent::Select(_) => {}
        },
    );
    ModelPicker {
        choices: choices.to_vec(),
        list,
        _subscription: subscription,
    }
}

/// Marks `model` as the model in use and moves the keyboard selection to it,
/// so arrows start from there.
fn set_current_model(
    list: &Entity<ListState<ModelList>>,
    model: &ModelRef,
    window: &mut Window,
    cx: &mut App,
) {
    list.update(cx, |list, cx| {
        list.delegate_mut().current.clone_from(model);
        let row = list.delegate().row_of(model);
        list.set_selected_index(row, window, cx);
        cx.notify();
    });
}

/// The model choices for a searchable [`List`].
struct ModelList {
    choices: Vec<ModelChoice>,
    /// Indices into `choices` that match the search, in order.
    matches: Vec<usize>,
    /// The model in use.
    current: ModelRef,
}

impl ModelList {
    fn new(choices: Vec<ModelChoice>, current: ModelRef) -> Self {
        Self {
            matches: (0..choices.len()).collect(),
            choices,
            current,
        }
    }

    fn choice_at(&self, ix: IndexPath) -> Option<&ModelChoice> {
        self.choices.get(*self.matches.get(ix.row)?)
    }

    fn model_at(&self, ix: IndexPath) -> Option<&ModelRef> {
        self.choice_at(ix).map(ModelChoice::model)
    }

    /// Where `model` is among the matches.
    fn row_of(&self, model: &ModelRef) -> Option<IndexPath> {
        self.matches
            .iter()
            .position(|&ix| self.choices[ix].model() == model)
            .map(|row| IndexPath::default().row(row))
    }
}

impl ListDelegate for ModelList {
    type Item = ListItem;

    /// Matches the label or `provider/model`.
    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        _: &mut Context<'_, ListState<Self>>,
    ) -> Task<()> {
        self.matches = self
            .choices
            .iter()
            .enumerate()
            .filter(|(_, choice)| {
                contains_ignore_case(choice.label(), query)
                    || contains_ignore_case(&choice.model().to_string(), query)
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
            ListItem::new(SharedString::from(format!("model-{}", choice.model())))
                .rounded(cx.theme().radius)
                .check_icon(IconName::Check)
                .confirmed(choice.model() == &self.current)
                .accessibility_label(choice.label().to_owned())
                .child(Label::new(choice.label().to_owned()).truncate()),
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
