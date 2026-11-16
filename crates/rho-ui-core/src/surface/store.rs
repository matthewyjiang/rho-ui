//! A session's A2UI processor, and which call each surface is drawn under.
//!
//! A view keeps one store beside its [`crate::Session`], and shows it each
//! surface the session places ([`crate::Session::take_placed_surfaces`]).
//! The daemon checked every message before sending it, so the store
//! trusts them; it keeps why the processor refused any anyway. Nothing
//! here outlasts the session's attachment: a client that fell behind
//! attaches again with a new session and a new store, and the replay
//! shows every call again, in order.

use std::{
    collections::{BTreeSet, HashMap},
    time::SystemTime,
};

use a2ui::{
    Action, Catalog, Change, Effect, Functions, Interaction, MessageProcessor, Pointer,
    RendererMessage, UserActivation, Validation,
};
use serde_json::Value;

use super::{
    build,
    tree::{SurfaceTree, Target},
};

/// A surface a `ui` call showed: the transcript's entry for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceRef {
    pub(crate) call: String,
    pub(crate) surface: String,
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

/// A surface a session just placed, with the A2UI messages its call
/// carried, for [`SurfaceStore::show`].
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedSurface {
    pub(crate) shown: SurfaceRef,
    pub(crate) messages: Vec<Value>,
}

impl PlacedSurface {
    /// The transcript's entry for it.
    pub fn shown(&self) -> &SurfaceRef {
        &self.shown
    }
}

/// What the user did on a surface, for the daemon:
/// [`crate::Daemon::surface_action`] sends it.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceAction {
    /// The surface, and the call it was drawn under when the user acted.
    pub(crate) shown: SurfaceRef,
    pub(crate) action: Value,
    pub(crate) data_model: Option<Value>,
}

impl SurfaceAction {
    /// The surface the user acted on.
    pub fn surface(&self) -> &str {
        &self.shown.surface
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
    /// The latest call that showed each surface: the one whose entry it is
    /// drawn under.
    drawn_under: HashMap<String, String>,
    /// The surfaces the daemon refused an action on, with what it said:
    /// the session no longer has them as shown here.
    stale: HashMap<String, String>,
    /// Why the processor refused messages of the latest call that showed
    /// each surface, for the surfaces it refused any of.
    refused: HashMap<String, Vec<String>>,
    /// The surfaces that changed since [`Self::take_changed`].
    changed: BTreeSet<String>,
}

impl Default for SurfaceStore {
    fn default() -> Self {
        let mut processor = MessageProcessor::new(Validation::Skip);
        processor
            .add_catalog(Catalog::basic().clone(), Functions::basic())
            .expect("the basic catalog compiles");
        Self {
            processor,
            drawn_under: HashMap::new(),
            stale: HashMap::new(),
            refused: HashMap::new(),
            changed: BTreeSet::new(),
        }
    }
}

impl SurfaceStore {
    /// Applies the messages of `placed`, whose call draws the surface from
    /// now on, current again if it was stale.
    pub fn show(&mut self, placed: PlacedSurface) {
        let PlacedSurface { shown, messages } = placed;
        let processed = self.processor.process(&Value::Array(messages));
        for change in &processed.changes {
            let (Change::SurfaceCreated { surface }
            | Change::SurfaceDeleted { surface }
            | Change::ComponentAdded { surface, .. }
            | Change::ComponentUpdated { surface, .. }
            | Change::ComponentRemoved { surface, .. }
            | Change::DataUpdated { surface, .. }) = change;
            self.changed.insert(surface.clone());
        }
        let refused: Vec<String> = processed
            .errors()
            .map(|error| error.message().to_owned())
            .collect();
        if refused.is_empty() {
            self.refused.remove(&shown.surface);
        } else {
            self.refused.insert(shown.surface.clone(), refused);
        }
        self.stale.remove(&shown.surface);
        self.changed.insert(shown.surface.clone());
        self.drawn_under.insert(shown.surface, shown.call);
    }

    /// Why the processor refused messages of the latest call that showed
    /// `surface`, which draws without them: the daemon checked them all,
    /// so one refused here means its A2UI is newer than this one.
    pub fn refused(&self, surface: &str) -> &[String] {
        self.refused.get(surface).map_or(&[], Vec::as_slice)
    }

    /// The call whose entry `surface` is drawn under, while it exists or
    /// says why it can't be shown.
    pub fn drawn_under(&self, surface: &str) -> Option<&str> {
        let shows = self.processor.surface(surface).is_some() || self.refused.contains_key(surface);
        self.drawn_under
            .get(surface)
            .filter(|_| shows)
            .map(String::as_str)
    }

    /// Whether the entry `shown` draws its surface.
    pub fn draws(&self, shown: &SurfaceRef) -> bool {
        self.drawn_under(&shown.surface) == Some(shown.call.as_str())
    }

    /// `surface` as it stands, or `None` when there is no such surface or
    /// it has no `root` yet.
    pub fn tree(&self, surface: &str) -> Option<SurfaceTree> {
        build::build(&self.processor, surface)
    }

    /// What the daemon said when it refused an action on `surface`, which
    /// then takes no more; `None` while the surface is current.
    pub fn stale(&self, surface: &str) -> Option<&str> {
        self.stale.get(surface).map(String::as_str)
    }

    /// Records that the daemon refused `action`, saying `message`: the
    /// session no longer has its surface as shown here. A call that showed
    /// the surface again since the user acted made it current, and it
    /// stays so.
    pub fn mark_stale(&mut self, action: &SurfaceAction, message: String) {
        if self.draws(&action.shown) {
            self.stale.insert(action.shown.surface.clone(), message);
            self.changed.insert(action.shown.surface.clone());
        }
    }

    /// Writes what the user entered at `binding`, as a two-way binding
    /// does. A write the data model refuses, as one through a value that
    /// is not a container, changes nothing.
    pub fn write(&mut self, surface: &str, binding: &Pointer, value: Value) {
        if let Some(model) = self.processor.surface_mut(surface) {
            if model.set_data(binding, value).is_ok() {
                self.changed.insert(surface.to_owned());
            }
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
        self.changed.insert(surface.to_owned());
        let shown = SurfaceRef {
            call: self.drawn_under(surface).unwrap_or_default().to_owned(),
            surface: surface.to_owned(),
        };
        let mut acted = Acted::default();
        for message in &dispatched.outbound {
            match message {
                RendererMessage::Action(_) => {
                    acted.send = Some(SurfaceAction {
                        shown: shown.clone(),
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

    /// The surfaces that changed since the last call, by ID: messages,
    /// edits and actions may change what any control of them shows, which
    /// checks pass, and whether and where they are drawn.
    pub fn take_changed(&mut self) -> Vec<String> {
        std::mem::take(&mut self.changed).into_iter().collect()
    }
}
