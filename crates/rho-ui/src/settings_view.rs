//! Settings, opened with Ctrl+, or the gear at the bottom of the sidebar, in a
//! dialog over the window. Built from GPUI Kit's `Settings` component: pages
//! on the left with a search, groups of items on the right. Each change
//! applies at once and is saved. rho-ui's own settings come first, kept in
//! its settings file: Appearance, Notifications, the Machines page in
//! [`machines`], and the forge CLIs on each machine in [`source_control`].
//! Then come rho's own: each machine's sign-ins to providers, in
//! [`providers`], the worktrees it added for sessions, in [`worktrees`], a
//! page per category its daemon describes, saved in that machine's
//! `config.toml`: see [`rho`], and the MCP servers and hooks its
//! configuration sets up, read-only, in [`configured`]. Last, [`about`]
//! shows the versions of rho-ui and of Rho on each machine.

mod about;
mod configured;
mod machines;
mod providers;
mod rho;
mod source_control;
mod worktrees;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    dialog::DialogEntrance,
    group_box::GroupBoxVariant,
    h_flex,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings as SettingsPanel},
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, size, App, AppContext as _, Context, Entity, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, SharedString, Size,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription, WeakEntity,
    Window,
};

use crate::{
    app_update::AppUpdate,
    auths::Auths,
    configured::Configured,
    forge::Forges,
    machines::{MachineId, MachineStatus, Machines, Place},
    projects::Projects,
    rho_install::Installs,
    rho_settings::RhoSettings,
    settings::{
        update, Appearance, Notifications, Settings, SmoothScroll, UiScale, DEFAULT_CODE_FONT,
        DEFAULT_UI_FONT,
    },
    sidebar::SidebarView,
    workspaces::Workspaces,
    worktrees::Worktrees,
};
use configured::ConfiguredFields;
use machines::MachineFields;
use providers::ProvidersFields;
use rho::RhoFields;
use source_control::SourceControlFields;
use worktrees::WorktreesFields;

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
/// The page list on the panel's left: room for "Source control" beside
/// its icon and submenu caret.
const PAGES_REMS: f32 = 12.;
/// Every control's width, so they line up in one column.
const CONTROL_REMS: f32 = 14.;
/// Room on the right of the page title for the dialog's close button.
const CLOSE_ROOM_REMS: f32 = 3.;

/// Opens settings in a dialog, unless a dialog is already open. The dialog
/// owns the view: Escape, its close button, or a click outside drops it, and
/// the keyboard goes back where it was. The dialog has no title or padding:
/// the panel fills it, and the page title heads it. `app` holds what the
/// pages read of each machine.
pub(crate) fn open(app: AppState, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let view = cx.new(|cx| SettingsView::new(app, window, cx));
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

/// What Settings reads of each machine, from the app.
pub(crate) struct AppState {
    pub(crate) machines: Entity<Machines>,
    /// rho's own settings on each machine.
    pub(crate) rho_settings: Entity<RhoSettings>,
    /// The auth profiles each machine's rho can sign in to.
    pub(crate) auths: Entity<Auths>,
    /// The worktrees each machine's rho added for sessions.
    pub(crate) worktrees: Entity<Worktrees>,
    /// The MCP servers and hooks each machine's rho configures.
    pub(crate) configured: Entity<Configured>,
    /// The project of the session or draft on screen, whose MCP servers
    /// and hooks show first.
    pub(crate) project: Option<Place>,
    /// The user's projects, to read MCP servers and hooks for.
    pub(crate) projects: Entity<Projects>,
    /// The listed sessions, whose titles name their worktrees.
    pub(crate) sidebar: Entity<SidebarView>,
    /// Each machine's home folder, which paths show as `~`.
    pub(crate) workspaces: Entity<Workspaces>,
    /// The forge CLIs on each machine.
    pub(crate) forges: Entity<Forges>,
    /// Each machine's rho, for About: installing and updating.
    pub(crate) installs: Entity<Installs>,
    /// rho-ui's own updates, for About.
    pub(crate) app_update: Entity<AppUpdate>,
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

    /// Each row draws its name in its own font, as a preview. The trigger
    /// keeps the UI font, so a symbol font's name stays readable there.
    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().font_family(self.value.clone()).child(self.title())
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
    /// Every machine, which the machine-scoped pages pick from.
    machine_list: Entity<Machines>,
    machines: MachineFields,
    rho: RhoFields,
    source_control: SourceControlFields,
    providers: ProvidersFields,
    worktrees: WorktreesFields,
    configured: ConfiguredFields,
    /// Each machine's rho, for About.
    installs: Entity<Installs>,
    /// rho-ui's own updates, for About.
    app_update: Entity<AppUpdate>,
    _subscriptions: [Subscription; 4],
}

impl SettingsView {
    fn new(app: AppState, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let AppState {
            machines,
            rho_settings,
            auths,
            worktrees,
            configured,
            project,
            projects,
            sidebar,
            workspaces,
            forges,
            installs,
            app_update,
        } = app;
        let installs_subscription = cx.observe(&installs, |_, _, cx| cx.notify());
        let app_update_subscription = cx.observe(&app_update, |_, _, cx| cx.notify());
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
            configured: ConfiguredFields::new(
                &machines,
                configured,
                project,
                projects,
                workspaces.clone(),
                cx,
            ),
            source_control: SourceControlFields::new(&machines, workspaces, forges, cx),
            providers: ProvidersFields::new(&machines, auths, cx),
            worktrees: WorktreesFields::new(&machines, worktrees, sidebar, cx),
            rho: RhoFields::new(&machines, rho_settings, window, cx),
            machine_list: machines,
            installs,
            app_update,
            _subscriptions: [
                ui_subscription,
                code_subscription,
                installs_subscription,
                app_update_subscription,
            ],
        };
        view.sync_machine_fields(window, cx);
        view.sync_rho_controls(window, cx);
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

/// The machine a page shows, for pages that differ by machine: the one
/// picked, while it is still on the list, else this one.
struct PickedMachine(MachineId);

impl PickedMachine {
    /// This machine, until the user picks another.
    fn new() -> Self {
        Self(MachineId::local())
    }

    fn shown(&self, machines: &Machines) -> MachineId {
        if machines.get(&self.0).is_some() {
            self.0.clone()
        } else {
            MachineId::local()
        }
    }

    /// Picks `machine`; returns whether that changed the pick.
    fn pick(&mut self, machine: MachineId) -> bool {
        if self.0 == machine {
            return false;
        }
        self.0 = machine;
        true
    }
}

/// Why `machine`'s daemon can't be asked yet, as a page says it.
fn unreachable_note(machines: &Machines, machine: &MachineId) -> Option<String> {
    let label = machines.label(machine);
    match machines.status(machine) {
        None | Some(MachineStatus::Connecting) => Some(format!("Connecting to {label}…")),
        Some(MachineStatus::Failed(why)) => Some(format!("Can't reach {label}: {why}")),
        Some(MachineStatus::OlderDaemon { .. }) => Some(format!(
            "Rho on {label} is older than rho-ui: restart its daemon to connect"
        )),
        Some(MachineStatus::OlderRho) => Some(format!(
            "Rho on {label} is older than rho-ui: update it there to connect"
        )),
        Some(MachineStatus::Stopped) => Some(format!("Rho on {label} is stopped")),
        Some(MachineStatus::Ready { .. }) => None,
    }
}

/// The picker of the machine a page shows, among `machines`, set to
/// `shown`; `pick` shows another. `description` says why the page differs
/// by machine.
fn machine_picker(
    machines: &Machines,
    shown: &MachineId,
    description: &'static str,
    view: WeakEntity<SettingsView>,
    pick: fn(&mut SettingsView, MachineId, &mut Context<'_, SettingsView>),
) -> SettingItem {
    let options: Vec<(SharedString, SharedString)> = machines
        .iter()
        .map(|machine| {
            let id = SharedString::from(machine.id.as_str().to_owned());
            (id, machine.label.clone())
        })
        .collect();
    let shown = SharedString::from(shown.as_str().to_owned());
    SettingItem::new(
        "Machine",
        SettingField::dropdown(
            options,
            move |_: &App| shown.clone(),
            move |id: SharedString, cx: &mut App| {
                let _ = view.update(cx, |this, cx| pick(this, MachineId::from(&*id), cx));
            },
        )
        .w(rems(CONTROL_REMS))
        .justify_between(),
    )
    .description(description)
}

/// How a value reads: muted when it stands in for one not known yet.
#[derive(Clone, Copy)]
enum Tone {
    Plain,
    Muted,
}

impl Tone {
    fn color(self, cx: &App) -> Hsla {
        match self {
            Tone::Plain => cx.theme().foreground,
            Tone::Muted => cx.theme().muted_foreground,
        }
    }
}

/// A value to read, not change.
fn value(text: impl Into<SharedString>, tone: Tone) -> SettingField<SharedString> {
    let text = text.into();
    SettingField::render(move |_, _, cx| {
        div()
            .text_sm()
            .text_color(tone.color(cx))
            .child(text.clone())
    })
}

/// How a row's status reads.
#[derive(Clone, Copy)]
enum Mark {
    /// As it should be.
    On,
    /// Off, idle, or not the user's concern.
    Off,
    /// Needs the user before it runs.
    Warn,
    /// Broken.
    Bad,
}

impl Mark {
    fn icon(self) -> Option<IconName> {
        match self {
            Self::Warn => Some(IconName::TriangleAlert),
            Self::Bad => Some(IconName::CircleX),
            Self::On | Self::Off => None,
        }
    }

    fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Self::On => theme.foreground,
            Self::Off => theme.muted_foreground,
            Self::Warn => theme.warning,
            Self::Bad => theme.danger,
        }
    }
}

/// A row's status, `text` as `mark` reads, flagged with its icon. `id`
/// finds it in tests, which read it as `label`.
fn status_field(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    mark: Mark,
    text: impl Into<SharedString>,
) -> SettingField<SharedString> {
    let (id, label, text) = (id.into(), label.into(), text.into());
    SettingField::render(move |_, _, cx| {
        let color = mark.color(cx);
        h_flex()
            .id(id.clone())
            .test_support()
            .aria_label(label.clone())
            .gap_1p5()
            .text_sm()
            .text_color(color)
            .when_some(mark.icon(), |this, icon| {
                this.child(Icon::new(icon).small().text_color(color))
            })
            .child(text.clone())
    })
}

/// `item`, with `description` under its title if there is one. Kit gives
/// even an empty description a line, which sets the title off its control.
fn described(item: SettingItem, description: Option<String>) -> SettingItem {
    match description {
        Some(description) => item.description(description),
        None => item,
    }
}

fn set_font(role: FontRole, family: SharedString, cx: &mut App) {
    update(cx, |settings| match role {
        FontRole::Ui => settings.ui_font = family,
        FontRole::Code => settings.code_font = family,
    });
}

impl SettingsView {
    /// The Appearance page: the theme, the scale, the fonts, and whether
    /// scrolling glides.
    fn appearance_page(&self) -> SettingPage {
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
        let scrolls: Vec<(SharedString, SharedString)> = SmoothScroll::CHOICES
            .into_iter()
            .map(|choice| (choice.name().into(), choice.label().into()))
            .collect();
        SettingPage::new("Appearance")
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
                    SettingItem::new(
                        "Smooth scrolling",
                        control(
                            SettingField::dropdown(
                                scrolls,
                                |cx: &App| cx.global::<Settings>().smooth_scroll.name().into(),
                                |name: SharedString, cx: &mut App| {
                                    if let Some(smooth_scroll) = SmoothScroll::from_name(&name) {
                                        update(cx, |settings| {
                                            settings.smooth_scroll = smooth_scroll;
                                        });
                                    }
                                },
                            )
                            .default_value(SmoothScroll::default().name()),
                        ),
                    )
                    .description(
                        "Glide the mouse wheel, and carry a touchpad swipe on. System \
                         follows your desktop's animations setting.",
                    ),
                ]),
            )
    }
}

/// The Notifications page: which sessions send one while rho-ui is in the
/// background ([`crate::attention`]).
fn notifications_page() -> SettingPage {
    let choices: Vec<(SharedString, SharedString)> = Notifications::CHOICES
        .into_iter()
        .map(|choice| (choice.name().into(), choice.label().into()))
        .collect();
    SettingPage::new("Notifications")
        .icon(Lucide::Bell)
        .resettable(true)
        .header_style(&page_header())
        .group(
            SettingGroup::new().items([SettingItem::new(
                "Desktop notifications",
                SettingField::dropdown(
                    choices,
                    |cx: &App| cx.global::<Settings>().notifications.name().into(),
                    |name: SharedString, cx: &mut App| {
                        if let Some(notifications) = Notifications::from_name(&name) {
                            update(cx, |settings| settings.notifications = notifications);
                        }
                    },
                )
                .default_value(Notifications::default().name())
                .w(rems(CONTROL_REMS))
                .justify_between(),
            )
            .description(
                "Sent while rho-ui's window doesn't have the focus, when a session \
                 needs you or its turn ends.",
            )]),
        )
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let appearance = self.appearance_page();
        let notifications = notifications_page();
        let machines = self.machines_page(cx);
        let source_control = self.source_control_page(window, cx);
        let providers = self.providers_page(window, cx);
        let worktrees = self.worktrees_page(cx);
        let rho = self.rho_page(window, cx);
        let configured = self.configured_page(cx);
        let about = about::about_page(&self.machine_list, &self.installs, &self.app_update, cx);
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
                    .page(notifications)
                    .page(machines)
                    .page(source_control)
                    .page(providers)
                    .page(worktrees)
                    .page(rho)
                    .page(configured)
                    .page(about),
            )
    }
}
