//! A surface's modals: a button that opens the modal's content in a
//! dialog, drawn by the panel as it stands each time the panel changes.

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::Button, v_flex, Disableable as _, Icon, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
};
use rho_ui_core::surface::{Modal, Node, Target};

use super::{icons::lucide, Handle, Part, SurfacePanel};

impl SurfacePanel {
    /// The button that opens `modal`'s dialog.
    pub(super) fn render_modal(&self, modal: &Modal, panel: &Handle) -> AnyElement {
        let title = SharedString::from(modal.trigger.text.clone());
        let (opens, target) = (panel.clone(), modal.target.clone());
        let id = self.id(Part::Modal(&modal.target));
        Button::new(self.id(Part::Control(&modal.target)))
            .outline()
            .small()
            .label(title.clone())
            .accessibility_label(modal.accessible_name.clone())
            .when_some(modal.trigger.icon.as_deref(), |button, icon| {
                button.icon(Icon::new(lucide(icon)))
            })
            .disabled(modal.content.is_none())
            .on_click(move |_, window, cx| {
                open_modal(
                    opens.clone(),
                    target.clone(),
                    id.clone(),
                    title.clone(),
                    window,
                    cx,
                );
            })
            .into_any_element()
    }

    /// The content of the modal `target`, for its dialog.
    fn modal_content(&self, target: &Target, panel: &Handle, cx: &App) -> Option<AnyElement> {
        let content = self
            .tree
            .as_ref()?
            .root
            .descendants()
            .find_map(|node| match node {
                Node::Modal(modal) if modal.target == *target => modal.content.as_deref(),
                _ => None,
            })?;
        Some(self.render_node(content, panel, cx))
    }
}

/// What a modal's dialog holds: the modal's content, drawn by its panel,
/// as it stands each time the panel changes.
struct ModalBody {
    panel: Handle,
    target: Target,
    id: SharedString,
    _panel_changed: Option<Subscription>,
}

impl Render for ModalBody {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let content = self
            .panel
            .upgrade()
            .and_then(|panel| panel.read(cx).modal_content(&self.target, &self.panel, cx));
        v_flex()
            .id(self.id.clone())
            .test_support()
            .w_full()
            .children(content)
    }
}

/// Opens the dialog of modal `target` of `panel`, titled as its trigger.
fn open_modal(
    panel: Handle,
    target: Target,
    id: SharedString,
    title: SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    let body = cx.new(|cx| ModalBody {
        _panel_changed: panel
            .upgrade()
            .map(|panel| cx.observe(&panel, |_, _, cx| cx.notify())),
        panel,
        target,
        id,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let body = body.clone();
        dialog
            .title(title.clone())
            .content(move |content, _, _| content.child(body.clone()))
    });
}
