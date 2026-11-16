//! The Mermaid diagrams in the PR on screen. Anyone who can comment on a PR
//! writes its text, and Kit parses short text on the UI thread, so a PR's
//! diagrams are drawn first, off it, on pele's engine for PRs
//! ([`pele::Lane::PullRequests`]); then its text is parsed again with them.
//! A diagram shows as code until it is drawn.

use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::component::text::MarkdownExtensions;
use gpui_kit::{Context, SharedString, Task};

use super::PullRequestSurface;
use crate::{figures, pele};

/// How many diagrams of one text, a description or a post, are drawn; the
/// rest show as code. Each may take pele up to its one-second limit, so
/// this bounds what one comment costs. Of the 329 newest PRs of rho and
/// rho-ui, none has more than one diagram in its description.
const DIAGRAM_LIMIT: usize = 10;

/// The diagrams drawn for a surface's PR, and the drawing under way.
pub(super) struct Diagrams {
    drawn: Arc<figures::Drawings>,
    /// Kit's parser revision, bumped whenever `drawn` changes, so the
    /// text is parsed again with them.
    revision: u64,
    _drawing: Option<Task<()>>,
}

impl Diagrams {
    pub(super) fn new() -> Self {
        Self {
            drawn: Arc::default(),
            revision: 0,
            _drawing: None,
        }
    }

    /// The plugins to draw the PR's Markdown with.
    pub(super) fn extensions(&self) -> MarkdownExtensions {
        figures::drawn_extensions(Arc::clone(&self.drawn), self.revision)
    }
}

impl PullRequestSurface {
    /// Draws the diagrams in `texts`, the PR's Markdown, that aren't drawn
    /// yet, then parses its text again. A drawing under way is dropped,
    /// with the diagrams pele hasn't got to.
    pub(super) fn draw_diagrams(&mut self, texts: Vec<SharedString>, cx: &mut Context<'_, Self>) {
        let known = Arc::clone(&self.diagrams.drawn);
        let task = cx.spawn(async move |this, cx| {
            let sources = cx
                .background_executor()
                .spawn(async move { sources(&texts) })
                .await;
            // Asked all at once: pele draws them in turn.
            let asked: Vec<_> = sources
                .iter()
                .filter(|source| !known.contains_key(*source))
                .map(|source| {
                    let drawing = pele::ask(pele::Lane::PullRequests, source.clone());
                    let source = source.clone();
                    async move { (source, drawing.await) }
                })
                .collect();
            let fresh = futures::future::join_all(asked).await;
            // Only this PR's diagrams: those of one shown before go.
            let drawn: figures::Drawings = sources
                .into_iter()
                .filter_map(|source| {
                    let drawing = known.get(&source)?.clone();
                    Some((source, drawing))
                })
                .chain(fresh)
                .collect();
            let _ = this.update(cx, |this, cx| {
                let old = &this.diagrams.drawn;
                if drawn.len() != old.len() || drawn.keys().any(|key| !old.contains_key(key)) {
                    this.diagrams.drawn = Arc::new(drawn);
                    this.diagrams.revision += 1;
                    cx.notify();
                }
            });
        });
        self.diagrams = Diagrams {
            drawn: Arc::clone(&self.diagrams.drawn),
            revision: self.diagrams.revision,
            _drawing: Some(task),
        };
    }
}

/// The diagram sources in `texts`, each once, up to [`DIAGRAM_LIMIT`] of
/// each text.
#[expect(
    clippy::print_stderr,
    reason = "rho-ui has no log; stderr reaches the terminal or the journal"
)]
fn sources(texts: &[SharedString]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    for text in texts {
        let mut found = figures::diagram_sources(text);
        if found.len() > DIAGRAM_LIMIT {
            eprintln!(
                "rho-ui: a pull request's text has {} Mermaid diagrams; the first \
                 {DIAGRAM_LIMIT}, the limit, are drawn and the rest show as code",
                found.len()
            );
            found.truncate(DIAGRAM_LIMIT);
        }
        for source in found {
            if seen.insert(source.clone()) {
                sources.push(source);
            }
        }
    }
    sources
}
