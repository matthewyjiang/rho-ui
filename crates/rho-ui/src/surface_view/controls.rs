//! The Kit controls behind a panel's inputs, which keep the user's place
//! in them between frames, and what ties each to the data model.

use gpui_kit::component::{
    date_picker::{DatePickerEvent, DatePickerState},
    input::{InputState, TextareaState},
    slider::{SliderEvent, SliderState},
    time_field::{TimeFieldEvent, TimeFieldState, TimePrecision},
};
use gpui_kit::{AppContext as _, Context, Entity, Subscription, Window};
use rho_ui_core::surface::{
    Field, FieldKind, Moment, Pointer, Problem, Slider, SliderRange, Target,
};
use serde_json::Value;

use super::{SurfaceEvent, SurfacePanel};

/// A long text field's least and most rows: it grows with its text
/// between them, then scrolls.
const LONG_TEXT_ROWS: (usize, usize) = (3, 10);

/// A field's control, and where its value is kept: `None` for a field
/// that cannot be edited.
pub(super) struct FieldWidget {
    pub(super) control: FieldControl,
    binding: Option<Pointer>,
    _subscription: Subscription,
}

/// A field's Kit control, with the data model's value as it last showed
/// it. A tree that says otherwise was changed from elsewhere, and the
/// control then shows the tree's value.
pub(super) enum FieldControl {
    Text(TextControl),
    Moment(MomentControl),
}

pub(super) struct TextControl {
    pub(super) input: TextInput,
    placeholder: String,
    model: String,
    /// What the control shows, which is not the model's value while the
    /// user has typed something its kind does not take.
    shown: String,
    /// Why what the control shows is kept out of the data model.
    pub(super) problem: Option<Problem>,
}

/// A text field's Kit input, by the field's kind.
pub(super) enum TextInput {
    Line(Entity<InputState>),
    Number(Entity<InputState>),
    Obscured(Entity<InputState>),
    Long(Entity<TextareaState>),
}

pub(super) struct MomentControl {
    pub(super) picker: MomentPicker,
    model: String,
}

/// A date field's Kit picker, by the field's kind.
pub(super) enum MomentPicker {
    Date(Entity<DatePickerState>),
    DateTime(Entity<DatePickerState>),
    Time(Entity<TimeFieldState>),
}

/// A slider's Kit control, with the range it was made for, the data
/// model's value as it last showed it, and where the value is kept.
pub(super) struct SliderWidget {
    pub(super) state: Entity<SliderState>,
    range: SliderRange,
    model: f64,
    binding: Option<Pointer>,
    _subscription: Subscription,
}

impl FieldWidget {
    /// Whether it can be edited.
    pub(super) fn is_bound(&self) -> bool {
        self.binding.is_some()
    }
}

impl TextInput {
    fn kind(&self) -> FieldKind {
        match self {
            Self::Line(_) => FieldKind::Short,
            Self::Number(_) => FieldKind::Number,
            Self::Obscured(_) => FieldKind::Obscured,
            Self::Long(_) => FieldKind::Long,
        }
    }
}

impl MomentPicker {
    fn kind(&self) -> FieldKind {
        match self {
            Self::Date(_) => FieldKind::Date,
            Self::DateTime(_) => FieldKind::DateTime,
            Self::Time(_) => FieldKind::Time,
        }
    }
}

impl FieldControl {
    /// Whether it draws a field of `kind`.
    fn draws(&self, kind: FieldKind) -> bool {
        match self {
            Self::Text(text) => text.input.kind() == kind,
            Self::Moment(moment) => moment.picker.kind() == kind,
        }
    }
}

impl SurfacePanel {
    /// Whether any input shows what it keeps out of the data model: the
    /// surface's events wait until it doesn't, so none sends a stale value.
    pub(super) fn has_problem(&self) -> bool {
        self.fields.values().any(
            |field| matches!(&field.control, FieldControl::Text(text) if text.problem.is_some()),
        )
    }

    /// Makes or updates the control of `field`.
    pub(super) fn sync_field(
        &mut self,
        field: &Field,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let target = &field.input.target;
        let fits = self
            .fields
            .get(target)
            .is_some_and(|widget| widget.control.draws(field.kind));
        if !fits {
            let widget = field_widget(target, field.kind, window, cx);
            self.fields.insert(target.clone(), widget);
        }
        let Some(widget) = self.fields.get_mut(target) else {
            return;
        };
        field.input.binding.clone_into(&mut widget.binding);
        match &mut widget.control {
            FieldControl::Text(text) => text.show(field, window, cx),
            FieldControl::Moment(moment) => moment.show(&field.value, window, cx),
        }
    }

    /// Makes or updates the control of `slider`. A slider whose range
    /// changed gets a new control: Kit sets a range only as it makes one.
    pub(super) fn sync_slider(
        &mut self,
        slider: &Slider,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let target = &slider.input.target;
        let fits = self
            .sliders
            .get(target)
            .is_some_and(|widget| widget.range == slider.range);
        if !fits {
            let widget = slider_widget(target, slider, cx);
            self.sliders.insert(target.clone(), widget);
        }
        let Some(widget) = self.sliders.get_mut(target) else {
            return;
        };
        slider.input.binding.clone_into(&mut widget.binding);
        if !is_same(widget.model, slider.value) {
            widget.model = slider.value;
            let value = narrow(slider.value);
            widget
                .state
                .update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    /// The user left `text` in the text control of `target`. Text its kind
    /// does not take stays in the control and out of the data model.
    fn typed(&mut self, target: &Target, text: &str, cx: &mut Context<'_, Self>) {
        let Some(widget) = self.fields.get_mut(target) else {
            return;
        };
        let FieldControl::Text(control) = &mut widget.control else {
            return;
        };
        if control.shown == text {
            return;
        }
        text.clone_into(&mut control.shown);
        control.problem = control.input.kind().problem(text);
        if control.problem.is_none() && control.model != text {
            text.clone_into(&mut control.model);
            write(widget.binding.as_ref(), Value::String(text.to_owned()), cx);
        }
        cx.notify();
    }

    /// The user picked the moment `iso` in the picker of `target`.
    fn picked(&mut self, target: &Target, iso: String, cx: &mut Context<'_, Self>) {
        let Some(widget) = self.fields.get_mut(target) else {
            return;
        };
        let FieldControl::Moment(control) = &mut widget.control else {
            return;
        };
        if control.model != iso {
            control.model.clone_from(&iso);
            write(widget.binding.as_ref(), Value::String(iso), cx);
        }
    }

    /// The user slid the slider of `target` to `value`.
    fn slid(&mut self, target: &Target, value: f64, cx: &mut Context<'_, Self>) {
        let Some(widget) = self.sliders.get_mut(target) else {
            return;
        };
        if !is_same(widget.model, value) {
            widget.model = value;
            write(widget.binding.as_ref(), number_value(value), cx);
        }
    }
}

/// Writes `value` where `binding` keeps it, if the input has a binding.
fn write(binding: Option<&Pointer>, value: Value, cx: &mut Context<'_, SurfacePanel>) {
    if let Some(binding) = binding {
        cx.emit(SurfaceEvent::Write {
            binding: binding.clone(),
            value,
        });
    }
}

impl TextControl {
    /// Shows `field`'s placeholder and value, if the data model's value
    /// changed under the control.
    fn show(&mut self, field: &Field, window: &mut Window, cx: &mut Context<'_, SurfacePanel>) {
        let placeholder = (self.placeholder != field.placeholder).then(|| {
            field.placeholder.clone_into(&mut self.placeholder);
            field.placeholder.clone()
        });
        let value = (self.model != field.value).then(|| {
            field.value.clone_into(&mut self.model);
            field.value.clone_into(&mut self.shown);
            self.problem = None;
            field.value.clone()
        });
        match &self.input {
            TextInput::Line(state) | TextInput::Number(state) | TextInput::Obscured(state) => {
                state.update(cx, |state, cx| {
                    if let Some(placeholder) = placeholder {
                        state.set_placeholder(placeholder, window, cx);
                    }
                    if let Some(value) = value {
                        state.set_value(value, window, cx);
                    }
                });
            }
            TextInput::Long(state) => {
                state.update(cx, |state, cx| {
                    if let Some(placeholder) = placeholder {
                        state.set_placeholder(placeholder, window, cx);
                    }
                    if let Some(value) = value {
                        state.set_value(value, window, cx);
                    }
                });
            }
        }
    }
}

impl MomentControl {
    /// Shows `value`, if the data model's value changed under the picker.
    fn show(&mut self, value: &str, window: &mut Window, cx: &mut Context<'_, SurfacePanel>) {
        if self.model == value {
            return;
        }
        value.clone_into(&mut self.model);
        let moment = self.picker.kind().moment(value);
        match (&self.picker, moment) {
            (
                MomentPicker::Date(state) | MomentPicker::DateTime(state),
                Some(Moment::Date(date)),
            ) => {
                state.update(cx, |state, cx| state.set_date(date, window, cx));
            }
            (
                MomentPicker::Date(state) | MomentPicker::DateTime(state),
                Some(Moment::DateTime(moment)),
            ) => {
                state.update(cx, |state, cx| state.set_date_time(moment, window, cx));
            }
            (MomentPicker::Time(state), Some(Moment::Time(time))) => {
                state.update(cx, |state, cx| state.set_time(time, window, cx));
            }
            // Text that is no moment of the picker's kind leaves it as it
            // was.
            (_, None | Some(_)) => {}
        }
    }
}

/// An empty control for a field of `kind`, and what tells the panel of the
/// user's edits in it.
fn field_widget(
    target: &Target,
    kind: FieldKind,
    window: &mut Window,
    cx: &mut Context<'_, SurfacePanel>,
) -> FieldWidget {
    let edited = target.clone();
    let (control, subscription) = match kind {
        FieldKind::Short | FieldKind::Number | FieldKind::Obscured => {
            let state = cx.new(|cx| {
                let state = InputState::new(window, cx);
                match kind {
                    FieldKind::Obscured => state.masked(true),
                    FieldKind::Number => state.step(1.),
                    _ => state,
                }
            });
            // Observed, not subscribed to `InputEvent::Change`: a number
            // input's stepper changes the text without that event.
            let subscription = cx.observe(&state, move |this, state, cx| {
                let text = state.read(cx).value();
                this.typed(&edited, &text, cx);
            });
            let input = match kind {
                FieldKind::Number => TextInput::Number(state),
                FieldKind::Obscured => TextInput::Obscured(state),
                _ => TextInput::Line(state),
            };
            (text_control(input), subscription)
        }
        FieldKind::Long => {
            let state = cx.new(|cx| {
                TextareaState::new(window, cx).auto_grow(LONG_TEXT_ROWS.0, LONG_TEXT_ROWS.1)
            });
            let subscription = cx.observe(&state, move |this, state, cx| {
                let text = state.read(cx).value();
                this.typed(&edited, &text, cx);
            });
            (text_control(TextInput::Long(state)), subscription)
        }
        FieldKind::Time => {
            let state =
                cx.new(|cx| TimeFieldState::new(window, cx).precision(TimePrecision::Minute));
            let subscription = cx.subscribe(&state, move |this, _, event: &TimeFieldEvent, cx| {
                let TimeFieldEvent::Change(time) = event;
                this.picked(&edited, Moment::Time(*time).to_iso(), cx);
            });
            (moment_control(MomentPicker::Time(state)), subscription)
        }
        FieldKind::Date | FieldKind::DateTime => {
            let with_time = kind == FieldKind::DateTime;
            let state = cx.new(|cx| {
                let state = DatePickerState::new(window, cx);
                if with_time {
                    state.time_precision(TimePrecision::Minute)
                } else {
                    state.date_format("%Y-%m-%d")
                }
            });
            let subscription = cx.subscribe(&state, move |this, _, event: &DatePickerEvent, cx| {
                let DatePickerEvent::Change(picked) = event;
                let iso = picked.start().map_or_else(String::new, |moment| {
                    if with_time {
                        Moment::DateTime(moment).to_iso()
                    } else {
                        Moment::Date(moment.date()).to_iso()
                    }
                });
                this.picked(&edited, iso, cx);
            });
            let picker = if with_time {
                MomentPicker::DateTime(state)
            } else {
                MomentPicker::Date(state)
            };
            (moment_control(picker), subscription)
        }
    };
    FieldWidget {
        control,
        binding: None,
        _subscription: subscription,
    }
}

fn text_control(input: TextInput) -> FieldControl {
    FieldControl::Text(TextControl {
        input,
        placeholder: String::new(),
        model: String::new(),
        shown: String::new(),
        problem: None,
    })
}

fn moment_control(picker: MomentPicker) -> FieldControl {
    FieldControl::Moment(MomentControl {
        picker,
        model: String::new(),
    })
}

/// A control for `slider`, at its value, and what tells the panel when
/// the user slides it.
fn slider_widget(
    target: &Target,
    slider: &Slider,
    cx: &mut Context<'_, SurfacePanel>,
) -> SliderWidget {
    let SliderRange { min, max, step } = slider.range;
    let state = cx.new(|_| {
        SliderState::new()
            .min(narrow(min))
            .max(narrow(max))
            .step(narrow(step))
            .default_value(narrow(slider.value))
    });
    let slid = target.clone();
    let subscription = cx.subscribe(&state, move |this, _, event: &SliderEvent, cx| {
        let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
        this.slid(&slid, widen(value.start()), cx);
    });
    SliderWidget {
        state,
        range: slider.range,
        model: slider.value,
        binding: None,
        _subscription: subscription,
    }
}

/// The largest magnitude a slider takes. Kit's slider counts in `f32`;
/// within this, its range's width stays finite there.
const SLIDER_LIMIT: f64 = 1e30;

/// A slider's value or range for Kit's slider, within [`SLIDER_LIMIT`].
fn narrow(value: f64) -> f32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::as_conversions,
        reason = "clamped well within f32's range; the model keeps its own value"
    )]
    let narrowed = value.clamp(-SLIDER_LIMIT, SLIDER_LIMIT) as f32;
    narrowed
}

/// A value Kit's slider gave, for the data model. Without rounding, 0.3
/// is written as 0.30000001192092896.
fn widen(value: f32) -> f64 {
    (f64::from(value) * 1e6).round() / 1e6
}

/// Whether a slider's value is the one it was: both come from the same
/// reads and writes, so they match exactly or differ.
#[expect(
    clippy::float_cmp,
    reason = "the same value read twice is equal exactly"
)]
fn is_same(was: f64, is: f64) -> bool {
    was == is
}

/// A slider's value as JSON: an integer when it is whole, so a model that
/// asked for 1 to 10 reads `7`, not `7.0`.
pub(super) fn number_value(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::as_conversions,
            reason = "whole and under 1e15, which i64 holds exactly"
        )]
        let whole = value as i64;
        Value::from(whole)
    } else {
        Value::from(value)
    }
}
