//! What a turn asks a person, as the UI shows it: a title, some detail, and
//! the answers on offer. Built from the daemon's input requests.
//!
//! Approvals and plan approvals offer every answer the daemon takes. A
//! question offers its choices only when it is one single-choice question;
//! any other can be declined here and answered in rho's terminal UI.

use std::collections::BTreeMap;

use rho_protocol::{Ask, InputAnswer, Operation, QuestionAsk, Selection};

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

/// One answer on offer for an [`InputRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputChoice {
    id: String,
    label: String,
    kind: ChoiceKind,
    pub(crate) answer: InputAnswer,
}

impl InputChoice {
    fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        kind: ChoiceKind,
        answer: InputAnswer,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            answer,
        }
    }

    /// Stable within its request, such as `allow-once`; see
    /// [`InputRequest::choice`].
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
}

/// An open request. Answer it with [`crate::Daemon::answer`]; any client may,
/// and the first answer wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputRequest {
    id: InputId,
    title: String,
    description: Option<String>,
    choices: Vec<InputChoice>,
}

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

    /// The answers on offer, the default first where there is one.
    pub fn choices(&self) -> &[InputChoice] {
        &self.choices
    }

    /// The choice whose [`InputChoice::id`] is `id`.
    pub fn choice(&self, id: &str) -> Option<&InputChoice> {
        self.choices.iter().find(|choice| choice.id == id)
    }

    pub(crate) fn from_wire(request: rho_protocol::InputRequest) -> Self {
        let id = InputId(request.id.0);
        let (title, description, choices) = match request.ask {
            Ask::Approval(ask) => (
                approval_title(&ask.operation),
                Some(ask.reason).filter(|reason| !reason.is_empty()),
                vec![
                    InputChoice::new(
                        "allow-once",
                        "Allow once",
                        ChoiceKind::Accept,
                        InputAnswer::AllowOnce,
                    ),
                    InputChoice::new(
                        "allow-session",
                        "Allow for this session",
                        ChoiceKind::Other,
                        InputAnswer::AllowForSession,
                    ),
                    InputChoice::new(
                        "deny",
                        "Deny",
                        ChoiceKind::Reject,
                        InputAnswer::Deny { reason: None },
                    ),
                ],
            ),
            Ask::PlanApproval(ask) => {
                let approve = ask.modes.iter().filter_map(|mode| {
                    let (value, name) = mode_label(*mode)?;
                    Some(InputChoice::new(
                        format!("approve-{value}"),
                        format!("Approve in {name}"),
                        ChoiceKind::Other,
                        InputAnswer::ApprovePlan {
                            mode: *mode,
                            feedback: None,
                        },
                    ))
                });
                let keep = InputChoice::new(
                    "keep-planning",
                    "Keep planning",
                    ChoiceKind::Reject,
                    InputAnswer::KeepPlanning { feedback: None },
                );
                (
                    "Carry out this plan?".to_owned(),
                    Some(ask.plan),
                    approve.chain([keep]).collect(),
                )
            }
            Ask::Question(ask) => question(ask),
            // A kind a newer daemon added: nothing here can answer it.
            _ => (
                "rho is waiting for an answer this app can't show".to_owned(),
                Some(ELSEWHERE.to_owned()),
                Vec::new(),
            ),
        };
        Self {
            id,
            title,
            description,
            choices,
        }
    }
}

/// Where to answer what this UI can't.
const ELSEWHERE: &str = "Answer it in rho's terminal UI.";

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

fn question(ask: QuestionAsk) -> (String, Option<String>, Vec<InputChoice>) {
    let decline = InputChoice::new(
        "decline",
        "Decline",
        ChoiceKind::Reject,
        InputAnswer::Decline,
    );
    let prompts: Vec<&str> = ask.questions.iter().map(|q| q.prompt.as_str()).collect();
    let simple = match ask.questions.as_slice() {
        [only] if only.selection == Selection::One && !only.choices.is_empty() => Some(only),
        _ => None,
    };
    let Some(only) = simple else {
        let description = format!("{}\n\n{ELSEWHERE}", prompts.join("\n"));
        return (ask.title, Some(description), vec![decline]);
    };
    let choices = only.choices.iter().enumerate().map(|(ix, choice)| {
        InputChoice::new(
            format!("choice-{ix}"),
            choice.label.clone(),
            ChoiceKind::Other,
            InputAnswer::Answers {
                answers: BTreeMap::from([(only.id.clone(), vec![choice.value.clone()])]),
            },
        )
    });
    let choices = choices.chain([decline]).collect();
    (ask.title, Some(only.prompt.clone()), choices)
}
