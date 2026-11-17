//! pele in an embedded `QuickJS`, off the browser. One thread owns each
//! engine, as a `QuickJS` runtime can't move between threads; [`draw`] and
//! [`ask`] send it a diagram. There are two, one per [`Lane`]. Every
//! diagram is untrusted, written by a model or by anyone who can comment
//! on a PR, so each runs under [`MEMORY_LIMIT`] and [`TIME_LIMIT`]. A
//! diagram that goes over either, or that pele can't read, is not drawn.

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

use futures::channel::oneshot;
use rquickjs::{Context, Ctx, Function, Object, Runtime};

use super::{narrow, Drawing, Lane, KEPT, PELE};

/// The engine's heap. pele holds about 3 MiB once loaded, and drawing any
/// of the diagram kinds rho's prompt allows adds little to it; this leaves
/// room for large diagrams and stops a runaway one.
const MEMORY_LIMIT: usize = 64 << 20;

/// How long one diagram may take. Drawing each kind rho's prompt allows
/// takes 0.2 to 1.3 ms once pele is loaded; this stops only a runaway one.
const TIME_LIMIT: Duration = Duration::from_secs(1);

struct Request {
    source: String,
    reply: oneshot::Sender<Option<Arc<Drawing>>>,
}

/// Starts the messages' engine, which takes about 100 ms to load pele, so
/// the first diagram doesn't wait for it. [`draw`] starts it too.
pub(crate) fn start() {
    engine(Lane::Messages);
}

fn engine(lane: Lane) -> &'static mpsc::Sender<Request> {
    static MESSAGES: OnceLock<mpsc::Sender<Request>> = OnceLock::new();
    static PULL_REQUESTS: OnceLock<mpsc::Sender<Request>> = OnceLock::new();
    let (engine, name) = match lane {
        Lane::Messages => (&MESSAGES, "pele"),
        Lane::PullRequests => (&PULL_REQUESTS, "pele-prs"),
    };
    engine.get_or_init(|| {
        let (requests, received) = mpsc::channel();
        thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || serve(&received))
            .expect("the OS starts a thread for the diagram engine");
        requests
    })
}

/// Draws Mermaid `source` for a message, waiting for the engine; `None`
/// when pele can't.
pub(crate) fn draw(source: &str) -> Option<Arc<Drawing>> {
    futures::executor::block_on(ask(Lane::Messages, source.to_owned()))
}

/// Asks `lane`'s engine to draw Mermaid `source`, without waiting; the
/// future resolves to `None` when pele can't. Dropped before the engine
/// gets to it, the diagram isn't drawn.
pub(crate) fn ask(lane: Lane, source: String) -> impl Future<Output = Option<Arc<Drawing>>> {
    let (reply, answer) = oneshot::channel();
    let sent = engine(lane).send(Request { source, reply }).is_ok();
    async move {
        if !sent {
            return None;
        }
        answer.await.ok()?
    }
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
        } else if reply.is_canceled() {
            // No one waits for it any more: a PR's text that changed.
            continue;
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
