//! A surface a session's `ui` tool showed, drawn in the transcript under
//! the call that showed it, with GPUI Kit's controls.
//!
//! [`Surfaces`] keeps the session's [`SurfaceStore`] and a
//! [`SurfacePanel`] for each surface in it. A panel draws a
//! [`SurfaceTree`] and owns what Kit's controls keep between frames: the
//! text being typed, a slider's thumb, the tab showing. What the user does
//! leaves as a [`SurfaceEvent`]; the session view gives it to the store,
//! and the panel takes the surface as it then stands
//! ([`SurfacePanel::show`]), so checks and bindings follow each edit.

mod controls;
mod host;
mod icons;
mod media;
mod modal;
mod render;

use std::collections::{HashMap, HashSet};

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{v_flex, ActiveTheme as _};
use gpui_kit::{
    div, App, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity,
    Window,
};
use rho_ui_core::surface::{Action, MediaKind, Node, Pointer, SurfaceStore, SurfaceTree, Target};
use serde_json::Value;

use controls::{FieldWidget, SliderWidget};
pub(crate) use host::Surfaces;
use media::Pictures;

/// What the user did on a surface.
#[derive(Clone, Debug)]
pub(crate) enum SurfaceEvent {
    /// Entered `value` in a control bound to `binding`.
    Write { binding: Pointer, value: Value },
    /// Pressed the button `target`, which runs `action`.
    Act { target: Target, action: Action },
}

/// An element of a surface panel, for its ID.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Part<'a> {
    /// The panel itself.
    Panel,
    /// What draws the component instance.
    Control(&'a Target),
    /// The option at an index of a choice picker.
    Option(&'a Target, usize),
    /// The value beside a slider.
    SliderValue(&'a Target),
    /// Why what an input shows is kept out of the data model.
    Problem(&'a Target),
    /// The content of a modal, in its dialog.
    Modal(&'a Target),
    /// The line that says the surface is out of date.
    Stale,
}

/// The ID of `part` of the panel of `surface`. Each name is quoted, so no
/// component or surface ID, whatever it holds, makes two parts' IDs alike.
pub(crate) fn element_id(surface: &str, part: Part<'_>) -> SharedString {
    let instance = |target: &Target| match target.item() {
        Some(item) => format!("{:?} at {item:?}", target.component()),
        None => format!("{:?}", target.component()),
    };
    let part = match part {
        Part::Panel => String::new(),
        Part::Control(target) => format!(" control {}", instance(target)),
        Part::Option(target, ix) => format!(" option {ix} of {}", instance(target)),
        Part::SliderValue(target) => format!(" value of {}", instance(target)),
        Part::Problem(target) => format!(" problem of {}", instance(target)),
        Part::Modal(target) => format!(" modal {}", instance(target)),
        Part::Stale => " stale".to_owned(),
    };
    SharedString::from(format!("surface {surface:?}{part}"))
}

/// A handler's way back to the panel from one of its controls.
type Handle = WeakEntity<SurfacePanel>;

fn emit(panel: &Handle, event: SurfaceEvent, cx: &mut App) {
    let _ = panel.update(cx, |_, cx| cx.emit(event));
}

/// One surface, as drawn.
pub(crate) struct SurfacePanel {
    surface: String,
    tree: Option<SurfaceTree>,
    /// What the daemon said when it refused an action on the surface,
    /// which then takes no more.
    stale: Option<String>,
    /// Why the app could not show some of the surface's messages.
    refused: Vec<String>,
    /// An action is on its way to the daemon.
    sending: bool,
    /// The controls of its text and date fields.
    fields: HashMap<Target, FieldWidget>,
    /// The controls of its sliders.
    sliders: HashMap<Target, SliderWidget>,
    /// The tab each `Tabs` shows; the first until the user picks another.
    tabs: HashMap<Target, usize>,
    pictures: Pictures,
    _released: Subscription,
}

impl EventEmitter<SurfaceEvent> for SurfacePanel {}

impl SurfacePanel {
    pub(crate) fn new(surface: String, cx: &mut Context<'_, Self>) -> Self {
        Self {
            surface,
            tree: None,
            stale: None,
            refused: Vec::new(),
            sending: false,
            fields: HashMap::new(),
            sliders: HashMap::new(),
            tabs: HashMap::new(),
            pictures: Pictures::default(),
            _released: cx.on_release(|this, cx| this.pictures.release(cx)),
        }
    }

    /// The surface's ID.
    pub(crate) fn surface(&self) -> &str {
        &self.surface
    }

    /// An action is on its way to the daemon: the buttons wait.
    pub(crate) fn sending(&mut self, cx: &mut Context<'_, Self>) {
        self.sending = true;
        cx.notify();
    }

    /// The daemon answered the action.
    pub(crate) fn sent(&mut self, cx: &mut Context<'_, Self>) {
        self.sending = false;
        cx.notify();
    }

    /// Takes the surface as `store` has it. Controls keep what the user is
    /// doing in them unless the data model changed under them.
    pub(crate) fn show(
        &mut self,
        store: &SurfaceStore,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let tree = store.tree(&self.surface);
        self.stale = store.stale(&self.surface).map(str::to_owned);
        self.refused = store.refused(&self.surface).to_vec();
        let (mut fields, mut sliders) = (HashSet::new(), HashSet::new());
        let mut images = HashSet::new();
        for node in tree.iter().flat_map(|tree| tree.root.descendants()) {
            match node {
                Node::Field(field) => {
                    self.sync_field(field, window, cx);
                    fields.insert(field.input.target.clone());
                }
                Node::Slider(slider) => {
                    self.sync_slider(slider, window, cx);
                    sliders.insert(slider.input.target.clone());
                }
                Node::Media(media) if media.kind == MediaKind::Image => {
                    images.insert(media.url.as_str());
                }
                Node::Text(_)
                | Node::Icon(_)
                | Node::Media(_)
                | Node::Divider(_)
                | Node::Stack(_)
                | Node::Card(_)
                | Node::Tabs(_)
                | Node::Modal(_)
                | Node::Button(_)
                | Node::CheckBox(_)
                | Node::Choices(_)
                | Node::Placeholder(_) => {}
            }
        }
        self.fields.retain(|target, _| fields.contains(target));
        self.sliders.retain(|target, _| sliders.contains(target));
        self.pictures.keep(&images, cx);
        self.tree = tree;
        cx.notify();
    }

    fn id(&self, part: Part<'_>) -> SharedString {
        element_id(&self.surface, part)
    }
}

impl Render for SurfacePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let panel = cx.entity().downgrade();
        let root = self
            .tree
            .as_ref()
            .map(|tree| self.render_node(&tree.root, &panel, cx));
        let muted = cx.theme().muted_foreground;
        let refused = (!self.refused.is_empty()).then(|| {
            div().text_xs().text_color(muted).child(format!(
                "Part of this form uses A2UI this app can't show yet: {}",
                self.refused.join(" ")
            ))
        });
        let stale = self.stale.as_ref().map(|said| {
            div()
                .id(self.id(Part::Stale))
                .test_support()
                .text_xs()
                .text_color(muted)
                .child(format!(
                    "Rho no longer has this form, so it takes no more answers from it: {said}"
                ))
        });
        v_flex()
            .id(self.id(Part::Panel))
            .test_support()
            .aria_label(self.surface.clone())
            .w_full()
            .gap_2()
            .children(root)
            .children(refused)
            .children(stale)
    }
}
