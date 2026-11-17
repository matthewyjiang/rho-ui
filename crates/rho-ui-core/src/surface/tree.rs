//! One surface read out of the A2UI processor: what to draw, with what
//! each control writes to or runs.
//!
//! The processor holds components by catalog name with untyped properties.
//! [`super::build`] reads them once into [`Node`]s, so a view matches on
//! types and never looks a property up by name.

use a2ui::{Action, Pointer, Scope, Severity};
use serde::Deserialize;

use super::field::FieldKind;

/// A surface as it stands.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceTree {
    /// The component named `root`.
    pub root: Node,
}

/// A component instance: one component, in the scope it was resolved in.
/// A template draws one component once per item of its list, each in its
/// own scope.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    component: String,
    pub(crate) scope: Scope,
}

impl Target {
    /// The component `component` outside any template.
    pub fn new(component: impl Into<String>) -> Self {
        Self {
            component: component.into(),
            scope: Scope::root(),
        }
    }

    pub(crate) fn in_scope(component: String, scope: Scope) -> Self {
        Self { component, scope }
    }

    /// The component's ID.
    pub fn component(&self) -> &str {
        &self.component
    }

    /// The data path of the template item this instance draws, for one a
    /// template repeats.
    pub fn item(&self) -> Option<&str> {
        self.scope.index().map(|_| self.scope.path())
    }
}

/// Something to draw.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// Markdown text.
    Text(Text),
    /// An icon.
    Icon(Icon),
    /// A picture, a video or a sound.
    Media(Media),
    /// A rule between its neighbors, across `Axis::Horizontal` or upright.
    Divider(Axis),
    /// Children side by side or one under another.
    Stack(Stack),
    /// A framed group.
    Card(Card),
    /// Tabs, of which one shows.
    Tabs(Tabs),
    /// A dialog its trigger opens.
    Modal(Modal),
    /// A button.
    Button(Button),
    /// A checkbox.
    CheckBox(CheckBox),
    /// A text or date field.
    Field(Field),
    /// A choice among options.
    Choices(Choices),
    /// A slider.
    Slider(Slider),
    /// What stands where a component cannot be drawn.
    Placeholder(Placeholder),
}

/// Markdown text.
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    /// Which component this is.
    pub target: Target,
    /// The Markdown.
    pub text: String,
    /// How it looks.
    pub style: TextStyle,
}

/// How text looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextStyle {
    /// As a reply's text.
    #[default]
    Body,
    /// Smaller and dimmer.
    Caption,
}

/// An icon, by its A2UI name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Icon {
    /// Its name; `None` for one drawn from an SVG path, which this client
    /// does not draw.
    pub name: Option<String>,
}

/// A picture, a video or a sound.
#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    /// Which component this is.
    pub target: Target,
    /// Which of the three.
    pub kind: MediaKind,
    /// What it shows, in words.
    pub description: String,
    /// Where it is.
    pub url: String,
    /// How an image fills its box.
    pub fit: ImageFit,
    /// How large an image shows.
    pub size: ImageSize,
}

/// What a media component plays or shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    /// A picture.
    Image,
    /// A video.
    Video,
    /// A sound.
    Audio,
}

/// How an image fills its box, as CSS `object-fit` does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageFit {
    /// All of it shows, in proportion.
    Contain,
    /// It covers the box, in proportion, cut at the edges.
    Cover,
    /// It is stretched to the box.
    #[default]
    Fill,
    /// Its own size.
    None,
    /// As [`Self::Contain`], never larger than its own size.
    ScaleDown,
}

/// How large an image is meant to show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageSize {
    /// As small as an icon.
    Icon,
    /// A small round picture of a person.
    Avatar,
    /// A small picture.
    SmallFeature,
    /// A picture of ordinary size.
    #[default]
    MediumFeature,
    /// A large picture.
    LargeFeature,
    /// As wide as its container, at its top.
    Header,
}

/// Which way a layout or a divider runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Axis {
    /// Side by side.
    Horizontal,
    /// One under another.
    Vertical,
}

/// Children side by side or one under another: a `Row`, a `Column` or a
/// `List`.
#[derive(Clone, Debug, PartialEq)]
pub struct Stack {
    /// Which way it runs.
    pub axis: Axis,
    /// The children, in order.
    pub children: Vec<Child>,
    /// Where they sit along its axis.
    pub justify: Justify,
    /// Where they sit across it.
    pub align: Align,
}

/// Where a layout puts its children along its direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Justify {
    /// At the start.
    #[default]
    Start,
    /// In the middle.
    Center,
    /// At the end.
    End,
    /// Spread out, the first and last at the edges.
    SpaceBetween,
    /// Spread out, with half a gap at the edges.
    SpaceAround,
    /// Spread out, with a whole gap at the edges.
    SpaceEvenly,
    /// Stretched to fill.
    Stretch,
}

/// Where a layout puts its children across its direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Align {
    /// At the start.
    Start,
    /// In the middle.
    Center,
    /// At the end.
    End,
    /// Stretched to fill.
    #[default]
    Stretch,
}

/// A child of a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Child {
    /// The child.
    pub node: Node,
    /// Its share of the room left over along the layout's direction, as
    /// CSS `flex-grow`.
    pub weight: Option<f64>,
}

/// A framed group.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    /// Which component this is.
    pub target: Target,
    /// What it frames.
    pub child: Box<Node>,
}

/// Tabs, of which one shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Tabs {
    /// Which component this is.
    pub target: Target,
    /// The tabs, in order.
    pub tabs: Vec<Tab>,
}

/// One tab.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    /// What its handle says.
    pub title: String,
    /// What it shows.
    pub body: Option<Node>,
}

/// What a button or a modal's trigger shows: the text of its child and of
/// everything under it, on one line, and the first icon among them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label {
    /// The text.
    pub text: String,
    /// The icon's name, if it has one.
    pub icon: Option<String>,
}

/// A dialog its trigger opens.
#[derive(Clone, Debug, PartialEq)]
pub struct Modal {
    /// Which component this is.
    pub target: Target,
    /// What the trigger shows.
    pub trigger: Label,
    /// What assistive technology calls the trigger.
    pub accessible_name: String,
    /// What the dialog shows.
    pub content: Option<Box<Node>>,
}

/// A button.
#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    /// Which component this is.
    pub target: Target,
    /// What it shows.
    pub label: Label,
    /// What assistive technology calls it.
    pub accessible_name: String,
    /// How it looks.
    pub variant: ButtonVariant,
    /// What pressing it runs; `None` while one of its checks blocks it,
    /// or when it has no action.
    pub action: Option<Action>,
    /// Its checks that fail.
    pub failures: Vec<Failure>,
}

/// How a button looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ButtonVariant {
    /// An ordinary button.
    #[default]
    Default,
    /// The main action.
    Primary,
    /// Text alone, as a link.
    Borderless,
}

/// What an input that the user edits shares: what it is called, where its
/// value is kept, and its checks that fail. An input whose value is not
/// bound to the data model has no `binding`: nothing would keep what the
/// user entered, so it cannot be edited.
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    /// Which component this is.
    pub target: Target,
    /// What it asks for.
    pub label: String,
    /// What assistive technology calls it.
    pub accessible_name: String,
    /// Where its value is kept.
    pub binding: Option<Pointer>,
    /// Its checks that fail.
    pub failures: Vec<Failure>,
}

/// A checkbox.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckBox {
    /// What it is.
    pub input: Input,
    /// Whether it is checked.
    pub checked: bool,
}

/// A text or date field.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// What it is.
    pub input: Input,
    /// Its value in the data model.
    pub value: String,
    /// What it shows while empty.
    pub placeholder: String,
    /// What it takes.
    pub kind: FieldKind,
}

/// A choice among options.
#[derive(Clone, Debug, PartialEq)]
pub struct Choices {
    /// What it is; its value is the chosen options' values, as a list.
    pub input: Input,
    /// The options, in order.
    pub options: Vec<ChoiceOption>,
    /// How it draws them, and how many can be chosen.
    pub style: ChoiceStyle,
}

/// How a choice picker draws its options, and how many can be chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceStyle {
    /// One of a list.
    Radios,
    /// Any of a list.
    Checkboxes,
    /// One of a row of chips.
    OneChip,
    /// Any of a row of chips.
    Chips,
}

/// One option of a choice picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceOption {
    /// What the option says.
    pub label: String,
    /// What it adds to the picker's value.
    pub value: String,
    /// Whether it is chosen.
    pub selected: bool,
}

/// A slider.
#[derive(Clone, Debug, PartialEq)]
pub struct Slider {
    /// What it is.
    pub input: Input,
    /// Its value, within its range.
    pub value: f64,
    /// Where it can be set.
    pub range: SliderRange,
}

/// Where a slider can be set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SliderRange {
    /// The least it can be.
    pub min: f64,
    /// The most it can be, never less than `min`.
    pub max: f64,
    /// How far one step moves it, more than zero.
    pub step: f64,
}

/// A check that failed, as the surface shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What to show.
    pub message: String,
    /// How much it matters: an error makes the component invalid.
    pub severity: Severity,
}

impl Failure {
    /// Whether it makes the component invalid, not only a warning.
    pub fn blocks(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// What stands where a component cannot be drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placeholder {
    /// A component this client cannot draw, by its catalog name: one it
    /// does not know, or a card without its child.
    Unknown(String),
    /// A child that names no component, by the ID it names.
    Missing(String),
    /// Where a surface too large or too deep to draw was cut off.
    Truncated,
}

impl Node {
    /// The nodes this one holds, in drawing order: a layout's children, a
    /// card's child, each tab's body and a modal's content.
    pub fn children(&self) -> Vec<&Node> {
        match self {
            Self::Stack(stack) => stack.children.iter().map(|child| &child.node).collect(),
            Self::Card(card) => vec![&card.child],
            Self::Tabs(tabs) => tabs
                .tabs
                .iter()
                .filter_map(|tab| tab.body.as_ref())
                .collect(),
            Self::Modal(modal) => modal.content.as_deref().into_iter().collect(),
            Self::Text(_)
            | Self::Icon(_)
            | Self::Media(_)
            | Self::Divider(_)
            | Self::Button(_)
            | Self::CheckBox(_)
            | Self::Field(_)
            | Self::Choices(_)
            | Self::Slider(_)
            | Self::Placeholder(_) => Vec::new(),
        }
    }

    /// This node and every node under it, in drawing order.
    pub fn descendants(&self) -> impl Iterator<Item = &Node> {
        let mut stack = vec![self];
        std::iter::from_fn(move || {
            let node = stack.pop()?;
            stack.extend(node.children().into_iter().rev());
            Some(node)
        })
    }
}

impl Label {
    /// What `node` shows as a label: its text and the text of everything
    /// under it, on one line, and the first icon among them.
    pub(super) fn of(node: &Node) -> Self {
        let mut text = Vec::new();
        let mut icon = None;
        for node in node.descendants() {
            match node {
                Node::Text(shown) => text.push(shown.text.as_str()),
                Node::Icon(Icon { name }) => icon = icon.or_else(|| name.clone()),
                _ => {}
            }
        }
        Self {
            text: text.join(" "),
            icon,
        }
    }
}
