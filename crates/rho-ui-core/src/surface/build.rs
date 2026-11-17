//! Reads a surface out of the A2UI processor into a [`SurfaceTree`]. A
//! component this module does not know reads as a placeholder.

use std::collections::BTreeMap;

use a2ui::{
    resolve_path, ChildRef, Expr, MessageProcessor, Pointer, Property, Resolved, ResolvedComponent,
    Scope,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;

use super::{
    field::FieldKind,
    tree::{
        Axis, Button, Card, CheckBox, Child, ChoiceOption, ChoiceStyle, Choices, Failure, Field,
        Icon, Input, Justify, Label, Media, MediaKind, Modal, Node, Placeholder, Slider,
        SliderRange, Stack, SurfaceTree, Tab, Tabs, Target, Text,
    },
};

/// How many components one surface draws. A template over a long list
/// draws a component per item, and a surface that large is cut off here.
const MAX_NODES: usize = 2_000;

/// How deep components nest before the rest is cut off.
const MAX_DEPTH: usize = 64;

/// Reads `surface` out of `processor`, or `None` when it has no such
/// surface or the surface has no `root` yet.
pub(super) fn build(processor: &MessageProcessor, surface: &str) -> Option<SurfaceTree> {
    let root = ChildRef {
        id: "root".to_owned(),
        scope: Scope::root(),
    };
    processor.resolve(surface, &root.id, &root.scope)?;
    let mut builder = Builder {
        processor,
        surface,
        nodes: 0,
    };
    Some(SurfaceTree {
        root: builder.node(&root, 0).node,
    })
}

struct Builder<'a> {
    processor: &'a MessageProcessor,
    surface: &'a str,
    /// How many nodes it has read.
    nodes: usize,
}

/// A resolved component's properties, read by name.
struct Properties<'a>(&'a BTreeMap<String, Resolved>);

impl<'a> Properties<'a> {
    fn value(&self, name: &str) -> Option<&'a Value> {
        self.0.get(name)?.value()
    }

    fn text(&self, name: &str) -> String {
        self.0
            .get(name)
            .and_then(Resolved::text)
            .unwrap_or_default()
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

    /// An enumerated property, such as a button's `variant`; `None` when
    /// it is missing or names a value this client does not know.
    fn choice<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        T::deserialize(self.value(name)?).ok()
    }

    /// An enumerated property, or its default.
    fn choice_or_default<T: DeserializeOwned + Default>(&self, name: &str) -> T {
        self.choice(name).unwrap_or_default()
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
}

impl Builder<'_> {
    /// Whether a node at `depth` is past what one surface draws.
    fn is_past_limits(&self, depth: usize) -> bool {
        self.nodes >= MAX_NODES || depth >= MAX_DEPTH
    }

    fn node(&mut self, child: &ChildRef, depth: usize) -> Child {
        let truncated = |node| Child { node, weight: None };
        if self.is_past_limits(depth) {
            return truncated(Node::Placeholder(Placeholder::Truncated));
        }
        self.nodes += 1;
        let Some(resolved) = self
            .processor
            .resolve(self.surface, &child.id, &child.scope)
        else {
            return truncated(Node::Placeholder(Placeholder::Missing(child.id.clone())));
        };
        let target = Target::in_scope(child.id.clone(), child.scope.clone());
        let properties = Properties(&resolved.properties);
        let weight = properties.number("weight").filter(|weight| *weight > 0.0);
        Child {
            node: self.component(target, &resolved, &properties, depth),
            weight,
        }
    }

    /// The nodes of `children`, which stop at one placeholder where the
    /// surface is cut off.
    fn nodes_of(&mut self, children: &[ChildRef], depth: usize) -> Vec<Child> {
        let mut nodes = Vec::new();
        for child in children {
            let past_limits = self.is_past_limits(depth + 1);
            nodes.push(self.node(child, depth + 1));
            if past_limits {
                break;
            }
        }
        nodes
    }

    fn component(
        &mut self,
        target: Target,
        resolved: &ResolvedComponent,
        properties: &Properties<'_>,
        depth: usize,
    ) -> Node {
        match resolved.kind.as_str() {
            "Text" => Node::Text(Text {
                target,
                text: properties.text("text"),
                style: properties.choice_or_default("variant"),
            }),
            "Icon" => Node::Icon(Icon {
                name: properties
                    .value("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }),
            "Image" => media(target, MediaKind::Image, properties),
            "Video" => media(target, MediaKind::Video, properties),
            "AudioPlayer" => media(target, MediaKind::Audio, properties),
            "Divider" => Node::Divider(properties.choice("axis").unwrap_or(Axis::Horizontal)),
            "Row" => self.stack(
                Axis::Horizontal,
                properties.choice_or_default("justify"),
                properties,
                depth,
            ),
            "Column" => self.stack(
                Axis::Vertical,
                properties.choice_or_default("justify"),
                properties,
                depth,
            ),
            // A list lines its items up from the start.
            "List" => self.stack(
                properties.choice("direction").unwrap_or(Axis::Vertical),
                Justify::Start,
                properties,
                depth,
            ),
            "Card" => match self.child_at(properties, "child", depth) {
                Some(child) => Node::Card(Card {
                    target,
                    child: Box::new(child),
                }),
                None => Node::Placeholder(Placeholder::Unknown(resolved.kind.clone())),
            },
            "Tabs" => Node::Tabs(Tabs {
                target,
                tabs: self.tabs(resolved, depth),
            }),
            "Modal" => self.modal(target, resolved, properties, depth),
            "Button" => self.button(target, resolved, properties, depth),
            "CheckBox" => Node::CheckBox(CheckBox {
                input: self.input(target, resolved, properties),
                checked: properties.flag("value"),
            }),
            "TextField" => {
                let kind = match properties.choice_or_default("variant") {
                    TextFieldVariant::ShortText => FieldKind::Short,
                    TextFieldVariant::LongText => FieldKind::Long,
                    TextFieldVariant::Number => FieldKind::Number,
                    TextFieldVariant::Obscured => FieldKind::Obscured,
                };
                self.field(target, resolved, properties, kind)
            }
            "DateTimeInput" => {
                let kind = match (properties.flag("enableDate"), properties.flag("enableTime")) {
                    (true, false) => FieldKind::Date,
                    (false, true) => FieldKind::Time,
                    // One that enables neither takes both.
                    (true, true) | (false, false) => FieldKind::DateTime,
                };
                self.field(target, resolved, properties, kind)
            }
            "ChoicePicker" => self.choices(target, resolved, properties),
            "Slider" => self.slider(target, resolved, properties),
            kind => Node::Placeholder(Placeholder::Unknown(kind.to_owned())),
        }
    }

    /// The node of the child property `name` names, if it names one.
    fn child_at(&mut self, properties: &Properties<'_>, name: &str, depth: usize) -> Option<Node> {
        let child = properties.child(name)?;
        Some(self.node(child, depth + 1).node)
    }

    fn stack(
        &mut self,
        axis: Axis,
        justify: Justify,
        properties: &Properties<'_>,
        depth: usize,
    ) -> Node {
        Node::Stack(Stack {
            axis,
            children: self.nodes_of(properties.children("children"), depth),
            justify,
            align: properties.choice_or_default("align"),
        })
    }

    fn modal(
        &mut self,
        target: Target,
        resolved: &ResolvedComponent,
        properties: &Properties<'_>,
        depth: usize,
    ) -> Node {
        let trigger = self
            .child_at(properties, "trigger", depth)
            .as_ref()
            .map(Label::of)
            .unwrap_or_default();
        let content = self.child_at(properties, "content", depth).map(Box::new);
        Node::Modal(Modal {
            accessible_name: accessible_name(resolved, &trigger),
            target,
            trigger,
            content,
        })
    }

    fn button(
        &mut self,
        target: Target,
        resolved: &ResolvedComponent,
        properties: &Properties<'_>,
        depth: usize,
    ) -> Node {
        let label = self
            .child_at(properties, "child", depth)
            .as_ref()
            .map(Label::of)
            .unwrap_or_default();
        let failures = failures(resolved);
        let action = match resolved.properties.get("action") {
            Some(Resolved::Action(action)) if !failures.iter().any(Failure::blocks) => {
                Some(action.clone())
            }
            _ => None,
        };
        Node::Button(Button {
            accessible_name: accessible_name(resolved, &label),
            target,
            label,
            variant: properties.choice_or_default("variant"),
            action,
            failures,
        })
    }

    fn tabs(&mut self, resolved: &ResolvedComponent, depth: usize) -> Vec<Tab> {
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
                    Some(Tab {
                        title: tab.text("title"),
                        body,
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// What every input has: its label and name, binding and failures.
    fn input(
        &self,
        target: Target,
        resolved: &ResolvedComponent,
        properties: &Properties<'_>,
    ) -> Input {
        let label = properties.text("label");
        Input {
            accessible_name: accessible_name(
                resolved,
                &Label {
                    text: label.clone(),
                    icon: None,
                },
            ),
            binding: self.binding(&target, "value"),
            target,
            label,
            failures: failures(resolved),
        }
    }

    fn field(
        &self,
        target: Target,
        resolved: &ResolvedComponent,
        properties: &Properties<'_>,
        kind: FieldKind,
    ) -> Node {
        Node::Field(Field {
            input: self.input(target, resolved, properties),
            value: properties.text("value"),
            placeholder: properties.text("placeholder"),
            kind,
        })
    }

    fn choices(
        &self,
        target: Target,
        resolved: &ResolvedComponent,
        properties: &Properties<'_>,
    ) -> Node {
        let selected: Vec<&str> = properties
            .value("value")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        // The options' list is one value: rho-a2ui reads no property of an
        // object in it.
        let options = properties
            .value("options")
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
        let style = match (
            properties.choice_or_default("displayStyle"),
            properties.choice_or_default("variant"),
        ) {
            (DisplayStyle::Checkbox, ChoiceVariant::MutuallyExclusive) => ChoiceStyle::Radios,
            (DisplayStyle::Checkbox, ChoiceVariant::MultipleSelection) => ChoiceStyle::Checkboxes,
            (DisplayStyle::Chips, ChoiceVariant::MutuallyExclusive) => ChoiceStyle::OneChip,
            (DisplayStyle::Chips, ChoiceVariant::MultipleSelection) => ChoiceStyle::Chips,
        };
        Node::Choices(Choices {
            input: self.input(target, resolved, properties),
            options,
            style,
        })
    }

    fn slider(
        &self,
        target: Target,
        resolved: &ResolvedComponent,
        properties: &Properties<'_>,
    ) -> Node {
        let min = properties.number("min").unwrap_or(0.0);
        let max = properties.number("max").unwrap_or(min).max(min);
        let span = max - min;
        let step = match properties.number("steps") {
            Some(steps) if steps >= 1.0 && span > 0.0 => span / steps,
            _ if span > 20.0 => span / 20.0,
            _ => 1.0,
        };
        Node::Slider(Slider {
            input: self.input(target, resolved, properties),
            value: properties.number("value").unwrap_or(min).clamp(min, max),
            range: SliderRange { min, max, step },
        })
    }

    /// Where in the data model `property` of `target` is bound, for a
    /// two-way binding to write what the user enters. A literal or a
    /// computed value has no such place.
    ///
    /// rho-a2ui resolves a property to its value only, so this reads the
    /// component's raw property for the path.
    fn binding(&self, target: &Target, property: &str) -> Option<Pointer> {
        let component = self
            .processor
            .surface(self.surface)?
            .component(target.component())?;
        let Property::Value(Expr::Path(path)) = component.properties().get(property)? else {
            return None;
        };
        Pointer::parse(&resolve_path(path, Some(target.scope.path()))).ok()
    }
}

/// A `TextField`'s `variant`.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
enum TextFieldVariant {
    #[default]
    ShortText,
    LongText,
    Number,
    Obscured,
}

/// A `ChoicePicker`'s `variant`.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ChoiceVariant {
    #[default]
    MutuallyExclusive,
    MultipleSelection,
}

/// A `ChoicePicker`'s `displayStyle`.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
enum DisplayStyle {
    #[default]
    Checkbox,
    Chips,
}

fn media(target: Target, kind: MediaKind, properties: &Properties<'_>) -> Node {
    Node::Media(Media {
        target,
        kind,
        description: properties.text("description"),
        url: properties.text("url"),
        fit: properties.choice_or_default("fit"),
        size: properties.choice_or_default("variant"),
    })
}

/// What assistive technology calls an interactive component: its
/// `@accessibility` label, else what it shows, else its icon's name.
fn accessible_name(resolved: &ResolvedComponent, shown: &Label) -> String {
    resolved
        .accessibility
        .as_ref()
        .and_then(|accessibility| accessibility.label.clone())
        .filter(|label| !label.is_empty())
        .or_else(|| Some(shown.text.clone()).filter(|text| !text.is_empty()))
        .or_else(|| shown.icon.clone())
        .unwrap_or_default()
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
            severity: failure.severity,
        })
        .collect()
}
