//! What a fake session keeps for the sessions it starts: the mode and model
//! a client chose, or a saved choice the daemon can't read, and the changes
//! to it sent while a turn runs, which wait for that turn's end.

use rho_protocol::{
    methods::{ModelRef, StartedSessionDefaults},
    ErrorCode, PermissionMode, RpcError, SessionConfigureResponse, SessionEvent,
};
use tokio::sync::oneshot;

use super::{model_choices, FakeSession, MODELS};

/// What a client chose for the sessions a session starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum StartedChoice {
    Chosen(StartedSessionDefaults),
    /// A saved choice the daemon can't read, as a newer rho's: reported as
    /// an `unknown` mode and no model. A model is refused until a mode
    /// replaces it.
    Unreadable,
}

impl StartedChoice {
    /// What the daemon reports of it.
    pub(super) fn reported(&self) -> StartedSessionDefaults {
        match self {
            Self::Chosen(chosen) => chosen.clone(),
            Self::Unreadable => StartedSessionDefaults {
                permission_mode: Some(PermissionMode::Unknown),
                model: None,
            },
        }
    }
}

/// A change to [`StartedChoice`], `None` clearing a part.
#[derive(Debug)]
pub(super) enum StartedChange {
    Mode(Option<PermissionMode>),
    Model(Option<ModelRef>),
}

/// The daemon's answer to a [`StartedChange`].
pub(super) type StartedAnswer = Result<SessionConfigureResponse, RpcError>;

/// A [`StartedChange`] sent while a turn ran, waiting for its end.
#[derive(Debug)]
pub(super) struct HeldChange {
    change: StartedChange,
    answer: oneshot::Sender<StartedAnswer>,
}

impl FakeSession {
    /// Applies `change` now, or, while a turn runs, once the turn ends, as
    /// rho does; the receiver gets the answer then.
    pub(super) fn change_started(
        &mut self,
        change: StartedChange,
    ) -> oneshot::Receiver<StartedAnswer> {
        let (answer, answered) = oneshot::channel();
        if self.turn.is_some() {
            self.held_started.push(HeldChange { change, answer });
        } else {
            // The client may have gone.
            let _ = answer.send(self.apply_started(change));
        }
        answered
    }

    /// Applies, and answers, the changes held for the turn that ended.
    pub(super) fn apply_held_started(&mut self) {
        for held in std::mem::take(&mut self.held_started) {
            let _ = held.answer.send(self.apply_started(held.change));
        }
    }

    /// Applies `change`, answering with the session's own options, which
    /// stay as they were.
    fn apply_started(&mut self, change: StartedChange) -> StartedAnswer {
        match change {
            StartedChange::Mode(mode) => self.choose_started_mode(mode)?,
            StartedChange::Model(model) => self.choose_started_model(model)?,
        }
        Ok(SessionConfigureResponse {
            options: self.settings.options(),
        })
    }

    /// Sets the mode the sessions it starts open in: refuses `unknown`,
    /// and replaces a saved choice it can't read, model and all.
    fn choose_started_mode(&mut self, mode: Option<PermissionMode>) -> Result<(), RpcError> {
        if mode == Some(PermissionMode::Unknown) {
            return Err(RpcError::new(
                ErrorCode::InvalidParams,
                "unknown is not a permission mode to start sessions in",
            ));
        }
        let model = match &self.started {
            StartedChoice::Chosen(chosen) => chosen.model.clone(),
            StartedChoice::Unreadable => None,
        };
        self.keep_started(StartedSessionDefaults {
            permission_mode: mode,
            model,
        });
        Ok(())
    }

    /// Sets the model the sessions it starts open on: refuses one the
    /// session doesn't offer, and any while it keeps a saved choice it
    /// can't read.
    fn choose_started_model(&mut self, model: Option<ModelRef>) -> Result<(), RpcError> {
        let refuse = |why: &str| Err(RpcError::new(ErrorCode::InvalidParams, why));
        let StartedChoice::Chosen(chosen) = &self.started else {
            return refuse(
                "the saved choice for started sessions can't be read: \
                 choose their permission mode first",
            );
        };
        let offered = |model: &ModelRef| {
            model_choices(&MODELS)
                .iter()
                .any(|choice| choice.model == *model)
        };
        if model.as_ref().is_some_and(|model| !offered(model)) {
            return refuse("the session does not offer that model");
        }
        let chosen = StartedSessionDefaults {
            model,
            ..chosen.clone()
        };
        self.keep_started(chosen);
        Ok(())
    }

    /// Keeps `chosen`, and tells clients of it only if it changed what
    /// the daemon reports, as rho does.
    fn keep_started(&mut self, chosen: StartedSessionDefaults) {
        let changed = self.started.reported() != chosen;
        self.started = StartedChoice::Chosen(chosen.clone());
        if changed {
            self.publish(SessionEvent::StartedSessionDefaults(chosen));
        }
    }
}
