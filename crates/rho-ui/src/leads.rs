//! Leads: sessions opened as rho's [`LEAD_AGENT`], which run a goal
//! through sessions they start. A lead works in a folder of its own, in no
//! project; [`crate::no_project`] makes it.

use rho_ui_core::LEAD_AGENT;

/// Whether a session is a lead: one opened as rho's [`LEAD_AGENT`], which
/// runs a goal through sessions it starts. A lead works in a folder of its
/// own, in no project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Session,
    Lead,
}

impl Kind {
    /// The kind of a session that runs as `agent`, by id.
    pub(crate) fn of(agent: &str) -> Self {
        if agent == LEAD_AGENT {
            Kind::Lead
        } else {
            Kind::Session
        }
    }

    /// What the empty composer of such a session reads.
    pub(crate) fn placeholder(self) -> &'static str {
        match self {
            Kind::Session => "Ask Rho to do something",
            Kind::Lead => "What should this lead get done?",
        }
    }
}
