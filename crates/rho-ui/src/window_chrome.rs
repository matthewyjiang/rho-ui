//! The window's title bar. On Windows and macOS rho-ui hides the system's
//! and uses the window's top row instead, the rows of the sidebar, the main
//! pane and the right panel, each [`TOP_ROW_REMS`] tall: their parts that
//! take no clicks move the window, macOS's traffic lights sit at the left
//! of the sidebar's row, and Windows' window buttons at the right of the
//! row at the window's right edge. On Linux and in the browser the window
//! manager draws the title bar, and this adds nothing.
//!
//! GPUI Kit's `TitleBar` is a bar of its own above the window's content,
//! and its window buttons are private to it; this merges into the rows
//! already there instead, moving the window on macOS as Kit's bar does.
//!
//! The rows sit in cached views, and GPUI forgets a reused view's window
//! control areas (`Window::reuse_paint` replays its hitboxes, not their
//! areas), so Windows would lose a row's drag area on every frame that
//! view sits out. Each drag area records its hitbox in [`DragAreas`] as it
//! lays out, and [`drag_areas`], in the uncached root, marks every one
//! each frame. A reused view keeps its hitbox, id and all; GPUI numbers
//! hitboxes afresh for the window's life, so a stale one never matches.

use std::collections::HashMap;

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{h_flex, ActiveTheme as _, Icon, IconName, Sizable as _};
use gpui_kit::{
    canvas, deferred, div, point, px, rems, App, Div, Global, Hitbox, HitboxBehavior, Hsla,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Point, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, TitlebarOptions, Window,
    WindowControlArea,
};

use crate::title_bar::TOP_ROW_REMS;

/// Who draws the window's title bar. [`install`] picks this platform's;
/// UI tests set another to lay the app out as that platform does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Chrome {
    /// The window manager, as on Linux and in the browser.
    System,
    /// rho-ui, with Windows' window buttons drawn in `glyphs`, a font of
    /// Windows' caption glyphs, or with GPUI Kit's icons when there is
    /// none.
    Windows { glyphs: Option<SharedString> },
    /// rho-ui, around macOS's traffic lights.
    Mac,
}

impl Global for Chrome {}

impl Chrome {
    /// This platform's chrome.
    pub(crate) fn native(cx: &App) -> Self {
        if cfg!(target_os = "windows") {
            Self::windows(cx)
        } else if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::System
        }
    }

    /// Windows' chrome, drawn in the newest caption font installed:
    /// Segoe Fluent Icons comes with Windows 11, Segoe MDL2 Assets with
    /// Windows 10.
    pub(crate) fn windows(cx: &App) -> Self {
        let installed = cx.text_system().all_font_names();
        let glyphs = ["Segoe Fluent Icons", "Segoe MDL2 Assets"]
            .into_iter()
            .find(|family| installed.iter().any(|name| name == family))
            .map(SharedString::from);
        Self::Windows { glyphs }
    }

    fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

/// Sets this platform's [`Chrome`] for the app.
pub(crate) fn install(cx: &mut App) {
    let chrome = Chrome::native(cx);
    cx.set_global(chrome);
}

/// The width of one of Windows' window buttons, the system's own in
/// effective pixels; it doesn't grow with the UI scale.
const CAPTION_BUTTON: Pixels = px(46.);

/// The size of the window buttons' glyphs, as Windows draws its own.
const CAPTION_GLYPH: Pixels = px(10.);

/// Windows 11's red behind a hovered close button.
const CLOSE_RED: u32 = 0x00C4_2B1C;

/// How far the traffic lights sit from the window's left edge: the open
/// sidebar's inset at the default scale, so the close button lines up with
/// the rail's buttons under it.
const LIGHTS_X: Pixels = px(12.);

/// The width of macOS's three traffic lights: 14px buttons, each 20px
/// after the last.
const LIGHTS_SPAN: Pixels = px(54.);

/// The height of one traffic light's frame. GPUI Kit's title bar centers
/// them in its 34px with a 9px margin above and below.
const LIGHT_HEIGHT: Pixels = px(16.);

/// The gap between the traffic lights and the first control after them.
const LIGHTS_GAP: Pixels = px(8.);

/// The system title bar's replacement for `window_options`: none on
/// Windows and macOS, where the app draws its own, with the traffic lights
/// centered in the top row.
pub(crate) fn titlebar_options(cx: &App) -> TitlebarOptions {
    let chrome = Chrome::get(cx);
    TitlebarOptions {
        title: Some("Rho".into()),
        appears_transparent: *chrome != Chrome::System,
        traffic_light_position: (*chrome == Chrome::Mac).then(|| traffic_lights(cx)),
    }
}

/// Whether the app, not the system, moves the window from its top row: on
/// macOS, so a click on a button there never waits on macOS telling it
/// from a double click.
pub(crate) fn app_owns_titlebar_drag(cx: &App) -> bool {
    *Chrome::get(cx) == Chrome::Mac
}

/// Where the traffic lights go: centered in the top row, which grows with
/// the UI scale.
fn traffic_lights(cx: &App) -> Point<Pixels> {
    let row = cx.theme().font_size * TOP_ROW_REMS;
    point(LIGHTS_X, ((row - LIGHT_HEIGHT) / 2.).max(Pixels::ZERO))
}

/// Centers the traffic lights in the top row again, after the UI scale
/// changed. GPUI moves them on macOS only.
pub(crate) fn place_traffic_lights(window: &Window, cx: &App) {
    #[cfg(target_os = "macos")]
    if *Chrome::get(cx) == Chrome::Mac {
        window.set_traffic_light_position(traffic_lights(cx));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (window, cx);
}

/// The title bar the app draws in a window, if it draws one: none where
/// the system draws it, or in fullscreen, which has none.
enum Drawn<'a> {
    Windows { glyphs: &'a Option<SharedString> },
    Mac,
}

fn drawn<'a>(window: &Window, cx: &'a App) -> Option<Drawn<'a>> {
    if window.is_fullscreen() {
        return None;
    }
    match Chrome::get(cx) {
        Chrome::System => None,
        Chrome::Windows { glyphs } => Some(Drawn::Windows { glyphs }),
        Chrome::Mac => Some(Drawn::Mac),
    }
}

/// The room the traffic lights take at the left of the sidebar's top row,
/// which moves the window: the row starts with it in place of its inset.
/// `None` without them.
pub(crate) fn traffic_light_room(window: &Window, cx: &App) -> Option<Div> {
    matches!(drawn(window, cx), Some(Drawn::Mac)).then(|| {
        div()
            .flex_shrink_0()
            .w(LIGHTS_X + LIGHTS_SPAN + LIGHTS_GAP)
            .h_full()
            .moves_window(DragArea::TrafficLights, window, cx)
    })
}

/// The least the sidebar is wide, in rems, so the traffic lights fit over
/// the rail: zero without them. Kept in fullscreen too, so the rail doesn't
/// jump as the window goes in and out of it.
pub(crate) fn sidebar_least_rems(cx: &App) -> f32 {
    match Chrome::get(cx) {
        Chrome::Mac => (LIGHTS_X + LIGHTS_SPAN + LIGHTS_X) / cx.theme().font_size,
        Chrome::System | Chrome::Windows { .. } => 0.,
    }
}

/// The room, in rems, that Windows' window buttons take of the row at the
/// window's right edge: zero without them. Kept in fullscreen too, as
/// [`sidebar_least_rems`] is.
pub(crate) fn caption_room_rems(cx: &App) -> f32 {
    match Chrome::get(cx) {
        Chrome::Windows { .. } => CAPTION_BUTTON * 3. / cx.theme().font_size,
        Chrome::System | Chrome::Mac => 0.,
    }
}

/// The room Windows' window buttons take at the right of the top row at
/// the window's right edge, which that row ends with in place of its
/// inset. `None` without them.
pub(crate) fn caption_room(window: &Window, cx: &App) -> Option<Div> {
    matches!(drawn(window, cx), Some(Drawn::Windows { .. }))
        .then(|| div().flex_shrink_0().w(CAPTION_BUTTON * 3.).h_full())
}

/// A strip as tall as the top row that moves the window, for a pane with
/// no row of its own to lay over its top. `None` where the app draws no
/// title bar.
pub(crate) fn top_strip(window: &Window, cx: &App) -> Option<Div> {
    drawn(window, cx).is_some().then(|| {
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(rems(TOP_ROW_REMS))
            .moves_window(DragArea::TopStrip, window, cx)
    })
}

/// Windows' window buttons, laid over the window's top right corner, on
/// the room [`caption_room`] leaves. Drawn after the rest of the window,
/// dialogs included: a Kit dialog's backdrop moves the window, and would
/// otherwise take the buttons' clicks. Menus and popups still go on top.
/// `None` elsewhere.
pub(crate) fn caption_buttons(window: &Window, cx: &App) -> Option<impl IntoElement> {
    let Some(Drawn::Windows { glyphs }) = drawn(window, cx) else {
        return None;
    };
    Some(deferred(CaptionButtons {
        glyphs: glyphs.clone(),
    }))
}

/// A part of the top row that moves the window. At most one of each is on
/// screen at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DragArea {
    SidebarTitle,
    SessionTitle,
    PanelHeader,
    TopStrip,
    TrafficLights,
}

/// Moving the window from a part of the top row.
pub(crate) trait MovesWindow: InteractiveElement + ParentElement + Sized {
    /// Makes this element `area`, which moves the window as a title bar
    /// does where the app draws its own: dragged, and on macOS
    /// double-clicked too, as the system settings say. Put it on parts that
    /// take no clicks: on Windows the system takes every click in it, so a
    /// button inside loses its clicks.
    fn moves_window(self, area: DragArea, window: &Window, cx: &App) -> Self {
        match drawn(window, cx) {
            None => self,
            // A hitbox laid over the element, which [`drag_areas`] marks.
            Some(Drawn::Windows { .. }) => self.child(
                canvas(
                    move |bounds, window, cx| {
                        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                        cx.default_global::<DragAreas>().0.insert(area, hitbox);
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            ),
            // As GPUI Kit's title bar: the window moves once a press moves,
            // so a click stays a click.
            Some(Drawn::Mac) => self
                .on_mouse_down(MouseButton::Left, |event, window, cx| {
                    if event.click_count == 2 {
                        window.titlebar_double_click();
                    }
                    cx.set_global(Pressed(event.click_count == 1));
                })
                // Anywhere, so a press that left without moving the window
                // can't move it on a later drag that only passes over.
                .on_mouse_up(MouseButton::Left, |_, _, cx| cx.set_global(Pressed(false)))
                .on_mouse_up_out(MouseButton::Left, |_, _, cx| cx.set_global(Pressed(false)))
                .on_mouse_move(|event, window, cx| {
                    let pressed = cx.try_global::<Pressed>().is_some_and(|pressed| pressed.0);
                    if pressed && event.pressed_button == Some(MouseButton::Left) {
                        cx.set_global(Pressed(false));
                        window.start_window_move();
                    }
                }),
        }
    }
}

impl<E: InteractiveElement + ParentElement> MovesWindow for E {}

/// The hitbox each drag area last laid out. Windows only; one window's, as
/// rho-ui has one.
#[derive(Default)]
struct DragAreas(HashMap<DragArea, Hitbox>);

impl Global for DragAreas {}

/// Marks every drag area's hitbox as one each frame, those of reused
/// views too: see the module's docs. For the uncached root to draw after
/// the views, whose hitboxes are laid out by then; `None` off Windows.
pub(crate) fn drag_areas(cx: &App) -> Option<impl IntoElement> {
    matches!(Chrome::get(cx), Chrome::Windows { .. }).then(|| {
        canvas(
            |_, _, _| {},
            |_, (), window, cx| {
                let Some(areas) = cx.try_global::<DragAreas>() else {
                    return;
                };
                for hitbox in areas.0.values() {
                    window.insert_window_control_hitbox(WindowControlArea::Drag, hitbox.clone());
                }
            },
        )
        .absolute()
    })
}

/// Whether the left button went down on a part of the top row that moves
/// the window and hasn't moved it yet. macOS only.
struct Pressed(bool);

impl Global for Pressed {}

/// Windows' minimize, maximize or restore, and close buttons. The system
/// acts on a click, as on its own title bar's: each is a window control
/// area, which also gives maximize its snap layouts. Plain elements, as
/// Kit's are private to its title bar, with Windows' own colors rather
/// than the theme's, as every Windows app's buttons have.
#[derive(IntoElement)]
struct CaptionButtons {
    glyphs: Option<SharedString>,
}

/// One of the window buttons.
#[derive(Clone, Copy)]
enum Caption {
    Minimize,
    Maximize,
    Restore,
    Close,
}

impl Caption {
    fn id(self) -> &'static str {
        match self {
            Self::Minimize => "window-minimize",
            Self::Maximize | Self::Restore => "window-maximize",
            Self::Close => "window-close",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Minimize => "Minimize",
            Self::Maximize => "Maximize",
            Self::Restore => "Restore",
            Self::Close => "Close",
        }
    }

    /// Its glyph in Windows' caption fonts, which put them at the same
    /// code points.
    fn glyph(self) -> &'static str {
        match self {
            Self::Minimize => "\u{E921}",
            Self::Maximize => "\u{E922}",
            Self::Restore => "\u{E923}",
            Self::Close => "\u{E8BB}",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Minimize => IconName::WindowMinimize,
            Self::Maximize => IconName::WindowMaximize,
            Self::Restore => IconName::WindowRestore,
            Self::Close => IconName::WindowClose,
        }
    }

    fn area(self) -> WindowControlArea {
        match self {
            Self::Minimize => WindowControlArea::Min,
            Self::Maximize | Self::Restore => WindowControlArea::Max,
            Self::Close => WindowControlArea::Close,
        }
    }
}

impl RenderOnce for CaptionButtons {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        // Dimmed while another window is active, as Windows' own.
        let foreground = if window.is_window_active() {
            theme.foreground
        } else {
            theme.muted_foreground
        };
        // Windows 11's subtle fills: the text color, faint.
        let (hover, active) = (
            theme.foreground.opacity(0.06),
            theme.foreground.opacity(0.04),
        );
        let red = Hsla::from(gpui_kit::rgb(CLOSE_RED));
        let maximize = if window.is_maximized() {
            Caption::Restore
        } else {
            Caption::Maximize
        };
        let button = |caption: Caption| {
            let (hover, active, hover_text) = match caption {
                Caption::Close => (red, red.opacity(0.9), gpui_kit::white()),
                _ => (hover, active, foreground),
            };
            let glyph = match &self.glyphs {
                Some(family) => div()
                    .font_family(family.clone())
                    .text_size(CAPTION_GLYPH)
                    .child(caption.glyph())
                    .into_any_element(),
                None => Icon::new(caption.icon()).small().into_any_element(),
            };
            div()
                .id(caption.id())
                .test_support()
                .aria_label(caption.label())
                .flex()
                .items_center()
                .justify_center()
                .w(CAPTION_BUTTON)
                .h_full()
                .text_color(foreground)
                .hover(move |style| style.bg(hover).text_color(hover_text))
                .active(move |style| style.bg(active).text_color(hover_text))
                .window_control_area(caption.area())
                .child(glyph)
        };
        h_flex()
            .id("window-buttons")
            .absolute()
            .top_0()
            .right_0()
            // Above the row's bottom border.
            .h(window.rem_size() * TOP_ROW_REMS - px(1.))
            // Hides the row under the buttons from hit tests, so Windows
            // finds the button's area there, not the row's.
            .occlude()
            .child(button(Caption::Minimize))
            .child(button(maximize))
            .child(button(Caption::Close))
    }
}
