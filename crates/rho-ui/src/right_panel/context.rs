//! The Context surface: how full the session's context window is, what
//! fills it, and when it compacts, as its daemon reports between turns
//! (`rho/session/report`).
//!
//! [`super::RightPanel`] asks it to read the reports when its tab opens,
//! when the session's turn ends or it comes back on screen, once the
//! session is loaded again after a read found it unloaded, and when the
//! composer's context ring is clicked. The daemon reads reports between
//! turns only, so a read during a turn keeps the numbers shown.
//!
//! It doesn't read the hooks report: rho reloads the session's hooks to
//! answer it, so reading it at every turn end would apply hook edits and
//! fail with any hooks file problem.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    accordion::Accordion, h_flex, hover_card::HoverCard, progress::Progress, spinner::Spinner,
    v_flex, ActiveTheme as _, Icon, Sizable as _,
};
use gpui_kit::{
    div, px, relative, Anchor, AnyElement, App, Context, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, Window,
};
use rho_ui_core::{
    ContextMessageKind, Daemon, PromptSourceKind, ReportError, SessionCompactionReport,
    SessionContextReport, SessionId, SessionProviderContextReport, ToolSchemaOrigin,
};

use super::message;
use crate::{time::took, usage::tokens, workspaces::tilde};

/// The reports the surface draws, as one read found them.
struct Reports {
    context: SessionContextReport,
    native: SessionProviderContextReport,
    compaction: SessionCompactionReport,
    /// The home folder of the session's machine, so paths show with `~`.
    home: Option<PathBuf>,
}

impl Reports {
    /// Reads `session`'s reports at once; the daemon answers them in turn.
    async fn read(daemon: Daemon, session: SessionId) -> Result<Self, ReportError> {
        let (context, native, compaction) = futures::try_join!(
            daemon.session_report(&session),
            daemon.session_report(&session),
            daemon.session_report(&session),
        )?;
        Ok(Self {
            context,
            native,
            compaction,
            home: daemon.info().home().map(PathBuf::from),
        })
    }
}

/// What fills the context, as the surface groups it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    SystemPrompt,
    Tools,
    Messages,
    ToolResults,
    Other,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::SystemPrompt => "System prompt",
            Source::Tools => "Tools",
            Source::Messages => "Messages",
            Source::ToolResults => "Tool results",
            Source::Other => "Other",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Source::SystemPrompt => "system-prompt",
            Source::Tools => "tools",
            Source::Messages => "messages",
            Source::ToolResults => "tool-results",
            Source::Other => "other",
        }
    }
}

/// A source's tokens, and its parts', largest first.
struct Category {
    source: Source,
    tokens: u64,
    parts: Vec<(String, u64)>,
}

/// What fills the context, by source, largest first, leaving out those
/// with no tokens. Paths show with `~` for `home`.
fn categories(report: &SessionContextReport, home: Option<&Path>) -> Vec<Category> {
    let prompt = report.system_prompt.iter().map(|part| {
        let label = match (part.source, &part.path) {
            (PromptSourceKind::InstructionFile, Some(path)) => tilde(path, home),
            (source, _) => prompt_source(source).to_owned(),
        };
        (label, part.tokens)
    });
    let tools = report.tool_schemas.iter().map(|part| {
        let origin = match &part.origin {
            ToolSchemaOrigin::BuiltIn => "Built-in".to_owned(),
            ToolSchemaOrigin::Mcp { server } => format!("MCP: {server}"),
            _ => "Other".to_owned(),
        };
        let count = part.schemas;
        let noun = if count == 1 { "tool" } else { "tools" };
        (format!("{origin} ({count} {noun})"), part.tokens)
    });
    let messages = report
        .messages
        .iter()
        .map(|part| (message_kind(part.kind).to_owned(), part.tokens));
    let results = report.tool_results.iter().map(|part| {
        let tool = part.tool.clone();
        (
            tool.unwrap_or_else(|| "Calls no longer in the conversation".to_owned()),
            part.tokens,
        )
    });
    let mut categories = vec![
        category(Source::SystemPrompt, prompt.collect()),
        category(Source::Tools, tools.collect()),
        category(Source::Messages, messages.collect()),
        category(Source::ToolResults, results.collect()),
    ];
    // Fixed request framing is in the total but in no part.
    let parted: u64 = categories.iter().map(|category| category.tokens).sum();
    let framing = report
        .tokens
        .saturating_sub(parted + report.request_context_tokens);
    let other = [
        ("Request context".to_owned(), report.request_context_tokens),
        ("Request framing".to_owned(), framing),
    ];
    categories.push(category(Source::Other, other.into()));
    categories.retain(|category| category.tokens > 0);
    categories.sort_by_key(|category| std::cmp::Reverse(category.tokens));
    categories
}

fn category(source: Source, mut parts: Vec<(String, u64)>) -> Category {
    parts.retain(|&(_, count)| count > 0);
    parts.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
    Category {
        source,
        tokens: parts.iter().map(|&(_, count)| count).sum(),
        parts,
    }
}

/// The Context surface of [`super::RightPanel`], for one session, which
/// says which daemon to read and when.
pub(super) struct ContextSurface {
    /// What the latest read that found them found, which shows through
    /// later reads that find nothing.
    reports: Option<Reports>,
    /// Why the latest read found nothing, until the next read starts.
    failed: Option<ReportError>,
    /// The sources whose parts show.
    open: Vec<Source>,
    /// The read under way; a newer one replaces it.
    load: Task<()>,
    /// Reads landed, for UI tests.
    #[cfg(test)]
    landed: usize,
}

impl ContextSurface {
    pub(super) fn new() -> Self {
        Self {
            reports: None,
            failed: None,
            open: Vec::new(),
            load: Task::ready(()),
            #[cfg(test)]
            landed: 0,
        }
    }

    /// Reads `session`'s reports from `daemon`, keeping what shows until
    /// the read lands.
    pub(super) fn reload(
        &mut self,
        daemon: Daemon,
        session: SessionId,
        cx: &mut Context<'_, Self>,
    ) {
        self.failed = None;
        cx.notify();
        self.load = cx.spawn(async move |this, cx| {
            let read = Reports::read(daemon, session).await;
            let _ = this.update(cx, |this, cx| this.landed(read, cx));
        });
    }

    /// Whether the latest read found the session unloaded, so a read once
    /// it is loaded again should replace it.
    pub(super) fn waits_for_load(&self) -> bool {
        self.failed == Some(ReportError::NotLoaded)
    }

    fn landed(&mut self, read: Result<Reports, ReportError>, cx: &mut Context<'_, Self>) {
        match read {
            Ok(reports) => self.reports = Some(reports),
            Err(error) => self.failed = Some(error),
        }
        #[cfg(test)]
        {
            self.landed += 1;
        }
        cx.notify();
    }

    /// Why the latest read found nothing, the context's tokens the surface
    /// last found, and how many reads have landed, for UI tests.
    #[cfg(test)]
    pub(super) fn state(&self) -> (Option<ReportError>, Option<u64>, usize) {
        let tokens = self.reports.as_ref().map(|reports| reports.context.tokens);
        (self.failed.clone(), tokens, self.landed)
    }

    /// What fills the context, by source; a source opens to its parts.
    fn render_sources(&self, reports: &Reports, cx: &Context<'_, Self>) -> AnyElement {
        let categories = categories(&reports.context, reports.home.as_deref());
        let order: Vec<Source> = categories.iter().map(|category| category.source).collect();
        let muted = cx.theme().muted_foreground;
        let surface = cx.entity().downgrade();
        categories
            .into_iter()
            .fold(
                // Kit's Accordion fills its parent; this one takes its rows'.
                Accordion::new("context-sources")
                    .multiple(true)
                    .small()
                    .h_auto()
                    .flex_none(),
                |accordion, category| {
                    let open = self.open.contains(&category.source);
                    let id = category.source.id();
                    let parts = category.parts.into_iter().enumerate().map(|(ix, part)| {
                        h_flex()
                            .id(SharedString::from(format!("context-part-{id}-{ix}")))
                            .test_support()
                            .gap_2()
                            .text_color(muted)
                            .child(div().flex_1().min_w_0().truncate().child(part.0))
                            .child(tokens(part.1))
                    });
                    accordion.item(|item| {
                        item.open(open)
                            .title(
                                h_flex()
                                    .id(SharedString::from(format!("context-source-{id}")))
                                    .test_support()
                                    .w_full()
                                    .gap_2()
                                    .child(div().flex_1().child(category.source.label()))
                                    .child(tokens(category.tokens)),
                            )
                            .child(v_flex().gap_1().children(parts))
                    })
                },
            )
            .on_toggle_click(move |open, _, cx| {
                let open: Vec<Source> = open
                    .iter()
                    .filter_map(|&ix| order.get(ix).copied())
                    .collect();
                let _ = surface.update(cx, |this, cx| {
                    this.open = open;
                    cx.notify();
                });
            })
            .into_any_element()
    }
}

impl Render for ContextSurface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let Some(reports) = &self.reports else {
            let spinner = || Some(Spinner::new().into_any_element());
            let body = match &self.failed {
                None => message("Reading the session's context…", spinner()),
                Some(ReportError::Busy) => {
                    message("A turn is running. The context shows once it ends.", None)
                }
                // It reads again once the session is loaded.
                Some(ReportError::NotLoaded) => {
                    message("Waiting for rho to load the session…", spinner())
                }
                Some(ReportError::Unserved) => message(
                    "This machine's rho is too old to report on sessions. Update rho to see \
                     what fills the context.",
                    None,
                ),
                Some(ReportError::Daemon(error)) => message(error.to_string(), None),
            };
            return div()
                .id("context-empty")
                .test_support()
                .size_full()
                .child(body)
                .into_any_element();
        };
        v_flex()
            .id("context-page")
            .overflow_y_scroll()
            .test_support()
            .size_full()
            .px_4()
            .py_3()
            .gap_4()
            .text_sm()
            .child(
                v_flex()
                    .gap_1p5()
                    .child(render_fill(reports, cx))
                    .children(left_out(&reports.native, cx))
                    .children(compacted(&reports.compaction, cx)),
            )
            .child(self.render_sources(reports, cx))
            .into_any_element()
    }
}

/// The tokens in use against the window, with a tick where automatic
/// compaction starts, and when it does.
fn render_fill(reports: &Reports, cx: &App) -> AnyElement {
    let used = reports.context.tokens;
    let compaction = &reports.compaction;
    let threshold = compaction
        .threshold_tokens
        .filter(|_| compaction.auto_compact);
    let window = reports.context.window_tokens.filter(|&window| window > 0);
    let figure = match window {
        Some(window) => format!("{} of {}", tokens(used), tokens(window)),
        None => format!("{} tokens", tokens(used)),
    };
    let when = if compaction.auto_compact {
        threshold.map(|threshold| format!("Compacts at {}", tokens(threshold)))
    } else {
        Some("Automatic compaction off".to_owned())
    };
    let bar = window.map(|window| {
        // The share of the window in thousandths, lossless as f32; past
        // the window shows full.
        let share = |count: u64| {
            let thousandths = count.min(window).saturating_mul(1_000) / window;
            f32::from(u16::try_from(thousandths).unwrap_or(1_000)) / 1_000.
        };
        // What the bar shows, for a screen reader.
        let label = match &when {
            Some(when) => format!("{figure}. {when}."),
            None => format!("{figure}."),
        };
        div()
            .relative()
            .py_1()
            .child(
                Progress::new("context-fill")
                    .value(share(used) * 100.)
                    .accessibility_label(label),
            )
            // Kit's Progress has no marks: the tick is laid over it.
            .children(threshold.map(|threshold| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(share(threshold)))
                    .w(px(2.))
                    .bg(cx.theme().muted_foreground)
            }))
    });
    v_flex()
        .gap_1()
        .child(
            div()
                .id("context-figure")
                .test_support()
                .font_weight(FontWeight::MEDIUM)
                .child(figure),
        )
        .children(bar)
        .children(when.map(|when| {
            div()
                .id("context-compacts-at")
                .test_support()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(when)
        }))
        .into_any_element()
}

/// A line of the surface's status, with more on hover when there is more.
fn status_line(
    id: &'static str,
    icon: Lucide,
    text: String,
    color: gpui_kit::Hsla,
    details: Option<Vec<String>>,
) -> AnyElement {
    let line = h_flex()
        .id(id)
        .test_support()
        .gap_1p5()
        .text_xs()
        .text_color(color)
        .child(Icon::new(icon).xsmall())
        .child(div().min_w_0().truncate().child(text));
    let Some(details) = details else {
        return line.into_any_element();
    };
    HoverCard::new(SharedString::from(format!("{id}-card")))
        .anchor(Anchor::TopLeft)
        .trigger(line)
        .content(move |_, _, _| {
            v_flex()
                .id(SharedString::from(format!("{id}-details")))
                .test_support()
                .max_w_72()
                .gap_1()
                .text_xs()
                .children(details.clone())
        })
        .into_any_element()
}

/// What of the conversation the model can't be sent, when there is any.
fn left_out(native: &SessionProviderContextReport, cx: &App) -> Option<AnyElement> {
    let count = native.omitted_blocks;
    if count == 0 {
        return None;
    }
    let kind = match native.omitted_kinds.as_slice() {
        [kind] => format!("{kind} "),
        _ => String::new(),
    };
    let noun = if count == 1 { "block" } else { "blocks" };
    let text = match &native.source {
        Some(source) => format!(
            "{count} {kind}{noun} left out: written by {}/{}",
            source.provider, source.model
        ),
        None => format!("{count} {kind}{noun} left out"),
    };
    let target = format!("{}/{}", native.target.provider, native.target.model);
    let mut details = vec![format!(
        "{target} can't be sent them, so the session's requests leave them out."
    )];
    if native.source_selectable {
        details.push("Switching back to the model that wrote them keeps them.".to_owned());
    }
    if native.can_compact {
        details.push(
            "Compacting can summarize the rest of the conversation first; it doesn't make them \
             sendable."
                .to_owned(),
        );
    }
    Some(status_line(
        "context-left-out",
        Lucide::TriangleAlert,
        text,
        cx.theme().warning,
        Some(details),
    ))
}

/// How often the session has compacted, once it has, and the latest
/// compaction's size.
fn compacted(report: &SessionCompactionReport, cx: &App) -> Option<AnyElement> {
    let completed = &report.completed;
    if completed.count == 0 {
        return None;
    }
    let count = completed.count;
    let text = match completed.last {
        Some(last) => format!(
            "Compacted {count}× · last {} → {}",
            tokens(last.before_local_tokens),
            tokens(last.after_local_tokens)
        ),
        None => format!("Compacted {count}×"),
    };
    let details = report.last_run.as_ref().map(|run| {
        let mut details = Vec::new();
        details.extend(
            run.model
                .as_ref()
                .map(|model| format!("By {}/{}", model.provider, model.model)),
        );
        details.extend(
            run.cost_usd_micros
                .map(|micros| format!("Cost {}", dollars(micros))),
        );
        details.extend(
            run.cache_read_tokens
                .map(|count| format!("{} tokens from cache", tokens(count))),
        );
        details.push(format!(
            "Took {}",
            took(Duration::from_millis(run.latency_ms))
        ));
        details
    });
    Some(status_line(
        "context-compacted",
        Lucide::FoldVertical,
        text,
        cx.theme().muted_foreground,
        details,
    ))
}

fn prompt_source(source: PromptSourceKind) -> &'static str {
    match source {
        PromptSourceKind::Base => "rho's instructions",
        PromptSourceKind::ModelAppend => "Model prompt, after rho's",
        PromptSourceKind::ModelReplace => "Model prompt, in place of rho's",
        PromptSourceKind::InstructionFile => "Instruction file",
        PromptSourceKind::Skills => "Skills",
        PromptSourceKind::Unattributed => "System prompt",
        _ => "Other",
    }
}

fn message_kind(kind: ContextMessageKind) -> &'static str {
    match kind {
        ContextMessageKind::Assistant => "Replies",
        ContextMessageKind::User => "Prompts",
        ContextMessageKind::CompactionSummary => "Compaction summary",
        ContextMessageKind::SystemNotice => "rho's notices",
        ContextMessageKind::Unclassified => "Unclassified",
        _ => "Other",
    }
}

/// Millionths of a dollar as "$0.12", rounded to the cent as the
/// composer's cost is.
fn dollars(micros: u64) -> String {
    let cents = micros.saturating_add(5_000) / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

#[cfg(test)]
mod tests {
    use rho_ui_core::{
        ContextBasis, ContextMessageKind, ContextMessagePart, ContextPromptPart,
        ContextToolSchemaPart, PromptSourceKind, SessionContextReport, ToolSchemaOrigin,
    };

    use super::{categories, Source};

    /// Sources come largest first, each with its parts largest first; one
    /// with no tokens is left out; and Other holds the request context and
    /// the framing the total has but no part does.
    #[test]
    fn sources_come_largest_first_with_the_framing_under_other() {
        let report = SessionContextReport {
            tokens: 10_500,
            basis: ContextBasis::LocalEstimate,
            window_tokens: Some(200_000),
            system_prompt: vec![
                ContextPromptPart {
                    source: PromptSourceKind::Base,
                    path: None,
                    tokens: 1_000,
                },
                ContextPromptPart {
                    source: PromptSourceKind::Skills,
                    path: None,
                    tokens: 2_000,
                },
            ],
            tool_schemas: vec![ContextToolSchemaPart {
                origin: ToolSchemaOrigin::BuiltIn,
                schemas: 3,
                tokens: 4_000,
            }],
            messages: vec![ContextMessagePart {
                kind: ContextMessageKind::User,
                tokens: 0,
            }],
            tool_results: Vec::new(),
            request_context_tokens: 400,
        };
        let shown: Vec<_> = categories(&report, None)
            .into_iter()
            .map(|category| (category.source, category.tokens, category.parts))
            .collect();
        let part = |label: &str, tokens: u64| (label.to_owned(), tokens);
        assert_eq!(
            shown,
            [
                (
                    Source::Tools,
                    4_000,
                    vec![part("Built-in (3 tools)", 4_000)]
                ),
                (
                    Source::Other,
                    3_500,
                    vec![part("Request framing", 3_100), part("Request context", 400)]
                ),
                (
                    Source::SystemPrompt,
                    3_000,
                    vec![part("Skills", 2_000), part("rho's instructions", 1_000)]
                ),
            ]
        );
    }
}
