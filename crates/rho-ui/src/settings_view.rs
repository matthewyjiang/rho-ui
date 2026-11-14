//! Settings, opened with Ctrl+, or the gear at the bottom of the sidebar, in a
//! dialog over the window. Built from GPUI Kit's `Settings` component: pages
//! on the left with a search, groups of items on the right. Each change
//! applies at once and is saved. The Machines page is in [`machines`].

mod machines;

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    dialog::DialogEntrance,
    group_box::GroupBoxVariant,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings as SettingsPanel},
    ActiveTheme as _, IconName, Sizable as _, WindowExt as _,
};
use gpui_kit::{
    div, rems, size, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Render, SharedString, Size, StyleRefinement,
    Styled as _, Subscription, Window,
};

use crate::settings::{Appearance, Settings, UiScale, DEFAULT_CODE_FONT, DEFAULT_UI_FONT};
use machines::MachineFields;

/// How much of the window the dialog takes in each direction, so a large
/// window gets a large dialog: 900 x 600 px in the default 1200 x 800 window.
const VIEWPORT_SHARE: f32 = 0.75;
/// The smallest dialog, in rems so it follows the UI scale: 832 x 384 px at
/// 100%. GPUI Kit gives an item's label up to 3/5 of its row, so the width
/// also has to leave the controls ([`CONTROL_REMS`]) their 2/5. Kit's dialog
/// still shrinks below this to fit a smaller window.
const MIN_WIDTH_REMS: f32 = 52.;
const MIN_HEIGHT_REMS: f32 = 24.;
/// The widest dialog, 1024 px at 100%: wider only spreads each label further
/// from its control.
const MAX_WIDTH_REMS: f32 = 64.;
/// The page list on the panel's left.
const PAGES_REMS: f32 = 11.;
/// Every control's width, so they line up in one column.
const CONTROL_REMS: f32 = 14.;
/// Room on the right of the page title for the dialog's close button.
const CLOSE_ROOM_REMS: f32 = 3.;

/// Opens settings in a dialog, unless a dialog is already open. The dialog
/// owns the view: Escape, its close button, or a click outside drops it, and
/// the keyboard goes back where it was. The dialog has no title or padding:
/// the panel fills it, and the page title heads it.
pub(crate) fn open(window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let view = cx.new(|cx| SettingsView::new(window, cx));
    window.open_dialog(cx, move |dialog, window, _| {
        let view = view.clone();
        dialog
            .entrance(DialogEntrance::Fade)
            .centered()
            .w(dialog_size(window).width)
            .p_0()
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
    });
}

/// The dialog's size in `window`: [`VIEWPORT_SHARE`] of it, within the rem
/// bounds above. Kit rebuilds the dialog every frame, so it follows a resize.
fn dialog_size(window: &Window) -> Size<Pixels> {
    let rem = window.rem_size();
    let viewport = window.viewport_size();
    size(
        (viewport.width * VIEWPORT_SHARE).clamp(rem * MIN_WIDTH_REMS, rem * MAX_WIDTH_REMS),
        (viewport.height * VIEWPORT_SHARE).max(rem * MIN_HEIGHT_REMS),
    )
}

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

/// The settings panel inside the dialog [`open`] shows.
struct SettingsView {
    ui_font: FontSelect,
    code_font: FontSelect,
    machines: MachineFields,
    _subscriptions: [Subscription; 2],
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
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
        let mut view = Self {
            ui_font,
            code_font,
            machines: MachineFields::new(window, cx),
            _subscriptions: [ui_subscription, code_subscription],
        };
        view.sync_rho_fields(window, cx);
        view
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
                    .w(rems(CONTROL_REMS))
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

/// Every page's title: large, with room for the dialog's close button.
fn page_header() -> StyleRefinement {
    StyleRefinement::default()
        .border_b_0()
        .pr(rems(CLOSE_ROOM_REMS))
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let control =
            |field: SettingField<SharedString>| field.w(rems(CONTROL_REMS)).justify_between();
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
            .header_style(&page_header())
            .group(
                // One group: a title would only repeat the page's.
                SettingGroup::new().items([
                    SettingItem::new(
                        "Theme",
                        control(
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
                        ),
                    )
                    .description("Dark, light, or whichever your desktop uses."),
                    SettingItem::new(
                        "UI scale",
                        control(
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
                        ),
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
        let machines = self.machines_page(cx);
        let theme = cx.theme();
        // The page list is tinted and rounded where it meets the dialog's
        // corners, since the dialog has no padding to keep it inside.
        let pages = StyleRefinement::default()
            .bg(theme.tokens.sidebar)
            .rounded_l(theme.radius_lg);
        div()
            .id("settings-view")
            .test_support()
            .h(dialog_size(window).height)
            .min_h_0()
            .child(
                SettingsPanel::new("settings")
                    .sidebar_width(window.rem_size() * PAGES_REMS)
                    .sidebar_style(&pages)
                    .with_group_variant(GroupBoxVariant::Fill)
                    .page(appearance)
                    .page(machines),
            )
    }
}
