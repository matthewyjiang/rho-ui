//! The settings screen, opened from the gear at the bottom of the sidebar.
//! Built from GPUI Kit's `Settings` component: pages on the left with a
//! search, groups of items on the right. Each change applies at once and is
//! saved.

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings as SettingsPanel},
    v_flex, ActiveTheme as _, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::{
    div, px, rems, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window,
};

use crate::{
    settings::{Appearance, Settings, UiScale, DEFAULT_CODE_FONT, DEFAULT_UI_FONT},
    CloseSettings,
};

/// One choice in a font Select.
#[derive(Clone)]
struct Choice {
    value: SharedString,
    title: SharedString,
}

impl Choice {
    fn new(value: SharedString, title: SharedString) -> Self {
        Self { value, title }
    }
}

impl SelectItem for Choice {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &SharedString {
        &self.value
    }
}

type FontSelect = Entity<SelectState<SearchableVec<Choice>>>;

/// Asks the app to close the settings screen.
pub(crate) struct Closed;

/// Which font a picker sets.
#[derive(Clone, Copy)]
enum FontRole {
    Ui,
    Code,
}

impl FontRole {
    fn current(self, settings: &Settings) -> &SharedString {
        match self {
            FontRole::Ui => &settings.ui_font,
            FontRole::Code => &settings.code_font,
        }
    }

    /// The picker's element ID.
    fn element_id(self) -> &'static str {
        match self {
            FontRole::Ui => "setting-ui-font",
            FontRole::Code => "setting-code-font",
        }
    }

    fn default_family(self) -> &'static str {
        match self {
            FontRole::Ui => DEFAULT_UI_FONT,
            FontRole::Code => DEFAULT_CODE_FONT,
        }
    }
}

pub(crate) struct SettingsView {
    ui_font: FontSelect,
    code_font: FontSelect,
    focus_handle: FocusHandle,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<Closed> for SettingsView {}

impl SettingsView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let fonts: Vec<Choice> = cx
            .text_system()
            .all_font_names()
            .into_iter()
            .map(|name| {
                let name = SharedString::from(name);
                Choice::new(name.clone(), name)
            })
            .collect();
        let (ui_font, ui_subscription) = font_select(FontRole::Ui, fonts.clone(), window, cx);
        let (code_font, code_subscription) = font_select(FontRole::Code, fonts, window, cx);
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            ui_font,
            code_font,
            focus_handle,
            _subscriptions: [ui_subscription, code_subscription],
        }
    }

    fn select(&self, role: FontRole) -> &FontSelect {
        match role {
            FontRole::Ui => &self.ui_font,
            FontRole::Code => &self.code_font,
        }
    }

    /// The font picker for `role`, with a reset to its default.
    fn font_item(&self, role: FontRole, title: &'static str, description: &str) -> SettingItem {
        let select = self.select(role).clone();
        let reset_select = select.clone();
        SettingItem::new(
            title,
            SettingField::render(move |options, _, _| {
                Select::new(&select)
                    .id(role.element_id())
                    .with_size(options.size())
                    .accessibility_label(title)
                    .w(rems(16.))
                    .menu_width(rems(20.))
            })
            .on_reset(
                move |cx| role.current(cx.global::<Settings>()) != role.default_family(),
                move |window, cx| {
                    let family = SharedString::from(role.default_family());
                    set_font(role, family.clone(), cx);
                    reset_select.update(cx, |state, cx| {
                        state.set_selected_value(&family, window, cx);
                    });
                },
            ),
        )
        .description(description.to_owned())
    }
}

/// A searchable Select over every installed font, showing `role`'s font.
fn font_select(
    role: FontRole,
    fonts: Vec<Choice>,
    window: &mut Window,
    cx: &mut Context<'_, SettingsView>,
) -> (FontSelect, Subscription) {
    let current = role.current(cx.global::<Settings>()).clone();
    let selected = fonts
        .iter()
        .position(|font| font.value() == &current)
        .map(|ix| gpui_kit::component::IndexPath::default().row(ix));
    let state = cx.new(|cx| {
        SelectState::new(SearchableVec::new(fonts), selected, window, cx).searchable(true)
    });
    let subscription = cx.subscribe_in(
        &state,
        window,
        move |_, _, event: &SelectEvent<SearchableVec<Choice>>, _, cx| {
            if let SelectEvent::Confirm(Some(family)) = event {
                set_font(role, family.clone(), cx);
            }
        },
    );
    (state, subscription)
}

/// Applies and saves a change to the current settings.
fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    let mut settings = cx.global::<Settings>().clone();
    change(&mut settings);
    settings.save();
    settings.apply(cx);
}

fn set_font(role: FontRole, family: SharedString, cx: &mut App) {
    update(cx, |settings| match role {
        FontRole::Ui => settings.ui_font = family,
        FontRole::Code => settings.code_font = family,
    });
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let scales: Vec<(SharedString, SharedString)> = UiScale::CHOICES
            .into_iter()
            .map(|scale| {
                let percent = SharedString::from(scale.percent().to_string());
                (percent, format!("{}%", scale.percent()).into())
            })
            .collect();
        let themes: Vec<(SharedString, SharedString)> = Appearance::CHOICES
            .into_iter()
            .map(|choice| (choice.name().into(), choice.label().into()))
            .collect();
        let appearance = SettingPage::new("Appearance")
            .icon(IconName::Palette)
            .default_open(true)
            .resettable(true)
            .group(
                SettingGroup::new().title("Interface").items([
                    SettingItem::new(
                        "Theme",
                        SettingField::dropdown(
                            themes,
                            |cx: &App| cx.global::<Settings>().appearance.name().into(),
                            |name: SharedString, cx: &mut App| {
                                if let Some(appearance) = Appearance::from_name(&name) {
                                    update(cx, |settings| settings.appearance = appearance);
                                }
                            },
                        )
                        .default_value(Appearance::default().name()),
                    )
                    .description("Dark, light, or whichever your desktop uses."),
                    SettingItem::new(
                        "UI scale",
                        SettingField::dropdown(
                            scales,
                            |cx: &App| {
                                cx.global::<Settings>()
                                    .ui_scale
                                    .percent()
                                    .to_string()
                                    .into()
                            },
                            |percent: SharedString, cx: &mut App| {
                                if let Some(scale) =
                                    percent.parse().ok().and_then(UiScale::from_percent)
                                {
                                    update(cx, |settings| settings.ui_scale = scale);
                                }
                            },
                        )
                        .default_value(UiScale::default().percent().to_string()),
                    )
                    .description("The size of text and controls."),
                    self.font_item(FontRole::Ui, "UI font", "The font for interface text."),
                    self.font_item(
                        FontRole::Code,
                        "Code font",
                        "The font for code blocks, tool output and diffs.",
                    ),
                ]),
            );
        v_flex()
            .id("settings-view")
            .key_context("SettingsView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &CloseSettings, _, cx| cx.emit(Closed)))
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h(px(44.))
                    .px_3()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().font_semibold().child("Settings"))
                    .child(
                        Button::new("close-settings")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .tooltip("Close (Esc)")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Closed))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(SettingsPanel::new("settings").page(appearance)),
            )
    }
}
