//! One surface read out of the A2UI processor: what to draw, with what
//! each control writes to or runs.
//!
//! The processor holds components by catalog name with untyped properties.
//! [`build`] reads them once into [`Node`]s, so a view matches on types and
//! never looks a property up by name. A component this module does not
//! know draws as a placeholder.

use std::collections::BTreeMap;

use a2ui::{
    resolve_path, Action, ChildRef, Expr, MessageProcessor, Pointer, Property, Resolved,
    ResolvedComponent, Scope, Severity,
};
use serde_json::Value;

use super::field::FieldKind;

/// How many components one surface draws. A template over a long list
/// draws a component per item, and a surface that large is cut off here.
const MAX_NODES: usize = 2_000;

/// How deep components nest before the rest is cut off.
const MAX_DEPTH: usize = 64;

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
    /// The component's ID.
    pub fn component(&self) -> &str {
        &self.component
    }

    /// A name for this instance, unique within its surface: the
    /// component's ID, with the item's place for one a template repeats.
    pub fn key(&self) -> String {
        match self.scope.index() {
            Some(_) => format!("{}@{}", self.component, self.scope.path()),
            None => self.component.clone(),
        }
    }
}

/// How a button looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonVariant {
    /// An ordinary button.
    Default,
    /// The main action.
    Primary,
    /// Text alone, as a link.
    Borderless,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFit {
    /// All of it shows, in proportion.
    Contain,
    /// It covers the box, in proportion, cut at the edges.
    Cover,
    /// It is stretched to the box.
    Fill,
    /// Its own size.
    None,
    /// As [`Self::Contain`], never larger than its own size.
    ScaleDown,
}

/// How large an image is meant to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageSize {
    /// As small as an icon.
    Icon,
    /// A small round picture of a person.
    Avatar,
    /// A small picture.
    SmallFeature,
    /// A picture of ordinary size.
    MediumFeature,
    /// A large picture.
    LargeFeature,
    /// As wide as its container, at its top.
    Header,
}

/// Where a layout puts its children along its direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Justify {
    /// At the start.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    /// At the start.
    Start,
    /// In the middle.
    Center,
    /// At the end.
    End,
    /// Stretched to fill.
    Stretch,
}

/// A check that failed, as the surface shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What to show.
    pub message: String,
    /// Whether it makes the component invalid, not only a warning.
    pub blocks: bool,
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

/// A child of a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Child {
    /// The child.
    pub node: Node,
    /// Its share of the room left over along the layout's direction, as
    /// CSS `flex-grow`.
    pub weight: Option<f64>,
}

/// Something to draw. An input whose value is not bound to the data model
/// has no `binding`: nothing would keep what the user entered, so it
/// cannot be edited.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// Markdown text.
    Text {
        /// Which component this is.
        target: Target,
        /// The Markdown.
        text: String,
        /// Whether it is a caption: smaller and dimmer.
        caption: bool,
    },
    /// An icon, by its A2UI name. A name that is not a string, as an SVG
    /// path, is empty.
    Icon(String),
    /// A picture, a video or a sound.
    Media {
        /// Which component this is.
        target: Target,
        /// Which of the three.
        kind: MediaKind,
        /// What it shows, in words.
        description: String,
        /// Where it is.
        url: String,
        /// How an image fills its box.
        fit: ImageFit,
        /// How large an image shows.
        size: ImageSize,
    },
    /// A rule between its neighbors.
    Divider {
        /// Whether it stands upright, between neighbors in a row.
        vertical: bool,
    },
    /// Children side by side.
    Row {
        /// The children, in order.
        children: Vec<Child>,
        /// Where they sit along the row.
        justify: Justify,
        /// Where they sit across it.
        align: Align,
    },
    /// Children one under another. A vertical `List` is one too.
    Column {
        /// The children, in order.
        children: Vec<Child>,
        /// Where they sit down the column.
        justify: Justify,
        /// Where they sit across it.
        align: Align,
    },
    /// A framed group.
    Card(Box<Node>),
    /// Tabs, of which one shows.
    Tabs {
        /// Which component this is.
        target: Target,
        /// Each tab's title and what it shows.
        tabs: Vec<(String, Option<Node>)>,
    },
    /// A dialog its trigger opens.
    Modal {
        /// Which component this is.
        target: Target,
        /// What the trigger says.
        label: String,
        /// The trigger's icon, if it has one.
        icon: Option<String>,
        /// What the dialog shows.
        content: Option<Box<Node>>,
    },
    /// A button.
    Button {
        /// Which component this is.
        target: Target,
        /// What it says.
        label: String,
        /// Its icon, if it has one.
        icon: Option<String>,
        /// How it looks.
        variant: ButtonVariant,
        /// What pressing it runs; `None` while one of its checks fails,
        /// or when it has no action.
        action: Option<Action>,
        /// Its checks that fail.
        failures: Vec<Failure>,
    },
    /// A checkbox.
    CheckBox {
        /// Which component this is.
        target: Target,
        /// What it says.
        label: String,
        /// Whether it is checked.
        checked: bool,
        /// Where its value is kept.
        binding: Option<Pointer>,
        /// Its checks that fail.
        failures: Vec<Failure>,
    },
    /// A text or date field.
    Field {
        /// Which component this is.
        target: Target,
        /// What it asks for.
        label: String,
        /// Its value in the data model.
        value: String,
        /// What it shows while empty.
        placeholder: String,
        /// What it takes.
        kind: FieldKind,
        /// Where its value is kept.
        binding: Option<Pointer>,
        /// Its checks that fail.
        failures: Vec<Failure>,
    },
    /// A choice among options.
    Choices {
        /// Which component this is.
        target: Target,
        /// What it asks for.
        label: String,
        /// The options, in order.
        options: Vec<ChoiceOption>,
        /// Whether more than one can be chosen.
        multiple: bool,
        /// Whether the options show as chips, not as a list.
        chips: bool,
        /// Where the chosen values are kept, as a list.
        binding: Option<Pointer>,
        /// Its checks that fail.
        failures: Vec<Failure>,
    },
    /// A slider.
    Slider {
        /// Which component this is.
        target: Target,
        /// What it sets.
        label: String,
        /// Its value, within its range.
        value: f64,
        /// The least it can be.
        min: f64,
        /// The most it can be.
        max: f64,
        /// How far one step moves it.
        step: f64,
        /// Where its value is kept.
        binding: Option<Pointer>,
        /// Its checks that fail.
        failures: Vec<Failure>,
    },
    /// A component this client cannot draw, by its catalog name, or the
    /// place where a surface too large to draw was cut off.
    Placeholder(String),
}

/// Reads `surface` out of `processor`, or `None` when it has no such
/// surface or the surface has no `root` yet.
pub(super) fn build(processor: &MessageProcessor, surface: &str) -> Option<SurfaceTree> {
    let mut builder = Builder {
        processor,
        surface,
        nodes: 0,
    };
    let root = ChildRef {
        id: "root".to_owned(),
        scope: Scope::root(),
    };
    processor.resolve(surface, &root.id, &root.scope)?;
    Some(SurfaceTree {
        root: builder.node(&root, 0).node,
    })
}

struct Builder<'a> {
    processor: &'a MessageProcessor,
    surface: &'a str,
    nodes: usize,
}

/// A resolved component's properties, read by name.
struct Properties<'a>(&'a BTreeMap<String, Resolved>);

impl<'a> Properties<'a> {
    fn text(&self, name: &str) -> String {
        self.0
            .get(name)
            .and_then(Resolved::text)
            .unwrap_or_default()
    }

    fn literal(&self, name: &str) -> Option<&'a str> {
        self.0.get(name)?.value()?.as_str()
    }

    fn number(&self, name: &str) -> Option<f64> {
        self.0.get(name).and_then(Resolved::number)
    }

    fn flag(&self, name: &str) -> bool {
        self.0
            .get(name)
            .and_then(Resolved::boolean)
            .unwrap_or(false)
    }

    fn child(&self, name: &str) -> Option<&'a ChildRef> {
        match self.0.get(name)? {
            Resolved::Child(child) => Some(child),
            _ => None,
        }
    }

    fn children(&self, name: &str) -> &'a [ChildRef] {
        match self.0.get(name) {
            Some(Resolved::Children(children)) => children,
            _ => &[],
        }
    }

    fn justify(&self) -> Justify {
        match self.literal("justify") {
            Some("center") => Justify::Center,
            Some("end") => Justify::End,
            Some("spaceBetween") => Justify::SpaceBetween,
            Some("spaceAround") => Justify::SpaceAround,
            Some("spaceEvenly") => Justify::SpaceEvenly,
            Some("stretch") => Justify::Stretch,
            _ => Justify::Start,
        }
    }

    fn align(&self) -> Align {
        match self.literal("align") {
            Some("start") => Align::Start,
            Some("center") => Align::Center,
            Some("end") => Align::End,
            _ => Align::Stretch,
        }
    }
}

impl Builder<'_> {
    fn node(&mut self, child: &ChildRef, depth: usize) -> Child {
        if self.nodes == MAX_NODES || depth == MAX_DEPTH {
            return Child {
                node: Node::Placeholder("more".to_owned()),
                weight: None,
            };
        }
        self.nodes += 1;
        let Some(resolved) = self
            .processor
            .resolve(self.surface, &child.id, &child.scope)
        else {
            return Child {
                node: Node::Placeholder(child.id.clone()),
                weight: None,
            };
        };
        let target = Target {
            component: child.id.clone(),
            scope: child.scope.clone(),
        };
        let properties = Properties(&resolved.properties);
        let weight = properties.number("weight").filter(|weight| *weight > 0.0);
        let node = self.component(target, &resolved, &properties, depth);
        Child { node, weight }
    }

    fn component(
        &mut self,
        target: Target,
        resolved: &ResolvedComponent,
        properties: &Properties<'_>,
        depth: usize,
    ) -> Node {
        match resolved.kind.as_str() {
            "Text" => Node::Text {
                target,
                text: properties.text("text"),
                caption: properties.literal("variant") == Some("caption"),
            },
            "Icon" => Node::Icon(properties.literal("name").unwrap_or_default().to_owned()),
            "Image" => media(target, MediaKind::Image, properties),
            "Video" => media(target, MediaKind::Video, properties),
            "AudioPlayer" => media(target, MediaKind::Audio, properties),
            "Divider" => Node::Divider {
                vertical: properties.literal("axis") == Some("vertical"),
            },
            "Row" => Node::Row {
                children: self.nodes_of(properties.children("children"), depth),
                justify: properties.justify(),
                align: properties.align(),
            },
            "Column" => Node::Column {
                children: self.nodes_of(properties.children("children"), depth),
                justify: properties.justify(),
                align: properties.align(),
            },
            "List" => {
                let children = self.nodes_of(properties.children("children"), depth);
                let (justify, align) = (Justify::Start, properties.align());
                if properties.literal("direction") == Some("horizontal") {
                    Node::Row {
                        children,
                        justify,
                        align,
                    }
                } else {
                    Node::Column {
                        children,
                        justify,
                        align,
                    }
                }
            }
            "Card" => match properties.child("child") {
                Some(child) => Node::Card(Box::new(self.node(child, depth + 1).node)),
                None => Node::Placeholder(resolved.kind.clone()),
            },
            "Tabs" => Node::Tabs {
                target,
                tabs: self.tabs(resolved, depth),
            },
            "Modal" => {
                let (label, icon) = properties
                    .child("trigger")
                    .map(|trigger| self.label(trigger, depth + 1))
                    .unwrap_or_default();
                let content = properties
                    .child("content")
                    .map(|content| Box::new(self.node(content, depth + 1).node));
                Node::Modal {
                    target,
                    label,
                    icon,
                    content,
                }
            }
            _ => self.control(target, resolved, properties, depth),
        }
    }

    /// A component the user operates, or a placeholder for one this
    /// module does not know.
    fn control(
        &mut self,
        target: Target,
        resolved: &ResolvedComponent,
        properties: &Properties<'_>,
        depth: usize,
    ) -> Node {
        let failures = failures(resolved);
        match resolved.kind.as_str() {
            "Button" => {
                let (label, icon) = properties
                    .child("child")
                    .map(|child| self.label(child, depth + 1))
                    .unwrap_or_default();
                let action = match resolved.properties.get("action") {
                    Some(Resolved::Action(action)) if !blocked(&failures) => Some(action.clone()),
                    _ => None,
                };
                Node::Button {
                    target,
                    label,
                    icon,
                    variant: match properties.literal("variant") {
                        Some("primary") => ButtonVariant::Primary,
                        Some("borderless") => ButtonVariant::Borderless,
                        _ => ButtonVariant::Default,
                    },
                    action,
                    failures,
                }
            }
            "CheckBox" => Node::CheckBox {
                binding: self.binding(&target, "value"),
                target,
                label: properties.text("label"),
                checked: properties.flag("value"),
                failures,
            },
            "TextField" => {
                let kind = match properties.literal("variant") {
                    Some("longText") => FieldKind::Long,
                    Some("number") => FieldKind::Number,
                    Some("obscured") => FieldKind::Obscured,
                    _ => FieldKind::Short,
                };
                self.field(target, properties, kind, failures)
            }
            "DateTimeInput" => {
                let kind = FieldKind::DateTime {
                    date: properties.flag("enableDate"),
                    time: properties.flag("enableTime"),
                };
                self.field(target, properties, kind, failures)
            }
            "ChoicePicker" => self.choices(target, properties, failures),
            "Slider" => {
                let min = properties.number("min").unwrap_or(0.0);
                let max = properties.number("max").unwrap_or(min).max(min);
                let step = match properties.number("steps") {
                    Some(steps) if steps >= 1.0 => (max - min) / steps,
                    _ if max - min > 20.0 => (max - min) / 20.0,
                    _ => 1.0,
                };
                Node::Slider {
                    binding: self.binding(&target, "value"),
                    target,
                    label: properties.text("label"),
                    value: properties.number("value").unwrap_or(min).clamp(min, max),
                    min,
                    max,
                    step,
                    failures,
                }
            }
            _ => Node::Placeholder(resolved.kind.clone()),
        }
    }

    fn nodes_of(&mut self, children: &[ChildRef], depth: usize) -> Vec<Child> {
        children
            .iter()
            .map(|child| self.node(child, depth + 1))
            .collect()
    }

    fn tabs(&mut self, resolved: &ResolvedComponent, depth: usize) -> Vec<(String, Option<Node>)> {
        let Some(Resolved::List(tabs)) = resolved.properties.get("tabs") else {
            return Vec::new();
        };
        tabs.iter()
            .filter_map(|tab| match tab {
                Resolved::Object(tab) => {
                    let tab = Properties(tab);
                    let body = tab
                        .child("child")
                        .map(|child| self.node(child, depth + 1).node);
                    Some((tab.text("title"), body))
                }
                _ => None,
            })
            .collect()
    }

    fn field(
        &mut self,
        target: Target,
        properties: &Properties<'_>,
        kind: FieldKind,
        failures: Vec<Failure>,
    ) -> Node {
        Node::Field {
            binding: self.binding(&target, "value"),
            target,
            label: properties.text("label"),
            value: properties.text("value"),
            placeholder: properties.text("placeholder"),
            kind,
            failures,
        }
    }

    fn choices(
        &mut self,
        target: Target,
        properties: &Properties<'_>,
        failures: Vec<Failure>,
    ) -> Node {
        let selected: Vec<&str> = properties
            .0
            .get("value")
            .and_then(Resolved::value)
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let options = properties
            .0
            .get("options")
            .and_then(Resolved::value)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|option| {
                let value = option.get("value")?.as_str()?;
                let label = match option.get("label") {
                    Some(Value::String(label)) => label.clone(),
                    _ => value.to_owned(),
                };
                Some(ChoiceOption {
                    label,
                    value: value.to_owned(),
                    selected: selected.contains(&value),
                })
            })
            .collect();
        Node::Choices {
            binding: self.binding(&target, "value"),
            target,
            label: properties.text("label"),
            options,
            multiple: properties.literal("variant") == Some("multipleSelection"),
            chips: properties.literal("displayStyle") == Some("chips"),
            failures,
        }
    }

    /// Where in the data model `property` of `target` is bound, for a
    /// two-way binding to write what the user enters. A literal or a
    /// computed value has no such place.
    fn binding(&self, target: &Target, property: &str) -> Option<Pointer> {
        let component = self
            .processor
            .surface(self.surface)?
            .component(&target.component)?;
        let Property::Value(Expr::Path(path)) = component.properties().get(property)? else {
            return None;
        };
        Pointer::parse(&resolve_path(path, Some(target.scope.path()))).ok()
    }

    /// What a button or a modal's trigger says: the text of the component
    /// at `child` and of everything under it, on one line, and the first
    /// icon among them.
    fn label(&mut self, child: &ChildRef, depth: usize) -> (String, Option<String>) {
        let mut parts = Vec::new();
        let mut icon = None;
        self.label_parts(child, depth, &mut parts, &mut icon);
        (parts.join(" "), icon)
    }

    fn label_parts(
        &mut self,
        child: &ChildRef,
        depth: usize,
        parts: &mut Vec<String>,
        icon: &mut Option<String>,
    ) {
        if self.nodes == MAX_NODES || depth == MAX_DEPTH {
            return;
        }
        self.nodes += 1;
        let Some(resolved) = self
            .processor
            .resolve(self.surface, &child.id, &child.scope)
        else {
            return;
        };
        let properties = Properties(&resolved.properties);
        match resolved.kind.as_str() {
            "Text" => parts.push(properties.text("text")),
            "Icon" => {
                if icon.is_none() {
                    *icon = properties.literal("name").map(str::to_owned);
                }
            }
            _ => {
                for property in resolved.properties.values() {
                    match property {
                        Resolved::Child(child) => self.label_parts(child, depth + 1, parts, icon),
                        Resolved::Children(children) => {
                            for child in children {
                                self.label_parts(child, depth + 1, parts, icon);
                            }
                        }
                        Resolved::Value(_)
                        | Resolved::Action(_)
                        | Resolved::List(_)
                        | Resolved::Object(_) => {}
                    }
                }
            }
        }
    }
}

fn media(target: Target, kind: MediaKind, properties: &Properties<'_>) -> Node {
    Node::Media {
        target,
        kind,
        description: properties.text("description"),
        url: properties.text("url"),
        fit: match properties.literal("fit") {
            Some("contain") => ImageFit::Contain,
            Some("cover") => ImageFit::Cover,
            Some("none") => ImageFit::None,
            Some("scaleDown") => ImageFit::ScaleDown,
            _ => ImageFit::Fill,
        },
        size: match properties.literal("variant") {
            Some("icon") => ImageSize::Icon,
            Some("avatar") => ImageSize::Avatar,
            Some("smallFeature") => ImageSize::SmallFeature,
            Some("largeFeature") => ImageSize::LargeFeature,
            Some("header") => ImageSize::Header,
            _ => ImageSize::MediumFeature,
        },
    }
}

fn failures(resolved: &ResolvedComponent) -> Vec<Failure> {
    resolved
        .checks
        .iter()
        .flat_map(a2ui::Checks::failures)
        .map(|failure| Failure {
            message: failure
                .message
                .clone()
                .or_else(|| failure.code.clone())
                .unwrap_or_else(|| "Invalid".to_owned()),
            blocks: failure.severity == Severity::Error,
        })
        .collect()
}

fn blocked(failures: &[Failure]) -> bool {
    failures.iter().any(|failure| failure.blocks)
}
