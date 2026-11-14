//! A workspace's initials on a colored tile.
//!
//! Not Kit's `Avatar`: that sizes its initials by its `Size` preset rather
//! than the side it is given, so tiles of different sides get text of
//! different proportions, and it centers the line box rather than the
//! capitals in it. Here everything scales with the side, and the capitals
//! sit in the middle.

use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{oklch, ActiveTheme as _};
use gpui_kit::{
    div, px, relative, rems, App, Hsla, IntoElement, ParentElement as _, Rems, RenderOnce,
    SharedString, StyleRefinement, Styled, Window,
};

/// The initials' size, as a share of the tile's side.
const TEXT_SHARE: f32 = 0.5;
/// The corner radius, as a share of the tile's side.
const RADIUS_SHARE: f32 = 0.25;

/// A tile `side` rems square with `initials` in the middle, in colors picked
/// from the initials, so a project keeps its colors everywhere it shows.
#[derive(IntoElement)]
pub(crate) struct Badge {
    initials: SharedString,
    side: Rems,
    style: StyleRefinement,
}

impl Badge {
    pub(crate) fn new(initials: SharedString, side: Rems) -> Self {
        Self {
            initials,
            side,
            style: StyleRefinement::default(),
        }
    }
}

impl Styled for Badge {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Badge {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = Colors::new(&self.initials, cx);
        let text_size = rems(self.side.0 * TEXT_SHARE).to_pixels(window.rem_size());
        // A line box centers ascent over descent, which puts capitals off
        // the middle by however much the font's ascent above cap height
        // differs from its descent. Shift by that difference. The ascent and
        // descent are the shaped line's, as painted: the font metrics' descent
        // is negative on some platforms. A font without a cap height stays put.
        let text_system = window.text_system();
        let style = window.text_style();
        let cap_height = text_system.cap_height(text_system.resolve_font(&style.font()), text_size);
        let run = style.to_run(self.initials.len());
        let line = text_system.shape_line(self.initials.clone(), text_size, &[run], None);
        let nudge = if cap_height > px(0.) {
            (cap_height - line.ascent + line.descent) / 2.
        } else {
            px(0.)
        };

        div()
            .size(self.side)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .rounded(rems(self.side.0 * RADIUS_SHARE))
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .text_color(colors.foreground)
            .child(
                div()
                    .relative()
                    .top(nudge)
                    .text_size(text_size)
                    .line_height(relative(1.))
                    .whitespace_nowrap()
                    .child(self.initials),
            )
            .refine_style(&self.style)
    }
}

/// Kit `Avatar`'s palette, which Kit keeps private: 12 evenly spaced
/// `OkLCH` hues at one lightness and chroma, so every tile has the same visual
/// weight and legible text.
struct Colors {
    background: Hsla,
    foreground: Hsla,
    border: Hsla,
}

impl Colors {
    const HUES: u8 = 12;

    fn new(initials: &SharedString, cx: &App) -> Self {
        let step = u8::try_from(gpui_kit::hash(initials) % u64::from(Self::HUES))
            .expect("a remainder of HUES fits in u8");
        let hue = f32::from(step) * (360. / f32::from(Self::HUES));
        if cx.theme().is_dark() {
            Self {
                background: oklch(0.30, 0.05, hue),
                foreground: oklch(0.82, 0.11, hue),
                border: oklch(0.36, 0.06, hue),
            }
        } else {
            Self {
                background: oklch(0.97, 0.032, hue),
                foreground: oklch(0.50, 0.145, hue),
                border: oklch(0.89, 0.05, hue),
            }
        }
    }
}
