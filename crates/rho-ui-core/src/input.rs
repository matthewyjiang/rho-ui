//! What a turn asks a person, as the UI shows it: a title, some detail, and
//! a [`Form`] to answer it with. Built from the daemon's input requests.
//!
//! - An approval offers Allow once, Allow for this session, and Deny, with
//!   a text box for the reason Deny gives the tool.
//! - A plan approval offers each mode the plan may be carried out in, and
//!   Keep planning, with a text box for feedback either gives the model.
//! - A question is a questionnaire of [`Question`]s, answered together, or
//!   declined. It may come with a [`Timeout`]: the daemon answers it itself
//!   at the deadline unless a client holds it first
//!   ([`crate::Daemon::hold`]).

use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rho_protocol::{
    Ask, DefaultStyle, InputAnswer, InputAsker, InputTimeout, Operation, PermissionMode,
    QuestionAsk, Selection,
};

use crate::options::mode_label;

/// Names one open request within its session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputId(pub(crate) u64);

/// How an answer reads, so a host can style it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceKind {
    /// Lets the turn go on once; the default answer.
    Accept,
    /// Turns the request down.
    Reject,
    /// Any other answer.
    Other,
}

/// An answer to send with [`crate::Daemon::answer`]: a choice's, with
/// [`InputChoice::answer`], or a questionnaire's, with [`Answer::questions`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer(pub(crate) InputAnswer);

impl Answer {
    /// Answers a questionnaire: the values each question's answer sends,
    /// by question ID. A question left out goes unanswered.
    pub fn questions(answers: BTreeMap<String, Vec<String>>) -> Self {
        Self(InputAnswer::Answers { answers })
    }
}

/// What a choice answers, and whether it carries the user's text.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Reply {
    Fixed(InputAnswer),
    Deny,
    ApprovePlan(PermissionMode),
    KeepPlanning,
}

/// One answer on offer for an [`InputRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputChoice {
    id: String,
    label: String,
    kind: ChoiceKind,
    reply: Reply,
}

impl InputChoice {
    fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        kind: ChoiceKind,
        reply: Reply,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            reply,
        }
    }

    /// Stable within its request, such as `allow-once`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Button text, such as "Allow once".
    pub fn label(&self) -> &str {
        &self.label
    }

    /// How the answer reads.
    pub fn kind(&self) -> ChoiceKind {
        self.kind
    }

    /// This choice as an answer. A denial carries `text` as its reason,
    /// and a plan approval as its feedback, unless `text` is blank; any
    /// other choice ignores it.
    pub fn answer(&self, text: &str) -> Answer {
        let given = Some(text.trim()).filter(|text| !text.is_empty());
        let given = given.map(str::to_owned);
        Answer(match &self.reply {
            Reply::Fixed(answer) => answer.clone(),
            Reply::Deny => InputAnswer::Deny { reason: given },
            Reply::ApprovePlan(mode) => InputAnswer::ApprovePlan {
                mode: *mode,
                feedback: given,
            },
            Reply::KeepPlanning => InputAnswer::KeepPlanning { feedback: given },
        })
    }
}

/// How a request is answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Form {
    /// One of `choices`, the default first where there is one.
    Choices {
        /// The answers on offer.
        choices: Vec<InputChoice>,
        /// The text some of the choices carry.
        text: Option<TextBox>,
    },
    /// A questionnaire.
    Questions(Questions),
    /// Nothing this app can answer: answer it in rho's terminal UI.
    Unsupported,
}

/// A box for the text some choices carry ([`InputChoice::answer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextBox {
    /// What it asks for, such as "Feedback (optional)".
    pub hint: &'static str,
    /// The choice Enter in the box answers with, if any: a denial, as a
    /// reason typed there asks not to run. A plan's feedback may go with
    /// either answer, so the user picks one.
    pub enter: Option<InputChoice>,
}

/// Questions answered together, or declined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Questions {
    /// No two share an ID, and no question offers two choices of one
    /// value.
    pub questions: Vec<Question>,
    /// Turns them all down.
    pub decline: InputChoice,
    /// When the daemon answers them itself, if it will; `None` once a
    /// client holds the request.
    pub timeout: Option<Timeout>,
}

/// One question of a questionnaire, as the agent asked it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    /// Names the question in [`Answer::questions`].
    pub id: String,
    /// What it asks.
    pub prompt: String,
    /// A short label for the question, such as a tab title.
    pub header: Option<String>,
    /// More about the question.
    pub help: Option<String>,
    /// The answers it offers.
    pub choices: Vec<QuestionChoice>,
    /// Whether an answer may pick several choices, rather than one.
    pub multiple: bool,
    /// Whether an answer may give the user's own text: when the question
    /// allows it, or offers no choices.
    pub takes_text: bool,
    /// Whether the questionnaire's answer must answer it.
    pub required: bool,
    /// The answer the agent suggests, if any.
    pub suggestion: Option<Suggestion>,
}

impl Question {
    /// The label of the choice of value `value`, if one has it.
    fn label_of(&self, value: &str) -> Option<&str> {
        let choice = self.choices.iter().find(|choice| choice.value == value)?;
        Some(&choice.label)
    }
}

/// One choice a [`Question`] offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionChoice {
    /// What an answer sends when it picks this choice.
    pub value: String,
    /// What the user reads.
    pub label: String,
    /// More about the choice.
    pub description: Option<String>,
}

/// The answer the agent suggests for a [`Question`]: as many choices as the
/// question takes, and text of its own where the question takes text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The values of the choices it picks; at most one for a question
    /// that takes one.
    pub choices: Vec<String>,
    /// Its own text, for a question that takes text; never beside a
    /// choice for a question that takes one.
    pub own: Option<String>,
    /// How to offer it.
    pub style: SuggestionStyle,
}

/// How a host offers a [`Suggestion`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionStyle {
    /// Picked already; the user may change it.
    Selected,
    /// Only marked, so the user still picks it.
    Marked,
}

/// When the daemon answers a request itself, and with what, unless a
/// client holds it first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timeout {
    deadline: SystemTime,
    fallback: String,
    reason: String,
}

impl Timeout {
    /// How long until the daemon answers, as of `now`; zero once due.
    pub fn remaining(&self, now: SystemTime) -> Duration {
        self.deadline.duration_since(now).unwrap_or_default()
    }

    /// The answer the daemon gives, as a person reads it: the labels of
    /// the choices it picks, for a questionnaire.
    pub fn fallback(&self) -> &str {
        &self.fallback
    }

    /// Why the agent offered that answer.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// An open request. Answer it with [`crate::Daemon::answer`]; any client may,
/// and the first answer wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputRequest {
    id: InputId,
    title: String,
    description: Option<String>,
    form: Form,
    asker: InputAsker,
}

/// Where to answer what this app can't.
const ELSEWHERE: &str = "Answer it in rho's terminal UI.";

impl InputRequest {
    /// Names the request in [`crate::Daemon::answer`].
    pub fn id(&self) -> InputId {
        self.id
    }

    /// The question, such as "Run `cargo test`?".
    pub fn title(&self) -> &str {
        &self.title
    }

    /// More detail, when there is any: why rho asks, or the plan.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// How to answer it.
    pub fn form(&self) -> &Form {
        &self.form
    }

    /// When the daemon answers the request itself, if it will; `None` once
    /// a client holds it.
    pub fn timeout(&self) -> Option<&Timeout> {
        match &self.form {
            Form::Questions(questions) => questions.timeout.as_ref(),
            _ => None,
        }
    }

    /// Whether a subagent asks, rather than the session's own turn.
    pub fn from_subagent(&self) -> bool {
        self.asker == InputAsker::Subagent
    }

    /// Drops the timeout, as the daemon does once a client holds it.
    pub(crate) fn hold(&mut self) {
        if let Form::Questions(questions) = &mut self.form {
            questions.timeout = None;
        }
    }

    pub(crate) fn from_wire(request: rho_protocol::InputRequest) -> Self {
        let (title, description, form) = match request.ask {
            Ask::Approval(ask) => {
                let deny = InputChoice::new("deny", "Deny", ChoiceKind::Reject, Reply::Deny);
                let choices = vec![
                    InputChoice::new(
                        "allow-once",
                        "Allow once",
                        ChoiceKind::Accept,
                        Reply::Fixed(InputAnswer::AllowOnce),
                    ),
                    InputChoice::new(
                        "allow-session",
                        "Allow for this session",
                        ChoiceKind::Other,
                        Reply::Fixed(InputAnswer::AllowForSession),
                    ),
                    deny.clone(),
                ];
                let text = TextBox {
                    hint: "Reason to deny (optional)",
                    enter: Some(deny),
                };
                (
                    approval_title(&ask.operation),
                    Some(ask.reason).filter(|reason| !reason.is_empty()),
                    Form::Choices {
                        choices,
                        text: Some(text),
                    },
                )
            }
            Ask::PlanApproval(ask) => {
                let approve = ask.modes.iter().filter_map(|mode| {
                    let (value, name) = mode_label(*mode)?;
                    Some(InputChoice::new(
                        format!("approve-{value}"),
                        format!("Approve in {name}"),
                        ChoiceKind::Other,
                        Reply::ApprovePlan(*mode),
                    ))
                });
                let keep = InputChoice::new(
                    "keep-planning",
                    "Keep planning",
                    ChoiceKind::Reject,
                    Reply::KeepPlanning,
                );
                let text = TextBox {
                    hint: "Feedback (optional)",
                    enter: None,
                };
                (
                    "Carry out this plan?".to_owned(),
                    Some(ask.plan),
                    Form::Choices {
                        choices: approve.chain([keep]).collect(),
                        text: Some(text),
                    },
                )
            }
            Ask::Question(ask) => questions(ask, request.timeout),
            // A kind a newer daemon added: nothing here can answer it.
            _ => (
                "rho is waiting for an answer this app can't show".to_owned(),
                Some(ELSEWHERE.to_owned()),
                Form::Unsupported,
            ),
        };
        Self {
            id: InputId(request.id.0),
            title,
            description,
            form,
            asker: request.asker,
        }
    }
}

/// A question request's title, detail and form. Answers name questions by
/// ID and choices by value, so a questionnaire that repeats one can't be
/// answered here.
fn questions(ask: QuestionAsk, timeout: Option<InputTimeout>) -> (String, Option<String>, Form) {
    let QuestionAsk { title, questions } = ask;
    let questions: Vec<Question> = questions.into_iter().map(question).collect();
    let ids: Vec<&str> = questions
        .iter()
        .map(|question| question.id.as_str())
        .collect();
    let answerable = unique(ids)
        && questions.iter().all(|question| {
            unique(
                question
                    .choices
                    .iter()
                    .map(|choice| choice.value.as_str())
                    .collect(),
            )
        });
    if !answerable {
        let why = format!("Two of its questions, or two choices of one, are the same. {ELSEWHERE}");
        return (title, Some(why), Form::Unsupported);
    }
    let decline = InputChoice::new(
        "decline",
        "Decline",
        ChoiceKind::Reject,
        Reply::Fixed(InputAnswer::Decline),
    );
    let timeout = timeout.map(|timeout| Timeout {
        deadline: UNIX_EPOCH + Duration::from_millis(timeout.deadline_ms),
        fallback: fallback(&timeout.answer, &questions),
        reason: timeout.reason,
    });
    let form = Form::Questions(Questions {
        questions,
        decline,
        timeout,
    });
    (title, None, form)
}

fn unique(mut values: Vec<&str>) -> bool {
    let all = values.len();
    values.sort_unstable();
    values.dedup();
    values.len() == all
}

fn approval_title(operation: &Operation) -> String {
    match operation {
        Operation::RunProcess {
            program,
            args,
            shell_command,
            ..
        } => {
            let command = shell_command.clone().unwrap_or_else(|| {
                std::iter::once(program.display().to_string())
                    .chain(args.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            format!("Run `{command}`?")
        }
        Operation::ReadPath { path, .. } => format!("Read {}?", path.display()),
        Operation::WritePath { path, .. } => format!("Write {}?", path.display()),
        Operation::DiscoverInstructions { path, .. } => {
            format!("Read instructions in {}?", path.display())
        }
        Operation::Network { url: Some(url) } => format!("Connect to {url}?"),
        Operation::Network { url: None } => "Use the network?".to_owned(),
        Operation::LoadSkill { name, .. } => format!("Load the skill {name}?"),
        Operation::ControlSession {
            action, workspace, ..
        } => format!(
            "Let the agent {} a session in {}?",
            action.as_str(),
            workspace.display()
        ),
        _ => "Allow this?".to_owned(),
    }
}

fn question(question: rho_protocol::Question) -> Question {
    // A selection a newer daemon sends: one choice answers any, as rho's
    // terminal UI reads it.
    let multiple = question.selection == Selection::Many;
    let takes_text = question.allow_other || question.choices.is_empty();
    let choices: Vec<QuestionChoice> = question
        .choices
        .into_iter()
        .map(|choice| QuestionChoice {
            value: choice.value,
            label: choice.label,
            description: choice.description,
        })
        .collect();
    let suggestion = question.default.map(|default| {
        let (mut picked, own): (Vec<String>, Vec<String>) = default
            .values
            .into_iter()
            .partition(|value| choices.iter().any(|choice| choice.value == *value));
        if !multiple {
            picked.truncate(1);
        }
        let own = own
            .into_iter()
            .next()
            .filter(|_| takes_text && (multiple || picked.is_empty()));
        Suggestion {
            choices: picked,
            own,
            style: match default.style {
                DefaultStyle::Selected => SuggestionStyle::Selected,
                // A style a newer daemon sends: the person still picks.
                _ => SuggestionStyle::Marked,
            },
        }
    });
    Question {
        id: question.id,
        prompt: question.prompt,
        header: question.header,
        help: question.help,
        choices,
        multiple,
        takes_text,
        required: question.required,
        suggestion,
    }
}

/// `answer` as a person reads it: each value as its choice's label, or as
/// the text it is. With several questions, each answer is named by its
/// question's header, else its prompt.
fn fallback(answer: &InputAnswer, questions: &[Question]) -> String {
    let answers = match answer {
        InputAnswer::Answers { answers } => answers,
        InputAnswer::Decline => return "Decline".to_owned(),
        InputAnswer::Dismiss => return "Dismiss".to_owned(),
        _ => return "an answer of its own".to_owned(),
    };
    let named = |question: &Question, values: &[String]| {
        let labels = values.iter().map(|value| {
            question
                .label_of(value)
                .map_or_else(|| format!("“{value}”"), str::to_owned)
        });
        labels.collect::<Vec<_>>().join(", ")
    };
    let answered: Vec<(&Question, &Vec<String>)> = questions
        .iter()
        .filter_map(|question| Some((question, answers.get(&question.id)?)))
        .collect();
    match answered.as_slice() {
        [(question, values)] if questions.len() == 1 => named(question, values),
        _ => answered
            .iter()
            .map(|(question, values)| {
                let name = question.header.as_deref().unwrap_or(&question.prompt);
                format!("{name}: {}", named(question, values))
            })
            .collect::<Vec<_>>()
            .join("; "),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn request(request: &serde_json::Value) -> InputRequest {
        let wire = serde_json::from_value(request.clone()).expect("a valid request");
        InputRequest::from_wire(wire)
    }

    fn ask(ask: &serde_json::Value) -> InputRequest {
        request(&json!({"id": 1, "ask": ask}))
    }

    /// The choices and text box of a request answered by choosing.
    fn choices(request: &InputRequest) -> (&[InputChoice], Option<&TextBox>) {
        match request.form() {
            Form::Choices { choices, text } => (choices, text.as_ref()),
            other => panic!("not answered by choosing: {other:?}"),
        }
    }

    fn questions(request: &InputRequest) -> &Questions {
        match request.form() {
            Form::Questions(questions) => questions,
            other => panic!("not a questionnaire: {other:?}"),
        }
    }

    fn offered(request: &InputRequest) -> Vec<(&str, &str)> {
        let (choices, _) = choices(request);
        choices
            .iter()
            .map(|choice| (choice.id(), choice.label()))
            .collect()
    }

    fn choice<'a>(request: &'a InputRequest, id: &str) -> &'a InputChoice {
        let (choices, _) = choices(request);
        let found = choices.iter().find(|choice| choice.id() == id);
        found.unwrap_or_else(|| panic!("{id} is offered"))
    }

    #[test]
    fn a_denial_carries_its_reason_and_an_allow_carries_none() {
        let request = ask(&json!({"kind": "approval",
            "source": {"kind": "built_in_tool", "name": "bash"},
            "operation": {"kind": "run_process", "program": "ls", "args": [], "cwd": "/w"},
            "reason": "asks"}));
        let (_, text) = choices(&request);
        let text = text.expect("a reason may go with Deny");
        let enter = text.enter.as_ref().expect("Enter in the box denies");
        assert_eq!(enter.id(), "deny", "Enter in the box denies");
        assert_eq!(
            enter.answer("  not that dir \n").0,
            InputAnswer::Deny {
                reason: Some("not that dir".to_owned())
            }
        );
        assert_eq!(enter.answer("  ").0, InputAnswer::Deny { reason: None });
        let allow = choice(&request, "allow-once");
        assert_eq!(allow.answer("ignored").0, InputAnswer::AllowOnce);
    }

    #[test]
    fn a_plan_approval_offers_each_mode_and_carries_feedback() {
        let request = ask(&json!({"kind": "plan_approval", "plan": "1. Refactor",
            "modes": ["allow_edits", "auto"]}));
        assert_eq!(request.description(), Some("1. Refactor"));
        assert_eq!(
            offered(&request),
            [
                ("approve-allow_edits", "Approve in Allow edits"),
                ("approve-auto", "Approve in Auto"),
                ("keep-planning", "Keep planning"),
            ]
        );
        let (_, text) = choices(&request);
        assert!(
            text.is_some_and(|text| text.enter.is_none()),
            "feedback goes with the answer the user picks"
        );
        assert_eq!(
            choice(&request, "approve-auto").answer("tests first").0,
            InputAnswer::ApprovePlan {
                mode: PermissionMode::Auto,
                feedback: Some("tests first".to_owned())
            }
        );
        assert_eq!(
            choice(&request, "keep-planning").answer("split step 2").0,
            InputAnswer::KeepPlanning {
                feedback: Some("split step 2".to_owned())
            }
        );
    }

    fn question_json(id: &str, selection: &str) -> serde_json::Value {
        json!({"id": id, "prompt": format!("Pick for {id}"), "selection": selection,
            "allow_other": true, "required": false,
            "choices": [{"value": "v1", "label": "One"}, {"value": "v2", "label": "Two"}]})
    }

    #[test]
    fn a_question_carries_every_question_and_can_be_declined() {
        let mut many = question_json("q2", "many");
        many["header"] = json!("Second");
        let request = ask(&json!({"kind": "question", "title": "Which?",
            "questions": [question_json("q1", "one"), many, question_json("q3", "ranked")]}));
        assert_eq!(request.title(), "Which?");
        let form = questions(&request);
        assert_eq!(form.decline.answer("").0, InputAnswer::Decline);
        let questions = &form.questions;
        assert_eq!(questions.len(), 3);
        assert!(!questions[0].multiple && questions[1].multiple);
        assert!(
            !questions[2].multiple,
            "a selection this app doesn't know takes one choice"
        );
        assert_eq!(questions[1].header.as_deref(), Some("Second"));
    }

    /// A suggestion names choices by value and gives text of its own in
    /// the same list; each question gets what it can take.
    #[test]
    fn a_suggestion_splits_into_choices_and_text_the_question_takes() {
        let suggest = |selection: &str, allow_other: bool, values: serde_json::Value| {
            let mut question = question_json("q", selection);
            question["allow_other"] = json!(allow_other);
            question["default"] = json!({"values": values, "style": "focused"});
            let request = ask(&json!({"kind": "question", "title": "T", "questions": [question]}));
            let suggestion = questions(&request).questions[0].suggestion.clone();
            let suggestion = suggestion.expect("the question has a suggestion");
            assert_eq!(suggestion.style, SuggestionStyle::Marked);
            (suggestion.choices, suggestion.own)
        };
        let all = json!(["v2", "mine", "v1"]);
        assert_eq!(
            suggest("many", true, all.clone()),
            (
                vec!["v2".to_owned(), "v1".to_owned()],
                Some("mine".to_owned())
            )
        );
        assert_eq!(
            suggest("one", true, all.clone()),
            (vec!["v2".to_owned()], None),
            "one choice, and no text beside it"
        );
        assert_eq!(
            suggest("one", true, json!(["mine"])),
            (Vec::new(), Some("mine".to_owned()))
        );
        assert_eq!(
            suggest("many", false, all),
            (vec!["v2".to_owned(), "v1".to_owned()], None),
            "no text for a question that takes none"
        );
    }

    #[test]
    fn questions_that_repeat_an_id_or_a_value_are_answered_elsewhere() {
        let mut twice = question_json("q2", "one");
        twice["choices"] = json!([{"value": "v", "label": "A"}, {"value": "v", "label": "B"}]);
        for questions in [
            json!([question_json("q1", "one"), question_json("q1", "many")]),
            json!([twice]),
        ] {
            let request = ask(&json!({"kind": "question", "title": "T", "questions": questions}));
            assert_eq!(request.form(), &Form::Unsupported);
            assert!(
                request
                    .description()
                    .is_some_and(|text| text.contains("terminal UI")),
                "it says where to answer: {:?}",
                request.description()
            );
        }
    }

    #[test]
    fn a_timeout_reads_its_fallback_as_the_choices_it_picks() {
        let mut second = question_json("q2", "many");
        second["header"] = json!("Second");
        let two = request(&json!({"id": 4,
            "ask": {"kind": "question", "title": "Which?",
                "questions": [question_json("q1", "one"), second]},
            "timeout": {"deadline_ms": 10_000, "reason": "safe defaults",
                "answer": {"kind": "answers", "answers": {"q1": ["v1"], "q2": ["v2", "mine"]}}}}));
        let timeout = two.timeout().expect("the request times out");
        assert_eq!(timeout.fallback(), "Pick for q1: One; Second: Two, “mine”");
        assert_eq!(timeout.reason(), "safe defaults");
        let now = UNIX_EPOCH + Duration::from_millis(4_000);
        assert_eq!(timeout.remaining(now), Duration::from_secs(6));
        let late = UNIX_EPOCH + Duration::from_secs(60);
        assert_eq!(timeout.remaining(late), Duration::ZERO);

        let one = request(&json!({"id": 5,
            "ask": {"kind": "question", "title": "Which?", "questions": [question_json("q1", "one")]},
            "timeout": {"deadline_ms": 1, "reason": "r",
                "answer": {"kind": "answers", "answers": {"q1": ["v2"]}}}}));
        assert_eq!(one.timeout().map(Timeout::fallback), Some("Two"));
    }
}
