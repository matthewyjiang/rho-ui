//! The Terminal surface: a shell in the folder of the session on screen,
//! on the session's machine. Ghostty's libghostty-vt keeps the screen: it
//! parses what the shell prints and encodes keys the way the program in it
//! asked. rho-ui runs the pseudo-terminal and draws the cells with GPUI.
//!
//! On this machine the shell is [`TerminalShell`]; on a remote one it is the
//! login shell there, through `ssh -tt` over the master the daemon
//! connection opened ([`SshLink::terminal_command`]). Each session gets its
//! own terminal, started the first time it is on screen with the surface
//! open, and kept until the surface closes.
//!
//! The mouse goes to a program that asks for it, such as vim or htop, and
//! otherwise selects text ([`mouse`]); Ctrl+Shift+C copies the selection,
//! and a selection also goes to the primary selection. Text an input method
//! commits goes to the shell as typed, and what it composes shows at the
//! cursor until then ([`EntityInputHandler`]).

mod mouse;

use std::{
    cell::RefCell,
    io::{Read as _, Write},
    ops::Range,
    path::PathBuf,
    rc::Rc,
};

use futures::{channel::mpsc, StreamExt as _};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::Button,
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader},
    ActiveTheme as _, IconName, Sizable as _,
};
use gpui_kit::{
    canvas, div, fill, point, px, size, AnyElement, App, AppContext as _, Bounds, ClipboardItem,
    Context, DispatchPhase, ElementInputHandler, Entity, EntityInputHandler, FocusHandle,
    Focusable, Font, FontStyle, FontWeight, Hitbox, HitboxBehavior, Hsla, InteractiveElement as _,
    IntoElement, KeyDownEvent, Keystroke, Modifiers, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, Render, ScrollEvent, SharedString, Size, Styled as _, Task,
    TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window,
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
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use self::mouse::{Mouse, Released};
use crate::{
    folders::Reader,
    machines::{Place, SessionKey},
    session_view::SessionView,
    workspaces::Workspaces,
};

/// The program a terminal on this machine runs: the user's `$SHELL` in the
/// app, `/bin/sh` in tests, so they don't read the user's shell files.
#[derive(Clone, Debug)]
pub(crate) struct TerminalShell(pub(crate) PathBuf);

impl TerminalShell {
    /// `$SHELL`, else `/bin/sh`.
    pub(crate) fn from_env() -> Self {
        let shell = std::env::var_os("SHELL").filter(|shell| !shell.is_empty());
        Self(shell.map_or_else(|| PathBuf::from("/bin/sh"), PathBuf::from))
    }
}

/// The terminal type the shell is told. Ghostty's own, `xterm-ghostty`,
/// isn't installed on most machines, a remote one least of all.
const TERM: &str = "xterm-256color";

/// Lines kept above the screen.
const SCROLLBACK_LINES: usize = 10_000;

/// The Terminal surface of [`super::RightPanel`]: the terminal of the
/// session on screen.
pub(super) struct TerminalSurface {
    workspaces: Entity<Workspaces>,
    shell: TerminalShell,
    /// The session on screen.
    session: Option<(SessionKey, Place)>,
    /// Each session's terminal, or why it couldn't start.
    terminals: Vec<(SessionKey, Result<Entity<TerminalView>, SharedString>)>,
}

impl TerminalSurface {
    pub(super) fn new(
        workspaces: Entity<Workspaces>,
        shell: TerminalShell,
        session: Option<&Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut this = Self {
            workspaces,
            shell,
            session: None,
            terminals: Vec::new(),
        };
        this.set_session(session, cx);
        this
    }

    /// Shows `session`'s terminal, starting it if it has none.
    pub(super) fn set_session(
        &mut self,
        session: Option<&Entity<SessionView>>,
        cx: &mut Context<'_, Self>,
    ) {
        self.session = session.map(|view| {
            let live = view.read(cx).live();
            (live.id.clone(), Place::new(live.id.machine, live.cwd))
        });
        if let Some((key, _)) = &self.session {
            if !self.terminals.iter().any(|(known, _)| known == key) {
                self.start(cx);
            }
        }
        cx.notify();
    }

    /// Starts the shell of the session on screen, in place of any it had.
    fn start(&mut self, cx: &mut Context<'_, Self>) {
        let Some((key, place)) = self.session.clone() else {
            return;
        };
        let reader = self.workspaces.read(cx).reader(&place.machine);
        let command = match reader {
            Reader::Here => {
                let mut command = CommandBuilder::new(&self.shell.0);
                command.cwd(&place.path);
                Ok(command)
            }
            Reader::There(ssh) => {
                let (program, args) = ssh.terminal_command(&place.path);
                let mut command = CommandBuilder::new(program);
                command.args(args);
                Ok(command)
            }
            Reader::Unread => Err("The session's machine isn't connected.".into()),
        };
        let terminal = command.and_then(|command| {
            TerminalView::spawn(command, cx).map_err(|why| format!("Couldn't start: {why}").into())
        });
        self.terminals.retain(|(known, _)| *known != key);
        self.terminals.push((key, terminal));
    }

    /// The terminal of the session on screen, for UI tests.
    #[cfg(test)]
    pub(super) fn current(&self) -> Option<Entity<TerminalView>> {
        let (key, _) = self.session.as_ref()?;
        self.terminals
            .iter()
            .find(|(known, _)| known == key)
            .and_then(|(_, terminal)| terminal.as_ref().ok().cloned())
    }
}

/// A message, with a button to start the shell again.
fn restart(text: SharedString, cx: &Context<'_, TerminalSurface>) -> AnyElement {
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

impl Render for TerminalSurface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
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
            Some((_, Ok(terminal))) if terminal.read(cx).exited => {
                restart("The shell exited.".into(), cx)
            }
            Some((_, Ok(terminal))) => terminal.clone().into_any_element(),
            Some((_, Err(why))) => restart(why.clone(), cx),
            None => div().into_any_element(),
        }
    }
}

/// One terminal: the shell under a pseudo-terminal, and libghostty-vt's
/// screen of it. All on the UI thread, as libghostty's handles must stay on
/// one; a thread of its own reads the pseudo-terminal.
pub(crate) struct TerminalView {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    keys: key::Encoder<'static>,
    key: key::Event<'static>,
    pty: Box<dyn MasterPty + Send>,
    /// Shared with the terminal's replies to the program's queries.
    writer: Rc<RefCell<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
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
    exited: bool,
    focus: FocusHandle,
    /// Feeds the terminal what the reader thread reads; dropped with it.
    reader: Task<()>,
}

impl TerminalView {
    /// Starts `command` under a pseudo-terminal of 80 by 24, which the
    /// first frame sizes to the panel.
    fn spawn(
        mut command: CommandBuilder,
        cx: &mut Context<'_, TerminalSurface>,
    ) -> Result<Entity<Self>, String> {
        let grid = (80, 24);
        let pair = native_pty_system()
            .openpty(pty_size(grid))
            .map_err(|error| error.to_string())?;
        command.env("TERM", TERM);
        command.env("COLORTERM", "truecolor");
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| error.to_string())?;
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| error.to_string())?;
        let writer: Rc<RefCell<Box<dyn Write + Send>>> = Rc::new(RefCell::new(
            pair.master
                .take_writer()
                .map_err(|error| error.to_string())?,
        ));
        let mut terminal = Terminal::new(grid.0, grid.1).map_err(|error| error.to_string())?;
        let replies = Rc::clone(&writer);
        terminal
            .set_scrollback_max_lines(Some(SCROLLBACK_LINES))
            .and_then(|terminal| terminal.set_terminfo_name(TERM))
            .and_then(|terminal| {
                terminal.on_pty_write(move |_, data| {
                    let _ = replies.borrow_mut().write_all(data);
                })
            })
            .map_err(|error| error.to_string())?;

        // Reads on a thread of its own, as the pseudo-terminal blocks.
        let (sender, mut output) = mpsc::unbounded::<Vec<u8>>();
        std::thread::Builder::new()
            .name("rho-ui-terminal".into())
            .spawn(move || {
                let mut buffer = vec![0; 64 * 1024];
                while let Ok(read @ 1..) = reader.read(&mut buffer) {
                    if sender.unbounded_send(buffer[..read].to_vec()).is_err() {
                        return;
                    }
                }
            })
            .map_err(|error| error.to_string())?;

        let ghostty = |error: libghostty_vt::Error| error.to_string();
        let view = Self {
            terminal,
            render: RenderState::new().map_err(ghostty)?,
            rows: RowIterator::new().map_err(ghostty)?,
            cells: CellIterator::new().map_err(ghostty)?,
            keys: key::Encoder::new().map_err(ghostty)?,
            key: key::Event::new().map_err(ghostty)?,
            pty: pair.master,
            writer,
            child,
            mouse: Mouse::new().map_err(ghostty)?,
            layout: None,
            cursor: None,
            marked: None,
            colors: None,
            scrolled: Pixels::ZERO,
            exited: false,
            focus: cx.focus_handle(),
            reader: Task::ready(()),
        };
        let view = cx.new(|_| view);
        let read = cx.spawn({
            let view = view.downgrade();
            async move |surface, cx| {
                while let Some(mut bytes) = output.next().await {
                    // Everything already read, in one write and one frame.
                    while let Ok(more) = output.try_recv() {
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
                let _ = view.update(cx, |view, cx| {
                    view.exited = true;
                    let _ = view.child.try_wait();
                    cx.notify();
                });
                let _ = surface.update(cx, |_, cx| cx.notify());
            }
        });
        view.update(cx, |view, _| view.reader = read);
        Ok(view)
    }

    /// Writes `bytes` to the program.
    fn write(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            let _ = self.writer.borrow_mut().write_all(bytes);
        }
    }

    /// Writes what a key, a paste or an input method types.
    fn send(&mut self, bytes: &[u8]) {
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
        let modifiers = keystroke.modifiers;
        if modifiers.control && modifiers.shift && keystroke.key == "v" {
            self.paste(cx);
            return;
        }
        if modifiers.control && modifiers.shift && keystroke.key == "c" {
            if let Some(text) = self.selected_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            cx.stop_propagation();
            return;
        }
        if keystroke.key_char.is_some() {
            // A dead key's accent, composed by the platform rather than an
            // input method, ends with a keystroke typing the letter: as the
            // platform would, that replaces the accent shown.
            self.marked = None;
        }
        let (key, unshifted) = key_of(&keystroke.key);
        let text = typed(keystroke);
        let consumed = if text.is_some() && modifiers.shift {
            key::Mods::SHIFT
        } else {
            key::Mods::empty()
        };
        self.key
            .set_action(if event.is_held {
                key::Action::Repeat
            } else {
                key::Action::Press
            })
            .set_key(key)
            .set_mods(mods(modifiers))
            .set_consumed_mods(consumed)
            .set_unshifted_codepoint(unshifted)
            .set_utf8(text.as_deref());
        let mut bytes = Vec::new();
        let encoded = self
            .keys
            .set_options_from_terminal(&self.terminal)
            .encode_to_vec(&self.key, &mut bytes);
        if encoded.is_err() || bytes.is_empty() {
            // A key libghostty doesn't know still types its text.
            if let Some(text) = text.filter(|_| !modifiers.control && !modifiers.alt) {
                bytes = text.into_bytes();
            }
        }
        self.send(&bytes);
        cx.stop_propagation();
        cx.notify();
    }

    /// Pastes the clipboard's text, bracketed if the program asked.
    fn paste(&mut self, cx: &mut Context<'_, Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.into_bytes();
        let mut buffer = vec![0; data.len() + 16];
        if let Ok(written) = paste::encode(&mut data, bracketed, &mut buffer) {
            buffer.truncate(written);
            self.send(&buffer);
        }
        cx.stop_propagation();
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
            self.write(&bytes);
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
        let bytes = self.mouse.down(&self.terminal, &grid, event);
        self.write(&bytes);
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
        self.write(&bytes);
        if held {
            cx.notify();
        }
    }

    /// Ends a press, wherever the button comes up: the paint listener
    /// calls this while a button pressed in the terminal is down.
    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<'_, Self>) {
        let Some(grid) = self.layout else {
            return;
        };
        match self.mouse.up(&self.terminal, &grid, event) {
            Released::Nothing => return,
            Released::Report(bytes) => self.write(&bytes),
            Released::Selection => self.write_primary(cx),
        }
        cx.notify();
    }

    /// Puts the selection in the primary selection, for a middle click
    /// elsewhere.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn write_primary(&self, cx: &App) {
        if let Some(text) = self.selected_text() {
            cx.write_to_primary(ClipboardItem::new_string(text));
        }
    }

    /// Only Linux and FreeBSD have a primary selection.
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    fn write_primary(&self, _: &App) {}

    /// Records where the grid is drawn, and tells the terminal and the
    /// shell when the grid that fits the panel changes.
    fn resize(&mut self, layout: Grid) {
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
        let _ = self.pty.resize(pty_size((columns, rows)));
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

impl Drop for TerminalView {
    fn drop(&mut self) {
        let _ = self.child.kill();
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
        self.send(text.as_bytes());
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

fn pty_size((cols, rows): (u16, u16)) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
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
    /// and text for what has no key of its own.
    #[test]
    fn keys_encode_as_a_terminal_sends_them() {
        let terminal = Terminal::new(80, 24).unwrap();
        let mut encoder = key::Encoder::new().unwrap();
        let mut event = key::Event::new().unwrap();
        let mut encode = |name: &str, mods: key::Mods| {
            let (key, unshifted) = key_of(name);
            let keystroke = Keystroke {
                modifiers: gpui_kit::Modifiers {
                    control: mods.contains(key::Mods::CTRL),
                    ..Default::default()
                },
                key: name.to_owned(),
                key_char: None,
            };
            let text = typed(&keystroke);
            event
                .set_action(key::Action::Press)
                .set_key(key)
                .set_mods(mods)
                .set_consumed_mods(key::Mods::empty())
                .set_unshifted_codepoint(unshifted)
                .set_utf8(text.as_deref());
            let mut bytes = Vec::new();
            encoder
                .set_options_from_terminal(&terminal)
                .encode_to_vec(&event, &mut bytes)
                .unwrap();
            bytes
        };
        assert_eq!(encode("a", key::Mods::empty()), b"a");
        assert_eq!(encode("c", key::Mods::CTRL), [0x03]);
        assert_eq!(encode("enter", key::Mods::empty()), b"\r");
        assert_eq!(encode("up", key::Mods::empty()), b"\x1b[A");
        assert_eq!(encode("space", key::Mods::empty()), b" ");
    }
}
