//! A background process's output, as the right panel's Background surface
//! shows it in place of its lists: the process and how it is doing, over
//! its lines in a virtual list that follows the newest line while it is
//! scrolled to the bottom. A line written to stderr is marked so.

use std::{rc::Rc, time::Duration};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyDescription, EmptyHeader},
    h_flex,
    message_scroller::{MessageScroller, MessageScrollerState},
    v_flex, ActiveTheme as _, Disableable as _, Icon, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, App, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled as _,
    Window,
};
use rho_ui_core::{BackgroundId, OutputLine, ProcessState, ProcessStream, SessionProcess};

use super::{
    processes::{is_live, Processes},
    StopHandler,
};
use crate::time;

/// How long a running process must have written nothing before its status
/// says how long it has been quiet: as rho's terminal UI does
/// (`QUIET_LABEL_AFTER` in rho's `tui/process_panel.rs`), since a pause of
/// seconds is ordinary.
const QUIET_AFTER_SECONDS: u64 = 60;

/// How long `process` has run, and while it runs and has been quiet a
/// while, for how long: "1m 20s", "4m 2s · quiet 3m 10s". `None` for one
/// that never started.
pub(crate) fn timing(process: &SessionProcess) -> Option<String> {
    if process.state == ProcessState::FailedToStart {
        return None;
    }
    let elapsed = time::running(Duration::from_secs(process.elapsed_seconds));
    let quiet = process
        .quiet_seconds
        .filter(|&quiet| is_live(process.state) && quiet >= QUIET_AFTER_SECONDS);
    Some(match quiet {
        Some(quiet) => format!(
            "{elapsed} · quiet {}",
            time::running(Duration::from_secs(quiet))
        ),
        None => elapsed,
    })
}

/// How `process` is doing, in words, and whether it ended badly.
fn status(process: &SessionProcess) -> (String, bool) {
    let (word, failed) = match (process.state, process.exit_code) {
        (ProcessState::Starting, _) => ("Starting".to_owned(), false),
        (ProcessState::Running, _) => ("Running".to_owned(), false),
        (ProcessState::Exited, Some(code)) => (format!("Exited {code}"), code != 0),
        (ProcessState::Exited, None) => ("Exited".to_owned(), false),
        (ProcessState::Terminated, _) => ("Stopped".to_owned(), false),
        (ProcessState::TimedOut, _) => ("Timed out".to_owned(), true),
        (ProcessState::FailedToStart, _) => ("Failed to start".to_owned(), true),
        _ => ("Ended".to_owned(), false),
    };
    match timing(process) {
        Some(timing) => (format!("{word} · {timing}"), failed),
        None => (word, failed),
    }
}

/// Goes back to the Background surface's lists.
type BackHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The output of the process `processes` follows, with Back, which
/// `on_back` takes, and Stop while `stop` offers it.
#[derive(IntoElement)]
pub(crate) struct ProcessOutput {
    processes: Entity<Processes>,
    /// What the session's background list calls it, until a read says.
    title: SharedString,
    on_back: BackHandler,
    /// Whether the stop was asked for already, and how to ask; `None`
    /// where it can't be stopped from here.
    stop: Option<(bool, StopHandler)>,
}

impl ProcessOutput {
    pub(crate) fn new(
        processes: Entity<Processes>,
        title: SharedString,
        on_back: impl Fn(&mut Window, &mut App) + 'static,
        stop: Option<(bool, StopHandler)>,
    ) -> Self {
        Self {
            processes,
            title,
            on_back: Rc::new(on_back),
            stop,
        }
    }
}

impl RenderOnce for ProcessOutput {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let processes = self.processes.read(cx);
        let Some(followed) = processes.followed() else {
            return div().into_any_element();
        };
        let process = followed.log.process();
        let title = process.map_or(self.title, |process| process.command.clone().into());
        let (status, failed) = process.map_or_else(|| (String::new(), false), status);
        let running = !followed.gone && process.is_none_or(|process| is_live(process.state));
        let id = followed.log.id().to_owned();
        let (lines, evicted) = (followed.log.lines().len(), followed.log.evicted());
        let scroller = followed.scroller.clone();
        let error = processes.error().cloned();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let on_back = self.on_back;
        let stop = self.stop.filter(|_| running).map(|(asked, on_stop)| {
            Button::new("process-output-stop")
                .ghost()
                .xsmall()
                .icon(Lucide::Square)
                .tooltip("Stop")
                .disabled(asked)
                .on_click(move |_, window, cx| {
                    on_stop(&BackgroundId::process(id.clone()), window, cx);
                })
        });
        // Laid out as the Files surface's header over an open file.
        let header = h_flex()
            .flex_shrink_0()
            .gap_1()
            .pl_2()
            .pr_3()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("process-output-back")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::ArrowLeft))
                    .accessibility_label("Back to the background work")
                    .tooltip("Back to the background work")
                    .on_click(move |_, window, cx| on_back(window, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .child(title),
            )
            .child(
                div()
                    .id("process-output-status")
                    .test_support()
                    .aria_label(status.clone())
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(if failed { theme.danger } else { muted })
                    .child(status),
            )
            .children(stop);
        let note = |text: SharedString| div().px_3().pt_2().text_xs().text_color(muted).child(text);
        let empty = if followed.gone {
            "The session no longer keeps this process's output."
        } else if running {
            "No output yet."
        } else {
            "It wrote no output."
        };
        let body = render_lines(&self.processes, scroller, lines, empty);
        v_flex()
            .id("process-output")
            .test_support()
            .size_full()
            .child(header)
            .when(evicted, |this| {
                this.child(note("Earlier output is no longer kept.".into()))
            })
            .when_some(error, |this, error| {
                this.child(note(format!("Couldn't read the output: {error}").into()))
            })
            .child(body)
            .into_any_element()
    }
}

/// The output's `count` lines in a virtual list that follows the newest
/// while scrolled to the bottom, or, while there are none, `empty`.
fn render_lines(
    processes: &Entity<Processes>,
    scroller: Entity<MessageScrollerState>,
    count: usize,
    empty: &'static str,
) -> AnyElement {
    if count == 0 {
        return Empty::new()
            .flex_1()
            .header(EmptyHeader::new().description(EmptyDescription::new().child(empty)))
            .into_any_element();
    }
    let processes = processes.clone();
    MessageScroller::new("process-output-lines", scroller, move |ix, _, cx| {
        let processes = processes.read(cx);
        let line = processes
            .followed()
            .and_then(|followed| followed.log.lines().get(ix));
        match line {
            Some(line) => render_line(ix, line, cx),
            None => div().into_any_element(),
        }
    })
    // Kit spaces messages 32px apart; output lines sit together.
    .with_row_style(StyleRefinement::default().pb_0())
    .flex_1()
    .min_h_0()
    .into_any_element()
}

/// Line `ix` of the output, in the code font, wrapped; one written to
/// stderr is marked so, as rho's terminal UI marks it.
fn render_line(ix: usize, line: &OutputLine, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let text = SharedString::from(line.text().to_owned());
    h_flex()
        .id(("process-line", ix))
        .test_support()
        .aria_label(text.clone())
        .items_start()
        .gap_2()
        .w_full()
        // An empty line keeps the height of one with text.
        .min_h_4()
        .font_family(theme.mono_font_family.clone())
        .text_xs()
        .when(line.stream() == ProcessStream::Stderr, |this| {
            this.child(
                div()
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground)
                    .child("stderr"),
            )
        })
        .child(div().flex_1().min_w_0().child(text))
        .into_any_element()
}
