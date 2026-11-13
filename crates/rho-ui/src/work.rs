//! A work row: a run of tool calls and thoughts, drawn as one summary line
//! ("Ran 3 commands and read 2 files") that opens into a line per step, each
//! of which opens into its details: a tool's output or diff, or the whole
//! thought. A row with one step is just that step's line. While the turn
//! runs, the summary line shows the newest step instead.

use std::{rc::Rc, time::Duration};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    collapsible::Collapsible,
    diff::{Diff, DiffFile, DiffState},
    h_flex,
    separator::Separator,
    shimmer::ShimmerText,
    spinner::Spinner,
    text::TextView,
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, ElementId, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window,
};
use rho_ui_core::{Entry, Step, ToolCall, ToolContent, ToolKind, ToolStatus};

use crate::{time, transcript::Phase};

/// Output past this many lines shows only its tail. At `text_xs` that is
/// about seven screens of text, more than anyone reads inline, and it keeps
/// a huge `cat` from stalling layout.
const MAX_OUTPUT_LINES: usize = 400;

/// Details taller than this scroll inside the row, as in T3 Code.
const MAX_DETAILS_REMS: f32 = 24.;

/// The icon column every line starts with, so labels line up.
const ICON_REMS: f32 = 1.5;

/// Whether something that opens is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Disclosure {
    Open,
    #[default]
    Folded,
}

impl Disclosure {
    pub(crate) fn toggle(&mut self) {
        *self = match self {
            Disclosure::Open => Disclosure::Folded,
            Disclosure::Folded => Disclosure::Open,
        };
    }

    pub(crate) fn is_open(self) -> bool {
        self == Disclosure::Open
    }
}

/// What a click opens or folds in a work row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fold {
    /// The row's list of steps.
    Steps,
    /// One step's details, by entry index.
    Details(usize),
}

/// Opens or folds part of a row.
pub(crate) type FoldHandler = Rc<dyn Fn(Fold, &mut Window, &mut App)>;

/// Kit's diff viewer for one tool call's patches, made when its details
/// first open.
pub(crate) struct DiffView {
    /// The tool call's entry index.
    pub(crate) entry: usize,
    state: Entity<DiffState>,
    /// Which of the tool's contents it shows, by index. The rest show as
    /// plain text.
    shown: Vec<usize>,
    /// Rows the patches take, counting a file header as two and a half:
    /// Kit's viewer scrolls its own rows, so it needs a height.
    rows: f32,
}

impl DiffView {
    /// The viewer for `tool`, if any of its patches parse.
    pub(crate) fn new(entry: usize, tool: &ToolCall, cx: &mut App) -> Option<Self> {
        let Patches { files, shown, rows } = parse_patches(tool);
        if files.is_empty() {
            return None;
        }
        let state = cx.new(|cx| DiffState::new(files, cx));
        Some(Self {
            entry,
            state,
            shown,
            rows,
        })
    }

    /// Shows `tool`'s patches after the call changed.
    pub(crate) fn update(&mut self, tool: &ToolCall, cx: &mut App) {
        let Patches { files, shown, rows } = parse_patches(tool);
        self.shown = shown;
        self.rows = rows;
        self.state
            .update(cx, |state, cx| state.set_files(files, cx));
    }

    fn render(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let rem = theme.font_size;
        // As Kit's diff sizes its rows (gpui-component diff/mod.rs).
        let row = rem * (f32::from(theme.mono_font_size) / 16.) * 1.6;
        Diff::new(&self.state)
            .w_full()
            .h((row * self.rows).min(rem * MAX_DETAILS_REMS))
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .into_any_element()
    }
}

/// A tool call's patches, as Kit parsed them.
struct Patches {
    files: Vec<DiffFile>,
    /// The contents they came from, by index.
    shown: Vec<usize>,
    rows: f32,
}

/// The files in `tool`'s patches, and the rows they take. A patch Kit can't
/// parse is left out, and shows as plain text instead.
fn parse_patches(tool: &ToolCall) -> Patches {
    let mut files = Vec::new();
    let mut shown = Vec::new();
    let mut rows = 0usize;
    for (ix, content) in tool.content().iter().enumerate() {
        let ToolContent::Diff { patch, .. } = content else {
            continue;
        };
        let Ok(parsed) = DiffFile::parse(patch) else {
            continue;
        };
        files.extend(parsed);
        shown.push(ix);
        rows += patch
            .lines()
            .filter(|line| {
                line.starts_with("@@")
                    || (line.starts_with([' ', '+', '-'])
                        && !line.starts_with("+++")
                        && !line.starts_with("---"))
            })
            .count();
    }
    let count = |n: usize| f32::from(u16::try_from(n).unwrap_or(u16::MAX));
    let rows = count(rows) + 2.5 * count(files.len());
    Patches { files, shown, rows }
}

/// Everything a work row needs to draw.
pub(crate) struct WorkRow<'a> {
    pub(crate) entries: &'a [Entry],
    /// The first entry's index in the session.
    pub(crate) start: usize,
    /// Whether the steps are listed.
    pub(crate) steps: Disclosure,
    /// Whether each step shows its details, one per entry.
    pub(crate) details: &'a [Disclosure],
    pub(crate) diffs: &'a [DiffView],
    pub(crate) phase: Phase,
    pub(crate) on_fold: FoldHandler,
}

pub(crate) fn render(row: WorkRow<'_>, cx: &App) -> AnyElement {
    let WorkRow {
        entries,
        start,
        steps: listed,
        details,
        diffs,
        phase,
        on_fold,
    } = row;
    let live = phase == Phase::Live;
    let steps = || {
        entries
            .iter()
            .zip(details)
            .enumerate()
            .filter_map(|(n, (entry, &details))| {
                let ix = start + n;
                let state = StepState {
                    ix,
                    details,
                    newest: live && n + 1 == entries.len(),
                    diff: diffs.iter().find(|diff| diff.entry == ix),
                };
                Some(step(entry.as_step()?, state, &on_fold, cx))
            })
    };
    if entries.len() == 1 {
        return v_flex().w_full().children(steps()).into_any_element();
    }

    let summary = rho_ui_core::summary(entries);
    let (icon, label) = match entries.last().and_then(Entry::as_step) {
        Some(newest) if live => (
            Spinner::new().xsmall().into_any_element(),
            ShimmerText::new(step_label(newest)).into_any_element(),
        ),
        _ => {
            let icon = if rho_ui_core::any_failed(entries) {
                failed_icon(cx)
            } else {
                kind_icon(summary.lead.unwrap_or(ToolKind::Think), cx)
            };
            (icon, summary.text.into_any_element())
        }
    };
    let toggle = Rc::clone(&on_fold);
    v_flex()
        .id(SharedString::from(format!("work-{start}")))
        .test_support()
        .w_full()
        .child(
            line(Some(icon), label, Some(listed), cx)
                .id("work-summary")
                .test_support()
                .on_click(move |_, window, cx| toggle(Fold::Steps, window, cx)),
        )
        .child(
            Collapsible::new()
                .open(listed.is_open())
                // Under the summary's label, so the nesting shows.
                .pl(rems(ICON_REMS))
                // Folded steps aren't built at all.
                .when(listed.is_open(), |this| {
                    this.content(v_flex().children(steps()))
                }),
        )
        .into_any_element()
}

/// Where a step is: its entry index, whether its details are open, whether
/// it is the newest step of a running turn, and its diff viewer if any.
#[derive(Clone, Copy)]
struct StepState<'a> {
    ix: usize,
    details: Disclosure,
    newest: bool,
    diff: Option<&'a DiffView>,
}

/// One step: its line, and its details when open.
fn step(step: Step<'_>, state: StepState<'_>, on_fold: &FoldHandler, cx: &App) -> AnyElement {
    let StepState {
        ix,
        details,
        newest,
        diff,
    } = state;
    let (icon, has_details) = match step {
        Step::Thought(thought) => (
            kind_icon(ToolKind::Think, cx),
            !thought.text().trim().is_empty(),
        ),
        Step::Tool(tool) => (status_icon(tool, cx), !tool.content().is_empty()),
    };
    // Built only when open: a folded step copies none of its text.
    let body = || {
        let boxed = |content: AnyElement| {
            div()
                .id(SharedString::from(format!("output-{ix}")))
                .max_h(rems(MAX_DETAILS_REMS))
                .overflow_y_scroll()
                .p_2()
                .rounded(cx.theme().radius)
                .bg(cx.theme().secondary)
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(content)
        };
        match step {
            Step::Thought(thought) => boxed(
                TextView::markdown(
                    ElementId::named_usize("thought", ix),
                    thought.text().to_owned(),
                )
                .selectable(true)
                .into_any_element(),
            )
            .into_any_element(),
            Step::Tool(tool) => v_flex()
                .gap_2()
                .when_some(output(tool, diff, cx), |this, output| {
                    this.child(boxed(output))
                })
                .when_some(diff, |this, diff| this.child(diff.render(cx)))
                .into_any_element(),
        }
    };
    // An open thought's text is right below, so its line just says so.
    let label = match step {
        Step::Thought(_) if details.is_open() => "Thought".into(),
        _ => step_label(step),
    };
    let label = if newest && is_running(step) {
        ShimmerText::new(label).into_any_element()
    } else {
        label.into_any_element()
    };
    let toggle = Rc::clone(on_fold);
    v_flex()
        .id(SharedString::from(format!("step-{ix}")))
        .test_support()
        .w_full()
        .child(
            line(Some(icon), label, has_details.then_some(details), cx)
                .id("step-line")
                .test_support()
                .when(has_details, |this| {
                    this.on_click(move |_, window, cx| toggle(Fold::Details(ix), window, cx))
                }),
        )
        .when(has_details, |this| {
            this.child(
                Collapsible::new()
                    .open(details.is_open())
                    .pl(rems(ICON_REMS))
                    .when(details.is_open(), |this| {
                        this.content(
                            div()
                                .id(SharedString::from(format!("details-{ix}")))
                                .test_support()
                                .child(body()),
                        )
                    }),
            )
        })
        .into_any_element()
}

/// What a turn's header, under its prompt, says.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Header {
    /// "Working for 38s": the turn runs, and has for this long.
    Working(Duration),
    /// "Worked for 1m 20s". `took` is the turn's length, when this run saw
    /// it; `fold` is whether its working shows, if it has any to fold.
    Worked {
        took: Option<Duration>,
        fold: Option<Disclosure>,
    },
}

/// A turn's header: a line with a rule to its right, so it reads as a
/// divider. Once the turn finishes it opens and folds the turn's working.
/// It is keyed by `turn`, the same element while the turn runs and after,
/// and GPUI keeps hover and click state per element id, so turns sharing
/// one id would hover and open as one.
pub(crate) fn turn_header(
    turn: usize,
    header: Header,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let (label, fold) = match header {
        Header::Working(running) => (format!("Working for {}", time::running(running)), None),
        Header::Worked {
            took: Some(took),
            fold,
        } => (format!("Worked for {}", time::took(took)), fold),
        Header::Worked { took: None, fold } => ("Worked".to_owned(), fold),
    };
    line(None, label.clone().into_any_element(), fold, cx)
        .id(SharedString::from(format!("worked-{turn}")))
        .test_support()
        .aria_label(label)
        .child(Separator::horizontal().flex_1())
        .when(fold.is_some(), |this| {
            this.on_click(move |_, window, cx| on_click(window, cx))
        })
        .into_any_element()
}

/// An icon column, a label and, when the line opens something, a chevron
/// saying which way. Kit's `Button` centres and pads its content; these
/// lines need a fixed icon column so labels stack, so they're laid out here.
fn line(
    icon: Option<AnyElement>,
    label: AnyElement,
    chevron: Option<Disclosure>,
    cx: &App,
) -> gpui_kit::Div {
    let theme = cx.theme();
    let hover = theme.foreground;
    h_flex()
        .w_full()
        .min_h(rems(ICON_REMS))
        .gap_1p5()
        .text_sm()
        .text_color(theme.muted_foreground)
        .when(chevron.is_some(), |this| {
            this.cursor_pointer()
                .hover(move |style| style.text_color(hover))
        })
        .when_some(icon, |this, icon| {
            this.child(
                div()
                    .flex_shrink_0()
                    .w(rems(ICON_REMS))
                    .flex()
                    .justify_center()
                    .child(icon),
            )
        })
        .child(div().min_w_0().truncate().child(label))
        .when_some(chevron, |this, disclosure| {
            this.child(
                Icon::new(match disclosure {
                    Disclosure::Open => IconName::ChevronDown,
                    Disclosure::Folded => IconName::ChevronRight,
                })
                .xsmall()
                .flex_shrink_0()
                .opacity(0.7),
            )
        })
}

/// A step in one line; a thought shows its first line.
fn step_label(step: Step<'_>) -> SharedString {
    match step {
        Step::Thought(thought) => {
            let line = rho_ui_core::thought_line(thought.text());
            if line.is_empty() {
                "Thinking".into()
            } else {
                line.into()
            }
        }
        Step::Tool(tool) if tool.title().is_empty() => "Tool call".into(),
        Step::Tool(tool) => tool.title().to_owned().into(),
    }
}

fn is_running(step: Step<'_>) -> bool {
    match step {
        Step::Tool(tool) => matches!(tool.status(), ToolStatus::Pending | ToolStatus::Running),
        // A thought is the newest step only while it is being written.
        Step::Thought(_) => true,
    }
}

fn kind_icon(kind: ToolKind, cx: &App) -> AnyElement {
    let icon = match kind {
        ToolKind::Read => Icon::new(Lucide::Eye),
        ToolKind::Edit => Icon::new(Lucide::SquarePen),
        ToolKind::Search => Icon::new(IconName::Search),
        ToolKind::Execute => Icon::new(Lucide::Terminal),
        ToolKind::Think => Icon::new(Lucide::Brain),
        ToolKind::Fetch => Icon::new(Lucide::Globe),
        ToolKind::Other => Icon::new(Lucide::Wrench),
    };
    icon.small()
        .text_color(cx.theme().muted_foreground)
        .into_any_element()
}

fn failed_icon(cx: &App) -> AnyElement {
    Icon::new(IconName::CircleX)
        .small()
        .text_color(cx.theme().danger)
        .into_any_element()
}

/// A tool's kind, or how it ended if not well.
fn status_icon(tool: &ToolCall, cx: &App) -> AnyElement {
    match tool.status() {
        ToolStatus::Pending | ToolStatus::Running => Spinner::new().xsmall().into_any_element(),
        ToolStatus::Done => kind_icon(tool.kind(), cx),
        ToolStatus::Failed => failed_icon(cx),
    }
}

/// A tool's text output, and its file changes as plain text where `diff`
/// doesn't show them; `None` when that leaves nothing.
fn output(tool: &ToolCall, diff: Option<&DiffView>, cx: &App) -> Option<AnyElement> {
    let parts: Vec<AnyElement> = tool
        .content()
        .iter()
        .enumerate()
        .filter(|(ix, _)| diff.is_none_or(|diff| !diff.shown.contains(ix)))
        .map(|(_, content)| match content {
            ToolContent::Text(text) => monospace(text, cx).into_any_element(),
            ToolContent::Diff { path, patch } => v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(path.display().to_string()),
                )
                .child(monospace(patch, cx))
                .into_any_element(),
        })
        .collect();
    (!parts.is_empty()).then(|| v_flex().gap_2().children(parts).into_any_element())
}

/// The last `MAX_OUTPUT_LINES` lines of `text`, and how many were hidden.
fn tail(text: &str) -> (&str, usize) {
    let total = text.lines().count();
    if total <= MAX_OUTPUT_LINES {
        return (text, 0);
    }
    let hidden = total - MAX_OUTPUT_LINES;
    let start = text
        .match_indices('\n')
        .nth(hidden - 1)
        .map_or(0, |(at, _)| at + 1);
    (&text[start..], hidden)
}

fn monospace(text: &str, cx: &App) -> impl IntoElement {
    let (shown, hidden) = tail(text.trim_end());
    v_flex()
        .gap_1()
        .when(hidden > 0, |this| {
            this.child(
                div()
                    .text_xs()
                    .child(format!("{hidden} earlier lines hidden")),
            )
        })
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().foreground)
                .font_family(cx.theme().mono_font_family.clone())
                .child(shown.to_owned()),
        )
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    #[test]
    fn tail_keeps_the_last_lines() {
        let text = (0..MAX_OUTPUT_LINES + 3).fold(String::new(), |mut text, n| {
            writeln!(text, "line {n}").expect("writing to a String cannot fail");
            text
        });
        let (shown, hidden) = tail(text.trim_end());
        assert_eq!(hidden, 3);
        assert!(shown.starts_with("line 3\n"));
        assert_eq!(tail("short"), ("short", 0));
    }
}
