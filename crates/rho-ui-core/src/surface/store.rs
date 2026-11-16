//! A session's A2UI processor, and which call each surface is drawn under.
//!
//! A [`crate::Session`] places each `ui` call's surface in its transcript
//! as an [`crate::Entry::Surface`]. Whoever draws the transcript keeps one
//! store beside the session and shows it those entries, in order
//! ([`SurfaceStore::show`]). The daemon checked every message before
//! sending it, so the store trusts them, and applies each call's once.
//! Nothing here outlasts the session's attachment: a client that fell
//! behind attaches again with a new session and a new store, and the
//! replay shows every call again, in order.

use std::{collections::BTreeSet, sync::Arc, time::SystemTime};

use a2ui::{
    Action, Catalog, Effect, Functions, Interaction, MessageProcessor, Pointer, RendererMessage,
    UserActivation, Validation,
};
use serde_json::Value;

use super::tree::{self, SurfaceTree, Target};

/// A surface a `ui` call showed: the transcript's entry for it. Cheap to
/// clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceRef {
    pub(crate) call: String,
    pub(crate) surface: String,
    /// The A2UI messages the call carried, which show the surface.
    pub(crate) messages: Arc<[Value]>,
}

impl SurfaceRef {
    /// The ID of the tool call that showed the surface.
    pub fn call(&self) -> &str {
        &self.call
    }

    /// The surface's ID, unique within its session.
    pub fn surface(&self) -> &str {
        &self.surface
    }
}

/// What the user did on a surface, for the daemon:
/// [`crate::Daemon::surface_action`] sends it.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceAction {
    pub(crate) surface: String,
    pub(crate) action: Value,
    pub(crate) data_model: Option<Value>,
}

impl SurfaceAction {
    /// The surface the user acted on.
    pub fn surface(&self) -> &str {
        &self.surface
    }
}

/// What pressing a button did, besides what it wrote to the data model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Acted {
    /// The action to send the session, when the button's action is an
    /// event.
    pub send: Option<SurfaceAction>,
    /// Links to open, when the action runs `openUrl`: `http`, `https`,
    /// `mailto` or `tel`, already checked.
    pub open: Vec<String>,
    /// Why the action could not run, if it could not.
    pub error: Option<String>,
}

/// The surfaces of one session.
#[derive(Debug)]
pub struct SurfaceStore {
    processor: MessageProcessor,
    /// The calls whose messages the processor has.
    applied: BTreeSet<String>,
    /// Each surface a call showed and the latest call that showed it, the
    /// one whose card it is drawn under; most recently shown last.
    shown: Vec<(String, String)>,
    /// Surfaces the daemon refused an action on: the session no longer
    /// has them as shown here.
    stale: BTreeSet<String>,
}

impl Default for SurfaceStore {
    fn default() -> Self {
        let mut processor = MessageProcessor::new(Validation::Skip);
        processor
            .add_catalog(Catalog::basic().clone(), Functions::basic())
            .expect("the basic catalog compiles");
        Self {
            processor,
            applied: BTreeSet::new(),
            shown: Vec::new(),
            stale: BTreeSet::new(),
        }
    }
}

impl SurfaceStore {
    /// Applies the messages of `shown`, a transcript's entry, unless it
    /// has that call's already, and says whether it applied them.
    pub fn show(&mut self, shown: &SurfaceRef) -> bool {
        self.apply(&shown.call, &shown.surface, &shown.messages)
    }

    /// Applies the `messages` call `call` carries about `surface`, unless
    /// it has them already, and says whether it applied them. A message
    /// the processor refuses is skipped: the daemon checked them all, so
    /// one refused here means its A2UI is newer than this one.
    pub fn apply(&mut self, call: &str, surface: &str, messages: &[Value]) -> bool {
        if !self.applied.insert(call.to_owned()) {
            return false;
        }
        let _ = self.processor.process(&Value::Array(messages.to_vec()));
        self.shown.retain(|(shown, _)| shown != surface);
        self.shown.push((surface.to_owned(), call.to_owned()));
        self.stale.remove(surface);
        true
    }

    /// The call whose card `surface` is drawn under, while it exists.
    pub fn drawn_under(&self, surface: &str) -> Option<&str> {
        self.processor.surface(surface)?;
        self.shown
            .iter()
            .find_map(|(shown, call)| (shown == surface).then_some(call.as_str()))
    }

    /// `surface` as it stands, or `None` when there is no such surface or
    /// it has no `root` yet.
    pub fn tree(&self, surface: &str) -> Option<SurfaceTree> {
        tree::build(&self.processor, surface)
    }

    /// Whether the daemon refused an action on `surface`, which then
    /// takes no more.
    pub fn is_stale(&self, surface: &str) -> bool {
        self.stale.contains(surface)
    }

    /// Records that the daemon refused an action on `surface`: the
    /// session no longer has it as shown here. A later call that shows
    /// the surface again makes it current.
    pub fn mark_stale(&mut self, surface: &str) {
        self.stale.insert(surface.to_owned());
    }

    /// The value at `binding` in `surface`'s data model.
    pub fn read(&self, surface: &str, binding: &Pointer) -> Option<&Value> {
        self.processor.surface(surface)?.data().get(binding)
    }

    /// Writes what the user entered at `binding`, as a two-way binding
    /// does. A write the data model refuses, as one through a value that
    /// is not a container, changes nothing.
    pub fn write(&mut self, surface: &str, binding: &Pointer, value: Value) {
        if let Some(surface) = self.processor.surface_mut(surface) {
            let _ = surface.set_data(binding, value);
        }
    }

    /// Runs `action` of `target`, which the user just activated. An event
    /// becomes the message to send the session; a function runs here, and
    /// what it wrote is in the data model when this returns.
    pub fn act(&mut self, surface: &str, target: &Target, action: &Action) -> Acted {
        let interaction = Interaction {
            surface_id: surface,
            source_component_id: target.component(),
            scope: target.scope.clone(),
            activation: UserActivation::Present,
            metadata: None,
            at: SystemTime::now(),
        };
        let Some(dispatched) = self.processor.dispatch_action(&interaction, action) else {
            return Acted::default();
        };
        let mut acted = Acted::default();
        for message in &dispatched.outbound {
            match message {
                RendererMessage::Action(_) => {
                    acted.send = Some(SurfaceAction {
                        surface: surface.to_owned(),
                        action: message.to_json(),
                        data_model: dispatched.data_model.clone(),
                    });
                }
                RendererMessage::Error(error) => acted.error = Some(error.message().to_owned()),
                // Only an agent's function call is answered with one.
                RendererMessage::FunctionResponse(_) => {}
            }
        }
        for effect in dispatched.effects {
            match effect {
                Effect::OpenUrl(url) => acted.open.push(url),
                // The processor applied it to the data model.
                Effect::SetData { .. } => {}
            }
        }
        acted
    }
}
