//! Smooth, weighted scrolling for every scroll area in the window.
//!
//! GPUI applies each scroll event at once, and on Linux nothing adds
//! momentum: a wheel notch jumps a few lines, and a touchpad scroll stops
//! dead when the fingers lift (Wayland's `axis_stop` doesn't reach GPUI).
//! This layer sits at the window root. A wheel step becomes a short glide;
//! a touchpad scroll passes straight through, and when its events stop
//! arriving its last speed carries on and decays. Both are replayed as
//! ordinary scroll events, one per frame, so lists and text areas need no
//! changes.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::{
    canvas, App, DispatchPhase, InputEvent as _, IntoElement, MouseDownEvent, Pixels, Point,
    ScrollDelta, ScrollEvent, ScrollSource, Styled as _, TouchPhase, Window,
};

/// How a wheel notch's distance is spread over frames: the remainder decays
/// with this time constant, so 95% lands within three of them (about
/// 270 ms). A starting point; tune it by feel.
const WHEEL_EASE: Duration = Duration::from_millis(90);

/// How fast a fling slows: speed decays with this time constant. iOS's
/// normal deceleration (0.998 per ms) is about 500 ms; this is a little
/// shorter, so a flick doesn't run on for long.
const FLING_DECAY: Duration = Duration::from_millis(400);

/// No touchpad event for this long means the fingers lifted. libinput
/// reports every 7 to 14 ms while fingers move, so a few missed reports.
const LIFT_GAP: Duration = Duration::from_millis(50);

/// The fling speed is the touchpad's average speed over this last stretch.
const VELOCITY_WINDOW: Duration = Duration::from_millis(80);

/// Flings slower than this, in px/s, don't start: a scroll that eased to a
/// stop shouldn't creep on.
const MIN_FLING: f64 = 150.;

/// A fling stops below this speed, in px/s: under a pixel per frame at 60 Hz.
const STOP_SPEED: f64 = 20.;

/// A frame longer than this is treated as this long, so a stall doesn't
/// throw the content a whole notch at once.
const MAX_FRAME: Duration = Duration::from_millis(50);

/// The motion state, kept apart from GPUI so it can be tested with made-up
/// clocks. Distances are in GPUI's scroll direction.
#[derive(Debug, Default)]
pub(crate) struct Glide {
    /// Wheel distance not yet delivered.
    pending: Point<Pixels>,
    /// Fling speed, in pixels per second, once the fingers lift.
    velocity: Point<Pixels>,
    /// Touchpad deltas from the last [`VELOCITY_WINDOW`], to measure the
    /// fling speed.
    samples: Vec<(Instant, Point<Pixels>)>,
    last_tick: Option<Instant>,
}

impl Glide {
    /// A wheel notch: glide `delta` over the next frames.
    pub(crate) fn wheel(&mut self, delta: Point<Pixels>) {
        self.velocity = Point::default();
        self.samples.clear();
        self.pending += delta;
    }

    /// A touchpad delta, already applied by the caller. Stops any glide and
    /// remembers the speed for a fling.
    pub(crate) fn touchpad(&mut self, delta: Point<Pixels>, now: Instant) {
        self.pending = Point::default();
        self.velocity = Point::default();
        self.samples
            .retain(|(at, _)| now.duration_since(*at) <= VELOCITY_WINDOW);
        self.samples.push((now, delta));
    }

    /// Stops all motion, as when the user clicks.
    pub(crate) fn stop(&mut self) {
        *self = Self::default();
    }

    /// Whether frames are still needed.
    pub(crate) fn is_moving(&self) -> bool {
        self.pending != Point::default()
            || self.velocity != Point::default()
            || !self.samples.is_empty()
    }

    /// The distance to scroll this frame, if any.
    pub(crate) fn tick(&mut self, now: Instant) -> Option<Point<Pixels>> {
        let dt = self
            .last_tick
            .map_or(Duration::from_millis(4), |last| now.duration_since(last))
            .min(MAX_FRAME)
            .as_secs_f32();
        self.last_tick = Some(now);

        let mut step = Point::default();
        if self.pending != Point::default() {
            let share = 1. - (-dt / WHEEL_EASE.as_secs_f32()).exp();
            let mut part = self.pending * share;
            if (self.pending - part).magnitude() < 0.5 {
                part = self.pending;
            }
            self.pending -= part;
            step += part;
        }

        if let Some(&(last, _)) = self.samples.last() {
            if now.duration_since(last) > LIFT_GAP {
                self.velocity = fling_velocity(&self.samples);
                if self.velocity.magnitude() < MIN_FLING {
                    self.velocity = Point::default();
                }
                self.samples.clear();
            }
        }

        if self.velocity != Point::default() {
            // Exact integral of exponential decay over the frame.
            let tau = FLING_DECAY.as_secs_f32();
            let decay = (-dt / tau).exp();
            step += self.velocity * (tau * (1. - decay));
            self.velocity *= decay;
            if self.velocity.magnitude() < STOP_SPEED {
                self.velocity = Point::default();
            }
        }

        if !self.is_moving() {
            self.last_tick = None;
        }
        (step != Point::default()).then_some(step)
    }
}

/// Average speed over `samples`, in pixels per second. The first sample's
/// distance was covered before its own timestamp, so it only marks the start.
fn fling_velocity(samples: &[(Instant, Point<Pixels>)]) -> Point<Pixels> {
    let [(first, _), rest @ ..] = samples else {
        return Point::default();
    };
    let Some((last, _)) = rest.last() else {
        return Point::default();
    };
    let span = last.duration_since(*first).as_secs_f32();
    if span <= 0. {
        return Point::default();
    }
    let distance = rest
        .iter()
        .fold(Point::default(), |sum, (_, delta)| sum + *delta);
    distance * (1. / span)
}

/// The glide plus what the GPUI hooks share: whether a replayed event is in
/// flight (so the hook lets it through) and whether a frame is booked.
#[derive(Default)]
pub(crate) struct Kinetic {
    glide: RefCell<Glide>,
    replaying: Cell<bool>,
    ticking: Cell<bool>,
}

impl Kinetic {
    /// An invisible element that hooks the window's scroll events. Put it
    /// first in the root view, so its capture listener runs before any
    /// scroll area's.
    pub(crate) fn hook(this: &Rc<Self>) -> impl IntoElement {
        let this = Rc::clone(this);
        canvas(
            |_, _, _| (),
            move |_, (), window, _| {
                let scroll = Rc::clone(&this);
                window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                    scroll.on_scroll(event, phase, window, cx);
                });
                window.on_mouse_event(move |_: &MouseDownEvent, phase, _, _| {
                    if phase == DispatchPhase::Capture {
                        this.glide.borrow_mut().stop();
                    }
                });
            },
        )
        .absolute()
        .size_full()
    }

    fn on_scroll(
        self: &Rc<Self>,
        event: &ScrollEvent,
        phase: DispatchPhase,
        window: &mut Window,
        cx: &mut App,
    ) {
        let modified = event.modifiers.control
            || event.modifiers.alt
            || event.modifiers.platform
            || event.modifiers.function;
        if phase != DispatchPhase::Capture
            || self.replaying.get()
            || event.source == ScrollSource::Touch
            || modified
            || cx.reduce_motion()
        {
            return;
        }
        let delta = event.delta.pixel_delta(window.line_height());
        // Wheels report lines: a notch is 3, a high-resolution wheel's step
        // a fraction. Touchpads report pixels on Wayland. X11 reports them in
        // lines too, so there they glide instead of flinging.
        if matches!(event.delta, ScrollDelta::Lines(_)) {
            self.glide.borrow_mut().wheel(delta);
            // The glide delivers it instead.
            cx.stop_propagation();
        } else {
            self.glide.borrow_mut().touchpad(delta, Instant::now());
        }
        self.book_frame(window);
    }

    fn book_frame(self: &Rc<Self>, window: &mut Window) {
        if self.ticking.replace(true) {
            return;
        }
        let this = Rc::clone(self);
        window.on_next_frame(move |window, cx| {
            this.ticking.set(false);
            let step = this.glide.borrow_mut().tick(Instant::now());
            if let Some(step) = step {
                this.replaying.set(true);
                window.dispatch_event(
                    ScrollEvent {
                        position: window.mouse_position(),
                        delta: ScrollDelta::Pixels(step),
                        modifiers: window.modifiers(),
                        touch_phase: TouchPhase::Moved,
                        source: ScrollSource::Pointer,
                    }
                    .to_platform_input(),
                    cx,
                );
                this.replaying.set(false);
            }
            if this.glide.borrow().is_moving() {
                this.book_frame(window);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{point, px};

    /// A downward scroll of `y` pixels.
    fn down(y: f32) -> Point<Pixels> {
        point(px(0.), px(y))
    }

    /// Runs `glide` at 240 Hz until it stops, returning each frame's step.
    fn run(glide: &mut Glide, start: Instant) -> Vec<f32> {
        (1..2000)
            .map(|n| start + Duration::from_micros(4167 * n))
            .map_while(|now| {
                let step = glide.tick(now);
                (glide.is_moving() || step.is_some())
                    .then(|| step.map_or(0., |step| f32::from(step.y)))
            })
            .collect()
    }

    #[test]
    fn a_wheel_notch_glides_its_whole_distance_easing_out() {
        let mut glide = Glide::default();
        glide.wheel(down(60.));
        let steps = run(&mut glide, Instant::now());
        let total: f32 = steps.iter().sum();
        assert!(
            (total - 60.).abs() < 0.01,
            "delivers exactly the notch: {total}"
        );
        assert!(steps.len() > 10, "spread over frames: {}", steps.len());
        assert!(steps[0] > steps[steps.len() / 2], "fastest first");
        assert!(!glide.is_moving());
    }

    #[test]
    fn a_fast_touchpad_swipe_flings_on_and_a_slow_one_stops() {
        let start = Instant::now();
        let mut glide = Glide::default();
        // 20 px every 8 ms: 2500 px/s.
        for n in 0..10 {
            glide.touchpad(down(20.), start + Duration::from_millis(8 * n));
        }
        let lifted = start + Duration::from_millis(72);
        let fling: f32 = run(&mut glide, lifted).iter().sum();
        // 2500 px/s decaying with a 0.4 s time constant travels about 1000 px.
        assert!((800. ..1100.).contains(&fling), "fling distance: {fling}");

        let mut slow = Glide::default();
        for n in 0..10 {
            slow.touchpad(down(0.5), start + Duration::from_millis(8 * n));
        }
        let crept: f32 = run(&mut slow, lifted).iter().sum();
        assert!(
            crept.abs() < f32::EPSILON,
            "62 px/s is under the fling threshold"
        );
    }

    #[test]
    fn touching_again_or_clicking_stops_a_glide() {
        let start = Instant::now();
        let mut glide = Glide::default();
        glide.wheel(down(60.));
        glide.tick(start);
        glide.touchpad(down(1.), start);
        assert_eq!(glide.pending, Point::default());
        glide.stop();
        assert!(!glide.is_moving());
    }
}
