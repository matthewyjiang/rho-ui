//! The session settings in the composer footer, laid out by what rho lets a
//! session change. The permission mode sits on the left as a menu. On the
//! right, one button names the model and its reasoning and opens a panel: the
//! model list with search, beside the reasoning levels and fast mode. Under
//! them, once the session has said what they open with, the mode and model of
//! the sessions it starts.
//!
//! The daemon owns the values. [`SessionSettings`] sends a user change itself
//! and tells its owner, a session or a new session's draft, what the daemon
//! answered; options the daemon pushes come in through [`SessionSettings::sync`].
//! A change made while a turn runs applies, and is answered, when the turn
//! ends; the controls wait for it. The sessions it starts are the same, but
//! their choice comes back as a session event, through
//! [`SessionSettings::sync_started`], not in the answer, and the section says
//! when a change to it waits for the turn.

mod started;

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
    SessionId, SessionOptions, StartedSessionChange, StartedSessionDefaults,
};

use crate::session_list::contains_ignore_case;
use started::Applies;

/// Whether the user can change settings right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Enabled,
    /// The session is still opening, or a change is on its way.
    Disabled,
}

/// Whether the session's turn is under way, which holds a change back
/// until it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Turn {
    Idle,
    UnderWay,
}

/// What the daemon answered to a change.
pub(crate) enum SettingsEvent {
    /// The daemon took it; these are all the options now. A change to the
    /// sessions it starts leaves them as they were, and the session hears
    /// of the choice as an event.
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
    /// Whether the session's turn is under way, as its owner last said.
    turn: Turn,
    /// The change on its way, one at a time. Controls wait for the answer.
    in_flight: Option<Change>,
    /// The options as the daemon last sent them.
    synced: Option<SessionOptions>,
    /// What the controls show: `synced` plus a change on its way.
    shown: Option<SessionOptions>,
    /// What the sessions this one starts open with, as the session last
    /// heard. `None` leaves their controls out until the daemon has said.
    started: Option<StartedSessionDefaults>,
    /// What those controls show: `started` plus a change on its way.
    started_shown: Option<StartedSessionDefaults>,
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
            turn: Turn::Idle,
            in_flight: None,
            synced: None,
            shown: None,
            started: None,
            started_shown: None,
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

    /// Says whether the session's turn is under way: a change sent then
    /// waits for its end.
    pub(crate) fn set_turn(&mut self, turn: Turn, cx: &mut Context<'_, Self>) {
        if self.turn != turn {
            self.turn = turn;
            cx.notify();
        }
    }

    fn availability(&self) -> Availability {
        if self.in_flight.is_none() {
            self.allowed
        } else {
            Availability::Disabled
        }
    }

    /// When the change to the sessions it starts on its way applies: once
    /// the turn under way ends, if there is one.
    fn started_applies(&self) -> Applies {
        match (&self.in_flight, self.turn) {
            (Some(Change::Started(_)), Turn::UnderWay) => Applies::WhenTurnEnds,
            _ => Applies::Now,
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

    /// Makes the controls for the sessions this one starts show `defaults`,
    /// the session's choice, once it differs from the one they last had,
    /// dropping any change still shown as requested. Until then a change
    /// the daemon took stays shown: its answer doesn't carry the choice,
    /// and the session's event may come after it. `None` leaves them out:
    /// the daemon hasn't said yet.
    pub(crate) fn sync_started(
        &mut self,
        defaults: Option<&StartedSessionDefaults>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.started.as_ref() == defaults {
            return;
        }
        self.started = defaults.cloned();
        self.started_shown = defaults.cloned();
        cx.notify();
    }

    /// Sends a change. Until the daemon answers, the controls show it and
    /// take no other.
    fn request(&mut self, change: Change, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.availability() == Availability::Disabled {
            return;
        }
        match &change {
            Change::Own(change) => {
                if let Some(shown) = &mut self.shown {
                    shown.apply_change(change);
                }
                if let (Some(picker), OptionChange::Model(model)) = (&self.models, change) {
                    set_current_model(&picker.list, model, window, cx);
                }
            }
            Change::Started(change) => {
                if let Some(shown) = &mut self.started_shown {
                    shown.apply_change(change);
                }
            }
        }
        self.in_flight = Some(change.clone());
        cx.notify();

        let daemon = self.daemon.clone();
        let session = self.session.clone();
        let name = setting_name(&change);
        cx.spawn_in(window, async move |this, cx| {
            // A change to the sessions it starts is answered with the
            // session's own options, unchanged; what the section shows stays
            // until the session's event brings the choice.
            let result = match change {
                Change::Own(change) => daemon.set_option(&session, change).await,
                Change::Started(change) => {
                    daemon.set_started_session_default(&session, change).await
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.in_flight = None;
                match result {
                    Ok(options) => {
                        this.sync(Some(&options), window, cx);
                        cx.emit(SettingsEvent::Changed(options));
                    }
                    Err(error) => {
                        // Put back what the daemon still has.
                        let synced = this.synced.take();
                        this.sync(synced.as_ref(), window, cx);
                        let started = this.started.take();
                        this.sync_started(started.as_ref(), cx);
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
        let started = settings.started_shown.clone();
        let applies = settings.started_applies();
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
                .content(move |_, _, cx| {
                    render_panel(
                        &list,
                        &options,
                        started.as_ref(),
                        applies,
                        availability,
                        &weak,
                        cx,
                    )
                }),
        )
    }
}

/// The panel button's label: [`model_summary`], or "Settings" when there is
/// none.
fn summary(options: &SessionOptions) -> SharedString {
    model_summary(options).unwrap_or_else(|| "Settings".into())
}

/// "Claude Opus 5.5 · High": the model and its reasoning level, when the
/// options name either. Fast mode is the panel button's icon instead.
pub(crate) fn model_summary(options: &SessionOptions) -> Option<SharedString> {
    let model = options.model().current_choice().map(ModelChoice::label);
    let reasoning = options
        .reasoning()
        .map(|reasoning| reasoning.current().label());
    let parts: Vec<&str> = model.into_iter().chain(reasoning).collect();
    (!parts.is_empty()).then(|| parts.join(" · ").into())
}

/// A change the controls send: to the session's own settings, or to what
/// the sessions it starts open with.
#[derive(Clone, Debug)]
enum Change {
    Own(OptionChange),
    Started(StartedSessionChange),
}

/// What `change` changes, for the message that says the daemon refused it.
fn setting_name(change: &Change) -> &'static str {
    match change {
        Change::Own(OptionChange::PermissionMode(_)) => "mode",
        Change::Own(OptionChange::Model(_)) => "model",
        Change::Own(OptionChange::Reasoning(_)) => "reasoning",
        Change::Own(OptionChange::FastMode(_)) => "fast mode",
        Change::Started(StartedSessionChange::PermissionMode(_)) => "the started sessions' mode",
        Change::Started(StartedSessionChange::Model(_)) => "the started sessions' model",
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

/// One entry of a [`choice_menu`].
struct MenuChoice {
    label: SharedString,
    /// Whether it is the choice in effect, which the menu checks.
    checked: bool,
    change: Change,
}

/// Opens a menu of `choices` from `button`, its `anchor` corner at the
/// button's; choosing one sends its change. A long menu scrolls within
/// 18rem, less than the panel's 22rem model list, so one opened from the
/// panel stays inside it: a press outside the panel closes the panel before
/// the menu's item takes the click. The menu stays on a disabled button, as
/// a change in flight disables it: taking the menu away then would drop it
/// before it hands the keyboard back.
fn choice_menu(
    button: Button,
    availability: Availability,
    anchor: Anchor,
    choices: Vec<MenuChoice>,
    settings: &WeakEntity<SessionSettings>,
) -> impl IntoElement {
    let settings = settings.clone();
    button
        .dropdown_caret(true)
        .disabled(availability == Availability::Disabled)
        .dropdown_menu_with_anchor(anchor, move |menu, window, _| {
            let menu = menu
                .scrollable(true)
                .max_h(rems(18.).to_pixels(window.rem_size()));
            choices.iter().fold(menu, |menu, choice| {
                let settings = settings.clone();
                let change = choice.change.clone();
                menu.item(
                    PopupMenuItem::new(choice.label.clone())
                        .checked(choice.checked)
                        .on_click(move |_, window, cx| {
                            let _ = settings.update(cx, |this, cx| {
                                this.request(change.clone(), window, cx);
                            });
                        }),
                )
            })
        })
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
    let choices = modes
        .available()
        .iter()
        .map(|&mode| MenuChoice {
            label: mode.label().into(),
            checked: mode == current,
            change: Change::Own(OptionChange::PermissionMode(mode)),
        })
        .collect();
    choice_menu(
        Button::new("config-mode")
            .custom(footer_button(cx))
            .small()
            .icon(Lucide::Shield)
            .label(current.label())
            .accessibility_label("Mode"),
        availability,
        Anchor::BottomLeft,
        choices,
        settings,
    )
}

/// The panel's content: models on the left, reasoning and fast mode on the
/// right, and under them the sessions it starts, once `started` says what
/// they open with.
fn render_panel(
    models: &Entity<ListState<ModelList>>,
    options: &SessionOptions,
    started: Option<&StartedSessionDefaults>,
    applies: Applies,
    availability: Availability,
    settings: &WeakEntity<SessionSettings>,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .gap_3()
        .child(render_own(models, options, availability, settings, cx))
        .when_some(started, |this, defaults| {
            this.child(Separator::horizontal()).child(started::render(
                defaults,
                applies,
                options,
                availability,
                settings,
                cx,
            ))
        })
}

/// The session's own model, reasoning and fast mode.
fn render_own(
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
                                                    Change::Own(OptionChange::FastMode(*on)),
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
                    this.request(Change::Own(OptionChange::Reasoning(level)), window, cx);
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
                this.request(Change::Own(OptionChange::Model(model)), window, cx);
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
