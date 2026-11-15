//! A picture at full size in a dialog over the window, its caption under
//! it. It opens fitted to the dialog, never larger than its own pixels.
//! The buttons, the mouse wheel, a pinch, a double click, and the keys
//! `+`, `-`, `0` (fit) and `1` (full size) zoom it, around the pointer
//! where there is one; a trackpad's scroll and a drag move it.

use std::{cell::Cell, rc::Rc};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    dialog::DialogEntrance,
    h_flex, v_flex, ActiveTheme as _, Icon, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    canvas, div, img, point, px, size, App, AppContext as _, Bounds, Context, ElementId,
    FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, ObjectFit, ParentElement as _, PinchEvent, Pixels, Point, Render, ScrollDelta,
    ScrollEvent, SharedString, Size, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window,
};

use crate::attachments::ImagePreview;

/// How much of the window the picture's stage takes: 1080 x 576 px in the
/// default 1200 x 800 window, leaving room for the bar above and the
/// caption below.
const STAGE_SHARE: Size<f32> = Size {
    width: 0.9,
    height: 0.72,
};

/// What the dialog holds besides the stage, in rems, so a short window
/// shrinks the stage rather than the dialog cutting it off: the padding
/// above and below (2), the bar of small buttons (2), the gaps around the
/// stage (1), and a caption of at most two lines of small text (2.5).
const CHROME_REMS: f32 = 7.5;

/// The most a picture zooms in: 16 screen pixels to one of its own, close
/// enough to tell its pixels apart.
const MAX_SCALE: f32 = 16.;

/// How much one press of a zoom button or key zooms.
const STEP: f32 = 1.5;

/// How much one notch of a mouse wheel zooms.
const WHEEL_STEP: f32 = 1.2;

/// The dialog's padding, and the room its close button takes at the top
/// right, in rems.
const PAD_REMS: f32 = 1.;
const CLOSE_ROOM_REMS: f32 = 3.;

/// Opens `picture` in the lightbox, with `caption` under it. Escape, the
/// close button, or a click outside closes it.
pub(crate) fn open(
    picture: ImagePreview,
    caption: SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    if window.has_active_dialog(cx) {
        return;
    }
    let view = cx.new(|cx| Lightbox::new(picture, caption, cx));
    let first = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        // The view takes the keys, once: the dialog's own Escape still
        // reaches the dialog, which holds the view.
        if first.replace(false) {
            let focus = view.read(cx).focus.clone();
            focus.focus(window, cx);
        }
        let stage = stage_size(window, cx);
        let pad = window.rem_size() * PAD_REMS;
        let view = view.clone();
        dialog
            .entrance(DialogEntrance::Fade)
            .centered()
            .w(stage.width + pad * 2.)
            .p_0()
            .content(move |content, _, _| content.child(view.clone()))
    });
}

/// The stage's size in `window`: its share of it, no taller than leaves
/// the rest of the dialog room within the margins Kit keeps around one.
fn stage_size(window: &Window, cx: &App) -> Size<Pixels> {
    let viewport = window.viewport_size();
    let margin = cx.theme().spacing_tokens().lg;
    let room = viewport.height - margin * 2. - window.rem_size() * CHROME_REMS;
    size(
        viewport.width * STAGE_SHARE.width,
        (viewport.height * STAGE_SHARE.height).min(room).max(px(0.)),
    )
}

/// How large the picture shows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Zoom {
    /// As large as fits the stage, up to its own size, centered.
    Fit,
    /// This many screen pixels to one of the picture's.
    Scale(f32),
}

/// The lightbox's view: the picture, how it is zoomed and moved, and the
/// bar and caption around it.
pub(crate) struct Lightbox {
    picture: ImagePreview,
    caption: SharedString,
    zoom: Zoom,
    /// The picture's center from the stage's, while zoomed.
    offset: Point<Pixels>,
    /// While a drag moves the picture: where the pointer was.
    drag: Option<Point<Pixels>>,
    /// Where the stage was last laid out, in the window.
    stage: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
}

impl Lightbox {
    fn new(picture: ImagePreview, caption: SharedString, cx: &mut Context<'_, Self>) -> Self {
        Self {
            picture,
            caption,
            zoom: Zoom::Fit,
            offset: Point::default(),
            drag: None,
            stage: Rc::new(Cell::new(Bounds::default())),
            focus: cx.focus_handle(),
        }
    }

    /// The picture's size in its own pixels.
    fn natural(&self) -> Size<f32> {
        let (width, height) = self.picture.pixels();
        #[expect(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "f32 holds every whole number of pixels up to 2^24 exactly"
        )]
        size(width as f32, height as f32)
    }

    /// The scale [`Zoom::Fit`] shows the picture at on a stage of `stage`.
    fn fit_scale(&self, stage: Size<Pixels>) -> f32 {
        let natural = self.natural();
        (f32::from(stage.width) / natural.width)
            .min(f32::from(stage.height) / natural.height)
            .min(1.)
    }

    fn scale(&self, stage: Size<Pixels>) -> f32 {
        match self.zoom {
            Zoom::Fit => self.fit_scale(stage),
            Zoom::Scale(scale) => scale,
        }
    }

    /// Zooms to `scale`, keeping the point under `around`, in the window,
    /// where it is; the stage's center without one. At or under the fit,
    /// the picture fits again.
    fn zoom_to(&mut self, scale: f32, around: Option<Point<Pixels>>, cx: &mut Context<'_, Self>) {
        let stage = self.stage.get();
        // Not laid out yet: there is nothing to zoom.
        if stage.size.width <= px(0.) || stage.size.height <= px(0.) {
            return;
        }
        let fit = self.fit_scale(stage.size);
        let before = self.scale(stage.size);
        let scale = scale.min(MAX_SCALE);
        if scale <= fit {
            self.zoom = Zoom::Fit;
            self.offset = Point::default();
        } else {
            let around = around.unwrap_or_else(|| stage.center());
            let center = stage.center() + self.offset;
            let moved = center + (around - center) * (1. - scale / before);
            self.zoom = Zoom::Scale(scale);
            self.offset = self.clamped(moved - stage.center(), scale, stage.size);
        }
        cx.notify();
    }

    fn zoom_by(&mut self, factor: f32, around: Option<Point<Pixels>>, cx: &mut Context<'_, Self>) {
        let scale = self.scale(self.stage.get().size) * factor;
        self.zoom_to(scale, around, cx);
    }

    /// `offset` kept so the picture covers the stage on any side it is
    /// larger than the stage, and is centered on any side it is smaller.
    fn clamped(&self, offset: Point<Pixels>, scale: f32, stage: Size<Pixels>) -> Point<Pixels> {
        let natural = self.natural();
        let slack = |side: f32, stage: Pixels| px(((side * scale - f32::from(stage)) / 2.).max(0.));
        let (x, y) = (
            slack(natural.width, stage.width),
            slack(natural.height, stage.height),
        );
        point(offset.x.clamp(-x, x), offset.y.clamp(-y, y))
    }

    /// Moves the picture by `delta`, while zoomed.
    fn pan(&mut self, delta: Point<Pixels>, cx: &mut Context<'_, Self>) {
        let Zoom::Scale(scale) = self.zoom else {
            return;
        };
        self.offset = self.clamped(self.offset + delta, scale, self.stage.get().size);
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        cx.stop_propagation();
        match event.delta {
            // A mouse wheel zooms, as in an image viewer.
            ScrollDelta::Lines(lines) => {
                self.zoom_by(WHEEL_STEP.powf(lines.y), Some(event.position), cx);
            }
            // A trackpad moves the picture, or zooms with the platform's
            // zoom modifier held, as in a browser.
            ScrollDelta::Pixels(_) if event.modifiers.secondary(cx) => {
                let lines = event.delta.pixel_delta(window.line_height()).y / window.line_height();
                self.zoom_by(WHEEL_STEP.powf(lines), Some(event.position), cx);
            }
            ScrollDelta::Pixels(delta) => self.pan(delta, cx),
        }
    }

    fn on_pinch(&mut self, event: &PinchEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.zoom_by(1. + event.delta, Some(event.position), cx);
    }

    /// A press starts a drag; a double click switches between the fit and
    /// full size, at the pointer.
    fn on_press(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        if event.click_count == 2 {
            let full = match self.zoom {
                Zoom::Fit => 1.,
                Zoom::Scale(_) => 0.,
            };
            self.zoom_to(full, Some(event.position), cx);
            return;
        }
        self.drag = Some(event.position);
    }

    fn on_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(from) = self.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        self.drag = Some(event.position);
        self.pan(event.position - from, cx);
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.secondary(cx) || keystroke.modifiers.alt {
            return;
        }
        match keystroke.key.as_str() {
            "+" | "=" => self.zoom_by(STEP, None, cx),
            "-" => self.zoom_by(1. / STEP, None, cx),
            "0" => self.zoom_to(0., None, cx),
            "1" => self.zoom_to(1., None, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The picture on the stage, at its zoom.
    fn picture(&self, stage: Size<Pixels>) -> impl IntoElement {
        let natural = self.natural();
        let scale = self.scale(stage);
        let shown = size(px(natural.width * scale), px(natural.height * scale));
        let origin = point(
            (stage.width - shown.width) / 2. + self.offset.x,
            (stage.height - shown.height) / 2. + self.offset.y,
        );
        div()
            .id("lightbox-picture")
            .test_support()
            .absolute()
            .left(origin.x)
            .top(origin.y)
            .w(shown.width)
            .h(shown.height)
            .child(
                img(self.picture.image())
                    .size_full()
                    .object_fit(ObjectFit::Fill),
            )
    }

    /// The bar above the stage: the picture's size and format, how far it
    /// is zoomed, and the zoom buttons.
    fn bar(
        &self,
        stage: Size<Pixels>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let percent = (self.scale(stage) * 100.).round();
        let button = |id: &'static str, icon: Lucide, label: &'static str| {
            Button::new(id)
                .ghost()
                .small()
                .icon(Icon::new(icon))
                .accessibility_label(label)
                .tooltip(label)
        };
        h_flex()
            .w_full()
            .gap_1()
            .pr(window.rem_size() * CLOSE_ROOM_REMS)
            .text_sm()
            .child(
                div()
                    .flex_1()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.picture.label()),
            )
            .child(
                button("lightbox-zoom-out", Lucide::ZoomOut, "Zoom out")
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(1. / STEP, None, cx))),
            )
            .child(
                div()
                    .id("lightbox-zoom")
                    .test_support()
                    .w(window.rem_size() * 3.5)
                    .text_center()
                    .child(format!("{percent}%")),
            )
            .child(
                button("lightbox-zoom-in", Lucide::ZoomIn, "Zoom in")
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(STEP, None, cx))),
            )
            .child(
                button("lightbox-fit", Lucide::Scan, "Fit to the window")
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_to(0., None, cx))),
            )
            .child(
                Button::new("lightbox-full-size")
                    .ghost()
                    .small()
                    .label("1:1")
                    .tooltip("Full size")
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_to(1., None, cx))),
            )
    }
}

impl Render for Lightbox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let stage = stage_size(window, cx);
        let zoomed = matches!(self.zoom, Zoom::Scale(_));
        let laid_out = Rc::clone(&self.stage);
        let theme = cx.theme();
        let (backdrop, muted) = (theme.muted, theme.muted_foreground);
        v_flex()
            .id("lightbox")
            .test_support()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .p(window.rem_size() * PAD_REMS)
            .gap_2()
            .child(self.bar(stage, window, cx))
            .child(
                div()
                    .id("lightbox-stage")
                    .test_support()
                    .relative()
                    .w(stage.width)
                    .h(stage.height)
                    .overflow_hidden()
                    .rounded(cx.theme().radius)
                    .bg(backdrop)
                    .when(zoomed, |this| {
                        if self.drag.is_some() {
                            this.cursor_grabbing()
                        } else {
                            this.cursor_grab()
                        }
                    })
                    .on_scroll(cx.listener(Self::on_scroll))
                    .on_pinch(cx.listener(Self::on_pinch))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_press))
                    .on_mouse_move(cx.listener(Self::on_move))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.drag = None;
                            cx.notify();
                        }),
                    )
                    // Its bounds, for zooming around the pointer.
                    .child(
                        canvas(move |bounds, _, _| laid_out.set(bounds), |_, (), _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .child(self.picture(stage)),
            )
            .when(!self.caption.is_empty(), |this| {
                this.child(
                    div()
                        .id("lightbox-caption")
                        .test_support()
                        .w(stage.width)
                        .text_sm()
                        .text_center()
                        .text_color(muted)
                        .line_clamp(2)
                        .child(self.caption.clone()),
                )
            })
    }
}

/// `child`, an image's thumbnail, with `id`, opening `picture` in the
/// lightbox with `caption` when clicked. Screen readers name it as the
/// image it is, such as "PNG image, 1920 × 1080".
pub(crate) fn opens(
    id: impl Into<ElementId>,
    picture: &ImagePreview,
    caption: SharedString,
    child: impl IntoElement,
) -> impl IntoElement {
    let picture = picture.clone();
    div()
        .id(id)
        .aria_label(picture.label())
        .test_support()
        .cursor_pointer()
        // A press on a picture in a message starts no text selection,
        // which would take the keyboard back from the lightbox.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            open(picture.clone(), caption.clone(), window, cx);
        })
        .child(child)
}
