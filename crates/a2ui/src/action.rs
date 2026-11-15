//! A component's `Action`: an event for the agent, or a function to run
//! locally.

use serde_json::{Map, Value};
use thiserror::Error;

use crate::expr::{Call, Expr, ExpressionError};

/// What a component does when the user interacts with it.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send an `action` message to the agent.
    Event(Event),
    /// Run a function on the renderer.
    Call(Call),
}

/// An event action: the `action` message to send, unevaluated.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// The event's name.
    pub name: String,
    /// The context, an object whose members may be dynamic.
    pub context: Expr,
    /// A `DynamicString` describing what the user did.
    pub user_message: Option<Expr>,
}

/// Why a value is not an `Action`.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ActionError {
    /// It is not an object holding exactly one of `event` and
    /// `functionCall`.
    #[error("an action must be an object with exactly one of event and functionCall")]
    NotAnAction,
    /// The event has no string `name`.
    #[error("an event action needs a string name")]
    MissingName,
    /// The event has a key other than `name`, `context` and `userMessage`.
    #[error("an event action has unknown key {0:?}")]
    UnknownKey(String),
    /// The event's `context` is not an object.
    #[error("an event action's context must be an object")]
    ContextNotAnObject,
    /// The event's `userMessage` is neither a string, a binding nor a call.
    #[error("an event action's userMessage must be a string, a binding or a call")]
    InvalidUserMessage,
    /// `functionCall` is not a function call.
    #[error("functionCall must be a function call")]
    NotACall,
    /// A dynamic value in it is malformed.
    #[error(transparent)]
    Expression(#[from] ExpressionError),
}

impl Action {
    /// Parses an action in its wire form.
    ///
    /// # Errors
    ///
    /// Fails when the value is not an `Action` of `common_types.json`, or
    /// holds a malformed binding or call.
    pub fn from_json(value: &Value) -> Result<Self, ActionError> {
        let map = value.as_object().ok_or(ActionError::NotAnAction)?;
        match (map.get("event"), map.get("functionCall"), map.len()) {
            (Some(event), None, 1) => Ok(Self::Event(parse_event(event)?)),
            (None, Some(call), 1) => match Expr::from_json(call)? {
                Expr::Call(call) => Ok(Self::Call(call)),
                _ => Err(ActionError::NotACall),
            },
            _ => Err(ActionError::NotAnAction),
        }
    }
}

fn parse_event(event: &Value) -> Result<Event, ActionError> {
    let map = event.as_object().ok_or(ActionError::NotAnAction)?;
    if let Some(key) = map
        .keys()
        .find(|key| !matches!(key.as_str(), "name" | "context" | "userMessage"))
    {
        return Err(ActionError::UnknownKey(key.clone()));
    }
    let name = map
        .get("name")
        .and_then(Value::as_str)
        .ok_or(ActionError::MissingName)?
        .to_owned();
    let context = match map.get("context") {
        None => Expr::Literal(Value::Object(Map::new())),
        Some(context @ Value::Object(_)) => Expr::from_json(context)?,
        Some(_) => return Err(ActionError::ContextNotAnObject),
    };
    let user_message = map
        .get("userMessage")
        .map(|message| match Expr::from_json(message)? {
            Expr::Literal(Value::String(text)) => Ok(Expr::Literal(Value::String(text))),
            dynamic @ (Expr::Path(_) | Expr::Call(_)) => Ok(dynamic),
            _ => Err(ActionError::InvalidUserMessage),
        })
        .transpose()?;
    Ok(Event {
        name,
        context,
        user_message,
    })
}
