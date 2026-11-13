//! A block that folds away and back with a spring: a finished turn's
//! working collapses under its "Worked for" header instead of vanishing.
//!
//! Kit's `Collapsible::motion_id` does this too, but its reveal learns the
//! content's height a frame late. That puts a one-frame flash at the start
//! of every fold, and a transcript row it lays out off screen (where it was
//! never painted) measures as empty. So an open [`Folding`] lays its content
//! out normally and only remembers the height; the reveal clips to that
//! height only while the spring moves.

use gpui_kit::base::spring;
use gpui_kit::component::{v_flex, ActiveTheme as _};
use gpui_kit::{
    relative, size, AnyElement, App, AvailableSpace, Bounds, ContentMask, Display, Element,
    ElementId, FlexDirection, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    ParentElement as _, Pixels, RenderOnce, Style, Styled as _, Window,
};

use crate::work::Disclosure;

/// Builds a fold's content, when some of it shows.
type Content = Box<dyn FnOnce(&mut Window, &mut App) -> AnyElement>;

/// Folds `content` away when closed. Content is built only while some of
/// it shows, so a folded block costs nothing. The spring's state is keyed
/// by `id`; a fold first drawn already at its target doesn't move.
#[derive(IntoElement)]
pub(crate) struct Folding {
    id: ElementId,
    shows: Disclosure,
    content: Content,
}

impl Folding {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        shows: Disclosure,
        content: impl FnOnce(&mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            shows,
            content: Box::new(content),
        }
    }
}

impl RenderOnce for Folding {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let target = if self.shows.is_open() { 1. } else { 0. };
        let policy = cx.theme().motion_tokens().spring_control;
        let shown: f32 = spring((self.id.clone(), "fold"), target, policy, window, cx);
        let shown = shown.clamp(0., 1.);
        // Faded faster than it folds: clipped mid-line, text at full
        // strength reads as a rendering glitch.
        let child = (shown > 0.).then(|| {
            v_flex()
                .w_full()
                .opacity(shown * shown)
                .child((self.content)(window, cx))
                .into_any_element()
        });
        Reveal {
            id: self.id,
            shown,
            child,
        }
    }
}

/// The content's natural height, from the last frame it was painted.
#[derive(Clone, Copy, Default)]
struct Height(Option<Pixels>);

/// Shows the top `shown` share of `child`, clipped.
struct Reveal {
    id: ElementId,
    /// From 0, hidden, to 1, laid out as if unwrapped.
    shown: f32,
    child: Option<AnyElement>,
}

impl Reveal {
    fn is_whole(&self) -> bool {
        self.shown >= 1.
    }
}

impl IntoElement for Reveal {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Reveal {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            ..Style::default()
        };
        style.size.width = relative(1.).into();
        if self.is_whole() {
            if let Some(child) = &mut self.child {
                let child = child.request_layout(window, cx);
                return (window.request_layout(style, [child], cx), ());
            }
        }
        let id = id.expect("Reveal has an id");
        let Height(height) = window.with_element_state(id, |state: Option<Height>, _| {
            let state = state.unwrap_or_default();
            (state, state)
        });
        style.size.height = (height.unwrap_or(Pixels::ZERO) * self.shown).into();
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let whole = self.is_whole();
        let Some(child) = &mut self.child else {
            return;
        };
        let id = id.expect("Reveal has an id");
        let height = if whole {
            child.prepaint(window, cx);
            bounds.size.height
        } else {
            // Partly shown: the child takes its natural height, painted
            // through a window as tall as `bounds`.
            let natural = child
                .layout_as_root(
                    size(
                        AvailableSpace::Definite(bounds.size.width),
                        AvailableSpace::MinContent,
                    ),
                    window,
                    cx,
                )
                .height;
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                child.prepaint_at(bounds.origin, window, cx);
            });
            natural
        };
        let changed = window.with_element_state(id, |state: Option<Height>, _| {
            let changed = state.is_none_or(|Height(old)| old != Some(height));
            (changed, Height(Some(height)))
        });
        // A height that changed mid-fold resizes the window next frame.
        if changed && !whole {
            window.request_animation_frame();
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        (): &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(child) = &mut self.child else {
            return;
        };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            child.paint(window, cx);
        });
    }
}
