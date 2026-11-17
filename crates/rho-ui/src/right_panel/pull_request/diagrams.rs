//! The Mermaid diagrams in the PR on screen. Anyone who can comment on a PR
//! writes its text, and Kit parses short text on the UI thread, so a PR's
//! diagrams are drawn first, off it, on pele's engine for PRs
//! ([`pele::Lane::PullRequests`]); then its text is parsed again with them.
//! A diagram shows as code until it is drawn.

use std::collections::HashSet;
use std::sync::Arc;

use futures::stream::{FuturesUnordered, StreamExt as _};
use gpui_kit::component::text::MarkdownExtensions;
use gpui_kit::{Context, SharedString, Task};

use super::PullRequestSurface;
use crate::{figures, pele};

/// How many different diagrams of one PR are drawn, its description's
/// first; the rest show as code. Each may take pele up to its one-second
/// limit, so this bounds what a PR's text costs each time it changes. Of
/// the 329 newest PRs of rho and rho-ui, none has more than one diagram in
/// its description.
const DIAGRAM_LIMIT: usize = 20;

/// The diagrams drawn for a surface's PR, and the drawing under way.
pub(super) struct DiagramState {
    drawn: Arc<figures::Drawings>,
    /// Kit's parser revision, bumped whenever `drawn` gains a diagram, so
    /// the text is parsed again with it.
    revision: u64,
    drawing: Option<Task<()>>,
}

impl DiagramState {
    pub(super) fn new() -> Self {
        Self {
            drawn: Arc::default(),
            revision: 0,
            drawing: None,
        }
    }

    /// The plugins to draw the PR's Markdown with.
    pub(super) fn extensions(&self) -> MarkdownExtensions {
        figures::drawn_extensions(Arc::clone(&self.drawn), self.revision)
    }

    /// Drops the drawing under way, with the diagrams pele hasn't got to.
    pub(super) fn stop(&mut self) {
        self.drawing = None;
    }

    /// Whether no drawing runs or ran since the last [`Self::stop`].
    pub(super) fn stopped(&self) -> bool {
        self.drawing.is_none()
    }
}

impl PullRequestSurface {
    /// Draws the diagrams in `texts`, the PR's Markdown in the order it
    /// shows, that aren't drawn yet, taking in each batch as pele finishes
    /// it. A drawing under way is dropped first; what it drew stays.
    pub(super) fn draw_diagrams(&mut self, texts: Vec<SharedString>, cx: &mut Context<'_, Self>) {
        let known = Arc::clone(&self.diagrams.drawn);
        let task = cx.spawn(async move |this, cx| {
            let sources = cx
                .background_executor()
                .spawn(async move { sources(&texts) })
                .await;
            let _ = this.update(cx, |this, _| {
                // Only this PR's diagrams: those of one shown before go.
                // What is drawn already doesn't change, so no parse waits.
                let drawn = Arc::make_mut(&mut this.diagrams.drawn);
                drawn.retain(|source, _| sources.contains(source));
            });
            // Asked all at once: pele draws them in turn.
            let drawings: FuturesUnordered<_> = sources
                .into_iter()
                .filter(|source| !known.contains_key(source))
                .map(|source| {
                    let drawing = pele::ask(pele::Lane::PullRequests, source.clone());
                    async move { (source, drawing.await) }
                })
                .collect();
            // Each batch is what pele finished since the last was taken in,
            // so a parse takes in as many as are ready.
            let mut batches = drawings.ready_chunks(DIAGRAM_LIMIT);
            while let Some(batch) = batches.next().await {
                let _ = this.update(cx, |this, cx| {
                    Arc::make_mut(&mut this.diagrams.drawn).extend(batch);
                    this.diagrams.revision += 1;
                    cx.notify();
                });
            }
        });
        self.diagrams.drawing = Some(task);
    }
}

/// The different diagram sources in `texts`, in order, up to
/// [`DIAGRAM_LIMIT`].
#[expect(
    clippy::print_stderr,
    reason = "rho-ui has no log; stderr reaches the terminal or the journal"
)]
fn sources(texts: &[SharedString]) -> Vec<String> {
    let mut seen = HashSet::new();
    let sources: Vec<String> = texts
        .iter()
        .flat_map(|text| figures::diagram_sources(text))
        .filter(|source| seen.insert(source.clone()))
        .collect();
    if sources.len() > DIAGRAM_LIMIT {
        eprintln!(
            "rho-ui: a pull request has {} Mermaid diagrams; the first {DIAGRAM_LIMIT}, \
             the limit, are drawn and the rest show as code",
            sources.len()
        );
    }
    sources.into_iter().take(DIAGRAM_LIMIT).collect()
}
