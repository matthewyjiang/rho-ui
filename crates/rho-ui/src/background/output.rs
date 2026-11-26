//! A background process's output, as the right panel's Background surface
//! shows it in place of its lists: the process and how it is doing, as its
//! row says, over its lines in a virtual list that follows the newest line
//! while it is scrolled to the bottom. A line written to stderr is marked
//! so.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyDescription, EmptyHeader},
    h_flex,
    message_scroller::{MessageScroller, MessageScrollerState},
    v_flex, ActiveTheme as _, Icon, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, AnyElement, App, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled as _,
    Window,
};
use rho_ui_core::{BackgroundId, BackgroundStatus, OutputLine, ProcessStream};

use super::{
    processes::{Done, Processes},
    stop_button, Listed, OpensOutput, Row, Stop, StopHandler,
};

/// Goes back to the Background surface's lists.
pub(crate) type BackHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The output of the process `processes` follows, `listed` in the
/// session's background work, with Back, which `on_back` takes, and Stop
/// once [`Self::stoppable`].
#[derive(IntoElement)]
pub(super) struct OutputView {
    processes: Entity<Processes>,
    listed: Listed,
    on_back: BackHandler,
    /// The work the user asked to stop, and how to ask.
    stop: Option<(Vec<BackgroundId>, StopHandler)>,
}

impl OutputView {
    pub(super) fn new(processes: Entity<Processes>, listed: Listed, on_back: BackHandler) -> Self {
        Self {
            processes,
            listed,
            on_back,
            stop: None,
        }
    }

    /// Offers to stop the process, as its row does; `stopping` holds the
    /// work the user asked to stop.
    pub(super) fn stoppable(mut self, stopping: &[BackgroundId], on_stop: StopHandler) -> Self {
        self.stop = Some((stopping.to_vec(), on_stop));
        self
    }
}

impl RenderOnce for OutputView {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let processes = self.processes.read(cx);
        let Some(followed) = processes.followed() else {
            return div().into_any_element();
        };
        let work = &self.listed.work;
        let running = work.status() == BackgroundStatus::Running;
        let stop = self.stop.as_ref().filter(|_| running && work.can_stop());
        let state = match stop {
            None => Stop::Never,
            Some((asked, _)) if asked.contains(work.id()) => Stop::Asked,
            Some(_) => Stop::Offered,
        };
        // Worded as its row is.
        let row = Row::new(&self.listed, OpensOutput::No, state);
        let stop =
            stop.map(|(_, on_stop)| stop_button("process-output-stop", work, state, on_stop));
        let (lines, evicted) = (followed.log.lines().len(), followed.log.evicted());
        let scroller = followed.scroller.clone();
        let error = processes.error().cloned();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let on_back = self.on_back;
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
                    .child(row.title.clone()),
            )
            .child(
                div()
                    .id("process-output-status")
                    .test_support()
                    .aria_label(row.status.clone())
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(if row.failed { theme.danger } else { muted })
                    .child(row.status),
            )
            .children(stop);
        let note = |text: SharedString| div().px_3().pt_2().text_xs().text_color(muted).child(text);
        let empty = match followed.done() {
            Some(Done::Gone) => "The session no longer keeps this process's output.",
            Some(Done::Ended) => "It wrote no output.",
            None => "No output yet.",
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
