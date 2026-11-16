//! What a panel draws for each node of its tree.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _, Toggle, ToggleGroup, ToggleVariants as _},
    checkbox::Checkbox,
    date_picker::DatePicker,
    group_box::{GroupBox, GroupBoxVariants as _},
    h_flex,
    input::{Input, NumberInput, Textarea},
    radio::{Radio, RadioGroup},
    separator::Separator,
    slider::Slider as KitSlider,
    tab::{Tab, TabBar},
    text::TextView,
    time_field::TimeField,
    v_flex, ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _,
};
use rho_ui_core::surface::{
    Action, Align, Axis, Button as SurfaceButton, ButtonVariant, Card, CheckBox, Child,
    ChoiceStyle, Choices, Failure, Field, Justify, Node, Placeholder, Problem, Slider, Stack, Tabs,
    Target, Text, TextStyle,
};
use serde_json::Value;

use super::{
    controls::{number_value, FieldControl, MomentPicker, TextInput},
    emit,
    icons::lucide,
    Handle, Part, SurfaceEvent, SurfacePanel,
};

/// The widest a card grows, in rems: a form reads badly stretched over
/// the transcript's whole measure.
const CARD_MAX_REMS: f32 = 36.;

impl SurfacePanel {
    pub(super) fn render_node(&self, node: &Node, panel: &Handle, cx: &App) -> AnyElement {
        match node {
            Node::Text(text) => self.render_text(text, cx),
            Node::Icon(icon) => {
                Icon::new(icon.name.as_deref().map_or(Lucide::SquareDashed, lucide))
                    .into_any_element()
            }
            Node::Media(media) => self.render_media(media, cx),
            Node::Divider(Axis::Vertical) => div()
                .h_full()
                .min_h_4()
                .child(Separator::vertical())
                .into_any_element(),
            Node::Divider(Axis::Horizontal) => Separator::horizontal().into_any_element(),
            Node::Stack(stack) => self.render_stack(stack, panel, cx),
            Node::Card(card) => self.render_card(card, panel, cx),
            Node::Tabs(tabs) => self.render_tabs(tabs, panel, cx),
            Node::Modal(modal) => self.render_modal(modal, panel),
            Node::Button(button) => self.render_button(button, panel),
            Node::CheckBox(checkbox) => self.render_checkbox(checkbox, panel, cx),
            Node::Field(field) => self.render_field(field, cx),
            Node::Choices(choices) => self.render_choices(choices, panel, cx),
            Node::Slider(slider) => self.render_slider(slider, cx),
            Node::Placeholder(placeholder) => {
                let says = match placeholder {
                    Placeholder::Unknown(name) | Placeholder::Missing(name) => format!("[{name}]"),
                    Placeholder::Truncated => "[The rest is too large to show]".to_owned(),
                };
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(says)
                    .into_any_element()
            }
        }
    }

    fn render_text(&self, text: &Text, cx: &App) -> AnyElement {
        let view = TextView::markdown(self.id(Part::Control(&text.target)), text.text.clone())
            .selectable(true);
        match text.style {
            TextStyle::Body => view.into_any_element(),
            TextStyle::Caption => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(view)
                .into_any_element(),
        }
    }

    fn render_card(&self, card: &Card, panel: &Handle, cx: &App) -> AnyElement {
        GroupBox::new()
            .id(self.id(Part::Control(&card.target)))
            .outline()
            .max_w(rems(CARD_MAX_REMS))
            .child(self.render_node(&card.child, panel, cx))
            .into_any_element()
    }

    /// A row or a column of children.
    fn render_stack(&self, stack: &Stack, panel: &Handle, cx: &App) -> AnyElement {
        let layout = match stack.axis {
            Axis::Horizontal => h_flex().w_full().flex_wrap().gap_2(),
            Axis::Vertical => v_flex().w_full().gap_3(),
        };
        let layout = match stack.justify {
            Justify::Start | Justify::Stretch => layout,
            Justify::Center => layout.justify_center(),
            Justify::End => layout.justify_end(),
            Justify::SpaceBetween => layout.justify_between(),
            Justify::SpaceAround | Justify::SpaceEvenly => layout.justify_around(),
        };
        let layout = match (stack.align, stack.axis) {
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
                stack
                    .children
                    .iter()
                    .map(|child| self.render_child(child, stack.axis, panel, cx)),
            )
            .into_any_element()
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
                    Node::Field(_) | Node::Slider(_) | Node::Choices(_)
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

    fn render_tabs(&self, tabs: &Tabs, panel: &Handle, cx: &App) -> AnyElement {
        let active = self
            .tabs
            .get(&tabs.target)
            .copied()
            .unwrap_or(0)
            .min(tabs.tabs.len().saturating_sub(1));
        let (picks, picked) = (panel.clone(), tabs.target.clone());
        let bar = TabBar::new(self.id(Part::Control(&tabs.target)))
            .underline()
            .small()
            .selected_index(active)
            .children(
                tabs.tabs
                    .iter()
                    .map(|tab| Tab::new().label(SharedString::from(tab.title.clone()))),
            )
            .on_click(move |ix, _, cx| {
                let _ = picks.update(cx, |this, cx| {
                    this.tabs.insert(picked.clone(), *ix);
                    cx.notify();
                });
            });
        let body = tabs
            .tabs
            .get(active)
            .and_then(|tab| tab.body.as_ref())
            .map(|body| self.render_node(body, panel, cx));
        v_flex()
            .w_full()
            .gap_3()
            .child(bar)
            .children(body)
            .into_any_element()
    }

    fn render_button(&self, button: &SurfaceButton, panel: &Handle) -> AnyElement {
        let kit = Button::new(self.id(Part::Control(&button.target)))
            .small()
            .accessibility_label(button.accessible_name.clone())
            .when(!button.label.text.is_empty(), |kit| {
                kit.label(button.label.text.clone())
            })
            .when_some(button.label.icon.as_deref(), |kit, icon| {
                kit.icon(Icon::new(lucide(icon)))
            });
        let kit = match button.variant {
            ButtonVariant::Primary => kit.primary(),
            ButtonVariant::Default => kit.outline(),
            ButtonVariant::Borderless => kit.link(),
        };
        // A button whose check fails says why where the pointer is.
        let kit = match button.failures.first() {
            Some(failure) => kit.tooltip(failure.message.clone()),
            None => kit,
        };
        // An event waits while a field shows what it keeps out of the data
        // model, which would otherwise send the value before it.
        let action = button.action.as_ref().filter(|action| match action {
            Action::Event(_) => !self.has_problem(),
            Action::Call(_) => true,
        });
        let pressed = action.map(|action| (button.target.clone(), action.clone()));
        let panel = panel.clone();
        kit.disabled(pressed.is_none() || self.stale.is_some() || self.sending)
            .when_some(pressed, |kit, (target, action)| {
                kit.on_click(move |_, _, cx| {
                    let event = SurfaceEvent::Act {
                        target: target.clone(),
                        action: action.clone(),
                    };
                    emit(&panel, event, cx);
                })
            })
            .into_any_element()
    }

    fn render_checkbox(&self, checkbox: &CheckBox, panel: &Handle, cx: &App) -> AnyElement {
        let input = &checkbox.input;
        let (panel, writes) = (panel.clone(), input.binding.clone());
        let kit = Checkbox::new(self.id(Part::Control(&input.target)))
            .label(SharedString::from(input.label.clone()))
            .accessibility_label(input.accessible_name.clone())
            .checked(checkbox.checked)
            .disabled(input.binding.is_none() || self.stale.is_some())
            .on_click(move |checked, _, cx| {
                if let Some(binding) = writes.clone() {
                    let value = Value::Bool(*checked);
                    emit(&panel, SurfaceEvent::Write { binding, value }, cx);
                }
            });
        self.with_messages(
            &input.target,
            kit.into_any_element(),
            &input.failures,
            None,
            cx,
        )
    }

    fn render_field(&self, field: &Field, cx: &App) -> AnyElement {
        let input = &field.input;
        let Some(widget) = self.fields.get(&input.target) else {
            return div().into_any_element();
        };
        let disabled = !widget.is_bound() || self.stale.is_some();
        let name = SharedString::from(input.accessible_name.clone());
        let id = self.id(Part::Control(&input.target));
        let (control, problem) = match &widget.control {
            FieldControl::Text(text) => {
                let control = match &text.input {
                    TextInput::Line(state) => Input::new(state)
                        .id(id)
                        .aria_label(name)
                        .small()
                        .disabled(disabled)
                        .into_any_element(),
                    TextInput::Obscured(state) => Input::new(state)
                        .id(id)
                        .aria_label(name)
                        .small()
                        .disabled(disabled)
                        .mask_toggle()
                        .into_any_element(),
                    // Kit's number input takes no ID: what holds it does.
                    TextInput::Number(state) => div()
                        .id(id)
                        .test_support()
                        .child(
                            NumberInput::new(state)
                                .accessibility_label(name)
                                .small()
                                .disabled(disabled),
                        )
                        .into_any_element(),
                    TextInput::Long(state) => Textarea::new(state)
                        .aria_label(name)
                        .disabled(disabled)
                        .into_any_element(),
                };
                (control, text.problem)
            }
            // Kit's date picker and time field take no accessibility
            // label yet. A date or time takes a few characters: its
            // control keeps its own width instead of the field's.
            FieldControl::Moment(moment) => {
                let picker = match &moment.picker {
                    MomentPicker::Date(state) | MomentPicker::DateTime(state) => {
                        DatePicker::new(state)
                            .small()
                            .disabled(disabled)
                            .into_any_element()
                    }
                    MomentPicker::Time(state) => TimeField::new(state)
                        .small()
                        .disabled(disabled)
                        .into_any_element(),
                };
                (h_flex().child(picker).into_any_element(), None)
            }
        };
        labelled(
            &input.label,
            self.with_messages(&input.target, control, &input.failures, problem, cx),
            cx,
        )
    }

    fn render_choices(&self, choices: &Choices, panel: &Handle, cx: &App) -> AnyElement {
        let input = &choices.input;
        let target = &input.target;
        let options = &choices.options;
        let disabled = input.binding.is_none() || self.stale.is_some();
        let values: Vec<String> = options.iter().map(|option| option.value.clone()).collect();
        let chosen: Vec<bool> = options.iter().map(|option| option.selected).collect();
        // The picker's value is the chosen options' values, in the
        // options' order.
        let write = {
            let (panel, binding) = (panel.clone(), input.binding.clone());
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
        // Kit's radio group and toggle group take no accessibility label
        // of their own yet; each option is named by what it says.
        let picker = match choices.style {
            ChoiceStyle::Chips | ChoiceStyle::OneChip => {
                let group = ToggleGroup::new(self.id(Part::Control(target)))
                    .outline()
                    .small()
                    .disabled(disabled)
                    .children(options.iter().enumerate().map(|(ix, option)| {
                        Toggle::new(self.id(Part::Option(target, ix)))
                            .label(option.label.clone())
                            .checked(option.selected)
                    }))
                    .on_click(move |chosen, _, cx| write(chosen, cx));
                let group = match choices.style {
                    ChoiceStyle::OneChip => group.single(),
                    ChoiceStyle::Chips | ChoiceStyle::Radios | ChoiceStyle::Checkboxes => group,
                };
                h_flex().flex_wrap().child(group).into_any_element()
            }
            ChoiceStyle::Checkboxes => v_flex()
                .gap_2()
                .children(options.iter().enumerate().map(|(ix, option)| {
                    let (write, chosen) = (write.clone(), chosen.clone());
                    Checkbox::new(self.id(Part::Option(target, ix)))
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
            ChoiceStyle::Radios => {
                let count = options.len();
                RadioGroup::vertical(self.id(Part::Control(target)))
                    .disabled(disabled)
                    .selected_index(options.iter().position(|option| option.selected))
                    .children(options.iter().enumerate().map(|(ix, option)| {
                        Radio::new(self.id(Part::Option(target, ix)))
                            .label(SharedString::from(option.label.clone()))
                    }))
                    .on_click(move |picked, _, cx| {
                        let chosen: Vec<bool> = (0..count).map(|ix| ix == *picked).collect();
                        write(&chosen, cx);
                    })
                    .into_any_element()
            }
        };
        labelled(
            &input.label,
            self.with_messages(target, picker, &input.failures, None, cx),
            cx,
        )
    }

    fn render_slider(&self, slider: &Slider, cx: &App) -> AnyElement {
        let input = &slider.input;
        let Some(control) = self.sliders.get(&input.target) else {
            return div().into_any_element();
        };
        let kit = h_flex()
            .w_full()
            .gap_3()
            .child(
                div()
                    .id(self.id(Part::Control(&input.target)))
                    .test_support()
                    .flex_1()
                    .child(
                        KitSlider::new(&control.state)
                            .accessibility_label(input.accessible_name.clone())
                            .disabled(input.binding.is_none() || self.stale.is_some()),
                    ),
            )
            .child(
                div()
                    .id(self.id(Part::SliderValue(&input.target)))
                    .test_support()
                    .min_w_8()
                    .text_right()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(number_value(slider.value).to_string()),
            );
        labelled(
            &input.label,
            self.with_messages(
                &input.target,
                kit.into_any_element(),
                &input.failures,
                None,
                cx,
            ),
            cx,
        )
    }
}

/// `control` under its `label`, if it has one. Kit's form `Field` would
/// draw the same, but takes its ID and layout from a `Form` around it.
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

impl SurfacePanel {
    /// `control` over why what it shows is kept out of the data model, if
    /// it is, and the messages of its checks that fail, each colored by
    /// how much it matters: Kit's form `Field` has one muted description
    /// instead.
    fn with_messages(
        &self,
        target: &Target,
        control: AnyElement,
        failures: &[Failure],
        problem: Option<Problem>,
        cx: &App,
    ) -> AnyElement {
        if failures.is_empty() && problem.is_none() {
            return control;
        }
        let (danger, warning) = (cx.theme().danger, cx.theme().warning);
        let message = |text: String, blocks: bool| {
            div()
                .text_xs()
                .text_color(if blocks { danger } else { warning })
                .child(text)
        };
        let problem = problem.map(|problem| {
            let says = match problem {
                Problem::NotANumber => "Enter a number",
            };
            message(says.to_owned(), true)
                .id(self.id(Part::Problem(target)))
                .test_support()
        });
        v_flex()
            .w_full()
            .gap_1()
            .child(control)
            .children(problem)
            .children(
                failures
                    .iter()
                    .map(|failure| message(failure.message.clone(), failure.blocks())),
            )
            .into_any_element()
    }
}
