//! The mouse in a terminal. A program that asked for the mouse gets its
//! presses, releases, motion and wheel as escape sequences, from
//! libghostty's encoder; otherwise, or with Shift held, the left button
//! selects text through libghostty's selection gestures.

use std::time::{Duration, Instant};

use gpui_kit::{
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
};
use libghostty_vt::{
    error::Result,
    mouse::{self, EncoderSize},
    point::{self, Coordinate},
    selection::gesture::{DragEvent, Geometry, Gesture, PressEvent, ReleaseEvent},
    Terminal,
};

use super::{fit, mods, Grid};

/// Presses closer together than this count as a double or triple click:
/// the default double-click time of Windows and of libghostty's example.
const REPEAT_INTERVAL: Duration = Duration::from_millis(500);

/// Pointer positions as libghostty takes them.
impl Grid {
    /// A cell's size in whole pixels, as libghostty takes it. A cell is
    /// rarely a whole number of pixels wide, so positions given libghostty
    /// are scaled to these cells rather than rounded, which would drift a
    /// pixel every few columns.
    pub(super) fn unit(&self) -> (u16, u16) {
        let whole = |length: Pixels| fit(f32::from(length).round());
        (whole(self.cell.width), whole(self.cell.height))
    }

    /// `position` in libghostty's surface: from the grid's top left corner,
    /// in [`Self::unit`] cells, held to the grid.
    fn surface(&self, position: Point<Pixels>) -> (f32, f32) {
        let local = position - self.bounds.origin;
        let (width, height) = self.unit();
        let scale = |offset: Pixels, cell: Pixels, unit: u16, count: u16| {
            let span = f32::from(unit) * f32::from(count);
            (offset / cell * f32::from(unit)).clamp(0., span - 1.)
        };
        (
            scale(local.x, self.cell.width, width, self.columns),
            scale(local.y, self.cell.height, height, self.rows),
        )
    }

    /// The viewport cell under `position`, held to the grid: the one
    /// [`Self::surface`] puts it in.
    fn coordinate(&self, position: Point<Pixels>) -> Coordinate {
        let (x, y) = self.surface(position);
        let (width, height) = self.unit();
        Coordinate {
            x: whole(x / f32::from(width)),
            y: u32::from(whole(y / f32::from(height))),
        }
    }

    fn encoder_size(&self) -> EncoderSize {
        let (width, height) = self.unit();
        EncoderSize {
            screen_width: u32::from(width) * u32::from(self.columns),
            screen_height: u32::from(height) * u32::from(self.rows),
            cell_width: u32::from(width),
            cell_height: u32::from(height),
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        }
    }

    fn geometry(&self) -> Geometry {
        let (width, height) = self.unit();
        Geometry {
            columns: u32::from(self.columns),
            cell_width: u32::from(width),
            padding_left: 0,
            screen_height: u32::from(height) * u32::from(self.rows),
        }
    }
}

/// What the button held down is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// The left button, selecting text.
    Selecting,
    /// A button whose press went to the program, as its release will.
    Reporting(MouseButton),
}

impl Held {
    fn button(self) -> MouseButton {
        match self {
            Self::Selecting => MouseButton::Left,
            Self::Reporting(button) => button,
        }
    }
}

/// What a button's release did.
#[derive(Debug)]
pub(super) enum Released {
    Nothing,
    /// Bytes for the program.
    Report(Vec<u8>),
    /// A selection gesture ended.
    Selection,
}

/// libghostty's mouse handles for one terminal, reused for every event,
/// and what the held button is doing.
pub(super) struct Mouse {
    encoder: mouse::Encoder<'static>,
    event: mouse::Event<'static>,
    gesture: Gesture<'static>,
    press: PressEvent<'static>,
    drag: DragEvent<'static>,
    release: ReleaseEvent<'static>,
    /// What press times count from, to tell double clicks.
    epoch: Instant,
    held: Option<Held>,
}

impl Mouse {
    pub(super) fn new() -> Result<Self> {
        let mut press = PressEvent::new()?;
        press.set_repeat_interval(REPEAT_INTERVAL)?;
        Ok(Self {
            encoder: mouse::Encoder::new()?,
            event: mouse::Event::new()?,
            gesture: Gesture::new()?,
            press,
            drag: DragEvent::new()?,
            release: ReleaseEvent::new()?,
            epoch: Instant::now(),
            held: None,
        })
    }

    /// Whether a button pressed in the terminal is still down, so its
    /// motion matters wherever the pointer is.
    pub(super) fn is_held(&self) -> bool {
        self.held.is_some()
    }

    /// Starts a selection, or reports the press to a program that tracks
    /// the mouse; returns the bytes for the program.
    pub(super) fn down(
        &mut self,
        terminal: &Terminal<'_, '_>,
        grid: &Grid,
        event: &MouseDownEvent,
    ) -> Vec<u8> {
        let tracking = tracking(terminal);
        if event.button == MouseButton::Left && (!tracking || event.modifiers.shift) {
            self.held = Some(Held::Selecting);
            let _ = self.select_press(terminal, grid, event.position);
            return Vec::new();
        }
        let Some(button) = button_of(event.button).filter(|_| tracking) else {
            return Vec::new();
        };
        // The program owns the mouse now: no click sequence carries over.
        self.gesture.reset(terminal);
        self.held = Some(Held::Reporting(event.button));
        let (press, at) = (mouse::Action::Press, (event.position, event.modifiers));
        self.report(terminal, grid, Some(button), press, at)
    }

    /// Extends the selection, or reports motion to a program that tracks
    /// the mouse; returns the bytes for the program.
    pub(super) fn moved(
        &mut self,
        terminal: &Terminal<'_, '_>,
        grid: &Grid,
        event: &MouseMoveEvent,
    ) -> Vec<u8> {
        let action = mouse::Action::Motion;
        let at = (event.position, event.modifiers);
        let pressed = event.pressed_button;
        match self.held {
            Some(held) if pressed != Some(held.button()) => {
                // The button came up where the terminal couldn't hear it,
                // such as while it was hidden: its press ends now.
                self.held = None;
                match held {
                    Held::Selecting => {
                        let _ = self.release.apply(&mut self.gesture, terminal, None);
                        Vec::new()
                    }
                    Held::Reporting(button) => {
                        let release = mouse::Action::Release;
                        self.report(terminal, grid, button_of(button), release, at)
                    }
                }
            }
            Some(Held::Selecting) => {
                let _ = self.select_drag(terminal, grid, event);
                Vec::new()
            }
            Some(Held::Reporting(button)) => {
                self.report(terminal, grid, button_of(button), action, at)
            }
            // Plain motion: a button pressed elsewhere, such as to drag a
            // tab across, is no drag of the program's.
            None if tracking(terminal) => self.report(terminal, grid, None, action, at),
            None => Vec::new(),
        }
    }

    /// Ends what the press of `event`'s button started.
    pub(super) fn up(
        &mut self,
        terminal: &Terminal<'_, '_>,
        grid: &Grid,
        event: &MouseUpEvent,
    ) -> Released {
        match self.held {
            Some(Held::Selecting) if event.button == MouseButton::Left => {
                self.held = None;
                // A release past the grid still ends the gesture.
                let at = terminal
                    .grid_ref(point::Point::Viewport(grid.coordinate(event.position)))
                    .ok();
                let _ = self.release.apply(&mut self.gesture, terminal, at);
                Released::Selection
            }
            Some(Held::Reporting(button)) if event.button == button => {
                self.held = None;
                let (release, at) = (mouse::Action::Release, (event.position, event.modifiers));
                Released::Report(self.report(terminal, grid, button_of(button), release, at))
            }
            _ => Released::Nothing,
        }
    }

    /// Reports `lines` of wheel, up when positive, to a program that
    /// tracks the mouse, as presses and releases of buttons 4 and 5.
    /// `None` when no program takes it, and the wheel scrolls back instead.
    pub(super) fn wheel(
        &mut self,
        terminal: &Terminal<'_, '_>,
        grid: &Grid,
        lines: i32,
        at: (Point<Pixels>, Modifiers),
    ) -> Option<Vec<u8>> {
        if !tracking(terminal) {
            return None;
        }
        let button = Some(if lines > 0 {
            mouse::Button::Four
        } else {
            mouse::Button::Five
        });
        let mut bytes = Vec::new();
        for _ in 0..lines.unsigned_abs() {
            for action in [mouse::Action::Press, mouse::Action::Release] {
                bytes.extend(self.report(terminal, grid, button, action, at));
            }
        }
        // X10 mode reports no wheel: the wheel scrolls back there too.
        Some(bytes).filter(|bytes| !bytes.is_empty())
    }

    fn select_press(
        &mut self,
        terminal: &Terminal<'_, '_>,
        grid: &Grid,
        position: Point<Pixels>,
    ) -> Result<()> {
        let (x, y) = grid.surface(position);
        let at = terminal.grid_ref(point::Point::Viewport(grid.coordinate(position)))?;
        let selection = self
            .press
            .set_repeat_distance(f64::from(grid.unit().0))?
            .set_time(self.epoch.elapsed())?
            .set_position(f64::from(x), f64::from(y))?
            .apply(&mut self.gesture, terminal, at)?;
        terminal.set_selection(selection.as_ref())?;
        Ok(())
    }

    fn select_drag(
        &mut self,
        terminal: &Terminal<'_, '_>,
        grid: &Grid,
        event: &MouseMoveEvent,
    ) -> Result<()> {
        let (x, y) = grid.surface(event.position);
        let at = terminal.grid_ref(point::Point::Viewport(grid.coordinate(event.position)))?;
        let selection = self
            .drag
            .set_rectangle(rectangle(event.modifiers))?
            .set_position(f64::from(x), f64::from(y))?
            .apply(&mut self.gesture, terminal, at, grid.geometry())?;
        terminal.set_selection(selection.as_ref())?;
        Ok(())
    }

    /// The bytes that tell the program of `button`'s `action` at a
    /// position with modifiers held, as it asked for them.
    fn report(
        &mut self,
        terminal: &Terminal<'_, '_>,
        grid: &Grid,
        button: Option<mouse::Button>,
        action: mouse::Action,
        (position, modifiers): (Point<Pixels>, Modifiers),
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        let Ok(encoder) = self
            .encoder
            .set_options_from_terminal(terminal)
            .set_size(grid.encoder_size())
        else {
            return bytes;
        };
        // Tells a drag from plain motion, which fewer programs want.
        let pressed =
            action == mouse::Action::Press || (action == mouse::Action::Motion && button.is_some());
        let (x, y) = grid.surface(position);
        encoder
            .set_any_button_pressed(pressed)
            .set_track_last_cell(true);
        self.event
            .set_action(action)
            .set_button(button)
            .set_mods(mods(modifiers))
            .set_position(mouse::Position { x, y });
        let _ = encoder.encode_to_vec(&self.event, &mut bytes);
        bytes
    }
}

/// Whether the program asked for the mouse.
fn tracking(terminal: &Terminal<'_, '_>) -> bool {
    terminal.is_mouse_tracking().unwrap_or(false)
}

/// The whole cells in `cells`, a fraction of cells no less than 0.
fn whole(cells: f32) -> u16 {
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to u16's range first"
    )]
    let whole = cells.floor().clamp(0., f32::from(u16::MAX)) as u16;
    whole
}

/// libghostty's button for GPUI's; `None` for the ones terminals don't
/// report.
fn button_of(button: MouseButton) -> Option<mouse::Button> {
    match button {
        MouseButton::Left => Some(mouse::Button::Left),
        MouseButton::Right => Some(mouse::Button::Right),
        MouseButton::Middle => Some(mouse::Button::Middle),
        MouseButton::Navigate(_) => None,
    }
}

/// Whether a drag with `modifiers` held selects a rectangle, as in
/// Ghostty: Option on macOS, Ctrl or Super with Alt elsewhere.
fn rectangle(modifiers: Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.alt
    } else {
        modifiers.alt && (modifiers.control || modifiers.platform)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{point, px, size, Bounds};

    use super::*;

    /// A press reports the cell under it, even far across a grid whose cells
    /// are a fraction of a pixel wide, and a press off the grid reports the
    /// nearest cell.
    #[test]
    fn a_press_reports_the_cell_under_it() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        // Normal tracking, in SGR's decimal format.
        terminal.vt_write(b"\x1b[?1000h\x1b[?1006h");
        let grid = Grid {
            bounds: Bounds::new(point(px(100.), px(50.)), size(px(672.), px(432.))),
            cell: size(px(8.4), px(18.)),
            columns: 80,
            rows: 24,
        };
        let mut mouse = Mouse::new().unwrap();
        for (x, y, want) in [
            (100., 50., "1;1"),
            // Column 50's left edge: rounding cells to 8 pixels would say 52.
            (100. + 8.4 * 50. + 0.5, 50. + 18. * 3. + 1., "51;4"),
            (90., 40., "1;1"),
            (2000., 2000., "80;24"),
        ] {
            let press = MouseDownEvent {
                button: MouseButton::Left,
                position: point(px(x), px(y)),
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            };
            let report = mouse.down(&terminal, &grid, &press);
            let release = MouseUpEvent {
                button: MouseButton::Left,
                position: press.position,
                modifiers: Modifiers::default(),
                click_count: 1,
            };
            mouse.up(&terminal, &grid, &release);
            assert_eq!(
                String::from_utf8_lossy(&report),
                format!("\x1b[<0;{want}M"),
                "press at ({x}, {y})"
            );
        }
    }
}
