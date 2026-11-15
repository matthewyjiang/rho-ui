//! The Terminal surface: shells in the folder of the session on screen,
//! on the session's machine. Ghostty's libghostty-vt keeps the screen: it
//! parses what the shell prints and encodes keys the way the program in it
//! asked. rho-ui runs the shell and draws the cells with GPUI.
//!
//! On this machine the shell is [`TerminalShell`], under a pseudo-terminal
//! here; on a remote one it is the login shell there, under a
//! pseudo-terminal sshd makes, in a channel on the ssh connection the
//! machine's terminals share, which asks the key agent once for them all. A
//! [`Program`] is either ([`program`] says how each runs).
//!
//! Each Terminal tab ([`TerminalTab`]) splits into panes ([`TerminalPane`]),
//! side by side or one above another. In each pane, each session gets its
//! own terminal, started the first time it is on screen there, and kept
//! until the pane closes or its shell exits. When the shell of the session
//! on screen exits, the pane closes ([`ShellExited`]), as a terminal
//! window does, though a job it left running in the background still
//! holds the pseudo-terminal open. A shell that couldn't start, or whose
//! ssh connection went, leaves why in its place, with Restart.
//!
//! A tab is named for the program its shell runs now ([`TerminalView::program`]):
//! the pseudo-terminal's foreground process on this machine, or the title
//! the program sets on a remote one.
//!
//! The mouse goes to a program that asks for it, such as vim or htop, and
//! otherwise selects text ([`mouse`]), scrolling while a drag is past the
//! top or bottom. Ctrl+Shift+C copies the selection and Ctrl+Shift+V pastes
//! (Cmd+C and Cmd+V on macOS); on Linux a selection also goes to the
//! primary selection, which a middle click pastes. Text an input method
//! commits goes to the shell as typed, and what it composes shows at the
//! cursor until then ([`EntityInputHandler`]).

mod mouse;
mod program;
mod tab;

use std::{ops::Range, path::PathBuf, time::Duration};

use futures::{FutureExt as _, StreamExt as _};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::Button,
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader},
    input::{Copy, Paste},
    ActiveTheme as _, IconName, Sizable as _,
};
use gpui_kit::{
    canvas, div, fill, point, px, size, AnyElement, App, AppContext as _, Bounds, ClipboardItem,
    Context, DispatchPhase, ElementInputHandler, Entity, EntityId, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, Font, FontStyle, FontWeight, Hitbox, HitboxBehavior,
    Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, Keystroke, Modifiers, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render, ScrollEvent,
    SharedString, Size, Styled as _, Task, TextAlign, TextRun, UTF16Selection, UnderlineStyle,
    Window,
};
use libghostty_vt::{
    color::Rgb,
    fmt::{Format, Formatter},
    key::{self, Key},
    mode::Mode,
    paste,
    render::{CellIterator, RowIterator},
    style::Underline,
    terminal::ScrollViewport,
    RenderState, Terminal,
};
use portable_pty::CommandBuilder;
use rho_ui_core::platform;

use self::mouse::{Mouse, Pressed, Released};
use self::program::{Control, Input, Program, OUTPUT_QUEUE, TERM};
pub(crate) use self::tab::{Direction, PaneCommand};
pub(super) use self::tab::{Emptied, TerminalTab};
use crate::{
    folders::Reader,
    machines::{Place, SessionKey},
    session_view::SessionView,
    workspaces::Workspaces,
};

/// The program a terminal on this machine runs: the user's shell in the
/// app ([`platform::default_shell`]), a plain `sh` in tests, so they don't
/// read the user's shell files.
#[derive(Clone, Debug)]
pub(crate) struct TerminalShell(pub(crate) PathBuf);

impl TerminalShell {
    /// The user's shell.
    pub(crate) fn from_env() -> Self {
        Self(platform::default_shell())
    }
}

/// Autoscroll ticks in a row with no frame drawn, after which the terminal
/// counts as hidden: 8 ticks of 15 ms is 120 ms, where a shown terminal,
/// notified each tick, draws every 16.7 ms at 60 Hz.
const HIDDEN_AFTER_TICKS: u8 = 8;

/// Lines kept above the screen.
const SCROLLBACK_LINES: usize = 10_000;

/// How often a pane reads the program its terminal runs, as a new one
/// may start without printing, as `sleep` does. A read of
/// `/proc/<pid>/comm` takes about 11 µs, so a tab costs a few thousandths
/// of a percent of a core; a tab's name lags the program by up to this.
pub(crate) const PROGRAM_POLL: Duration = Duration::from_millis(500);

/// Emitted when the shell of the session on screen exits, so the tab
/// closes the pane.
struct ShellExited;

/// A pane of a [`TerminalTab`]: the terminal of the session on screen.
struct TerminalPane {
    workspaces: Entity<Workspaces>,
    shell: TerminalShell,
    /// The session on screen.
    session: Option<(SessionKey, Place)>,
    /// Each session's terminal, or why it couldn't start.
    terminals: Vec<(SessionKey, Result<Entity<TerminalView>, SharedString>)>,
    /// The program the on-screen session's terminal runs, as last read.
    program: Option<SharedString>,
    /// Reads `program` every [`PROGRAM_POLL`]; dropped with the pane.
    _poll: Task<()>,
}

impl EventEmitter<ShellExited> for TerminalPane {}

impl TerminalPane {
    fn new(
        workspaces: Entity<Workspaces>,
        shell: TerminalShell,
        session: Option<&Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(PROGRAM_POLL).await;
            if this.update(cx, Self::follow_program).is_err() {
                break;
            }
        });
        let mut this = Self {
            workspaces,
            shell,
            session: None,
            terminals: Vec::new(),
            program: None,
            _poll: poll,
        };
        this.set_session(session, cx);
        this
    }

    /// Shows `session`'s terminal, which the pane's next frame starts if it
    /// has none.
    fn set_session(&mut self, session: Option<&Entity<SessionView>>, cx: &mut Context<'_, Self>) {
        self.session = session.map(|view| {
            let live = view.read(cx).live();
            (live.id.clone(), Place::new(live.id.machine, live.cwd))
        });
        self.follow_program(cx);
        cx.notify();
    }

    /// The program the on-screen session's terminal runs, if it runs one
    /// this can name.
    fn program(&self) -> Option<&SharedString> {
        self.program.as_ref()
    }

    /// Reads the program the on-screen session's terminal runs, and
    /// redraws if it changed.
    fn follow_program(&mut self, cx: &mut Context<'_, Self>) {
        let program = self.terminal().and_then(|view| view.read(cx).program());
        if program != self.program {
            self.program = program;
            cx.notify();
        }
    }

    /// The on-screen session's terminal, if it started.
    fn terminal(&self) -> Option<&Entity<TerminalView>> {
        let (key, _) = self.session.as_ref()?;
        self.terminals
            .iter()
            .find(|(known, _)| known == key)
            .and_then(|(_, terminal)| terminal.as_ref().ok())
    }

    /// Starts the shell of the session on screen if it has none: when the
    /// pane shows or takes the keyboard, not for every pane of every tab a
    /// session switch reaches.
    fn start_if_new(&mut self, cx: &mut Context<'_, Self>) {
        if let Some((key, _)) = &self.session {
            if !self.terminals.iter().any(|(known, _)| known == key) {
                self.start(cx);
            }
        }
    }

    /// Starts the shell of the session on screen, in place of any it had.
    fn start(&mut self, cx: &mut Context<'_, Self>) {
        let Some((key, place)) = self.session.clone() else {
            return;
        };
        let reader = self.workspaces.read(cx).reader(&place.machine);
        // The first frame sizes it to the panel.
        let grid = (80, 24);
        let couldnt = |why: String| SharedString::from(format!("Couldn't start: {why}"));
        let program = match reader {
            Reader::Here => {
                let mut command = CommandBuilder::new(&self.shell.0);
                command.cwd(&place.path);
                Program::local(command, grid).map_err(couldnt)
            }
            Reader::There(ssh) => Ok(Program::remote(&ssh, &place.path, grid)),
            Reader::Unread => Err("The session's machine isn't connected.".into()),
        };
        let terminal = program.and_then(|program| TerminalView::new(program, cx).map_err(couldnt));
        self.terminals.retain(|(known, _)| *known != key);
        self.terminals.push((key, terminal));
    }

    /// Follows terminal `view`'s shell ending: [`Self::shell_exited`] when
    /// it exited; when it couldn't start, or its ssh connection went, the
    /// pane shows why in its place, with Restart.
    fn shell_ended(
        &mut self,
        view: EntityId,
        ended: Result<(), SharedString>,
        cx: &mut Context<'_, Self>,
    ) {
        match ended {
            Ok(()) => self.shell_exited(view, cx),
            Err(why) => {
                let failed = self
                    .terminals
                    .iter_mut()
                    .find(|(_, terminal)| is_view(terminal, view));
                if let Some((_, terminal)) = failed {
                    *terminal = Err(why);
                    self.follow_program(cx);
                    cx.notify();
                }
            }
        }
    }

    /// Follows terminal `view`'s shell exiting. The on-screen session's
    /// closes the pane, keeping the terminal until then so its keyboard
    /// goes on to the next pane; another session's terminal goes, and the
    /// next time that session shows it starts a new shell.
    fn shell_exited(&mut self, view: EntityId, cx: &mut Context<'_, Self>) {
        if self
            .terminal()
            .is_some_and(|terminal| terminal.entity_id() == view)
        {
            cx.emit(ShellExited);
        } else {
            self.terminals
                .retain(|(_, terminal)| !is_view(terminal, view));
        }
    }
}

/// Whether `terminal` started, as the terminal `view`.
fn is_view(terminal: &Result<Entity<TerminalView>, SharedString>, view: EntityId) -> bool {
    terminal
        .as_ref()
        .is_ok_and(|terminal| terminal.entity_id() == view)
}

/// Why the shell couldn't start, with a button to try again.
fn restart(text: SharedString, cx: &Context<'_, TerminalPane>) -> AnyElement {
    Empty::new()
        .h_full()
        .header(EmptyHeader::new().description(EmptyDescription::new().child(text)))
        .content(
            EmptyContent::new().child(
                Button::new("terminal-restart")
                    .outline()
                    .small()
                    .icon(IconName::RotateCw)
                    .label("Restart")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.start(cx);
                        cx.notify();
                    })),
            ),
        )
        .into_any_element()
}

impl Render for TerminalPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.start_if_new(cx);
        let Some((key, _)) = &self.session else {
            return Empty::new()
                .h_full()
                .header(
                    EmptyHeader::new().description(
                        EmptyDescription::new()
                            .child("Open a session to open a terminal in its folder."),
                    ),
                )
                .into_any_element();
        };
        match self.terminals.iter().find(|(known, _)| known == key) {
            Some((_, Ok(terminal))) => terminal.clone().into_any_element(),
            Some((_, Err(why))) => restart(why.clone(), cx),
            None => div().into_any_element(),
        }
    }
}

/// One terminal: a [`Program`], and libghostty-vt's screen of it. All on
/// the UI thread, as libghostty's handles must stay on one; threads of
/// their own, or rho-ui-core's I/O runtime, read, write and wait for the
/// shell.
pub(crate) struct TerminalView {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    keys: Keys,
    /// What is written to the program, in order; the terminal's replies
    /// to the program's queries go here too.
    input: Input,
    /// Sizes the program's terminal and names what runs in it; hangs up on
    /// the shell when the view goes.
    control: Control,
    mouse: Mouse,
    /// Where the grid was last drawn; its size is what the shell was told.
    layout: Option<Grid>,
    /// The cursor's column and row on screen, shown or not: where the
    /// input method's window goes. Set by [`Self::frame`] each frame.
    cursor: Option<(u16, u16)>,
    /// What an input method is composing, not yet sent.
    marked: Option<String>,
    /// The default colors last given the terminal, from the theme.
    colors: Option<(Rgb, Rgb)>,
    /// Wheel movement short of a line.
    scrolled: Pixels,
    /// Scrolls while a selection drag is past the top or bottom.
    autoscroll: Option<Task<()>>,
    /// Autoscroll ticks since the grid was last drawn.
    undrawn_ticks: u8,
    focus: FocusHandle,
    /// Feeds the terminal what the program prints; dropped with it.
    reader: Task<()>,
    /// Tells the pane when the shell ends; dropped with the view.
    waiter: Task<()>,
}

impl TerminalView {
    /// The terminal of `program`, sized as the terminal it started in; the
    /// first frame sizes it to the panel.
    fn new(program: Program, cx: &mut Context<'_, TerminalPane>) -> Result<Entity<Self>, String> {
        let Program {
            grid,
            output,
            input,
            control,
            ended,
        } = program;
        // Fused: a frame's batch below may reach the end, and the loop
        // then asks once more.
        let mut output = output.fuse();
        let mut terminal = Terminal::new(grid.0, grid.1).map_err(|error| error.to_string())?;
        let replies = input.clone();
        terminal
            .set_scrollback_max_lines(Some(SCROLLBACK_LINES))
            .and_then(|terminal| terminal.set_terminfo_name(TERM))
            .and_then(|terminal| terminal.on_pty_write(move |_, data| replies.write(data.to_vec())))
            .map_err(|error| error.to_string())?;

        let ghostty = |error: libghostty_vt::Error| error.to_string();
        let view = Self {
            terminal,
            render: RenderState::new().map_err(ghostty)?,
            rows: RowIterator::new().map_err(ghostty)?,
            cells: CellIterator::new().map_err(ghostty)?,
            keys: Keys::new().map_err(ghostty)?,
            input,
            control,
            mouse: Mouse::new().map_err(ghostty)?,
            layout: None,
            cursor: None,
            marked: None,
            colors: None,
            scrolled: Pixels::ZERO,
            autoscroll: None,
            undrawn_ticks: 0,
            focus: cx.focus_handle(),
            reader: Task::ready(()),
            waiter: Task::ready(()),
        };
        let view = cx.new(|_| view);
        let read = cx.spawn({
            let view = view.downgrade();
            async move |_, cx| {
                while let Some(mut bytes) = output.next().await {
                    // What was queued, in one write and one frame; no more,
                    // as the reader refills the queue meanwhile.
                    for _ in 0..OUTPUT_QUEUE {
                        let Some(more) = output.next().now_or_never().flatten() else {
                            break;
                        };
                        bytes.extend(more);
                    }
                    let wrote = view.update(cx, |view, cx| {
                        view.terminal.vt_write(&bytes);
                        cx.notify();
                    });
                    if wrote.is_err() {
                        return;
                    }
                }
            }
        });
        // The shell exiting closes the pane, not the end of its output: a
        // job it left in the background may keep the pseudo-terminal open.
        let wait = cx.spawn({
            let view = view.downgrade();
            async move |pane, cx| {
                let ended = ended.await;
                let _ = pane.update(cx, |pane, cx| pane.shell_ended(view.entity_id(), ended, cx));
            }
        });
        view.update(cx, |view, _| {
            view.reader = read;
            view.waiter = wait;
        });
        Ok(view)
    }

    /// The name of the program the shell runs now, or of the shell when it
    /// runs none; `None` when there is no name to read.
    fn program(&self) -> Option<SharedString> {
        let title = || {
            let title = self.terminal.title().ok()?;
            let title = String::from_utf8_lossy(title).trim().to_owned();
            Some(title).filter(|title| !title.is_empty())
        };
        self.control
            .foreground_program()
            .or_else(title)
            .map(SharedString::from)
    }

    /// Queues `bytes` for the program.
    fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.input.write(bytes);
        }
    }

    /// Writes what a key, a paste or an input method types.
    fn send(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.write(bytes);
        // Typing goes back to the bottom and drops the selection, as in
        // Ghostty.
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
        let _ = self.terminal.set_selection(None);
    }

    /// The selected text, unwrapped where the program's lines wrapped,
    /// without trailing blanks.
    fn selected_text(&self) -> Option<String> {
        let mut formatter = Formatter::new_selection(&self.terminal).ok()??;
        formatter
            .set_format(Format::Plain)
            .and_then(|formatter| formatter.set_unwrap(true))
            .and_then(|formatter| formatter.set_trim(true))
            .ok()?;
        let text = formatter.format_alloc(None).ok()?;
        Some(String::from_utf8_lossy(&text).into_owned()).filter(|text| !text.is_empty())
    }

    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<'_, Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key_char.is_some() {
            // A dead key's accent, composed by the platform rather than an
            // input method, ends with a keystroke typing the letter: as the
            // platform would, that replaces the accent shown.
            self.marked = None;
        }
        let action = if event.is_held {
            key::Action::Repeat
        } else {
            key::Action::Press
        };
        let bytes = self.keys.encode(&self.terminal, keystroke, action);
        self.send(bytes);
        cx.stop_propagation();
        cx.notify();
    }

    /// Copies the selection to the clipboard, as the copy key does.
    fn copy(&self, cx: &mut App) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Pastes the clipboard's text, as the paste key does.
    fn paste_clipboard(&mut self, cx: &mut App) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.paste(text);
        }
    }

    /// Pastes `text`, bracketed if the program asked.
    fn paste(&mut self, text: String) {
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.into_bytes();
        let mut buffer = vec![0; data.len() + 16];
        if let Ok(written) = paste::encode(&mut data, bracketed, &mut buffer) {
            buffer.truncate(written);
            self.send(buffer);
        }
    }

    /// Sends the wheel to a program that tracks the mouse, or scrolls back.
    fn scroll(&mut self, event: &ScrollEvent, line: Pixels, cx: &mut Context<'_, Self>) {
        self.scrolled += event.delta.pixel_delta(line).y;
        let lines = (self.scrolled / line).trunc();
        if lines == 0. {
            return;
        }
        self.scrolled -= line * lines;
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "a wheel moves a few lines at a time"
        )]
        let lines = lines as i32;
        let at = (event.position, event.modifiers);
        let report = self
            .layout
            .and_then(|grid| self.mouse.wheel(&self.terminal, &grid, lines, at));
        if let Some(bytes) = report {
            self.write(bytes);
        } else {
            self.terminal.scroll_viewport(ScrollViewport::Delta(
                isize::try_from(-lines).unwrap_or_default(),
            ));
            cx.notify();
        }
    }

    fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        window.focus(&self.focus, cx);
        let Some(grid) = self.layout else {
            return;
        };
        match self.mouse.down(&self.terminal, &grid, event) {
            Pressed::Nothing => {}
            Pressed::Report(bytes) => {
                self.write(bytes);
                // The program's, not the pane's menu.
                cx.stop_propagation();
            }
            Pressed::PastePrimary => {
                if let Some(text) = read_primary(cx) {
                    self.paste(text);
                }
            }
        }
        cx.notify();
    }

    /// Follows the pointer: the paint listener calls this over the grid,
    /// or anywhere while a button pressed in it is down.
    fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<'_, Self>) {
        let Some(grid) = self.layout else {
            return;
        };
        let held = self.mouse.is_held();
        let bytes = self.mouse.moved(&self.terminal, &grid, event);
        self.write(bytes);
        if held {
            self.follow_autoscroll(cx);
            cx.notify();
        }
    }

    /// Starts scrolling a row every [`mouse::AUTOSCROLL_INTERVAL`] while a
    /// selection drag is past the top or bottom, as the pointer may sit
    /// still there.
    fn follow_autoscroll(&mut self, cx: &mut Context<'_, Self>) {
        if self.autoscroll.is_some() || !self.mouse.autoscrolls(&self.terminal) {
            return;
        }
        self.autoscroll = Some(cx.spawn(async move |view, cx| loop {
            cx.background_executor()
                .timer(mouse::AUTOSCROLL_INTERVAL)
                .await;
            let more = view.update(cx, TerminalView::autoscroll_tick);
            if !more.unwrap_or(false) {
                break;
            }
        }));
    }

    /// One tick of [`Self::follow_autoscroll`]; returns whether to go on,
    /// and drops the task when not. A terminal that stops being drawn,
    /// hidden mid-drag, stops scrolling: its button may come up unseen.
    fn autoscroll_tick(&mut self, cx: &mut Context<'_, Self>) -> bool {
        self.undrawn_ticks = self.undrawn_ticks.saturating_add(1);
        let shown = self.undrawn_ticks <= HIDDEN_AFTER_TICKS;
        let more = shown
            && self
                .layout
                .is_some_and(|grid| self.mouse.autoscroll(&self.terminal, &grid));
        cx.notify();
        if !more {
            self.autoscroll = None;
        }
        more
    }

    /// Ends a press, wherever the button comes up: the paint listener
    /// calls this while a button pressed in the terminal is down.
    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<'_, Self>) {
        let Some(grid) = self.layout else {
            return;
        };
        match self.mouse.up(&self.terminal, &grid, event) {
            Released::Nothing => return,
            Released::Report(bytes) => self.write(bytes),
            Released::Selection => {
                if let Some(text) = self.selected_text() {
                    write_primary(text, cx);
                }
            }
        }
        cx.notify();
    }

    /// Records where the grid is drawn, and tells the terminal and the
    /// shell when the grid that fits the panel changes.
    fn resize(&mut self, layout: Grid) {
        self.undrawn_ticks = 0;
        let size = |grid: &Grid| (grid.columns, grid.rows, grid.unit());
        let resized = self.layout.is_none_or(|last| size(&last) != size(&layout));
        self.layout = Some(layout);
        if !resized {
            return;
        }
        let (columns, rows) = (layout.columns, layout.rows);
        // The cell size the mouse encoder counts in, so a program that asks
        // for both sees them agree.
        let (width, height) = layout.unit();
        let _ = self
            .terminal
            .resize(columns, rows, u32::from(width), u32::from(height));
        self.control.resize((columns, rows));
    }

    /// Gives the terminal the theme's colors when they change.
    fn follow_theme(&mut self, background: Hsla, foreground: Hsla) {
        let colors = (to_rgb(background), to_rgb(foreground));
        if self.colors == Some(colors) {
            return;
        }
        self.colors = Some(colors);
        let _ = self
            .terminal
            .set_default_bg_color(Some(colors.0))
            .and_then(|terminal| terminal.set_default_fg_color(Some(colors.1)));
    }

    /// What the screen shows, copied out of libghostty for one frame.
    /// Selected cells get `selection` over their background.
    fn frame(&mut self, font: &Font, selection: Hsla) -> Frame {
        let mut frame = Frame::default();
        let Ok(snapshot) = self.render.update(&self.terminal) else {
            return frame;
        };
        let colors = snapshot.colors().ok();
        let (background, foreground) = colors.map_or((Hsla::black(), Hsla::white()), |colors| {
            (to_hsla(colors.background), to_hsla(colors.foreground))
        });
        frame.background = background;
        let cursor = snapshot.cursor().ok();
        self.cursor = cursor
            .as_ref()
            .and_then(|cursor| cursor.viewport)
            .map(|at| (at.x, at.y));
        frame.cursor = self
            .cursor
            .filter(|_| cursor.as_ref().is_some_and(|cursor| cursor.visible));
        frame.marked = self.marked.as_deref().zip(self.cursor).map(|(text, at)| {
            let mut line = Line::default();
            let underline = UnderlineStyle {
                thickness: px(1.),
                color: Some(foreground),
                wavy: false,
            };
            line.push(text, font.clone(), foreground, Some(underline));
            (line, at)
        });
        let Ok(mut rows) = self.rows.update(&snapshot) else {
            return frame;
        };
        let mut text = String::new();
        while let Some(row) = rows.next() {
            let mut line = Line::default();
            let Ok(mut cells) = self.cells.update(row) else {
                break;
            };
            let mut column = 0u16;
            while let Some(cell) = cells.next() {
                let style = cell.style().ok();
                let inverse = style.is_some_and(|style| style.inverse);
                let mut fg = cell.fg_color().ok().flatten().map_or(foreground, to_hsla);
                let mut bg = cell.bg_color().ok().flatten().map(to_hsla);
                if inverse {
                    let swapped = bg.unwrap_or(background);
                    bg = Some(fg);
                    fg = swapped;
                }
                if let Some(bg) = bg {
                    line.backgrounds.push((column, bg));
                }
                if cell.is_selected().unwrap_or(false) {
                    line.backgrounds.push((column, selection));
                }
                if cell.graphemes_len().unwrap_or(0) == 0 || cell.graphemes_utf8(&mut text).is_err()
                {
                    text.clear();
                    text.push(' ');
                }
                let mut run_font = font.clone();
                if style.is_some_and(|style| style.bold) {
                    run_font.weight = FontWeight::BOLD;
                }
                if style.is_some_and(|style| style.italic) {
                    run_font.style = FontStyle::Italic;
                }
                if style.is_some_and(|style| style.faint) {
                    fg = fg.opacity(0.6);
                }
                let underline = style
                    .filter(|style| style.underline != Underline::None)
                    .map(|_| UnderlineStyle {
                        thickness: px(1.),
                        color: Some(fg),
                        wavy: false,
                    });
                line.push(&text, run_font, fg, underline);
                column += 1;
            }
            frame.lines.push(line);
        }
        frame
    }

    /// The length of the composed text, in the UTF-16 units input
    /// methods count in.
    fn marked_len(&self) -> usize {
        self.marked
            .as_deref()
            .map_or(0, |text| text.encode_utf16().count())
    }

    /// Where the cell at `column` and `row` is drawn, for UI tests.
    #[cfg(test)]
    pub(crate) fn cell_bounds(&self, column: u16, row: u16) -> Option<Bounds<Pixels>> {
        Some(self.layout?.cell_bounds(column, row))
    }

    /// The screen as text, a line a row, for UI tests.
    #[cfg(test)]
    pub(crate) fn text(&mut self) -> Vec<String> {
        let font = gpui_kit::font("monospace");
        self.frame(&font, Hsla::default())
            .lines
            .into_iter()
            .map(|line| line.text.trim_end().to_owned())
            .collect()
    }
}

/// libghostty's key encoder and the event it encodes, kept for the next
/// key.
struct Keys {
    encoder: key::Encoder<'static>,
    event: key::Event<'static>,
}

impl Keys {
    fn new() -> Result<Self, libghostty_vt::Error> {
        Ok(Self {
            encoder: key::Encoder::new()?,
            event: key::Event::new()?,
        })
    }

    /// The bytes `keystroke` sends the program, encoded as the program in
    /// `terminal` asked. Shift that typed the text counts as consumed, and
    /// a key libghostty doesn't know still types its text.
    fn encode(
        &mut self,
        terminal: &Terminal<'_, '_>,
        keystroke: &Keystroke,
        action: key::Action,
    ) -> Vec<u8> {
        let modifiers = keystroke.modifiers;
        let (key, unshifted) = key_of(&keystroke.key);
        let text = typed(keystroke);
        let consumed = if text.is_some() && modifiers.shift {
            key::Mods::SHIFT
        } else {
            key::Mods::empty()
        };
        self.event
            .set_action(action)
            .set_key(key)
            .set_mods(mods(modifiers))
            .set_consumed_mods(consumed)
            .set_unshifted_codepoint(unshifted)
            .set_utf8(text.as_deref());
        let mut bytes = Vec::new();
        let encoded = self
            .encoder
            .set_options_from_terminal(terminal)
            .encode_to_vec(&self.event, &mut bytes);
        if encoded.is_err() || bytes.is_empty() {
            if let Some(text) = text.filter(|_| !modifiers.control && !modifiers.alt) {
                bytes = text.into_bytes();
            }
        }
        bytes
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Input methods. A terminal has no text to edit, so the only text the
/// handler knows is what is being composed; committed text goes straight to
/// the program. Keys an input method doesn't take reach [`TerminalView::key_down`],
/// which stops them there, so the platform doesn't type them again.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<UTF16Selection> {
        let end = self.marked_len();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<'_, Self>) -> Option<Range<usize>> {
        self.marked.is_some().then(|| 0..self.marked_len())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.marked = None;
        self.send(text.as_bytes().to_vec());
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.marked = Some(text.to_owned()).filter(|text| !text.is_empty());
        cx.notify();
    }

    /// The cursor's cell, where the input method's window goes.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<Bounds<Pixels>> {
        let (column, row) = self.cursor?;
        Some(self.layout?.cell_bounds(column, row))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<usize> {
        None
    }
}

/// One row of the screen: its text, styled in runs, and the cells whose
/// background isn't the terminal's.
#[derive(Default)]
struct Line {
    text: String,
    runs: Vec<TextRun>,
    backgrounds: Vec<(u16, Hsla)>,
}

impl Line {
    /// Adds a cell's text, joining the last run if it looks the same.
    fn push(&mut self, text: &str, font: Font, color: Hsla, underline: Option<UnderlineStyle>) {
        self.text.push_str(text);
        if let Some(last) = self.runs.last_mut() {
            if last.font == font && last.color == color && last.underline == underline {
                last.len += text.len();
                return;
            }
        }
        self.runs.push(TextRun {
            len: text.len(),
            font,
            color,
            background_color: None,
            underline,
            strikethrough: None,
        });
    }
}

/// What one frame draws.
#[derive(Default)]
struct Frame {
    background: Hsla,
    lines: Vec<Line>,
    /// The cursor's column and row, while it shows on screen.
    cursor: Option<(u16, u16)>,
    /// What an input method is composing, and the cell it is drawn at:
    /// the cursor's, shown or not.
    marked: Option<(Line, (u16, u16))>,
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, foreground, cursor) = (theme.background, theme.foreground, theme.caret);
        let selection = theme.selection;
        self.follow_theme(background, foreground);
        let font_size = window.rem_size() * 0.875;
        let line_height = (font_size * 1.4).round();
        let font = gpui_kit::font(theme.mono_font_family.clone());
        let font_id = window.text_system().resolve_font(&font);
        let cell_width = window
            .text_system()
            .advance(font_id, font_size, 'm')
            .map_or(font_size * 0.6, |advance| advance.width);
        let cell = size(cell_width, line_height);
        let frame = self.frame(&font, selection);
        let focus = self.focus.clone();
        let focused = focus.is_focused(window);
        let view = cx.entity();
        div()
            .id("terminal")
            .test_support()
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .px_2()
            .py_1()
            .bg(frame.background)
            .cursor_text()
            .on_any_mouse_down(cx.listener(Self::mouse_down))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &Paste, _, cx| this.paste_clipboard(cx)))
            .on_key_down(cx.listener(|this, event, _, cx| this.key_down(event, cx)))
            .on_scroll(cx.listener(move |this, event, _, cx| {
                this.scroll(event, line_height, cx);
            }))
            .child(
                canvas(
                    {
                        let view = view.downgrade();
                        move |bounds: Bounds<Pixels>, window, cx| {
                            let grid = Grid::new(bounds, cell);
                            let _ = view.update(cx, |view, _| view.resize(grid));
                            (grid, window.insert_hitbox(bounds, HitboxBehavior::Normal))
                        }
                    },
                    move |bounds, (grid, hitbox), window, cx| {
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, view.clone()),
                            cx,
                        );
                        listen_to_mouse(&view, hitbox, window);
                        paint(&frame, &grid, font_size, (focused, cursor), window, cx);
                    },
                )
                .size_full(),
            )
    }
}

/// Follows the mouse over the grid, and anywhere while a button pressed in
/// it is down, so a drag goes on past the terminal and ends wherever the
/// button comes up. Raw GPUI: a div's listeners hear only the pointer over
/// it.
fn listen_to_mouse(view: &Entity<TerminalView>, hitbox: Hitbox, window: &mut Window) {
    let moves = view.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble {
            let hovered = hitbox.is_hovered(window);
            moves.update(cx, |view, cx| {
                if hovered || view.mouse.is_held() {
                    view.mouse_move(event, cx);
                }
            });
        }
    });
    let ups = view.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Capture {
            ups.update(cx, |view, cx| {
                if view.mouse.is_held() {
                    view.mouse_up(event, cx);
                }
            });
        }
    });
}

/// Draws `frame` on `grid`.
fn paint(
    frame: &Frame,
    grid: &Grid,
    font_size: Pixels,
    (focused, cursor_color): (bool, Hsla),
    window: &mut Window,
    cx: &mut App,
) {
    let (width, height) = (grid.cell.width, grid.cell.height);
    for (row, line) in (0..).zip(&frame.lines) {
        for &(column, color) in &line.backgrounds {
            window.paint_quad(fill(grid.cell_bounds(column, row), color));
        }
    }
    if let Some((column, row)) = frame.cursor {
        let cursor = grid.cell_bounds(column, row);
        let color = if focused {
            cursor_color.opacity(0.7)
        } else {
            cursor_color.opacity(0.25)
        };
        window.paint_quad(fill(cursor, color));
    }
    for (row, line) in (0..).zip(&frame.lines) {
        if line.text.trim_end().is_empty() {
            continue;
        }
        let shaped = window.text_system().shape_line(
            line.text.clone().into(),
            font_size,
            &line.runs,
            Some(width),
        );
        let origin = grid.cell_bounds(0, row).origin;
        let _ = shaped.paint(origin, height, TextAlign::Left, None, window, cx);
    }
    if let Some((marked, (column, row))) = &frame.marked {
        // Unforced widths, as composed text may hold wide characters.
        let shaped = window.text_system().shape_line(
            marked.text.clone().into(),
            font_size,
            &marked.runs,
            None,
        );
        let origin = grid.cell_bounds(*column, *row).origin;
        window.paint_quad(fill(
            Bounds::new(origin, size(shaped.width, height)),
            frame.background,
        ));
        let _ = shaped.paint(origin, height, TextAlign::Left, None, window, cx);
    }
}

/// Where the grid is drawn, from the last prepaint: the canvas, a cell's
/// size, and the columns and rows that fit. [`mouse`] adds its mapping of
/// pointer positions to libghostty's.
#[derive(Clone, Copy, Debug)]
struct Grid {
    bounds: Bounds<Pixels>,
    cell: Size<Pixels>,
    columns: u16,
    rows: u16,
}

impl Grid {
    /// The grid of `cell`s that fits `bounds`.
    fn new(bounds: Bounds<Pixels>, cell: Size<Pixels>) -> Self {
        Self {
            bounds,
            cell,
            columns: fit(bounds.size.width / cell.width),
            rows: fit(bounds.size.height / cell.height),
        }
    }

    /// The cell at `column` and `row`, in window coordinates.
    fn cell_bounds(&self, column: u16, row: u16) -> Bounds<Pixels> {
        let offset = point(
            self.cell.width * f32::from(column),
            self.cell.height * f32::from(row),
        );
        Bounds::new(self.bounds.origin + offset, self.cell)
    }
}

/// libghostty's modifiers for GPUI's.
fn mods(modifiers: Modifiers) -> key::Mods {
    let mut mods = key::Mods::empty();
    for (held, mod_) in [
        (modifiers.shift, key::Mods::SHIFT),
        (modifiers.control, key::Mods::CTRL),
        (modifiers.alt, key::Mods::ALT),
        (modifiers.platform, key::Mods::SUPER),
    ] {
        if held {
            mods |= mod_;
        }
    }
    mods
}

// The primary selection: what a selection puts there, and a middle click
// pastes. Only Linux and FreeBSD have one; elsewhere these do nothing.

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn write_primary(text: String, cx: &App) {
    cx.write_to_primary(ClipboardItem::new_string(text));
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn read_primary(cx: &App) -> Option<String> {
    cx.read_from_primary().and_then(|item| item.text())
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn write_primary(_: String, _: &App) {}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn read_primary(_: &App) -> Option<String> {
    None
}

/// How many whole cells fit, at least one.
fn fit(cells: f32) -> u16 {
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to u16's range first"
    )]
    let cells = cells.floor().clamp(1., f32::from(u16::MAX)) as u16;
    cells
}

fn to_hsla(color: Rgb) -> Hsla {
    gpui_kit::rgb(u32::from_be_bytes([0, color.r, color.g, color.b])).into()
}

fn to_rgb(color: Hsla) -> Rgb {
    let rgba = color.to_rgb();
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "each channel is between 0 and 1"
    )]
    let byte = |channel: f32| (channel.clamp(0., 1.) * 255.).round() as u8;
    Rgb {
        r: byte(rgba.r),
        g: byte(rgba.g),
        b: byte(rgba.b),
    }
}

/// The text a keystroke types: what the platform says it types, else the
/// key itself when it is one character, as simulated keys carry no text.
/// A key held with Control, Alt or Super types nothing of its own.
fn typed(keystroke: &Keystroke) -> Option<String> {
    let modifiers = keystroke.modifiers;
    if modifiers.control || modifiers.platform {
        return None;
    }
    if let Some(text) = &keystroke.key_char {
        return Some(text.clone());
    }
    let mut chars = keystroke.key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if modifiers.shift => Some(c.to_uppercase().collect()),
        (Some(c), None) => Some(c.to_string()),
        _ if keystroke.key == "space" => Some(" ".to_owned()),
        _ => None,
    }
}

/// libghostty's key for GPUI's key name, and the character it types with no
/// modifier held, or `'\0'` for a key that types none.
fn key_of(name: &str) -> (Key, char) {
    let named = match name {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        "space" => return (Key::Space, ' '),
        _ => Key::Unidentified,
    };
    if named != Key::Unidentified {
        return (named, '\0');
    }
    let mut chars = name.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return (Key::Unidentified, '\0');
    };
    let key = match c.to_ascii_lowercase() {
        'a' => Key::A,
        'b' => Key::B,
        'c' => Key::C,
        'd' => Key::D,
        'e' => Key::E,
        'f' => Key::F,
        'g' => Key::G,
        'h' => Key::H,
        'i' => Key::I,
        'j' => Key::J,
        'k' => Key::K,
        'l' => Key::L,
        'm' => Key::M,
        'n' => Key::N,
        'o' => Key::O,
        'p' => Key::P,
        'q' => Key::Q,
        'r' => Key::R,
        's' => Key::S,
        't' => Key::T,
        'u' => Key::U,
        'v' => Key::V,
        'w' => Key::W,
        'x' => Key::X,
        'y' => Key::Y,
        'z' => Key::Z,
        '0' => Key::Digit0,
        '1' => Key::Digit1,
        '2' => Key::Digit2,
        '3' => Key::Digit3,
        '4' => Key::Digit4,
        '5' => Key::Digit5,
        '6' => Key::Digit6,
        '7' => Key::Digit7,
        '8' => Key::Digit8,
        '9' => Key::Digit9,
        '-' => Key::Minus,
        '=' => Key::Equal,
        '[' => Key::BracketLeft,
        ']' => Key::BracketRight,
        '\\' => Key::Backslash,
        ';' => Key::Semicolon,
        '\'' => Key::Quote,
        ',' => Key::Comma,
        '.' => Key::Period,
        '/' => Key::Slash,
        '`' => Key::Backquote,
        _ => Key::Unidentified,
    };
    (key, c.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys reach libghostty as the program expects them: control
    /// characters for Ctrl+letters and Enter, escape sequences for arrows,
    /// and text for what has no key of its own. Through [`Keys::encode`],
    /// as [`TerminalView::key_down`] sends them.
    #[test]
    fn keys_encode_as_a_terminal_sends_them() {
        let terminal = Terminal::new(80, 24).unwrap();
        let mut keys = Keys::new().unwrap();
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        let mut encode = |name: &str, modifiers: Modifiers| {
            let keystroke = Keystroke {
                modifiers,
                key: name.to_owned(),
                key_char: None,
            };
            keys.encode(&terminal, &keystroke, key::Action::Press)
        };
        let none = Modifiers::default();
        assert_eq!(encode("a", none), b"a");
        assert_eq!(encode("c", ctrl), [0x03]);
        assert_eq!(encode("enter", none), b"\r");
        assert_eq!(encode("up", none), b"\x1b[A");
        assert_eq!(encode("space", none), b" ");
    }
}
