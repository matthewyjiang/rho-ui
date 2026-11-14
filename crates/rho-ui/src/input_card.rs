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

use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

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
        QuestionnaireItemDefinition, QuestionnaireItemStatus, QuestionnaireProgress,
        QuestionnaireState, QuestionnaireSubmission, QuestionnaireSubmit, QuestionnaireTitle,
    },
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
};
use rho_ui_core::{
    Answer, ChoiceKind, Form, InputChoice, InputId, InputRequest, Question, Questions,
    SuggestionStyle, Timeout,
};

/// How often the countdown redraws.
const TICK: Duration = Duration::from_secs(1);

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
    /// named by its tooltip.
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
        h_flex()
            .gap_2()
            .child(
                Icon::new(IconName::TriangleAlert)
                    .small()
                    .text_color(cx.theme().warning),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.request.title().to_owned()),
            )
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
            format!("rho is answering “{fallback}”")
        } else {
            format!(
                "rho answers “{fallback}” in {} unless you start on it",
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

    fn render_questions(
        &self,
        state: &Entity<QuestionnaireState>,
        decline: &InputChoice,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let Form::Questions(questions) = self.request.form() else {
            return gpui_kit::Empty.into_any_element();
        };
        let navigation = state.read(cx).navigation_state();
        let items = questions.questions.iter().map(|question| {
            let item = SharedString::from(question.id.clone());
            let choices = question.choices.iter().map(|choice| {
                div()
                    .id(SharedString::from(format!(
                        "question-{}-{}",
                        question.id, choice.value
                    )))
                    .test_support()
                    .w_full()
                    .child(QuestionnaireChoice::new(
                        state,
                        item.clone(),
                        choice.value.clone(),
                    ))
            });
            QuestionnaireItem::new(state, item.clone())
                .child(QuestionnaireTitle::new(state, item.clone()))
                .child(QuestionnaireDescription::new(state, item.clone()))
                .child(QuestionnaireChoices::new(state, item.clone()).children(choices))
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "question-{}-other",
                            question.id
                        )))
                        .test_support()
                        .child(QuestionnaireInput::new(state, item.clone())),
                )
                .child(QuestionnaireError::new(state, item))
        });
        let nav = |id: &'static str, icon: Lucide, label: &'static str, step: Step| {
            let state = state.clone();
            Button::new(id)
                .small()
                .icon(icon)
                .accessibility_label(label)
                .tooltip(label)
                .on_click(move |_, window, cx| {
                    state.update(cx, |state, cx| match step {
                        Step::Previous => state.go_previous(window, cx),
                        Step::Skip => state.skip_current(window, cx),
                        Step::Next => state.go_next(window, cx),
                    });
                })
        };
        Questionnaire::new(state)
            .small()
            .when(questions.questions.len() > 1, |this| {
                this.child(QuestionnaireProgress::new(state))
            })
            .children(items)
            .child(
                QuestionnaireActions::new(state)
                    .when(navigation.is_previous_visible(), |this| {
                        let previous = nav(
                            "questions-previous",
                            Lucide::ChevronLeft,
                            "Previous question",
                            Step::Previous,
                        );
                        this.child(previous.outline())
                    })
                    .children(self.buttons(std::slice::from_ref(decline), Emphasis::None, cx))
                    .child(div().flex_1())
                    .when(navigation.is_skip_visible(), |this| {
                        let skip = nav(
                            "questions-skip",
                            Lucide::SkipForward,
                            "Skip this question",
                            Step::Skip,
                        );
                        this.child(skip.ghost())
                    })
                    .when(navigation.is_next_visible(), |this| {
                        let next = nav(
                            "questions-next",
                            Lucide::ChevronRight,
                            "Next question",
                            Step::Next,
                        );
                        this.child(next.primary())
                    })
                    // Submitting answers everything: it keeps its word.
                    .when(navigation.is_submit_visible(), |this| {
                        this.child(
                            div()
                                .id("questions-submit")
                                .test_support()
                                .child(QuestionnaireSubmit::new(state).small()),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// A move between a questionnaire's questions.
#[derive(Clone, Copy)]
enum Step {
    Previous,
    Skip,
    Next,
}

/// Whether a row of buttons stresses the one Enter should pick.
#[derive(Clone, Copy)]
enum Emphasis {
    Default,
    None,
}

impl Render for InputCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let body = match &self.body {
            Body::Questions { state, decline } => self.render_questions(state, decline, cx),
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
            .border_color(cx.theme().warning)
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
/// labels its choices "suggested", and shows its own text as the text box's
/// placeholder.
fn item(question: &Question, window: &mut Window, cx: &mut App) -> QuestionnaireItemDefinition {
    let suggestion = question.suggestion.as_ref();
    let picks = suggestion.filter(|suggestion| suggestion.style == SuggestionStyle::Selected);
    let marks = suggestion.filter(|suggestion| suggestion.style == SuggestionStyle::Marked);
    let names = |suggested: Option<&rho_ui_core::Suggestion>, value: &str| {
        suggested.is_some_and(|suggestion| suggestion.choices.iter().any(|v| v == value))
    };
    let choices = question.choices.iter().map(|choice| {
        let label = if names(marks, &choice.value) {
            format!("{} (suggested)", choice.label)
        } else {
            choice.label.clone()
        };
        let definition = QuestionnaireChoiceDefinition::new(choice.value.clone(), label)
            .with_default_selected(names(picks, &choice.value));
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
