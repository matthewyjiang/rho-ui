//! Mermaid diagrams and LaTeX formulas in messages, drawn in the theme's
//! colors and fonts. Each is a GPUI Kit Markdown plugin: a ` ```mermaid `
//! fence becomes a diagram ([`pele`]), and `$$ … $$` and `$ … $` become
//! formulas (`RaTeX`). Each draws an SVG that GPUI rasterizes as an image.
//!
//! What the text can't be drawn as stays as written: a diagram pele can't
//! draw shows as its code block, a formula `RaTeX` can't lay out as its
//! source, and dollar signs that read as prices as prose.

use std::sync::{Arc, Mutex, PoisonError};

use gpui_kit::component::text::{
    markdown_ast as mdast, InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode,
    MarkdownParseContext, MarkdownPlugin,
};
use gpui_kit::component::{h_flex, ActiveTheme as _, Theme};
use gpui_kit::{base::TestSupportExt as _, InteractiveElement as _};

use crate::pele;
use gpui_kit::{
    div, img, px, size, AnyElement, App, Hsla, Image, ImageFormat, IntoElement, ParentElement as _,
    Pixels, Rgba, SharedString, Size, Styled as _, Window,
};
use ratex_layout::{layout, to_display_list, validate_layout, LayoutOptions};
use ratex_svg::{SvgColorSyntax, SvgOptions};
use ratex_types::{
    color::Color,
    display_item::{DisplayItem, DisplayList},
    math_style::MathStyle,
};

/// How much larger a formula's letters are than the text around it:
/// `KaTeX`'s `.katex { font-size: 1.21em }`, which `RaTeX`'s metrics follow.
const MATH_SCALE: f32 = 1.21;

/// The color `RaTeX` lays formulas out in, replaced with the text's color when
/// drawn: transparent black, which no formula asks for. A part with a color
/// of its own (`\color{red}`) keeps it.
const INK: Color = Color::new(0., 0., 0., 0.);

/// The plugins, for a [`gpui_kit::component::text::TextView`] showing a
/// message.
pub(crate) fn extensions() -> MarkdownExtensions {
    MarkdownExtensions::default()
        .plugin(Diagrams)
        .plugin(DisplayMath)
        .plugin(InlineMath)
}

/// A drawn figure: its image and the size it shows at.
#[derive(Clone)]
struct Drawn {
    image: Arc<Image>,
    size: Size<Pixels>,
}

/// The last drawing of one figure, and what it was drawn for. Kit renders
/// a message every frame and keeps its parsed nodes while the text stands,
/// so this redraws only when the theme or text size changes.
struct Memo<K>(Mutex<Option<(K, Drawn)>>);

impl<K: PartialEq> Memo<K> {
    fn new() -> Self {
        Self(Mutex::new(None))
    }

    fn get(&self, key: K, draw: impl FnOnce(&K) -> Option<Drawn>) -> Option<Drawn> {
        let mut memo = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((drawn_for, drawn)) = memo.as_ref() {
            if *drawn_for == key {
                return Some(drawn.clone());
            }
        }
        let drawn = draw(&key)?;
        *memo = Some((key, drawn.clone()));
        Some(drawn)
    }
}

/// An SVG as an image GPUI can draw.
fn svg_image(svg: String) -> Arc<Image> {
    Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()))
}

/// `color` laid over `under`, as an opaque `#rrggbb`: the SVGs get no alpha
/// to blend, and theme colors such as the selection have it.
fn hex(color: Hsla, under: Hsla) -> String {
    let Rgba { r, g, b, .. } = under.blend(color).to_rgb();
    format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
}

/// A color channel, 0 to 1, as a byte.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "clamped to 0..=255 and rounded first; f32 has no TryFrom to u8"
)]
fn byte(channel: f32) -> u8 {
    (channel.clamp(0., 1.) * 255.).round() as u8
}

/// A block figure, centered, at its own size or scaled down to fit the
/// message, keeping its proportions. `id` names its row, the message's
/// width; the figure in it is `picture`.
fn block(id: SharedString, drawn: Drawn) -> AnyElement {
    let Size { width, height } = drawn.size;
    h_flex()
        .id(id)
        .test_support()
        .w_full()
        .justify_center()
        .my_2()
        .child(
            div()
                .id("picture")
                .test_support()
                .w_full()
                .max_w(width)
                .aspect_ratio(width / height)
                .child(img(drawn.image).size_full()),
        )
        .into_any_element()
}

/// The ID of the figure `node` became, from where it starts in the message.
fn figure_id(kind: &str, node: &MarkdownNode) -> SharedString {
    let start = node.source_range().map_or(0, |range| range.start);
    format!("{kind}-{start}").into()
}

/// Whether `fence`, a fenced code block's source, has its closing fence. A
/// streaming reply's open block holds what arrived so far.
fn is_closed(fence: &str) -> bool {
    let fence = fence.trim();
    let Some(mark) = fence.chars().next().filter(|c| matches!(c, '`' | '~')) else {
        return false;
    };
    let opening = fence.chars().take_while(|&c| c == mark).count();
    let Some((_, last)) = fence.rsplit_once('\n') else {
        return false;
    };
    let last = last.trim();
    last.len() >= opening && last.chars().all(|c| c == mark)
}

/// ` ```mermaid ` fences, drawn as diagrams by [`pele`].
struct Diagrams;

/// pele's drawing of a diagram, the same in every theme, and its last
/// drawing in one.
struct Diagram {
    drawing: Arc<pele::Drawing>,
    memo: Memo<DiagramStyle>,
}

impl MarkdownPlugin for Diagrams {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "mermaid"
    }

    /// Kit parses off the UI thread, except for a short message, which
    /// waits about a millisecond for pele per diagram.
    fn parse(&self, node: &mdast::Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let mdast::Node::Code(code) = node else {
            return None;
        };
        if code.lang.as_deref() != Some("mermaid") {
            return None;
        }
        let fence = cx.node_source(node)?;
        // Until the fence closes, the code shows: a diagram would redraw,
        // and jump, with every chunk of a streaming reply.
        if !is_closed(fence) {
            return None;
        }
        let data = Diagram {
            drawing: pele::draw(&code.value)?,
            memo: Memo::new(),
        };
        Some(
            MarkdownNode::new("mermaid", data)
                .text(code.value.clone())
                .markdown(fence.to_owned())
                .accessibility_label("Mermaid diagram"),
        )
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(data) = node.data::<Diagram>() else {
            return div().into_any_element();
        };
        let drawn = data.memo.get(DiagramStyle::new(cx.theme()), |style| {
            // pele draws at its own font size; the diagram scales to the
            // theme's, which the transcript's text is set in.
            let scale = style.font_size / pele::FONT_SIZE;
            let drawing = &data.drawing;
            Some(Drawn {
                size: size(px(drawing.width * scale), px(drawing.height * scale)),
                image: svg_image(pele::themed(drawing, &style.palette)),
            })
        });
        match drawn {
            Some(drawn) => block(figure_id("diagram", node), drawn),
            None => div().into_any_element(),
        }
    }
}

/// The theme as a diagram draws it: a value for each of pele's custom
/// properties, and the text size.
#[derive(Clone, Debug, PartialEq)]
struct DiagramStyle {
    palette: Vec<(&'static str, String)>,
    font_size: f32,
}

impl DiagramStyle {
    fn new(theme: &Theme) -> Self {
        let hex = |color| hex(color, theme.background);
        let series = [
            theme.blue,
            theme.green,
            theme.yellow,
            theme.magenta,
            theme.cyan,
            theme.red,
            theme.blue_light,
            theme.green_light,
        ]
        .map(hex);
        let [s1, s2, s3, s4, s5, s6, s7, s8] = series;
        let palette = vec![
            ("bg", hex(theme.background)),
            ("fg", hex(theme.foreground)),
            ("muted", hex(theme.muted_foreground)),
            ("line", hex(theme.muted_foreground)),
            ("surface", hex(theme.muted)),
            ("surface-alt", hex(theme.secondary)),
            ("border", hex(theme.input)),
            ("accent", hex(theme.ring)),
            ("critical", hex(theme.danger)),
            ("error", hex(theme.danger)),
            ("radius", format!("{}px", f32::from(theme.radius))),
            // Single quotes: these land in double-quoted attributes. The
            // generic family covers one resvg can't find, such as Kit's
            // `.SystemUIFont`.
            ("font", format!("'{}', sans-serif", theme.font_family)),
            (
                "font-mono",
                format!("'{}', monospace", theme.mono_font_family),
            ),
            ("series-1", s1),
            ("series-2", s2),
            ("series-3", s3),
            ("series-4", s4),
            ("series-5", s5),
            ("series-6", s6),
            ("series-7", s7),
            ("series-8", s8),
        ];
        debug_assert!(
            pele::THEMED
                .iter()
                .all(|name| palette.iter().any(|(key, _)| key == name)),
            "the palette sets every property pele themes"
        );
        Self {
            palette,
            font_size: f32::from(theme.font_size),
        }
    }
}

/// A laid-out formula and its last drawing.
struct Formula {
    list: DisplayList,
    memo: Memo<Ink>,
}

/// What a formula is drawn for: its color, as RGBA bytes, and its em in
/// pixels, as bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ink {
    color: [u8; 4],
    em: u32,
}

impl Ink {
    fn new(color: Hsla, em: Pixels) -> Self {
        let Rgba { r, g, b, a } = color.to_rgb();
        Self {
            color: [byte(r), byte(g), byte(b), byte(a)],
            em: f32::from(em).to_bits(),
        }
    }

    fn color(self) -> Color {
        let [r, g, b, a] = self.color.map(|byte| f32::from(byte) / 255.);
        Color::new(r, g, b, a)
    }

    fn em(self) -> f32 {
        f32::from_bits(self.em)
    }
}

impl Formula {
    /// Lays out `tex` in `style`; `None` when `RaTeX` can't.
    fn new(tex: &str, style: MathStyle) -> Option<Self> {
        let nodes = ratex_parser::parser::parse(tex).ok()?;
        let options = LayoutOptions::default().with_style(style).with_color(INK);
        let laid_out = layout(&nodes, &options);
        validate_layout(&laid_out).ok()?;
        let list = to_display_list(&laid_out);
        (list.width > 0.).then(|| Self {
            list,
            memo: Memo::new(),
        })
    }

    /// The formula drawn in `ink`, with its baseline's distance from its
    /// top.
    fn draw(&self, ink: Ink) -> Option<(Drawn, Pixels)> {
        let em = ink.em();
        let drawn = self.memo.get(ink, |ink| {
            let mut list = self.list.clone();
            for item in &mut list.items {
                let (DisplayItem::GlyphPath { color, .. }
                | DisplayItem::Line { color, .. }
                | DisplayItem::Rect { color, .. }
                | DisplayItem::Path { color, .. }) = item;
                if *color == INK {
                    *color = ink.color();
                }
            }
            let options = SvgOptions {
                font_size: f64::from(em),
                padding: 0.,
                embed_glyphs: true,
                ..SvgOptions::default()
            };
            let svg =
                ratex_svg::render_to_svg_with_color_syntax(&list, &options, SvgColorSyntax::Rgb);
            let width = px(narrow(list.width) * em);
            let height = px(narrow(list.total_height()) * em);
            Some(Drawn {
                image: svg_image(svg),
                size: size(width, height),
            })
        })?;
        Some((drawn, px(narrow(self.list.height) * em)))
    }
}

/// A length in ems from `RaTeX`, as GPUI's `f32`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a formula is a few ems across, well inside f32"
)]
#[expect(clippy::as_conversions, reason = "f64 to f32 has no From")]
fn narrow(ems: f64) -> f32 {
    ems as f32
}

/// `$$ … $$` blocks, drawn as display formulas.
struct DisplayMath;

impl MarkdownPlugin for DisplayMath {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "display-math"
    }

    fn parse(&self, node: &mdast::Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let mdast::Node::Math(math) = node else {
            return None;
        };
        let formula = Formula::new(&math.value, MathStyle::Display)?;
        let source = cx.node_source(node).unwrap_or(&math.value);
        Some(
            MarkdownNode::new("display-math", formula)
                .text(math.value.clone())
                .markdown(source.to_owned())
                .accessibility_label(math.value.clone()),
        )
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let ink = Ink::new(theme.foreground, theme.font_size * MATH_SCALE);
        match node.data::<Formula>().and_then(|formula| formula.draw(ink)) {
            Some((drawn, _)) => block(figure_id("formula", node), drawn),
            None => div().into_any_element(),
        }
    }
}

/// `$ … $` in a line, drawn as inline formulas on the text's baseline.
struct InlineMath;

impl MarkdownPlugin for InlineMath {
    fn name(&self) -> &'static str {
        "inline-math"
    }

    fn parse(&self, node: &mdast::Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let mdast::Node::InlineMath(math) = node else {
            return None;
        };
        let source = cx.node_source(node)?;
        let end = node.position()?.end.offset;
        let next = cx.source().get(end..).and_then(|rest| rest.chars().next());
        if !is_formula(source, next) {
            return None;
        }
        let formula = Formula::new(&math.value, MathStyle::Text)?;
        Some(
            MarkdownNode::new("inline-math", formula)
                .text(source.to_owned())
                .markdown(source.to_owned())
                .accessibility_label(math.value.clone()),
        )
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        _: &mut App,
    ) -> Option<InlineElement> {
        let ink = Ink::new(context.text_style().color, context.font_size() * MATH_SCALE);
        let (drawn, baseline) = node.data::<Formula>()?.draw(ink)?;
        let Size { width, height } = drawn.size;
        let image = img(drawn.image).w(width).h(height);
        Some(InlineElement::new(image).with_baseline(baseline))
    }
}

/// Whether `source`, an inline math span with its dollar signs, followed by
/// `next`, reads as a formula rather than prices. Pandoc's rule: the
/// opening `$` has no space after it, the closing one none before it and no
/// digit after it, so "costs $5 and $10" stays prose and `$x^2$` is math.
fn is_formula(source: &str, next: Option<char>) -> bool {
    let inner = source.trim_matches('$');
    let starts = inner.chars().next().is_some_and(|c| !c.is_whitespace());
    let ends = inner.chars().last().is_some_and(|c| !c.is_whitespace());
    starts && ends && !next.is_some_and(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollar_amounts_stay_prose() {
        assert!(!is_formula("$5 and $", Some('1')));
        assert!(!is_formula("$ 5 $", None));
        assert!(
            !is_formula("$5 and $", Some(' ')),
            "a space before the closing $"
        );
        assert!(is_formula("$x^2$", Some('.')));
        assert!(is_formula("$$\\frac{a}{b}$$", None));
    }

    #[test]
    fn an_open_fence_is_still_streaming() {
        assert!(!is_closed("```mermaid\nflowchart TD\n  A --> B"));
        assert!(!is_closed("```mermaid"));
        assert!(
            !is_closed("````mermaid\nA --> B\n```"),
            "shorter than the opening"
        );
        assert!(is_closed("```mermaid\nflowchart TD\n  A --> B\n```"));
        assert!(is_closed("~~~mermaid\nA --> B\n~~~~\n"));
    }

    #[test]
    fn a_formula_takes_the_text_color_and_keeps_its_own() {
        let formula = Formula::new(r"x + \color{red}{y}", MathStyle::Text).expect("lays out");
        let blue = Hsla::from(Rgba {
            r: 0.,
            g: 0.,
            b: 1.,
            a: 1.,
        });
        let (drawn, baseline) = formula.draw(Ink::new(blue, px(20.))).expect("draws");
        let svg = std::str::from_utf8(drawn.image.bytes()).expect("SVG is text");
        assert!(svg.contains("rgb(0,0,255)"), "the text's color: {svg}");
        assert!(svg.contains("rgb(255,0,0)"), "red stays red: {svg}");
        assert!(baseline > px(0.) && baseline < drawn.size.height);
    }
}
