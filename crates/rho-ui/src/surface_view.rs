//! A surface a session's `ui` tool showed, drawn in the transcript under
//! the call that showed it, with GPUI Kit's controls.
//!
//! [`SurfacePanel`] draws a [`SurfaceTree`], which the session's
//! [`rho_ui_core::surface::SurfaceStore`] reads from its A2UI processor.
//! The panel owns what Kit's controls keep between frames: the text being
//! typed, a slider's thumb, the tab showing. What the user does leaves as a
//! [`SurfaceEvent`]; the session view writes it to the store and gives the
//! panel the tree as it then stands ([`SurfacePanel::show`]), so checks and
//! bindings follow each edit.

use std::{collections::HashMap, sync::Arc};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _, Toggle, ToggleGroup, ToggleVariants as _},
    checkbox::Checkbox,
    date_picker::{DatePicker, DatePickerEvent, DatePickerState},
    h_flex,
    input::{Input, InputState, NumberInput, Textarea, TextareaState},
    link::Link,
    radio::{Radio, RadioGroup},
    separator::Separator,
    slider::{Slider, SliderEvent, SliderState},
    tab::{Tab, TabBar},
    text::TextView,
    time_field::{TimeField, TimeFieldEvent, TimeFieldState, TimePrecision},
    v_flex, ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, img, rems, AnyElement, App, AppContext as _, Axis, Context, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, WeakEntity,
    Window,
};
use rho_ui_core::{
    surface::{
        number_value, Action, Align, ButtonVariant, Child, ChoiceOption, Failure, FieldKind,
        ImageFit, ImageSize, Justify, MediaKind, Moment, Node, Pointer, SurfaceTree, Target,
    },
    Image,
};
use serde_json::Value;

use crate::attachments::Preview;

/// What the user did on a surface.
#[derive(Clone, Debug)]
pub(crate) enum SurfaceEvent {
    /// Entered `value` in a control bound to `binding`.
    Write { binding: Pointer, value: Value },
    /// Pressed the button `target`, which runs `action`.
    Act { target: Target, action: Action },
}

/// The Kit control behind one input, which keeps the user's place in it.
enum Control {
    Input(Entity<InputState>),
    Textarea(Entity<TextareaState>),
    Slider(Entity<SliderState>),
    Date(Entity<DatePickerState>),
    Time(Entity<TimeFieldState>),
}

/// One input's control, and what ties it to the data model.
struct Widget {
    control: Control,
    kind: Option<FieldKind>,
    /// Where the input's value is kept; `None` for one that cannot be
    /// edited.
    binding: Option<Pointer>,
    /// The data model's value, as last read from the tree or written by
    /// the user. A tree that says otherwise was changed from elsewhere,
    /// and the control then shows the tree's value.
    model: String,
    /// What the control shows, which is not the model's value while the
    /// user has typed something its kind does not take.
    shown: String,
    /// Why what the control shows is kept out of the data model.
    invalid: Option<&'static str>,
    _subscription: Subscription,
}

/// One surface, as drawn.
pub(crate) struct SurfacePanel {
    surface: String,
    /// The call whose card the surface is drawn under, while it exists.
    drawn_under: Option<String>,
    tree: Option<SurfaceTree>,
    /// The session no longer has the surface as shown: it takes no action.
    stale: bool,
    /// An action is on its way to the daemon.
    sending: bool,
    /// By [`Target::key`].
    widgets: HashMap<String, Widget>,
    /// The tab each `Tabs` shows, by [`Target::key`]; the first until
    /// the user picks another.
    tabs: HashMap<String, usize>,
    /// The pictures its images show from `data:` URLs, by URL.
    pictures: HashMap<String, Option<Arc<gpui_kit::Image>>>,
}

impl EventEmitter<SurfaceEvent> for SurfacePanel {}

impl SurfacePanel {
    pub(crate) fn new(surface: String) -> Self {
        Self {
            surface,
            drawn_under: None,
            tree: None,
            stale: false,
            sending: false,
            widgets: HashMap::new(),
            tabs: HashMap::new(),
            pictures: HashMap::new(),
        }
    }

    /// The surface's ID.
    pub(crate) fn surface(&self) -> &str {
        &self.surface
    }

    /// The call whose transcript entry draws the surface.
    pub(crate) fn drawn_under(&self) -> Option<&str> {
        self.drawn_under.as_deref()
    }

    /// An action is on its way to the daemon: the buttons wait.
    pub(crate) fn sending(&mut self, cx: &mut Context<'_, Self>) {
        self.sending = true;
        cx.notify();
    }

    /// The daemon answered the action.
    pub(crate) fn sent(&mut self, cx: &mut Context<'_, Self>) {
        self.sending = false;
        cx.notify();
    }

    /// Takes the surface as it stands: its `tree`, the call it is drawn
    /// under, and whether it is stale. Controls keep what the user is
    /// doing in them unless the data model changed under them.
    pub(crate) fn show(
        &mut self,
        tree: Option<SurfaceTree>,
        drawn_under: Option<&str>,
        standing: Standing,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.drawn_under = drawn_under.map(str::to_owned);
        self.stale = standing == Standing::Stale;
        let mut keys = Vec::new();
        if let Some(tree) = &tree {
            self.sync(&tree.root, &mut keys, window, cx);
        }
        self.widgets.retain(|key, _| keys.contains(key));
        self.tree = tree;
        cx.notify();
    }

    /// Makes or updates the controls of `node` and of what it holds,
    /// adding their keys to `keys`.
    fn sync(
        &mut self,
        node: &Node,
        keys: &mut Vec<String>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match node {
            Node::Row { children, .. } | Node::Column { children, .. } => {
                for child in children {
                    self.sync(&child.node, keys, window, cx);
                }
            }
            Node::Card(child) => self.sync(child, keys, window, cx),
            Node::Tabs { tabs, .. } => {
                for body in tabs.iter().filter_map(|(_, body)| body.as_ref()) {
                    self.sync(body, keys, window, cx);
                }
            }
            Node::Modal { content, .. } => {
                if let Some(content) = content {
                    self.sync(content, keys, window, cx);
                }
            }
            Node::Field {
                target,
                value,
                placeholder,
                kind,
                binding,
                ..
            } => {
                let key = target.key();
                self.sync_field(
                    &key,
                    value,
                    placeholder,
                    *kind,
                    binding.as_ref(),
                    window,
                    cx,
                );
                keys.push(key);
            }
            Node::Slider {
                target,
                value,
                min,
                max,
                step,
                binding,
                ..
            } => {
                let key = target.key();
                self.sync_slider(
                    &key,
                    *value,
                    (*min, *max, *step),
                    binding.as_ref(),
                    window,
                    cx,
                );
                keys.push(key);
            }
            Node::Media {
                kind: MediaKind::Image,
                url,
                ..
            } => {
                if !self.pictures.contains_key(url) {
                    self.pictures.insert(url.clone(), data_url_image(url));
                }
            }
            Node::Text { .. }
            | Node::Icon(_)
            | Node::Media { .. }
            | Node::Divider { .. }
            | Node::Button { .. }
            | Node::CheckBox { .. }
            | Node::Choices { .. }
            | Node::Placeholder(_) => {}
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a field's properties, read once from its node"
    )]
    fn sync_field(
        &mut self,
        key: &str,
        value: &str,
        placeholder: &str,
        kind: FieldKind,
        binding: Option<&Pointer>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(widget) = self.widgets.get_mut(key) {
            if widget.kind == Some(kind) {
                widget.binding = binding.cloned();
                if widget.model != value {
                    value.clone_into(&mut widget.model);
                    value.clone_into(&mut widget.shown);
                    widget.invalid = None;
                    widget.control.set(kind, value, window, cx);
                }
                return;
            }
        }
        let (control, subscription) = field_control(key, value, placeholder, kind, window, cx);
        self.widgets.insert(
            key.to_owned(),
            Widget {
                control,
                kind: Some(kind),
                binding: binding.cloned(),
                model: value.to_owned(),
                shown: value.to_owned(),
                invalid: None,
                _subscription: subscription,
            },
        );
    }

    fn sync_slider(
        &mut self,
        key: &str,
        value: f64,
        (min, max, step): (f64, f64, f64),
        binding: Option<&Pointer>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let model = value.to_string();
        if let Some(widget) = self.widgets.get_mut(key) {
            if let Control::Slider(state) = &widget.control {
                widget.binding = binding.cloned();
                if widget.model != model {
                    widget.model = model;
                    state.update(cx, |state, cx| state.set_value(narrow(value), window, cx));
                }
                return;
            }
        }
        let state = cx.new(|_| {
            SliderState::new()
                .min(narrow(min))
                .max(narrow(max))
                .step(narrow(step))
                .default_value(narrow(value))
        });
        let slid = key.to_owned();
        let subscription = cx.subscribe(&state, move |this, _, event: &SliderEvent, cx| {
            let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
            // The slider counts in f32: without rounding, 0.3 is written
            // as 0.30000001192092896.
            let value = (f64::from(value.start()) * 1e6).round() / 1e6;
            this.entered(&slid, &value.to_string(), number_value(value), cx);
        });
        self.widgets.insert(
            key.to_owned(),
            Widget {
                control: Control::Slider(state),
                kind: None,
                binding: binding.cloned(),
                shown: model.clone(),
                model,
                invalid: None,
                _subscription: subscription,
            },
        );
    }

    /// The user left `text` in the text control `key`. Text its kind does
    /// not take stays in the control and out of the data model.
    fn typed(&mut self, key: &str, text: &str, cx: &mut Context<'_, Self>) {
        let Some(widget) = self.widgets.get_mut(key) else {
            return;
        };
        if widget.shown == text {
            return;
        }
        text.clone_into(&mut widget.shown);
        widget.invalid = widget.kind.and_then(|kind| kind.problem(text));
        if widget.invalid.is_some() {
            cx.notify();
            return;
        }
        self.entered(key, text, Value::String(text.to_owned()), cx);
    }

    /// The user set control `key` to `value`, which the data model holds
    /// as the text `model`.
    fn entered(&mut self, key: &str, model: &str, value: Value, cx: &mut Context<'_, Self>) {
        let Some(widget) = self.widgets.get_mut(key) else {
            return;
        };
        if widget.model == model {
            return;
        }
        model.clone_into(&mut widget.model);
        model.clone_into(&mut widget.shown);
        widget.invalid = None;
        if let Some(binding) = widget.binding.clone() {
            cx.emit(SurfaceEvent::Write { binding, value });
        }
        cx.notify();
    }

    /// Whether the surface's controls take input.
    fn is_live(&self) -> bool {
        !self.stale
    }

    fn id(&self, key: &str) -> SharedString {
        SharedString::from(format!("surface-{}-{key}", self.surface))
    }
}

impl Control {
    /// Shows `value`, the data model's, which changed under the control.
    fn set(&self, kind: FieldKind, value: &str, window: &mut Window, cx: &mut App) {
        match self {
            Self::Input(state) => {
                state.update(cx, |state, cx| {
                    state.set_value(value.to_owned(), window, cx);
                });
            }
            Self::Textarea(state) => {
                state.update(cx, |state, cx| {
                    state.set_value(value.to_owned(), window, cx);
                });
            }
            Self::Date(state) => match kind.moment(value) {
                Some(Moment::Date(date)) => {
                    state.update(cx, |state, cx| state.set_date(date, window, cx));
                }
                Some(Moment::DateTime(moment)) => {
                    state.update(cx, |state, cx| state.set_date_time(moment, window, cx));
                }
                Some(Moment::Time(_)) | None => {}
            },
            Self::Time(state) => {
                if let Some(Moment::Time(time)) = kind.moment(value) {
                    state.update(cx, |state, cx| state.set_time(time, window, cx));
                }
            }
            Self::Slider(_) => {}
        }
    }
}

/// The control for a field of `kind` that holds `value`, and what tells
/// the panel of the user's edits in it.
fn field_control(
    key: &str,
    value: &str,
    placeholder: &str,
    kind: FieldKind,
    window: &mut Window,
    cx: &mut Context<'_, SurfacePanel>,
) -> (Control, Subscription) {
    let edited = key.to_owned();
    match kind {
        FieldKind::Short | FieldKind::Number | FieldKind::Obscured => {
            let state = cx.new(|cx| {
                let state = InputState::new(window, cx)
                    .placeholder(placeholder.to_owned())
                    .default_value(value.to_owned());
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
            (Control::Input(state), subscription)
        }
        FieldKind::Long => {
            let state = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(LONG_TEXT_ROWS.0, LONG_TEXT_ROWS.1)
                    .placeholder(placeholder.to_owned())
                    .default_value(value.to_owned())
            });
            let subscription = cx.observe(&state, move |this, state, cx| {
                let text = state.read(cx).value();
                this.typed(&edited, &text, cx);
            });
            (Control::Textarea(state), subscription)
        }
        FieldKind::DateTime {
            date: false,
            time: true,
        } => {
            let state = cx.new(|cx| {
                let mut state = TimeFieldState::new(window, cx).precision(TimePrecision::Minute);
                if let Some(Moment::Time(time)) = kind.moment(value) {
                    state.set_time(time, window, cx);
                }
                state
            });
            let subscription = cx.subscribe(&state, move |this, _, event: &TimeFieldEvent, cx| {
                let TimeFieldEvent::Change(time) = event;
                let iso = Moment::Time(*time).to_iso();
                this.entered(&edited, &iso, Value::String(iso.clone()), cx);
            });
            (Control::Time(state), subscription)
        }
        FieldKind::DateTime { date, time } => {
            // A field that enables neither takes both, as the core reads it.
            let with_time = time || !date;
            let state = cx.new(|cx| {
                let state = DatePickerState::new(window, cx);
                let mut state = if with_time {
                    state.time_precision(TimePrecision::Minute)
                } else {
                    state.date_format("%Y-%m-%d")
                };
                match kind.moment(value) {
                    Some(Moment::Date(date)) => state.set_date(date, window, cx),
                    Some(Moment::DateTime(moment)) => state.set_date_time(moment, window, cx),
                    Some(Moment::Time(_)) | None => {}
                }
                state
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
                this.entered(&edited, &iso, Value::String(iso.clone()), cx);
            });
            (Control::Date(state), subscription)
        }
    }
}

/// A long text field's least and most rows: it grows with its text
/// between them, then scrolls.
const LONG_TEXT_ROWS: (usize, usize) = (3, 10);

/// The widest a card or a field grows, in rems: a form reads badly
/// stretched over the transcript's whole measure.
const CARD_MAX_REMS: f32 = 36.;

/// A slider's value for Kit's slider, which counts in `f32`.
fn narrow(value: f64) -> f32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::as_conversions,
        reason = "a slider's range; a value past f32 saturates, and the model's own is written back"
    )]
    let narrowed = value as f32;
    narrowed
}

/// The picture a `data:` URL holds, if it is one this app draws.
fn data_url_image(url: &str) -> Option<Arc<gpui_kit::Image>> {
    let preview = Preview::of_image(&Image::from_data_url(url)?)?;
    Some(preview.image()?.image())
}

/// The app's icon for an A2UI icon name; a name it has none for draws as
/// a dashed square.
fn lucide(name: &str) -> Lucide {
    match name {
        "accountCircle" => Lucide::CircleUser,
        "add" => Lucide::Plus,
        "arrowBack" => Lucide::ArrowLeft,
        "arrowForward" => Lucide::ArrowRight,
        "attachFile" => Lucide::Paperclip,
        "calendarToday" => Lucide::Calendar,
        "call" | "phone" => Lucide::Phone,
        "camera" => Lucide::Camera,
        "check" => Lucide::Check,
        "close" => Lucide::X,
        "delete" => Lucide::Trash,
        "download" => Lucide::Download,
        "edit" => Lucide::Pencil,
        "event" => Lucide::CalendarDays,
        "error" => Lucide::CircleAlert,
        "fastForward" => Lucide::FastForward,
        "favorite" => Lucide::Heart,
        "favoriteOff" => Lucide::HeartOff,
        "folder" => Lucide::Folder,
        "help" => Lucide::CircleQuestionMark,
        "home" => Lucide::House,
        "info" => Lucide::Info,
        "locationOn" => Lucide::MapPin,
        "lock" => Lucide::Lock,
        "lockOpen" => Lucide::LockOpen,
        "mail" => Lucide::Mail,
        "menu" => Lucide::Menu,
        "moreVert" => Lucide::EllipsisVertical,
        "moreHoriz" => Lucide::Ellipsis,
        "notificationsOff" => Lucide::BellOff,
        "notifications" => Lucide::Bell,
        "pause" => Lucide::Pause,
        "payment" => Lucide::CreditCard,
        "person" => Lucide::User,
        "photo" => Lucide::Image,
        "play" => Lucide::Play,
        "print" => Lucide::Printer,
        "refresh" => Lucide::RefreshCw,
        "rewind" => Lucide::Rewind,
        "search" => Lucide::Search,
        "send" => Lucide::Send,
        "settings" => Lucide::Settings,
        "share" => Lucide::Share2,
        "shoppingCart" => Lucide::ShoppingCart,
        "skipNext" => Lucide::SkipForward,
        "skipPrevious" => Lucide::SkipBack,
        "star" => Lucide::Star,
        "starHalf" => Lucide::StarHalf,
        "starOff" => Lucide::StarOff,
        "stop" => Lucide::Square,
        "upload" => Lucide::Upload,
        "visibility" => Lucide::Eye,
        "visibilityOff" => Lucide::EyeOff,
        "volumeDown" => Lucide::Volume1,
        "volumeMute" => Lucide::Volume,
        "volumeOff" => Lucide::VolumeX,
        "volumeUp" => Lucide::Volume2,
        "warning" => Lucide::TriangleAlert,
        _ => Lucide::SquareDashed,
    }
}

/// Whether an input's value has a place in the data model.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bound {
    /// It is kept there, so the user can change it.
    Kept,
    /// Nothing would keep a change: the input only shows its value.
    Free,
}

/// How a choice picker draws its options.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    /// One of a list.
    Radios,
    /// Any of a list.
    Checkboxes,
    /// One of a row of chips.
    OneChip,
    /// Any of a row of chips.
    Chips,
}

/// Whether the session still has a surface as shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Standing {
    /// It takes the user's actions.
    Current,
    /// The daemon refused an action on it: it takes no more.
    Stale,
}

/// A handler that reaches the panel from one of its controls.
type Handle = WeakEntity<SurfacePanel>;

fn emit(panel: &Handle, event: SurfaceEvent, cx: &mut App) {
    let _ = panel.update(cx, |_, cx| cx.emit(event));
}

impl SurfacePanel {
    fn render_node(&self, node: &Node, panel: &Handle, cx: &App) -> AnyElement {
        match node {
            Node::Text {
                target,
                text,
                caption,
            } => {
                let view =
                    TextView::markdown(self.id(&target.key()), text.clone()).selectable(true);
                if *caption {
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(view)
                        .into_any_element()
                } else {
                    view.into_any_element()
                }
            }
            Node::Icon(name) => Icon::new(lucide(name)).into_any_element(),
            Node::Media {
                target,
                kind,
                description,
                url,
                fit,
                size,
            } => self.render_media(target, *kind, description, url, (*fit, *size), cx),
            Node::Divider { vertical: true } => div()
                .h_full()
                .min_h_4()
                .child(Separator::vertical())
                .into_any_element(),
            Node::Divider { vertical: false } => Separator::horizontal().into_any_element(),
            Node::Row {
                children,
                justify,
                align,
            } => self.render_layout(Axis::Horizontal, children, (*justify, *align), panel, cx),
            Node::Column {
                children,
                justify,
                align,
            } => self.render_layout(Axis::Vertical, children, (*justify, *align), panel, cx),
            Node::Card(child) => v_flex()
                .w_full()
                .max_w(rems(CARD_MAX_REMS))
                .p_4()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .child(self.render_node(child, panel, cx))
                .into_any_element(),
            Node::Tabs { target, tabs } => self.render_tabs(target, tabs, panel, cx),
            Node::Placeholder(name) => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("[{name}]"))
                .into_any_element(),
            Node::Modal { .. }
            | Node::Button { .. }
            | Node::CheckBox { .. }
            | Node::Field { .. }
            | Node::Choices { .. }
            | Node::Slider { .. } => self.render_control(node, panel, cx),
        }
    }

    /// A row or a column of `children`.
    fn render_layout(
        &self,
        axis: Axis,
        children: &[Child],
        (justify, align): (Justify, Align),
        panel: &Handle,
        cx: &App,
    ) -> AnyElement {
        let layout = match axis {
            Axis::Horizontal => h_flex().w_full().flex_wrap().gap_2(),
            Axis::Vertical => v_flex().w_full().gap_3(),
        };
        let layout = match justify {
            Justify::Start | Justify::Stretch => layout,
            Justify::Center => layout.justify_center(),
            Justify::End => layout.justify_end(),
            Justify::SpaceBetween => layout.justify_between(),
            Justify::SpaceAround | Justify::SpaceEvenly => layout.justify_around(),
        };
        let layout = match (align, axis) {
            (Align::Start, _) => layout.items_start(),
            (Align::Center, _) => layout.items_center(),
            // A2UI stretches a row's children to its height by default,
            // which makes a button beside a labelled field as tall as the
            // label and the field together. They sit on the row's bottom
            // edge instead, the button level with the field's box.
            (Align::End, _) | (Align::Stretch, Axis::Horizontal) => layout.items_end(),
            (Align::Stretch, Axis::Vertical) => layout.items_stretch(),
        };
        layout
            .children(
                children
                    .iter()
                    .map(|child| self.render_child(child, axis, panel, cx)),
            )
            .into_any_element()
    }

    /// A node the user operates.
    fn render_control(&self, node: &Node, panel: &Handle, cx: &App) -> AnyElement {
        match node {
            Node::Modal {
                target,
                label,
                icon,
                content,
            } => {
                let key = target.key();
                let title = SharedString::from(label.clone());
                let opens = panel.clone();
                let mut button = Button::new(self.id(&key))
                    .outline()
                    .small()
                    .label(title.clone())
                    .disabled(content.is_none())
                    .on_click(move |_, window, cx| {
                        open_modal(opens.clone(), key.clone(), title.clone(), window, cx);
                    });
                if let Some(icon) = icon {
                    button = button.icon(Icon::new(lucide(icon)));
                }
                button.into_any_element()
            }
            Node::Button { .. } => self.render_button(node, panel),
            Node::CheckBox {
                target,
                label,
                checked,
                binding,
                failures,
            } => {
                let panel = panel.clone();
                let writes = binding.clone();
                let checkbox = Checkbox::new(self.id(&target.key()))
                    .label(SharedString::from(label.clone()))
                    .checked(*checked)
                    .disabled(binding.is_none() || !self.is_live())
                    .on_click(move |checked, _, cx| {
                        if let Some(binding) = writes.clone() {
                            let value = Value::Bool(*checked);
                            emit(&panel, SurfaceEvent::Write { binding, value }, cx);
                        }
                    });
                with_failures(checkbox.into_any_element(), failures, None, cx)
            }
            Node::Field {
                target,
                label,
                failures,
                ..
            } => self.render_field(target, label, failures, cx),
            Node::Choices {
                target,
                label,
                options,
                multiple,
                chips,
                binding,
                failures,
            } => {
                let style = match (*chips, *multiple) {
                    (true, true) => Picker::Chips,
                    (true, false) => Picker::OneChip,
                    (false, true) => Picker::Checkboxes,
                    (false, false) => Picker::Radios,
                };
                let picker = self.render_choices(target, options, style, binding.as_ref(), panel);
                labelled(label, with_failures(picker, failures, None, cx), cx)
            }
            Node::Slider {
                target,
                label,
                value,
                binding,
                failures,
                ..
            } => {
                let bound = if binding.is_some() {
                    Bound::Kept
                } else {
                    Bound::Free
                };
                self.render_slider(target, label, *value, bound, failures, cx)
            }
            // `render_node` draws the rest.
            _ => div().into_any_element(),
        }
    }

    fn render_button(&self, node: &Node, panel: &Handle) -> AnyElement {
        let Node::Button {
            target,
            label,
            icon,
            variant,
            action,
            failures,
        } = node
        else {
            return div().into_any_element();
        };
        let button = Button::new(self.id(&target.key()))
            .small()
            .label(label.clone());
        let mut button = match variant {
            ButtonVariant::Primary => button.primary(),
            ButtonVariant::Default => button.outline(),
            ButtonVariant::Borderless => button.link(),
        };
        if let Some(icon) = icon {
            button = button.icon(Icon::new(lucide(icon)));
        }
        // A button whose check fails says why where the pointer is.
        if let Some(failure) = failures.first() {
            button = button.tooltip(failure.message.clone());
        }
        let pressed = action.clone().map(|action| (target.clone(), action));
        let panel = panel.clone();
        button
            .disabled(pressed.is_none() || !self.is_live() || self.sending)
            .when_some(pressed, |button, (target, action)| {
                button.on_click(move |_, _, cx| {
                    let event = SurfaceEvent::Act {
                        target: target.clone(),
                        action: action.clone(),
                    };
                    emit(&panel, event, cx);
                })
            })
            .into_any_element()
    }

    fn render_slider(
        &self,
        target: &Target,
        label: &str,
        value: f64,
        bound: Bound,
        failures: &[Failure],
        cx: &App,
    ) -> AnyElement {
        let Some(Control::Slider(state)) = self
            .widgets
            .get(&target.key())
            .map(|widget| &widget.control)
        else {
            return div().into_any_element();
        };
        let slider = h_flex()
            .w_full()
            .gap_3()
            .child(
                div().flex_1().child(
                    Slider::new(state)
                        .accessibility_label(SharedString::from(label.to_owned()))
                        .disabled(bound == Bound::Free || !self.is_live()),
                ),
            )
            .child(
                div()
                    .id(self.id(&format!("{}-value", target.key())))
                    .test_support()
                    .min_w_8()
                    .text_right()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(number_value(value).to_string()),
            );
        labelled(
            label,
            with_failures(slider.into_any_element(), failures, None, cx),
            cx,
        )
    }

    /// `child` in a layout along `axis`. In a row, a child with a weight,
    /// and an input without one, takes its share of the row; anything
    /// else keeps its own width.
    fn render_child(&self, child: &Child, axis: Axis, panel: &Handle, cx: &App) -> AnyElement {
        let element = self.render_node(&child.node, panel, cx);
        let in_row = axis == Axis::Horizontal;
        let fills = child.weight.is_some()
            || (in_row
                && matches!(
                    child.node,
                    Node::Field { .. } | Node::Slider { .. } | Node::Choices { .. }
                ));
        if fills {
            div()
                .flex_1()
                .min_w(rems(if in_row { 10. } else { 0. }))
                .child(element)
                .into_any_element()
        } else {
            element
        }
    }

    fn render_field(
        &self,
        target: &Target,
        label: &str,
        failures: &[Failure],
        cx: &App,
    ) -> AnyElement {
        let key = target.key();
        let Some(widget) = self.widgets.get(&key) else {
            return div().into_any_element();
        };
        let disabled = widget.binding.is_none() || !self.is_live();
        let name = SharedString::from(label.to_owned());
        let control = match &widget.control {
            Control::Input(state) if widget.kind == Some(FieldKind::Number) => {
                NumberInput::new(state)
                    .accessibility_label(name)
                    .small()
                    .disabled(disabled)
                    .into_any_element()
            }
            Control::Input(state) => {
                let input = Input::new(state)
                    .id(self.id(&key))
                    .aria_label(name)
                    .small()
                    .disabled(disabled);
                if widget.kind == Some(FieldKind::Obscured) {
                    input.mask_toggle().into_any_element()
                } else {
                    input.into_any_element()
                }
            }
            Control::Textarea(state) => Textarea::new(state)
                .aria_label(name)
                .disabled(disabled)
                .into_any_element(),
            Control::Date(state) => DatePicker::new(state)
                .small()
                .disabled(disabled)
                .into_any_element(),
            Control::Time(state) => TimeField::new(state)
                .small()
                .disabled(disabled)
                .into_any_element(),
            Control::Slider(_) => div().into_any_element(),
        };
        // A date or time takes a few characters: its control keeps its
        // own width instead of the field's.
        let control = match widget.control {
            Control::Date(_) | Control::Time(_) => h_flex().child(control).into_any_element(),
            _ => control,
        };
        labelled(
            label,
            with_failures(control, failures, widget.invalid, cx),
            cx,
        )
    }

    fn render_choices(
        &self,
        target: &Target,
        options: &[ChoiceOption],
        style: Picker,
        binding: Option<&Pointer>,
        panel: &Handle,
    ) -> AnyElement {
        let key = target.key();
        let disabled = binding.is_none() || !self.is_live();
        let values: Vec<String> = options.iter().map(|option| option.value.clone()).collect();
        let chosen: Vec<bool> = options.iter().map(|option| option.selected).collect();
        // The picker's value is the chosen options' values, in the
        // options' order.
        let write = {
            let (panel, binding) = (panel.clone(), binding.cloned());
            move |chosen: &[bool], cx: &mut App| {
                let Some(binding) = binding.clone() else {
                    return;
                };
                let value = values
                    .iter()
                    .zip(chosen)
                    .filter(|(_, chosen)| **chosen)
                    .map(|(value, _)| Value::String(value.clone()))
                    .collect();
                emit(&panel, SurfaceEvent::Write { binding, value }, cx);
            }
        };
        match style {
            Picker::Chips | Picker::OneChip => {
                let group = ToggleGroup::new(self.id(&key))
                    .outline()
                    .small()
                    .disabled(disabled)
                    .children(options.iter().map(|option| {
                        Toggle::new(self.id(&format!("{key}-{}", option.value)))
                            .label(option.label.clone())
                            .checked(option.selected)
                    }))
                    .on_click(move |chosen, _, cx| write(chosen, cx));
                let group = if style == Picker::OneChip {
                    group.single()
                } else {
                    group
                };
                h_flex().flex_wrap().child(group).into_any_element()
            }
            Picker::Checkboxes => v_flex()
                .gap_2()
                .children(options.iter().enumerate().map(|(ix, option)| {
                    let (write, chosen) = (write.clone(), chosen.clone());
                    Checkbox::new(self.id(&format!("{key}-{}", option.value)))
                        .label(SharedString::from(option.label.clone()))
                        .checked(option.selected)
                        .disabled(disabled)
                        .on_click(move |checked, _, cx| {
                            let mut chosen = chosen.clone();
                            if let Some(chosen) = chosen.get_mut(ix) {
                                *chosen = *checked;
                            }
                            write(&chosen, cx);
                        })
                }))
                .into_any_element(),
            Picker::Radios => {
                let count = options.len();
                RadioGroup::vertical(self.id(&key))
                    .disabled(disabled)
                    .selected_index(options.iter().position(|option| option.selected))
                    .children(options.iter().map(|option| {
                        Radio::new(self.id(&format!("{key}-{}", option.value)))
                            .label(SharedString::from(option.label.clone()))
                    }))
                    .on_click(move |picked, _, cx| {
                        let chosen: Vec<bool> = (0..count).map(|ix| ix == *picked).collect();
                        write(&chosen, cx);
                    })
                    .into_any_element()
            }
        }
    }

    fn render_tabs(
        &self,
        target: &Target,
        tabs: &[(String, Option<Node>)],
        panel: &Handle,
        cx: &App,
    ) -> AnyElement {
        let key = target.key();
        let active = self
            .tabs
            .get(&key)
            .copied()
            .unwrap_or(0)
            .min(tabs.len().saturating_sub(1));
        let picks = panel.clone();
        let picked = key.clone();
        let bar = TabBar::new(self.id(&key))
            .underline()
            .small()
            .selected_index(active)
            .children(
                tabs.iter()
                    .map(|(title, _)| Tab::new().label(SharedString::from(title.clone()))),
            )
            .on_click(move |ix, _, cx| {
                let _ = picks.update(cx, |this, cx| {
                    this.tabs.insert(picked.clone(), *ix);
                    cx.notify();
                });
            });
        let body = tabs
            .get(active)
            .and_then(|(_, body)| body.as_ref())
            .map(|body| self.render_node(body, panel, cx));
        v_flex()
            .w_full()
            .gap_3()
            .child(bar)
            .children(body)
            .into_any_element()
    }

    fn render_media(
        &self,
        target: &Target,
        kind: MediaKind,
        description: &str,
        url: &str,
        (fit, size): (ImageFit, ImageSize),
        cx: &App,
    ) -> AnyElement {
        let id = self.id(&target.key());
        let (icon, what) = match kind {
            MediaKind::Image => (Lucide::Image, "Image"),
            MediaKind::Video => (Lucide::Film, "Video"),
            MediaKind::Audio => (Lucide::Music, "Audio"),
        };
        // What stands for media this app does not play or cannot load: a
        // chip that opens it in the browser.
        let name = if description.is_empty() {
            url.rsplit('/').next().unwrap_or(what).to_owned()
        } else {
            description.to_owned()
        };
        let chip = |id: SharedString, name: String, url: String, cx: &App| {
            let chip = h_flex()
                .gap_2()
                .px_2()
                .py_1()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .text_sm()
                .child(Icon::new(icon).small())
                .child(name);
            // Only a link the browser takes is one to open.
            if url.starts_with("https://") || url.starts_with("http://") {
                Link::new(id).href(url).child(chip).into_any_element()
            } else {
                chip.text_color(cx.theme().muted_foreground)
                    .into_any_element()
            }
        };
        if kind != MediaKind::Image {
            return h_flex()
                .child(chip(id, name, url.to_owned(), cx))
                .into_any_element();
        }
        let picture = if let Some(Some(picture)) = self.pictures.get(url) {
            img(Arc::clone(picture))
        } else {
            // Not a `data:` URL: GPUI loads it, and a chip stands in where
            // it cannot.
            let (id, name, theme) = (id.clone(), name.clone(), cx.theme());
            let (border, radius, muted) = (theme.border, theme.radius, theme.muted_foreground);
            img(SharedString::from(url.to_owned())).with_fallback(move || {
                h_flex()
                    .id(id.clone())
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(radius)
                    .border_1()
                    .border_color(border)
                    .text_sm()
                    .text_color(muted)
                    .child(Icon::new(Lucide::ImageOff).small())
                    .child(name.clone())
                    .into_any_element()
            })
        };
        let picture = picture.object_fit(match fit {
            ImageFit::Contain => ObjectFit::Contain,
            ImageFit::Cover => ObjectFit::Cover,
            ImageFit::Fill => ObjectFit::Fill,
            ImageFit::None => ObjectFit::None,
            ImageFit::ScaleDown => ObjectFit::ScaleDown,
        });
        let radius = cx.theme().radius;
        let frame = div()
            .id(id)
            .test_support()
            .aria_label(name)
            .flex_shrink_0()
            .overflow_hidden();
        let frame = match size {
            ImageSize::Icon => frame.size_5(),
            ImageSize::Avatar => frame.size_10().rounded_full(),
            ImageSize::SmallFeature => frame.w(rems(8.)).h(rems(5.)).rounded(radius),
            ImageSize::MediumFeature => frame.w(rems(16.)).h(rems(10.)).rounded(radius),
            ImageSize::LargeFeature => frame.w(rems(28.)).h(rems(16.)).rounded(radius),
            ImageSize::Header => frame.w_full().h(rems(10.)).rounded(radius),
        };
        frame.child(picture.size_full()).into_any_element()
    }

    /// The content of the modal `key`, for its dialog.
    fn render_modal(&self, key: &str, panel: &Handle, cx: &App) -> Option<AnyElement> {
        let content = modal_content(&self.tree.as_ref()?.root, key)?;
        Some(self.render_node(content, panel, cx))
    }
}

/// `control` under its `label`, if it has one.
fn labelled(label: &str, control: AnyElement, cx: &App) -> AnyElement {
    if label.is_empty() {
        return control;
    }
    v_flex()
        .w_full()
        .gap_1p5()
        .child(
            div()
                .text_sm()
                .font_medium()
                .text_color(cx.theme().foreground)
                .child(label.to_owned()),
        )
        .child(control)
        .into_any_element()
}

/// `control` over the messages of its checks that fail, and over why what
/// it shows is `invalid`, if it is.
fn with_failures(
    control: AnyElement,
    failures: &[Failure],
    invalid: Option<&'static str>,
    cx: &App,
) -> AnyElement {
    if failures.is_empty() && invalid.is_none() {
        return control;
    }
    let (danger, warning) = (cx.theme().danger, cx.theme().warning);
    let message = |text: String, blocks: bool| {
        div()
            .text_xs()
            .text_color(if blocks { danger } else { warning })
            .child(text)
    };
    v_flex()
        .w_full()
        .gap_1()
        .child(control)
        .children(invalid.map(|why| message(why.to_owned(), true)))
        .children(
            failures
                .iter()
                .map(|failure| message(failure.message.clone(), failure.blocks)),
        )
        .into_any_element()
}

/// The content of the modal whose key is `key`, under `node`.
fn modal_content<'a>(node: &'a Node, key: &str) -> Option<&'a Node> {
    match node {
        Node::Modal {
            target, content, ..
        } if target.key() == key => content.as_deref(),
        Node::Modal { content, .. } => content.as_deref().and_then(|node| modal_content(node, key)),
        Node::Row { children, .. } | Node::Column { children, .. } => children
            .iter()
            .find_map(|child| modal_content(&child.node, key)),
        Node::Card(child) => modal_content(child, key),
        Node::Tabs { tabs, .. } => tabs
            .iter()
            .filter_map(|(_, body)| body.as_ref())
            .find_map(|body| modal_content(body, key)),
        _ => None,
    }
}

/// What a modal's dialog holds: the modal's content, drawn by its panel,
/// as it stands each time the panel changes.
struct ModalBody {
    panel: Handle,
    key: String,
    _panel_changed: Option<Subscription>,
}

impl Render for ModalBody {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let content = self
            .panel
            .upgrade()
            .and_then(|panel| panel.read(cx).render_modal(&self.key, &self.panel, cx));
        v_flex()
            .id(SharedString::from(format!("surface-modal-{}", self.key)))
            .test_support()
            .w_full()
            .children(content)
    }
}

/// Opens the dialog of modal `key` of `panel`, titled as its trigger.
fn open_modal(panel: Handle, key: String, title: SharedString, window: &mut Window, cx: &mut App) {
    let body = cx.new(|cx| ModalBody {
        _panel_changed: panel
            .upgrade()
            .map(|panel| cx.observe(&panel, |_, _, cx| cx.notify())),
        panel,
        key,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let body = body.clone();
        dialog
            .title(title.clone())
            .content(move |content, _, _| content.child(body.clone()))
    });
}

impl Render for SurfacePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let panel = cx.entity().downgrade();
        let root = self
            .tree
            .as_ref()
            .map(|tree| self.render_node(&tree.root, &panel, cx));
        let stale = self.stale.then(|| {
            div()
                // A component's ID has no hyphen, so none is `is-stale`.
                .id(self.id("is-stale"))
                .test_support()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Rho no longer has this form, so it takes no more answers from it.")
        });
        v_flex()
            .id(SharedString::from(format!("surface-{}", self.surface)))
            .test_support()
            .aria_label(self.surface.clone())
            .w_full()
            .gap_2()
            .children(root)
            .children(stale)
    }
}
