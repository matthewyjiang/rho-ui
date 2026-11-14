//! Mermaid diagrams, drawn by [pele](https://github.com/obsidianmd/pele),
//! Obsidian's renderer, running in an embedded `QuickJS`. pele is vendored in
//! `vendor/pele` (1.9.0, `dist/pele.global.js` from npm): update it by copying
//! that file again, and the tests here check what changed.
//!
//! pele colors a diagram with CSS custom properties (`var(--pele-fg)`), which
//! resvg, GPUI's SVG renderer, can't read. [`draw`] keeps them, so a drawing
//! is the same in every theme, and [`themed`] writes the theme's colors in
//! their place.
//!
//! One thread owns the engine, as a `QuickJS` runtime can't move between
//! threads; [`draw`] asks it and waits. Model-written diagrams are untrusted,
//! so each runs under [`MEMORY_LIMIT`] and [`TIME_LIMIT`]. A diagram that
//! goes over either, or that pele can't read, is not drawn.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

use rquickjs::{Context, Ctx, Function, Object, Runtime};

/// pele's browser build, which defines the global `pele`.
const PELE: &str = include_str!("../vendor/pele/pele.global.js");

/// The engine's heap. pele holds about 3 MiB once loaded, and drawing any
/// of the diagram kinds rho's prompt allows adds little to it; this leaves
/// room for large diagrams and stops a runaway one.
const MEMORY_LIMIT: usize = 64 << 20;

/// How long one diagram may take. Drawing each kind rho's prompt allows
/// takes 0.2 to 1.3 ms once pele is loaded; this stops only a runaway one.
const TIME_LIMIT: Duration = Duration::from_secs(1);

/// How many drawings the engine keeps. A streaming reply is parsed again
/// with each chunk, so its finished diagrams are asked for again; a
/// drawing is 2 to 4 KB, so this keeps every diagram of a long session in
/// well under a megabyte.
const KEPT: usize = 256;

/// The font size pele draws at, its default. [`Drawing::width`] and
/// [`Drawing::height`] are at this size.
pub(crate) const FONT_SIZE: f32 = 16.;

/// A diagram as pele draws it: an SVG colored with CSS custom properties,
/// and its size in pixels at [`FONT_SIZE`].
#[derive(Debug)]
pub(crate) struct Drawing {
    svg: String,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

struct Request {
    source: String,
    reply: mpsc::Sender<Option<Arc<Drawing>>>,
}

/// Starts the engine, which takes about 100 ms to load pele, so the first
/// diagram doesn't wait for it. [`draw`] starts it too.
pub(crate) fn start() {
    engine();
}

fn engine() -> &'static mpsc::Sender<Request> {
    static ENGINE: OnceLock<mpsc::Sender<Request>> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let (requests, received) = mpsc::channel();
        thread::Builder::new()
            .name("pele".to_owned())
            .spawn(move || serve(&received))
            .expect("the OS starts a thread for the diagram engine");
        requests
    })
}

/// Draws Mermaid `source`, waiting for the engine; `None` when pele can't.
pub(crate) fn draw(source: &str) -> Option<Arc<Drawing>> {
    let (reply, answer) = mpsc::channel();
    let request = Request {
        source: source.to_owned(),
        reply,
    };
    engine().send(request).ok()?;
    answer.recv().ok()?
}

/// The engine's thread: loads pele, then draws what it is asked. If pele
/// doesn't load, it returns, and every [`draw`] answers `None`.
fn serve(requests: &mpsc::Receiver<Request>) {
    let Some(engine) = Engine::new() else {
        return;
    };
    let mut kept: HashMap<String, Option<Arc<Drawing>>> = HashMap::new();
    for Request { source, reply } in requests {
        let drawing = if let Some(drawing) = kept.get(&source) {
            drawing.clone()
        } else {
            let drawing = engine.draw(&source).map(Arc::new);
            if kept.len() == KEPT {
                kept.clear();
            }
            kept.insert(source, drawing.clone());
            drawing
        };
        // The asker may have stopped waiting; nothing to do then.
        let _ = reply.send(drawing);
    }
}

struct Engine {
    runtime: Runtime,
    context: Context,
    /// When the diagram being drawn runs out of time, in milliseconds
    /// since `epoch`; `u64::MAX` between diagrams.
    deadline: Arc<AtomicU64>,
    epoch: Instant,
}

#[expect(
    clippy::print_stderr,
    reason = "rho-ui has no log; stderr reaches the terminal or the journal"
)]
impl Engine {
    fn new() -> Option<Self> {
        let loaded = || -> rquickjs::Result<Self> {
            let runtime = Runtime::new()?;
            runtime.set_memory_limit(MEMORY_LIMIT);
            let deadline = Arc::new(AtomicU64::new(u64::MAX));
            let epoch = Instant::now();
            let interrupt = Arc::clone(&deadline);
            runtime.set_interrupt_handler(Some(Box::new(move || {
                millis(epoch.elapsed()) > interrupt.load(Ordering::Relaxed)
            })));
            let context = Context::full(&runtime)?;
            context.with(|cx| cx.eval::<(), _>(PELE))?;
            Ok(Self {
                runtime,
                context,
                deadline,
                epoch,
            })
        };
        loaded()
            .inspect_err(|error| {
                eprintln!("rho-ui: pele didn't load, so diagrams show as code: {error}");
            })
            .ok()
    }

    fn draw(&self, source: &str) -> Option<Drawing> {
        let start = millis(self.epoch.elapsed());
        let limit = millis(TIME_LIMIT);
        self.deadline.store(start + limit, Ordering::Relaxed);
        let drawn = self.context.with(|cx| {
            let drawn = render(&cx, source);
            if drawn.is_err() {
                let error = cx.catch();
                let message = error
                    .as_object()
                    .and_then(|error| error.get::<_, String>("message").ok())
                    .unwrap_or_default();
                if millis(self.epoch.elapsed()) > start + limit {
                    eprintln!("rho-ui: a Mermaid diagram took over {TIME_LIMIT:?} to draw, the limit, so it shows as code");
                } else if message.contains("out of memory") {
                    eprintln!(
                        "rho-ui: a Mermaid diagram needed over {} MiB to draw, the limit, so it shows as code",
                        MEMORY_LIMIT >> 20
                    );
                }
            }
            drawn.ok()
        });
        self.deadline.store(u64::MAX, Ordering::Relaxed);
        if drawn.is_none() {
            self.runtime.run_gc();
        }
        drawn
    }
}

/// `pele.render(source)`, as a [`Drawing`].
fn render(cx: &Ctx<'_>, source: &str) -> rquickjs::Result<Drawing> {
    let pele: Object<'_> = cx.globals().get("pele")?;
    let render: Function<'_> = pele.get("render")?;
    let drawn: Object<'_> = render.call((source,))?;
    let size = |key| drawn.get::<_, f64>(key).map(narrow);
    Ok(Drawing {
        svg: drawn.get("svg")?,
        width: size("width")?,
        height: size("height")?,
    })
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// A pixel size from JavaScript's `f64`, as GPUI's `f32`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a diagram is at most thousands of pixels, well inside f32"
)]
#[expect(clippy::as_conversions, reason = "f64 to f32 has no From")]
fn narrow(pixels: f64) -> f32 {
    pixels as f32
}

/// What each of pele's custom properties is in the theme: `("fg",
/// "#e6e6ea")` for `--pele-fg`. A property left out takes pele's default.
pub(crate) type Palette = [(&'static str, String)];

/// The properties pele draws with that the theme sets; the rest are font
/// weights and zoom controls, which keep pele's defaults.
pub(crate) const THEMED: [&str; 21] = [
    "bg",
    "fg",
    "muted",
    "line",
    "surface",
    "surface-alt",
    "border",
    "accent",
    "critical",
    "error",
    "radius",
    "font",
    "font-mono",
    "series-1",
    "series-2",
    "series-3",
    "series-4",
    "series-5",
    "series-6",
    "series-7",
    "series-8",
];

/// `drawing` with `palette`'s values in place of pele's custom properties,
/// for resvg. pele's root `style` sets private properties (`--_fg`) from the
/// public ones (`--pele-fg`), and the rest of the drawing reads those. Their
/// declarations go once read: resvg ignores the rest of a `style` after
/// one, which would drop the root's font.
pub(crate) fn themed(drawing: &Drawing, palette: &Palette) -> String {
    let public = replace_vars(&drawing.svg, "--pele-", |name| {
        palette
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.clone())
    });
    let Some((style, private)) = root_style(&public) else {
        return public;
    };
    let declarations: HashMap<&str, &str> = private
        .split(';')
        .filter_map(|declaration| declaration.trim().strip_prefix("--_"))
        .filter_map(|declaration| declaration.split_once(':'))
        .map(|(name, value)| (name.trim(), value.trim()))
        .collect();
    let plain: Vec<&str> = private
        .split(';')
        .filter(|declaration| !declaration.trim().starts_with("--"))
        .collect();
    let svg = format!(
        "{}{}{}",
        &public[..style.start],
        plain.join(";"),
        &public[style.end..]
    );
    replace_vars(&svg, "--_", |name| {
        declarations.get(name).map(|value| (*value).to_owned())
    })
}

/// `svg` with each `var(--{prefix}NAME[, fallback])` replaced with
/// `value(NAME)`, else its fallback. One with neither stays.
fn replace_vars(svg: &str, prefix: &str, value: impl Fn(&str) -> Option<String>) -> String {
    let opening = format!("var({prefix}");
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(at) = rest.find(&opening) {
        let inner = &rest[at + opening.len()..];
        let Some(close) = inner.find(')') else {
            break;
        };
        let (name, fallback) = match inner[..close].split_once(',') {
            Some((name, fallback)) => (name.trim(), Some(fallback.trim())),
            None => (inner[..close].trim(), None),
        };
        out.push_str(&rest[..at]);
        match value(name).or_else(|| fallback.map(str::to_owned)) {
            Some(value) => out.push_str(&value),
            None => out.push_str(&rest[at..=at + opening.len() + close]),
        }
        rest = &inner[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Where the root element's `style` is in `svg`, and what it says.
fn root_style(svg: &str) -> Option<(std::ops::Range<usize>, &str)> {
    let tag_end = svg.find('>')?;
    let start = svg[..tag_end].find("style=\"")? + "style=\"".len();
    let len = svg[start..].find('"')?;
    Some((start..start + len, &svg[start..start + len]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One of each diagram kind rho's system prompt allows.
    const KINDS: [&str; 8] = [
        "flowchart TD\n  A[Start] --> B{Ready?}\n  B -- Yes --> C([Done])\n  subgraph Out\n    C\n  end",
        "sequenceDiagram\n  participant UI\n  participant D as daemon\n  UI->>D: handshake\n  D-->>UI: events\n  Note over UI: draws",
        "stateDiagram-v2\n  [*] --> Idle\n  Idle --> Running: prompt\n  Running --> [*]",
        "classDiagram\n  class Session {\n    +String id\n    +prompt(text) Turn\n  }\n  Session *-- Turn",
        "erDiagram\n  SESSION ||--o{ TURN : has\n  SESSION {\n    string id PK\n  }",
        "gitGraph\n  commit\n  branch feature\n  checkout feature\n  commit\n  checkout main\n  merge feature",
        "gantt\n  dateFormat YYYY-MM-DD\n  section Build\n    Render :a1, 2026-11-13, 2d\n    Tests :crit, after a1, 2d",
        "mindmap\n  root((rho-ui))\n    Transcript\n      Mermaid\n    Composer",
    ];

    fn palette() -> Vec<(&'static str, String)> {
        THEMED
            .iter()
            .map(|&name| (name, format!("#{:06x}", name.len())))
            .collect()
    }

    /// A pele update that draws with a property the theme doesn't set
    /// fails here, not in a diagram drawn in pele's own colors.
    #[test]
    fn every_kind_draws_in_the_theme_alone() {
        let palette = palette();
        for source in KINDS {
            let drawing = draw(source).unwrap_or_else(|| panic!("pele draws {source:?}"));
            assert!(drawing.width > 0. && drawing.height > 0., "{source:?}");
            for name in drawing.svg.split("var(--pele-").skip(1) {
                let name = name.split([',', ')']).next().unwrap_or_default();
                let default = name.ends_with("-weight");
                assert!(
                    THEMED.contains(&name) || default,
                    "the theme sets --pele-{name}"
                );
            }
            let svg = themed(&drawing, &palette);
            assert!(!svg.contains("var("), "every property is replaced: {svg}");
            assert!(!svg.contains(":var") && !svg.contains(";--"), "{svg}");
        }
    }

    #[test]
    fn a_property_takes_its_theme_value_else_its_fallback() {
        let drawing = Drawing {
            svg: r#"<svg style="--_fg:var(--pele-fg,#000);--_b:var(--pele-bold-weight,bold)"><text fill="var(--_fg)" font-weight="var(--_b)"/></svg>"#.to_owned(),
            width: 1.,
            height: 1.,
        };
        let svg = themed(&drawing, &[("fg", "#e6e6ea".to_owned())]);
        assert!(svg.contains(r##"fill="#e6e6ea""##), "{svg}");
        assert!(svg.contains(r#"font-weight="bold""#), "{svg}");
        assert!(!svg.contains("--"), "no declarations left for resvg: {svg}");
    }

    #[test]
    fn what_pele_cant_read_is_not_drawn() {
        assert!(draw("flowchart TD\n  A -->").is_none());
        assert!(draw("not a diagram").is_none());
    }
}
