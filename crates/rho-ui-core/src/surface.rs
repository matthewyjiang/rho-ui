//! The A2UI surfaces a session's `ui` tool shows.
//!
//! A `ui` call carries its surface as A2UI v1.0 messages
//! ([`rho_protocol::ToolContent::Surface`]). [`crate::Session`] places
//! the call's surface in the transcript as an [`crate::Entry::Surface`],
//! and hands its messages on as a [`PlacedSurface`]. A view keeps a
//! [`SurfaceStore`] beside the session: the session's one A2UI processor,
//! and which call each surface is drawn under. It asks the store for a
//! [`SurfaceTree`], draws that, and gives back what the user did: an edit
//! with [`SurfaceStore::write`], a pressed button with
//! [`SurfaceStore::act`], whose [`SurfaceAction`] goes to the daemon with
//! [`crate::Daemon::surface_action`].

mod build;
mod field;
mod store;
mod tree;

pub use a2ui::{Action, Pointer, Severity};
pub use field::{FieldKind, Moment, Problem};
pub use store::{Acted, PlacedSurface, SurfaceAction, SurfaceRef, SurfaceStore};
pub use tree::{
    Align, Axis, Button, ButtonVariant, Card, CheckBox, Child, ChoiceOption, ChoiceStyle, Choices,
    Failure, Field, Icon, ImageFit, ImageSize, Input, Justify, Label, Media, MediaKind, Modal,
    Node, Placeholder, Slider, SliderRange, Stack, SurfaceTree, Tab, Tabs, Target, Text, TextStyle,
};

#[cfg(all(test, feature = "test-support"))]
mod tests;
