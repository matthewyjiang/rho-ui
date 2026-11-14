//! The panel at the right of the window: a home for surfaces, the tools
//! that sit beside a session, such as its background work. Ctrl+Alt+B or
//! the toggle at the right of the title bar shows and hides it. Drag its
//! left edge to resize it, and double-click the edge for its default width.
//! The state database remembers both ([`PanelLayout`]).
//!
//! Each surface opens as a tab in the panel's header. With none open, or
//! after +, the panel lists the surfaces to open, and dimmed under them
//! those still to come ([`PLANNED`]). The panel is the window's, not a
//! session's: its tabs stay as sessions change, and each surface shows
//! the session on screen.
//!
//! To add a surface: add a [`Surface`] variant with its name and icon, draw
//! it in [`RightPanel::render_surface`], and take it off [`PLANNED`].

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyDescription, EmptyHeader},
    h_flex,
    list::ListItem,
    resizable::{resize_handle, HandleEdge},
    v_flex, ActiveTheme as _, Icon, IconName, InteractiveElementExt as _, Selectable as _,
    Sizable as _, StyledExt as _,
};
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, Axis, BorrowAppContext as _, Context, Entity,
    Global, InteractiveElement as _, IntoElement, ParentElement as _, Rems, Render, SharedString,
    Styled as _, Subscription, Window,
};

use crate::{
    session_view::SessionView,
    store::{Key, StateDb},
    title_bar::TOP_ROW_REMS,
};

/// The panel's default width, in rems so it follows the UI scale: at the
/// default scale, 384px, which leaves a 1200px window's transcript 528px
/// beside the open sidebar.
const DEFAULT_WIDTH_REMS: f32 = 24.;

/// The narrowest the panel goes: 256px at the default scale, room for a
/// tab, + and the toggle in its header.
const MIN_WIDTH_REMS: f32 = 16.;

/// The narrowest the panel leaves the main pane: 480px at the default
/// scale. In the UI tests' screenshots the composer's footer lays out its
/// buttons in about 400px, and the composer sits 40px in from the pane's
/// edges.
const MIN_PANE_REMS: f32 = 30.;

/// What the panel can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surface {
    /// The session's background work, running and ended.
    Background,
}

impl Surface {
    /// Every surface, in the order the panel lists them.
    const ALL: [Surface; 1] = [Surface::Background];

    /// The surface's part of its elements' IDs.
    fn id(self) -> &'static str {
        match self {
            Surface::Background => "background",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Surface::Background => "Background work",
        }
    }

    fn icon(self) -> Lucide {
        match self {
            Surface::Background => Lucide::ListChecks,
        }
    }
}

/// A surface still to come, listed dimmed under the ones there are.
struct Planned {
    label: &'static str,
    icon: Lucide,
}

const PLANNED: [Planned; 2] = [
    Planned {
        label: "Terminal",
        icon: Lucide::SquareTerminal,
    },
    Planned {
        label: "Diff",
        icon: Lucide::FileDiff,
    },
];

/// Whether the panel shows, and how wide, remembered across runs under
/// [`Key::RightPanel`] and [`Key::RightPanelWidth`]. A global, as the title
/// bar of every pane offers the toggle while it is hidden.
#[derive(Debug)]
pub(crate) struct PanelLayout {
    shown: bool,
    /// In rems, as the user last left it; [`Self::width`] fits it to the
    /// window.
    width: f32,
    db: StateDb,
}

impl Global for PanelLayout {}

impl PanelLayout {
    /// Loads the saved layout: hidden unless it was shown, at the default
    /// width unless it was resized.
    pub(crate) fn load(db: &StateDb) -> Self {
        let width = db
            .get(Key::RightPanelWidth)
            .and_then(|width| width.parse::<f32>().ok())
            .filter(|width| width.is_finite())
            .unwrap_or(DEFAULT_WIDTH_REMS);
        Self {
            shown: db.get(Key::RightPanel).as_deref() == Some("shown"),
            width,
            db: db.clone(),
        }
    }

    /// Whether the panel shows.
    pub(crate) fn get(cx: &App) -> bool {
        cx.try_global::<Self>().is_some_and(|this| this.shown)
    }

    /// The panel's width in a row `row` wide that also holds a sidebar
    /// `sidebar` wide, both in rems: as the user left it, but leaving the
    /// main pane [`MIN_PANE_REMS`], and never under [`MIN_WIDTH_REMS`].
    pub(crate) fn width(row: f32, sidebar: f32, cx: &App) -> Rems {
        let width = cx
            .try_global::<Self>()
            .map_or(DEFAULT_WIDTH_REMS, |this| this.width);
        rems(fit(width, row - sidebar - MIN_PANE_REMS))
    }

    /// Shows the panel, or hides it, and saves which.
    pub(crate) fn toggle(cx: &mut App) {
        cx.update_global(|this: &mut Self, _| {
            this.shown = !this.shown;
            let value = if this.shown { "shown" } else { "hidden" };
            this.db.set(Key::RightPanel, value);
        });
    }

    /// Makes the panel `width` rems wide, fitted to a row `row` wide beside
    /// a sidebar `sidebar` wide, as a drag of its edge goes. Saved by
    /// [`Self::save_width`] when the drag ends.
    pub(crate) fn resize(width: f32, row: f32, sidebar: f32, cx: &mut App) {
        let width = fit(width, row - sidebar - MIN_PANE_REMS);
        cx.update_global(|this: &mut Self, _| this.width = width);
    }

    /// Saves the width a drag left.
    pub(crate) fn save_width(cx: &App) {
        let this = cx.global::<Self>();
        this.db.set(Key::RightPanelWidth, &this.width.to_string());
    }

    /// Puts the panel back to its default width, and saves it.
    fn reset_width(cx: &mut App) {
        cx.update_global(|this: &mut Self, _| this.width = DEFAULT_WIDTH_REMS);
        Self::save_width(cx);
    }
}

/// `width`, held to the `room` there is, but at least [`MIN_WIDTH_REMS`]:
/// a window too narrow for both squeezes the pane, not the panel.
fn fit(width: f32, room: f32) -> f32 {
    width.min(room).max(MIN_WIDTH_REMS)
}

/// What a drag of the panel's edge carries. The drag draws nothing: the
/// panel itself follows the pointer.
#[derive(Clone, Copy)]
pub(crate) struct ResizeRightPanel;

impl Render for ResizeRightPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        gpui_kit::Empty
    }
}

/// Shows the panel, or hides it. Where it sits, at the window's right edge
/// in the title bar or in the panel's header, it stays put, so a second
/// click undoes the first.
pub(crate) fn toggle_button(cx: &App) -> Button {
    let (icon, label) = if PanelLayout::get(cx) {
        (IconName::PanelRightClose, "Hide panel")
    } else {
        (IconName::PanelRightOpen, "Show panel")
    };
    Button::new("toggle-right-panel")
        .ghost()
        .small()
        .icon(icon)
        .accessibility_label(label)
        .tooltip(format!("{label} (Ctrl+Alt+B)"))
        .on_click(|_, _, cx| PanelLayout::toggle(cx))
}

/// The panel's tabs and what it shows. [`crate::app::RhoApp`] lays it out
/// while [`PanelLayout`], and tells it which session is on screen.
pub(crate) struct RightPanel {
    /// The open surfaces, in the order opened.
    tabs: Vec<Surface>,
    /// One of `tabs`, or `None` for the list of surfaces.
    active: Option<Surface>,
    /// The session on screen, if any, with the subscription that redraws
    /// the panel as it changes.
    session: Option<(Entity<SessionView>, Subscription)>,
}

impl RightPanel {
    pub(crate) fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            session: None,
        }
    }

    /// Follows `session`, the one on screen, or none.
    pub(crate) fn set_session(
        &mut self,
        session: Option<Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.session.as_ref().map(|(view, _)| view) == session.as_ref() {
            return;
        }
        self.session = session.map(|view| {
            let redraw = cx.observe(&view, |_, _, cx| cx.notify());
            (view, redraw)
        });
        cx.notify();
    }

    /// Shows `surface`, opening its tab if it has none.
    fn open(&mut self, surface: Surface, cx: &mut Context<'_, Self>) {
        if !self.tabs.contains(&surface) {
            self.tabs.push(surface);
        }
        self.active = Some(surface);
        cx.notify();
    }

    /// Closes `surface`'s tab. If it was showing, the last tab opened
    /// shows, or the list of surfaces when none is left.
    fn close(&mut self, surface: Surface, cx: &mut Context<'_, Self>) {
        self.tabs.retain(|tab| *tab != surface);
        if self.active == Some(surface) {
            self.active = self.tabs.last().copied();
        }
        cx.notify();
    }

    /// The list of surfaces, in place of the open one.
    fn show_list(&mut self, cx: &mut Context<'_, Self>) {
        self.active = None;
        cx.notify();
    }

    /// The tabs, +, and the toggle at the right edge, in a row as tall as
    /// the title bar beside it.
    fn render_header(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let tabs = self.tabs.iter().map(|&surface| {
            h_flex()
                .flex_shrink_0()
                .child(
                    Button::new(SharedString::from(format!("panel-tab-{}", surface.id())))
                        .ghost()
                        .small()
                        .selected(self.active == Some(surface))
                        .icon(Icon::new(surface.icon()))
                        .label(surface.label())
                        .on_click(cx.listener(move |this, _, _, cx| this.open(surface, cx))),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "close-panel-tab-{}",
                        surface.id()
                    )))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip("Close")
                    .on_click(cx.listener(move |this, _, _, cx| this.close(surface, cx))),
                )
        });
        h_flex()
            .flex_shrink_0()
            .h(rems(TOP_ROW_REMS))
            .gap_1()
            // The title bar's inset, so the toggle doesn't move.
            .px_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .gap_1()
                    .children(tabs)
                    .child(
                        Button::new("panel-surfaces")
                            .ghost()
                            .small()
                            .selected(self.active.is_none())
                            .icon(IconName::Plus)
                            .accessibility_label("Open a surface")
                            .tooltip("Open a surface")
                            .on_click(cx.listener(|this, _, _, cx| this.show_list(cx))),
                    ),
            )
            .child(toggle_button(cx))
    }

    /// The surfaces to open, centered, and those to come, dimmed.
    fn render_list(cx: &Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme();
        let row = |id: SharedString, icon: Lucide, label: &'static str| {
            ListItem::new(id)
                .accessibility_label(label)
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .child(
                    h_flex()
                        .gap_3()
                        .child(Icon::new(icon).small())
                        .child(div().flex_1().child(label)),
                )
        };
        let open = Surface::ALL.into_iter().map(|surface| {
            row(
                format!("open-surface-{}", surface.id()).into(),
                surface.icon(),
                surface.label(),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open(surface, cx)))
        });
        let muted = theme.muted_foreground;
        let planned = PLANNED.iter().map(|planned| {
            row(
                format!("planned-surface-{}", planned.label.to_lowercase()).into(),
                planned.icon,
                planned.label,
            )
            .disabled(true)
            .text_color(muted.opacity(0.6))
            .suffix(move |_, _| div().text_xs().child("Soon"))
        });
        v_flex()
            .id("panel-surface-list")
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .px_4()
            .child(div().font_medium().child("Open a surface"))
            .child(
                v_flex()
                    .w_full()
                    .max_w(rems(18.))
                    .gap_0p5()
                    .children(open)
                    .children(planned),
            )
    }

    /// What `surface` shows of the session on screen.
    fn render_surface(&self, surface: Surface, cx: &App) -> AnyElement {
        match surface {
            Surface::Background => match &self.session {
                Some((view, _)) => SessionView::background_surface(view, cx).into_any_element(),
                None => no_session("Open a session to see its background work."),
            },
        }
    }
}

/// A surface's place when it needs a session and none is on screen.
fn no_session(text: &'static str) -> AnyElement {
    Empty::new()
        .h_full()
        .header(EmptyHeader::new().description(EmptyDescription::new().child(text)))
        .into_any_element()
}

impl Render for RightPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let body = match self.active {
            Some(surface) => self.render_surface(surface, cx),
            None => Self::render_list(cx).into_any_element(),
        };
        v_flex()
            .id("right-panel")
            .test_support()
            .relative()
            .size_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(body))
            // [`crate::app::RhoApp`] follows the drag, as it lays out the
            // row the panel sits in.
            .child(
                resize_handle::<ResizeRightPanel, ResizeRightPanel>(
                    "right-panel-edge",
                    Axis::Horizontal,
                )
                .inside(HandleEdge::Leading)
                .on_drag(ResizeRightPanel, |drag, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| *drag)
                })
                .on_double_click(|_, _, cx| PanelLayout::reset_width(cx)),
            )
    }
}
