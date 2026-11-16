//! The collapsed sidebar: a rail of buttons in the open sidebar's places.
//! The toggle and Settings stay where they were, New session sits under the
//! toggle, and a session that waits or works still shows as an icon. Which
//! [`Layout`] the sidebar has, and how wide it is open, is remembered
//! across runs.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex, v_flex, ActiveTheme as _, Icon, IconName, Sizable as _,
};
use gpui_kit::{
    div, rems, Context, InteractiveElement as _, IntoElement, ParentElement as _, Rems, Styled as _,
};

use super::{new_session_button, settings_button, SidebarEvent, SidebarView};
use crate::{
    session_list::Busy,
    store::{Key, StateDb},
};

/// How much of the sidebar shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Layout {
    /// The whole sidebar.
    #[default]
    Open,
    /// A rail of buttons; see [`SidebarView::render_rail`].
    Rail,
}

/// The rail's width, in rems so it follows the UI scale: one small button
/// with the open sidebar's inset on each side.
const RAIL_REMS: f32 = 3.;

/// The open sidebar's default width: 288px at the default scale.
const DEFAULT_OPEN_REMS: f32 = 18.;

/// The narrowest the open sidebar goes: 224px at the default scale, where
/// the search and filter still share a line, and a hovered row's Snooze
/// and Settle fit beside its workspace.
const MIN_OPEN_REMS: f32 = 14.;

impl Layout {
    fn toggled(self) -> Self {
        match self {
            Layout::Open => Layout::Rail,
            Layout::Rail => Layout::Open,
        }
    }
}

/// The sidebar's [`Layout`] and open width, remembered across runs under
/// [`Key::SidebarLayout`] as `open` or `collapsed`, and
/// [`Key::SidebarWidth`] in rems.
#[derive(Debug)]
pub(crate) struct SidebarLayout {
    current: Layout,
    /// The open sidebar's width in rems, as the user last left it.
    open_width: f32,
    db: StateDb,
}

impl SidebarLayout {
    /// Loads the saved layout: open unless it was collapsed, at the default
    /// width unless it was resized.
    pub(crate) fn load(db: &StateDb) -> Self {
        let collapsed = db.get(Key::SidebarLayout).as_deref() == Some("collapsed");
        let open_width = db
            .get(Key::SidebarWidth)
            .and_then(|width| width.parse::<f32>().ok())
            .filter(|width| width.is_finite())
            .unwrap_or(DEFAULT_OPEN_REMS)
            .max(MIN_OPEN_REMS);
        Self {
            current: if collapsed {
                Layout::Rail
            } else {
                Layout::Open
            },
            open_width,
            db: db.clone(),
        }
    }

    pub(super) fn current(&self) -> Layout {
        self.current
    }

    /// How wide the sidebar is laid out, open or as the rail.
    pub(super) fn width(&self) -> Rems {
        match self.current {
            Layout::Open => rems(self.open_width),
            Layout::Rail => rems(RAIL_REMS),
        }
    }

    /// Makes the open sidebar `width` rems wide, but no wider than `room`
    /// and never under [`MIN_OPEN_REMS`], as a drag of its edge goes. Saved
    /// by [`Self::save_width`] when the drag ends.
    pub(super) fn resize(&mut self, width: f32, room: f32) {
        self.open_width = width.min(room).max(MIN_OPEN_REMS);
    }

    /// Saves the width a drag left.
    pub(super) fn save_width(&self) {
        self.db.set(Key::SidebarWidth, &self.open_width.to_string());
    }

    /// Puts the open sidebar back to its default width, and saves it.
    pub(super) fn reset_width(&mut self) {
        self.open_width = DEFAULT_OPEN_REMS;
        self.save_width();
    }

    /// Collapses an open sidebar or opens a collapsed one, and saves which.
    pub(super) fn toggle(&mut self) {
        self.current = self.current.toggled();
        let value = match self.current {
            Layout::Open => "open",
            Layout::Rail => "collapsed",
        };
        self.db.set(Key::SidebarLayout, value);
    }
}

impl SidebarView {
    /// The collapsed sidebar. Every button sits on one column at the open
    /// sidebar's inset, so the toggle and Settings don't move when it
    /// collapses, and a second click on the toggle undoes the first. Under
    /// New session, an icon stands for the sessions that wait for a person
    /// and another for those working, so collapsing hides nobody's turn.
    pub(super) fn render_rail(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .id("sidebar-rail")
            .test_support()
            .size_full()
            .px_3()
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h(super::top_row())
                    .child(toggle_button(Layout::Rail, cx)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .child(new_session_button(cx))
                    // Status, not actions: a group of its own.
                    .child(
                        v_flex()
                            .gap_1()
                            .pt_2()
                            .children(busy_buttons(self.list.busy(), cx)),
                    ),
            )
            .child(div().flex_shrink_0().py_2().child(settings_button(cx)))
    }
}

/// Collapses the sidebar from `layout`, or opens it. Kit's
/// `SidebarToggleButton` takes no tooltip, so this is a Button with its icons.
pub(super) fn toggle_button(layout: Layout, cx: &Context<'_, SidebarView>) -> Button {
    let (icon, label) = match layout {
        Layout::Open => (IconName::PanelLeftClose, "Collapse sidebar"),
        Layout::Rail => (IconName::PanelLeftOpen, "Expand sidebar"),
    };
    Button::new("toggle-sidebar")
        .ghost()
        .small()
        .icon(icon)
        .accessibility_label(label)
        .tooltip_with_action(label, &crate::ToggleSidebar, Some("RhoApp"))
        .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::ToggleCollapsed)))
}

/// The rail's stand-ins for the rows: an icon for the sessions that wait,
/// and one for those working, in their rows' colors. Each says how many in
/// its tooltip and opens the sidebar, which shows which.
fn busy_buttons(busy: Busy, cx: &Context<'_, SidebarView>) -> Vec<Button> {
    let theme = cx.theme();
    [
        (
            "rail-waiting",
            busy.waiting,
            Lucide::CircleAlert,
            theme.warning,
            ["needs you", "need you"],
        ),
        (
            "rail-working",
            busy.working,
            Lucide::CircleDashed,
            theme.blue,
            ["working", "working"],
        ),
    ]
    .into_iter()
    .filter(|&(_, count, ..)| count > 0)
    .map(|(id, count, icon, color, [one, many])| {
        let label = if count == 1 {
            format!("1 session {one}")
        } else {
            format!("{count} sessions {many}")
        };
        Button::new(id)
            .ghost()
            .small()
            .icon(Icon::new(icon).text_color(color))
            .accessibility_label(label.clone())
            .tooltip(label)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::ToggleCollapsed)))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_is_remembered() {
        let db = StateDb::in_memory();
        let mut layout = SidebarLayout::load(&db);
        assert_eq!(layout.current, Layout::Open, "open until collapsed");
        layout.toggle();
        assert_eq!(SidebarLayout::load(&db).current, Layout::Rail);
        layout.toggle();
        assert_eq!(SidebarLayout::load(&db).current, Layout::Open);
    }
}
