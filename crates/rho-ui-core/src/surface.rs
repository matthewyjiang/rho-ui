//! The A2UI surfaces a session's `ui` tool shows.
//!
//! A `ui` call carries its surface as A2UI v1.0 messages
//! ([`rho_protocol::ToolContent::Surface`]), which [`crate::Session`]
//! places in the transcript as an [`crate::Entry::Surface`]. A view keeps
//! a [`SurfaceStore`] beside the session: the session's one A2UI
//! processor, and which call each surface is drawn under. It asks the
//! store for a [`SurfaceTree`], draws that, and gives back what the user
//! did: an edit with [`SurfaceStore::write`], a pressed button with
//! [`SurfaceStore::act`], whose [`SurfaceAction`] goes to the daemon with
//! [`crate::Daemon::surface_action`].

mod field;
mod store;
mod tree;

pub use a2ui::{Action, Pointer};
pub use field::{number_value, FieldKind, Moment};
pub use store::{Acted, SurfaceAction, SurfaceRef, SurfaceStore};
pub use tree::{
    Align, ButtonVariant, Child, ChoiceOption, Failure, ImageFit, ImageSize, Justify, MediaKind,
    Node, SurfaceTree, Target,
};

#[cfg(all(test, feature = "test-support"))]
mod tests;
