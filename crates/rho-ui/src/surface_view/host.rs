//! The surfaces of the session a session view shows: its store, and the
//! panel that draws each surface.

use std::collections::HashMap;

use gpui_kit::{AppContext as _, Context, Entity, Subscription, Window};
use rho_ui_core::{
    surface::{SurfaceRef, SurfaceStore},
    Session,
};

use super::SurfacePanel;
use crate::session_view::SessionView;

/// The surfaces a session's `ui` calls showed. It goes with the session
/// view, which a session that fell behind replaces, and the replay builds
/// them again.
#[derive(Default)]
pub(crate) struct Surfaces {
    store: SurfaceStore,
    /// The panel of each surface, by surface ID.
    panels: HashMap<String, Drawn>,
}

struct Drawn {
    panel: Entity<SurfacePanel>,
    _subscriptions: [Subscription; 2],
}

impl Surfaces {
    /// The store, for what the user does on a surface. Call
    /// [`Self::redraw`] after changing it.
    pub(crate) fn store_mut(&mut self) -> &mut SurfaceStore {
        &mut self.store
    }

    /// The panel that draws the surface of entry `shown`, if that entry
    /// draws it: a surface moves to the latest call that showed it.
    pub(crate) fn panel(&self, shown: &SurfaceRef) -> Option<&Entity<SurfacePanel>> {
        let drawn = self.panels.get(shown.surface())?;
        self.store.draws(shown).then_some(&drawn.panel)
    }

    /// Shows the store the surfaces `session` placed, then redraws.
    pub(crate) fn sync(
        &mut self,
        session: &mut Session,
        window: &mut Window,
        cx: &mut Context<'_, SessionView>,
    ) {
        for placed in session.take_placed_surfaces() {
            self.store.show(placed);
        }
        self.redraw(window, cx);
    }

    /// Gives each surface that changed its panel as the store has it,
    /// making one for a new surface. A deleted surface's panel draws
    /// nothing, and frees its pictures.
    pub(crate) fn redraw(&mut self, window: &mut Window, cx: &mut Context<'_, SessionView>) {
        for surface in self.store.take_changed() {
            let drawn = self
                .panels
                .entry(surface)
                .or_insert_with_key(|surface| draw(surface, window, cx));
            let store = &self.store;
            drawn
                .panel
                .update(cx, |panel, cx| panel.show(store, window, cx));
        }
    }
}

/// A panel for `surface`, whose events go to the session view.
fn draw(surface: &str, window: &mut Window, cx: &mut Context<'_, SessionView>) -> Drawn {
    let panel = cx.new(|cx| SurfacePanel::new(surface.to_owned(), cx));
    let changed = surface.to_owned();
    Drawn {
        _subscriptions: [
            cx.subscribe_in(&panel, window, SessionView::on_surface_event),
            // A panel that redraws may have changed height: it moved to
            // another call or went, a check's message came or went, or
            // another tab shows.
            cx.observe(&panel, move |this, _, cx| {
                this.surface_resized(&changed, cx);
            }),
        ],
        panel,
    }
}
