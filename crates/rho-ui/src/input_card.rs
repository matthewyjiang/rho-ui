//! The card pinned above a session's composer while a turn waits for a
//! person: permission to run a tool, approval of a plan, or questions.
//!
//! An approval or a plan approval is a row of buttons, with a text box for
//! what some of them carry: a denial's reason, a plan approval's feedback.
//! Questions are GPUI Kit's questionnaire, one question at a time, beside
//! Decline.
//!
//! A request with a timeout shows its countdown and the answer the daemon
//! gives at the deadline. The user's first key or click in the card holds
//! it ([`InputCardEvent::Hold`]), so it waits for them.

use std::{collections::BTreeMap, time::Duration};

use web_time::SystemTime;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    questionnaire::{
        Questionnaire, QuestionnaireActions, QuestionnaireChoice, QuestionnaireChoiceDefinition,
        QuestionnaireChoices, QuestionnaireDescription, QuestionnaireError, QuestionnaireEvent,
        QuestionnaireInput, QuestionnaireInputDefinition, QuestionnaireItem,
        QuestionnaireItemDefinition, QuestionnaireItemStatus, QuestionnaireNext,
        QuestionnairePrevious, QuestionnaireProgress, QuestionnaireSkip, QuestionnaireState,
        QuestionnaireSubmission, QuestionnaireSubmit, QuestionnaireTitle,
    },
    tag::Tag,
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, Size,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, relative, rems, AnyElement, App, AppContext as _, Context, Entity, EventEmitter,
    FocusHandle, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Render, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled as _,
    Subscription, Task, Window,
};
use rho_ui_core::{
    Answer, ChoiceKind, Form, InputChoice, InputId, InputRequest, Question, Questions,
    SuggestionStyle, Timeout,
};

/// How often the countdown redraws.
const TICK: Duration = Duration::from_secs(1);

/// A questionnaire's text size, in rems: the app's small text. Kit's own
/// sizes are in pixels, so they stay put when the UI scale changes; this
/// one goes to Kit as [`Size::Size`], which sizes the choices' text,
/// padding and indicators from it.
const QUESTION_TEXT_REMS: f32 = 0.875;

/// A choice's padding left of its indicator, as a share of Kit's
/// [`Size::Size`]. The text box starts on the same edge.
const CHOICE_INSET: f32 = 0.75;

/// What the user did with the card.
pub(crate) enum InputCardEvent {
    /// Send `answer` to request `id`. The card takes no other answer until
    /// [`InputCard::answer_failed`].
    Answer(InputId, Answer),
    /// The user started on request `id`, which has a timeout: hold it.
    Hold(InputId),
}

/// How the card is answered: [`Form`], with what the user fills in.
enum Body {
    Choices {
        choices: Vec<InputChoice>,
        /// The text box, and the choice Enter in it answers with, if any.
        text: Option<(Entity<InputState>, Option<InputChoice>)>,
    },
    Questions {
        state: Entity<QuestionnaireState>,
        decline: InputChoice,
    },
    Unsupported,
}

/// The requests waiting behind the card's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Queue {
    /// How many.
    pub(crate) behind: usize,
    /// The soonest timeout among them: the daemon may answer one before
    /// its card shows.
    pub(crate) timeout: Option<Timeout>,
}

impl Queue {
    /// The requests after the first of `inputs`.
    pub(crate) fn of(inputs: &[InputRequest]) -> Self {
        let behind = inputs.get(1..).unwrap_or_default();
        let now = SystemTime::now();
        let timeout = behind
            .iter()
            .filter_map(InputRequest::timeout)
            .min_by_key(|timeout| timeout.remaining(now));
        Self {
            behind: behind.len(),
            timeout: timeout.cloned(),
        }
    }
}

/// The oldest open [`InputRequest`] and what the user has filled in.
pub(crate) struct InputCard {
    request: InputRequest,
    queue: Queue,
    body: Body,
    /// Tracked by the card, so its owner can tell the keyboard is in it.
    focus_handle: FocusHandle,
    /// Tracked by the default button.
    default_focus: FocusHandle,
    /// An answer is on its way to the daemon.
    sending: bool,
    /// A hold went out for this request.
    held: bool,
    _subscriptions: Vec<Subscription>,
    /// Redraws the countdown.
    tick: Option<Task<()>>,
}

impl EventEmitter<InputCardEvent> for InputCard {}

impl InputCard {
    pub(crate) fn new(
        request: InputRequest,
        queue: Queue,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        let body = match request.form() {
            Form::Choices { choices, text } => {
                let text = text.as_ref().map(|text| {
                    let state = cx.new(|cx| InputState::new(window, cx).placeholder(text.hint));
                    subscriptions.push(cx.subscribe_in(
                        &state,
                        window,
                        |this: &mut Self, _, event: &InputEvent, _, cx| {
                            if let InputEvent::PressEnter { .. } = event {
                                this.enter_text(cx);
                            }
                        },
                    ));
                    (state, text.enter.clone())
                });
                Body::Choices {
                    choices: choices.clone(),
                    text,
                }
            }
            Form::Questions(questions) => {
                let state = questionnaire(questions, window, cx);
                subscriptions.push(cx.subscribe_in(
                    &state,
                    window,
                    |this: &mut Self, _, event: &QuestionnaireEvent, _, cx| {
                        if let QuestionnaireEvent::Submit(submission) = event {
                            this.send(Answer::questions(answers(submission)), cx);
                        }
                    },
                ));
                Body::Questions {
                    state,
                    decline: questions.decline.clone(),
                }
            }
            Form::Unsupported => Body::Unsupported,
        };
        let mut card = Self {
            request,
            queue,
            body,
            focus_handle: cx.focus_handle(),
            default_focus: cx.focus_handle(),
            sending: false,
            held: false,
            _subscriptions: subscriptions,
            tick: None,
        };
        card.sync_tick(cx);
        card
    }

    pub(crate) fn id(&self) -> InputId {
        self.request.id()
    }

    /// Takes the daemon's word on the same request, as once it is held,
    /// and on those behind it.
    pub(crate) fn set_request(
        &mut self,
        request: &InputRequest,
        queue: Queue,
        cx: &mut Context<'_, Self>,
    ) {
        if self.request == *request && self.queue == queue {
            return;
        }
        self.request = request.clone();
        self.queue = queue;
        self.sync_tick(cx);
        cx.notify();
    }

    /// The answer didn't reach the daemon: the card takes one again, with
    /// what the user filled in still there.
    pub(crate) fn answer_failed(&mut self, cx: &mut Context<'_, Self>) {
        self.sending = false;
        cx.notify();
    }

    /// Whether the keyboard is in the card.
    pub(crate) fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle.contains_focused(window, cx)
    }

    /// A hold failed, so the next key tries again.
    pub(crate) fn hold_failed(&mut self) {
        self.held = false;
    }

    /// Moves the keyboard into the card: to the current question, or its
    /// choice the agent marked, or else the default answer.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        let Body::Questions { state, .. } = &self.body else {
            window.focus(&self.default_focus, cx);
            return;
        };
        let current = state.read(cx).current_item().cloned();
        let marked = self.marked_choice(current.as_deref());
        state.update(cx, |state, cx| {
            let focused =
                marked.is_some_and(|(item, value)| state.focus_choice(item, value, window, cx));
            if !focused {
                state.focus_current_item(window, cx);
            }
        });
    }

    /// The choice the agent suggests, without picking it, for question
    /// `item`: its ID and the choice's value.
    fn marked_choice(&self, item: Option<&str>) -> Option<(&str, &str)> {
        let Form::Questions(questions) = self.request.form() else {
            return None;
        };
        let question = questions
            .questions
            .iter()
            .find(|question| Some(question.id.as_str()) == item)?;
        let suggestion = question.suggestion.as_ref()?;
        let value = suggestion.choices.first()?;
        (suggestion.style == SuggestionStyle::Marked).then_some((&question.id, value))
    }

    /// The user started on the request: the first time, hold it if it
    /// times out.
    fn start(&mut self, cx: &mut Context<'_, Self>) {
        if !self.held && self.request.timeout().is_some() {
            self.held = true;
            cx.emit(InputCardEvent::Hold(self.request.id()));
        }
    }

    fn send(&mut self, answer: Answer, cx: &mut Context<'_, Self>) {
        if !self.sending {
            self.sending = true;
            cx.emit(InputCardEvent::Answer(self.request.id(), answer));
            cx.notify();
        }
    }

    /// The text the user typed for the choices that carry it.
    fn typed(&self, cx: &App) -> SharedString {
        match &self.body {
            Body::Choices {
                text: Some((text, _)),
                ..
            } => text.read(cx).value(),
            _ => SharedString::default(),
        }
    }

    fn choose(&mut self, choice: &InputChoice, cx: &mut Context<'_, Self>) {
        let answer = choice.answer(&self.typed(cx));
        self.send(answer, cx);
    }

    /// Enter in the text box answers with what was typed, as a denial's
    /// reason or a plan's revisions.
    fn enter_text(&mut self, cx: &mut Context<'_, Self>) {
        let Body::Choices {
            text: Some((_, Some(enter))),
            ..
        } = &self.body
        else {
            return;
        };
        let enter = enter.clone();
        if !self.typed(cx).trim().is_empty() {
            self.choose(&enter, cx);
        }
    }

    /// Redraws once a second while this request, or one behind it, times
    /// out.
    fn sync_tick(&mut self, cx: &mut Context<'_, Self>) {
        if self.request.timeout().is_none() && self.queue.timeout.is_none() {
            self.tick = None;
        } else if self.tick.is_none() {
            self.tick = Some(cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }));
        }
    }

    /// The title, then icons for who asks and what waits behind it, each
    /// named by its tooltip. Questions are no warning, and their title only
    /// names the subject of the question under it, so both are quiet; of
    /// several, it says which one shows.
    fn render_header(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let queue = (self.queue.behind > 0).then(|| {
            let more = format!("{} more waiting", self.queue.behind);
            let left = self
                .queue
                .timeout
                .as_ref()
                .map(|timeout| countdown(timeout.remaining(SystemTime::now())));
            let label = match &left {
                Some(left) => format!("{more}; one answers itself in {left}"),
                None => more,
            };
            hint("input-queue", label, cx)
                .flex_shrink_0()
                .child(Icon::new(Lucide::Layers).xsmall())
                .child(format!("+{}", self.queue.behind))
                .when_some(left, |this, left| {
                    this.text_color(cx.theme().warning)
                        .child(Icon::new(Lucide::Timer).xsmall())
                        .child(left)
                })
                .test_support()
        });
        let questions = match &self.body {
            Body::Questions { state, .. } => Some(state),
            Body::Choices { .. } | Body::Unsupported => None,
        };
        let icon = match questions {
            Some(_) => {
                Icon::new(Lucide::MessageCircleQuestionMark).text_color(cx.theme().muted_foreground)
            }
            None => Icon::new(IconName::TriangleAlert).text_color(cx.theme().warning),
        };
        let progress = questions
            .filter(|state| state.read(cx).total() > 1)
            .map(|state| {
                // Kit sizes this in pixels too; see `QUESTION_TEXT_REMS`.
                QuestionnaireProgress::new(state)
                    .small()
                    .flex_shrink_0()
                    .text_xs()
                    .line_height(rems(1.))
            });
        h_flex()
            .gap_2()
            .child(icon.small())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .when(questions.is_some(), |this| {
                        this.text_color(cx.theme().muted_foreground)
                    })
                    .child(self.request.title().to_owned()),
            )
            .children(progress)
            .when(self.request.from_subagent(), |this| {
                this.child(
                    hint("input-subagent", "A subagent asks".into(), cx)
                        .flex_shrink_0()
                        .child(Icon::new(Lucide::Bot).xsmall())
                        .test_support(),
                )
            })
            .children(queue)
    }

    /// The countdown to the daemon's own answer, the answer, and why the
    /// agent offered it.
    fn render_timeout(&self, cx: &Context<'_, Self>) -> Option<AnyElement> {
        let timeout = self.request.timeout()?;
        let left = timeout.remaining(SystemTime::now());
        let fallback = timeout.fallback();
        let label = if left.is_zero() {
            format!("Rho is answering “{fallback}”")
        } else {
            format!(
                "Rho answers “{fallback}” in {} unless you start on it",
                countdown(left)
            )
        };
        let reason = timeout.reason().to_owned();
        Some(
            hint("input-timeout", label, cx)
                .text_color(cx.theme().warning)
                .child(Icon::new(Lucide::Timer).xsmall())
                .child(countdown(left))
                .child(Icon::new(Lucide::ArrowRight).xsmall())
                .child(format!("“{fallback}”"))
                .when(!reason.is_empty(), |this| {
                    this.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(reason),
                    )
                })
                .test_support()
                .into_any_element(),
        )
    }

    fn render_description(&self, cx: &Context<'_, Self>) -> Option<AnyElement> {
        let description = self.request.description()?.to_owned();
        // A plan runs long: it scrolls inside the card, so the answers stay
        // on screen.
        Some(
            div()
                .id("approval-detail")
                .max_h(rems(12.))
                .overflow_y_scroll()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .font_family(cx.theme().mono_font_family.clone())
                .child(description)
                .into_any_element(),
        )
    }

    /// `choices` as buttons. With [`Emphasis::Default`], the choice Enter should pick
    /// is primary and tracks [`Self::default_focus`]: the one that lets the
    /// turn go on once, else the first.
    fn buttons(
        &self,
        choices: &[InputChoice],
        emphasis: Emphasis,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let default_ix = match emphasis {
            Emphasis::Default => choices
                .iter()
                .position(|choice| choice.kind() == ChoiceKind::Accept)
                .or(Some(0)),
            Emphasis::None => None,
        };
        let choices = choices.iter().cloned().enumerate();
        choices
            .map(|(ix, choice)| {
                let kind = choice.kind();
                Button::new(SharedString::from(format!("approval-{}", choice.id())))
                    .small()
                    .label(choice.label().to_owned())
                    .disabled(self.sending)
                    .map(|button| {
                        if Some(ix) == default_ix {
                            button.primary().track_focus(&self.default_focus)
                        } else if kind == ChoiceKind::Reject {
                            button.outline()
                        } else {
                            button
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(&choice, cx)))
                    .into_any_element()
            })
            .collect()
    }

    /// The question that shows, then Decline on the left of the row, and
    /// the moves between questions on its right.
    fn render_questions(
        &self,
        state: &Entity<QuestionnaireState>,
        decline: &InputChoice,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let Form::Questions(questions) = self.request.form() else {
            return gpui_kit::Empty.into_any_element();
        };
        let text = window.rem_size() * QUESTION_TEXT_REMS;
        let items: Vec<_> = questions
            .questions
            .iter()
            .map(|question| render_question(state, question, text, cx))
            .collect();
        let navigation = state.read(cx).navigation_state();
        Questionnaire::new(state)
            .with_size(Size::Size(text))
            // Kit centers a choice's indicator on a line this tall.
            .text_size(text)
            .line_height(relative(1.5))
            .children(items)
            .child(
                QuestionnaireActions::new(state)
                    .gap_2()
                    .children(self.buttons(std::slice::from_ref(decline), Emphasis::None, cx))
                    .child(div().flex_1())
                    .when(navigation.is_previous_visible(), |this| {
                        let previous = QuestionnairePrevious::new(state).small();
                        this.child(action("questions-previous", previous))
                    })
                    .when(navigation.is_skip_visible(), |this| {
                        let skip = QuestionnaireSkip::new(state).small();
                        this.child(action("questions-skip", skip))
                    })
                    .when(navigation.is_next_visible(), |this| {
                        let next = QuestionnaireNext::new(state).small();
                        this.child(action("questions-next", next))
                    })
                    // Submitting answers everything: it keeps its word.
                    .when(navigation.is_submit_visible(), |this| {
                        let submit = QuestionnaireSubmit::new(state).small();
                        this.child(action("questions-submit", submit))
                    }),
            )
            .into_any_element()
    }
}

/// One of Kit's questionnaire buttons, named `id` for UI tests: Kit's own
/// ID carries its state's entity ID.
fn action(id: &'static str, button: impl IntoElement) -> impl IntoElement {
    div().id(id).test_support().flex_shrink_0().child(button)
}

/// `question` as Kit draws the one that shows: what it asks, its choices,
/// and a box for the user's own text. `text` is the questionnaire's text
/// size.
fn render_question(
    state: &Entity<QuestionnaireState>,
    question: &Question,
    text: Pixels,
    cx: &App,
) -> QuestionnaireItem {
    let item = SharedString::from(question.id.clone());
    let muted = cx.theme().muted_foreground;
    let choices = question.choices.iter().map(|choice| {
        let selected = state
            .read(cx)
            .choice_state(&question.id, &choice.value)
            .is_some_and(|choice| choice.is_selected());
        let label = h_flex()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .font_weight(FontWeight::MEDIUM)
                    .child(choice.label.clone()),
            )
            .when(question.marks(&choice.value), |this| {
                // As tall as the label's line, so it adds nothing to the row.
                let tag = Tag::secondary().outline().small().text_xs();
                this.child(tag.flex_shrink_0().child("Suggested"))
            });
        let description = choice
            .description
            .clone()
            .map(|description| div().text_color(muted).child(description));
        div()
            .id(SharedString::from(format!(
                "question-{}-{}",
                question.id, choice.value
            )))
            .test_support()
            .w_full()
            .child(
                // Kit's text column is a flex item as wide as its
                // unwrapped text; letting it shrink wraps a long
                // description instead of spilling out of the card.
                QuestionnaireChoice::new(state, item.clone(), choice.value.clone())
                    .content_style(StyleRefinement::default().min_w_0())
                    // Kit outlines an empty indicator in the color of an
                    // input's border, which the card's background hides.
                    .when(!selected, |this| {
                        let outline = StyleRefinement::default().border_color(muted.opacity(0.6));
                        this.indicator_style(outline)
                    })
                    .child(label)
                    .children(description),
            )
    });
    // What it asks and the help under it read as one heading; Kit spaces
    // an item's parts evenly, and pads a title that has no help.
    let heading = v_flex()
        .gap_0p5()
        .pb_1()
        .child(
            QuestionnaireTitle::new(state, item.clone())
                .text_base()
                .mb_0(),
        )
        .child(QuestionnaireDescription::new(state, item.clone()).text_size(text));
    QuestionnaireItem::new(state, item.clone())
        .gap_1p5()
        .child(heading)
        .child(
            QuestionnaireChoices::new(state, item.clone())
                .gap_1p5()
                .children(choices),
        )
        // Only where there is a box: an empty wrapper would still take a gap.
        .when(question.takes_text, |this| {
            let other = SharedString::from(format!("question-{}-other", question.id));
            let input = QuestionnaireInput::new(state, item.clone())
                .with_size(Size::Medium)
                .pl(text * CHOICE_INSET);
            this.child(div().id(other).test_support().child(input))
        })
        .child(QuestionnaireError::new(state, item))
}

/// Whether a row of buttons stresses the one Enter should pick.
#[derive(Clone, Copy)]
enum Emphasis {
    Default,
    None,
}

impl Render for InputCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let body = match &self.body {
            Body::Questions { state, decline } => self.render_questions(state, decline, window, cx),
            // The text box fills the buttons' row, so the card is no taller
            // for it.
            Body::Choices { choices, text } => h_flex()
                .flex_wrap()
                .gap_2()
                .children(self.buttons(choices, Emphasis::Default, cx))
                .when_some(text.as_ref(), |this, (text, _)| {
                    this.child(
                        div()
                            .flex_1()
                            .min_w(rems(12.))
                            .child(Input::new(text).id("input-text").small()),
                    )
                })
                .into_any_element(),
            Body::Unsupported => gpui_kit::Empty.into_any_element(),
        };
        v_flex()
            .id("input-card")
            .test_support()
            .track_focus(&self.focus_handle)
            .w_full()
            .gap_2()
            .p_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            // The composer's frame, which the card sits on.
            .border_color(cx.theme().input)
            .bg(cx.theme().secondary)
            // The first key, paste or click in the card starts on it.
            .capture_key_down(cx.listener(|this, _, _, cx| this.start(cx)))
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| this.start(cx)))
            .child(self.render_header(cx))
            .children(self.render_timeout(cx))
            .children(self.render_description(cx))
            .child(body)
    }
}

/// A small row of icons and short text that `label` names, in full, as
/// its tooltip and to assistive technology. Finish it with
/// `.test_support()` for UI tests to find it.
fn hint(id: &'static str, label: String, cx: &App) -> gpui_kit::Stateful<gpui_kit::Div> {
    let tooltip = SharedString::from(label);
    h_flex()
        .id(id)
        .aria_label(tooltip.clone())
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .gap_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
}

/// `left` as minutes and seconds, rounded up: `4:59`.
fn countdown(left: Duration) -> String {
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Kit's questionnaire for `questions`. It takes no shortcut keys: it takes
/// the keyboard as it opens, and a digit or letter meant for the composer
/// would answer a question.
fn questionnaire(
    questions: &Questions,
    window: &mut Window,
    cx: &mut App,
) -> Entity<QuestionnaireState> {
    let items: Vec<_> = questions
        .questions
        .iter()
        .map(|question| item(question, window, cx))
        .collect();
    cx.new(|cx| {
        QuestionnaireState::new(items, cx)
            .expect("the core keeps IDs and values unique, and one suggestion per single choice")
    })
}

/// `question` as Kit's questionnaire item. A suggestion to pick is picked:
/// its choices selected, its own text in the text box. A suggestion to mark
/// names its choices "suggested" to assistive technology, as their tag
/// does on screen, and shows its own text as the text box's placeholder.
fn item(question: &Question, window: &mut Window, cx: &mut App) -> QuestionnaireItemDefinition {
    let suggestion = question.suggestion.as_ref();
    let picks = suggestion.filter(|suggestion| suggestion.style == SuggestionStyle::Selected);
    let marks = suggestion.filter(|suggestion| suggestion.style == SuggestionStyle::Marked);
    let choices = question.choices.iter().map(|choice| {
        let label = if question.marks(&choice.value) {
            format!("{} (suggested)", choice.label)
        } else {
            choice.label.clone()
        };
        let picked = picks.is_some_and(|suggestion| suggestion.choices.contains(&choice.value));
        let definition = QuestionnaireChoiceDefinition::new(choice.value.clone(), label)
            .with_default_selected(picked);
        match &choice.description {
            Some(description) => definition.with_description(description.clone()),
            None => definition,
        }
    });
    let mut definition =
        QuestionnaireItemDefinition::new(question.id.clone(), question.prompt.clone())
            .with_required(question.required)
            .with_multiple(question.multiple)
            .with_choices(choices.collect::<Vec<_>>());
    if let Some(help) = &question.help {
        definition = definition.with_description(help.clone());
    }
    if question.takes_text {
        let placeholder = if question.choices.is_empty() {
            "Your answer"
        } else {
            "Or your own answer"
        };
        let picked = picks.and_then(|suggestion| suggestion.own.clone());
        let marked = marks.and_then(|suggestion| suggestion.own.as_deref());
        let state = cx.new(|cx| {
            let state = InputState::new(window, cx);
            match (picked, marked) {
                (Some(own), _) => state.placeholder(placeholder).default_value(own),
                (None, Some(own)) => state.placeholder(format!("Suggested: {own}")),
                (None, None) => state.placeholder(placeholder),
            }
        });
        definition = definition.with_input(QuestionnaireInputDefinition::new(state, placeholder));
    }
    definition
}

/// A questionnaire's answers as the daemon takes them: each answered
/// question's chosen values, then its own text. A skipped question is left
/// out.
fn answers(submission: &QuestionnaireSubmission) -> BTreeMap<String, Vec<String>> {
    submission
        .items()
        .iter()
        .filter(|item| item.status() == QuestionnaireItemStatus::Answered)
        .map(|item| {
            let answer = item.answer();
            let values = answer
                .choices()
                .iter()
                .map(ToString::to_string)
                .chain(answer.freeform().map(|text| text.trim().to_owned()));
            (item.name().to_string(), values.collect())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_countdown_rounds_up_to_the_second() {
        assert_eq!(countdown(Duration::from_millis(299_001)), "5:00");
        assert_eq!(countdown(Duration::from_secs(299)), "4:59");
        assert_eq!(countdown(Duration::from_millis(400)), "0:01");
    }
}
