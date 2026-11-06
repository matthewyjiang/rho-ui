//! The session list, drawn: one scrolling list of active sessions, newest
//! first, then the Snoozed and Settled shelves, held at the bottom until
//! opened shelves fill the list. An active row shows the
//! workspace, the session's status or age, its title, and its branch; hovering
//! it swaps the status for Snooze and Settle. A shelf opens from its header
//! into one-line rows, and hovering one shows its details in a card.
//!
//! The app draws this view as a cached view, so a hover or scroll here
//! redraws only the sidebar, and [`v_virtual_list`] builds only the rows on
//! screen.
//! The data lives in [`SessionList`] and [`Workspaces`], built when it
//! changes, never per frame.

use std::{
    ops::Range,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    alert::Alert,
    avatar::Avatar,
    button::{Button, ButtonVariant, ButtonVariants as _},
    description_list::DescriptionList,
    empty::{Empty, EmptyDescription, EmptyHeader},
    h_flex,
    hover_card::HoverCard,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    v_flex, v_virtual_list, ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt, Theme,
    VirtualListScrollHandle, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    canvas, div, px, rems, size, Anchor, AnyElement, AppContext as _, Context, Entity,
    EventEmitter, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement, Pixels,
    Rems, Render, ScrollStrategy, SharedString, StatefulInteractiveElement, StyleRefinement,
    Styled, Subscription, Task, WeakEntity, Window,
};
use rho_ui_core::{SessionId, SessionSummary};

use crate::{
    session_list::{Activity, Item, LiveSession, Row, SavedSession, SessionList},
    shelf::{Shelf, Shelves},
    time,
    workspaces::{Workspace, Workspaces},
};

// A row's parts, in rems so they follow the UI scale.
/// The workspace and branch lines.
const LINE_REMS: f32 = 1.;
/// The title line, a little taller for its larger text.
const TITLE_REMS: f32 = 1.375;
/// Between the lines.
const GAP_REMS: f32 = 0.25;
/// Inside the highlight, above and below the lines.
const PAD_REMS: f32 = 0.625;
/// Outside the highlight, so neighbours' highlights don't touch.
const GUTTER_REMS: f32 = 0.0625;

/// An active row's height. Every active row has it, so a row without a
/// branch keeps the space.
pub(crate) const ROW_REMS: f32 =
    2. * LINE_REMS + TITLE_REMS + 2. * GAP_REMS + 2. * PAD_REMS + 2. * GUTTER_REMS;

/// Inside a one-line shelved row's highlight, above and below its title.
const SHELVED_PAD_REMS: f32 = 0.5;

/// A snoozed or settled row's height.
const SHELVED_ROW_REMS: f32 = TITLE_REMS + 2. * SHELVED_PAD_REMS + 2. * GUTTER_REMS;

/// A shelf header's height.
const HEADER_REMS: f32 = 2.5;

/// Each item's height. The gap's comes from the list's height instead; see
/// [`SidebarView::render_list`].
fn item_rems(item: Item) -> f32 {
    match item {
        Item::Session(_) => ROW_REMS,
        Item::Shelved(_) => SHELVED_ROW_REMS,
        Item::Header(_) => HEADER_REMS,
        Item::Gap => 0.,
    }
}

/// How often ages, timers and snooze wake-ups refresh. They show whole
/// minutes, so 30 s keeps them current.
const TICK: Duration = Duration::from_secs(30);

/// How long a session can be snoozed for.
#[derive(Debug, PartialEq, Eq)]
struct Snooze {
    /// The inline button's element ID and label.
    id: &'static str,
    short: &'static str,
    /// The context menu label.
    long: &'static str,
    seconds: i64,
}

const SNOOZES: [Snooze; 3] = [
    Snooze {
        id: "snooze-1h",
        short: "1h",
        long: "Snooze 1 hour",
        seconds: 3600,
    },
    Snooze {
        id: "snooze-4h",
        short: "4h",
        long: "Snooze 4 hours",
        seconds: 4 * 3600,
    },
    Snooze {
        id: "snooze-1d",
        short: "1d",
        long: "Snooze 1 day",
        seconds: 24 * 3600,
    },
];

/// Moving a session between shelves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShelfAction {
    Settle,
    Snooze(&'static Snooze),
    Restore,
}

impl ShelfAction {
    const ON_ACTIVE: [ShelfAction; 4] = [
        ShelfAction::Settle,
        ShelfAction::Snooze(&SNOOZES[0]),
        ShelfAction::Snooze(&SNOOZES[1]),
        ShelfAction::Snooze(&SNOOZES[2]),
    ];

    /// What a row offers, inline and in its menu. A busy session offers
    /// nothing: it shows as active until its turn ends.
    fn offered(shelf: Shelf, activity: Activity) -> &'static [ShelfAction] {
        if !activity.is_idle() {
            return &[];
        }
        match shelf {
            Shelf::Active => &Self::ON_ACTIVE,
            Shelf::Snoozed => &[ShelfAction::Restore, ShelfAction::Settle],
            Shelf::Settled => &[ShelfAction::Restore],
        }
    }

    fn label(self) -> &'static str {
        match self {
            ShelfAction::Settle => "Settle",
            ShelfAction::Snooze(snooze) => snooze.long,
            ShelfAction::Restore => "Restore",
        }
    }

    fn icon(self) -> Icon {
        match self {
            ShelfAction::Settle => Icon::new(IconName::Check),
            ShelfAction::Snooze(_) => Icon::new(Lucide::AlarmClock),
            ShelfAction::Restore => Icon::new(IconName::Undo2),
        }
    }
}

/// What the sidebar asks of the app.
pub(crate) enum SidebarEvent {
    /// Open a session. `title` is the saved one, if any.
    Open {
        id: SessionId,
        cwd: PathBuf,
        title: Option<SharedString>,
    },
    NewSession,
    /// Delete a saved session; the user has confirmed.
    Delete {
        id: SessionId,
    },
    OpenSettings,
}

/// The row under the mouse, which shows its actions instead of its status.
struct Hover {
    id: SessionId,
    /// The snooze button was pressed: show the durations.
    snoozing: bool,
}

pub(crate) struct SidebarView {
    list: SessionList,
    workspaces: Entity<Workspaces>,
    shelves: Shelves,
    active: Option<SessionId>,
    hover: Option<Hover>,
    error: Option<SharedString>,
    /// The agent's name and version, for the footer.
    agent: Option<SharedString>,
    search: Entity<InputState>,
    scroll: VirtualListScrollHandle,
    /// The list's height in the last frame, which the gap above the
    /// shelves fills up to.
    viewport: Pixels,
    _subscriptions: [Subscription; 2],
    _tick: Task<()>,
}

impl EventEmitter<SidebarEvent> for SidebarView {}

impl SidebarView {
    pub(crate) fn new(
        workspaces: Entity<Workspaces>,
        shelves: Shelves,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let subscriptions = [
            cx.subscribe_in(&search, window, |this, state, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.list.set_query(state.read(cx).value().trim());
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    this.rebuild(cx);
                }
            }),
            // Workspace names are searchable.
            cx.observe(&workspaces, |this, _, cx| this.rebuild(cx)),
        ];
        let tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            if this.update(cx, Self::rebuild).is_err() {
                break;
            }
        });
        Self {
            list: SessionList::default(),
            workspaces,
            shelves,
            active: None,
            hover: None,
            error: None,
            agent: None,
            search,
            scroll: VirtualListScrollHandle::new(),
            viewport: Pixels::ZERO,
            _subscriptions: subscriptions,
            _tick: tick,
        }
    }

    /// Replaces the saved sessions with a fresh `list_sessions` result.
    pub(crate) fn set_saved(&mut self, sessions: Vec<SessionSummary>, cx: &mut Context<'_, Self>) {
        let saved = self.workspaces.update(cx, |workspaces, cx| {
            sessions
                .into_iter()
                .map(|session| SavedSession {
                    workspace: workspaces.index(session.cwd(), cx),
                    id: session.id().clone(),
                    cwd: session.cwd().to_path_buf(),
                    title: session.title().map(|title| title.to_owned().into()),
                    updated: session
                        .updated_at()
                        .and_then(time::unix_seconds)
                        .unwrap_or(0),
                })
                .collect()
        });
        self.list.set_saved(saved);
        self.error = None;
        self.rebuild(cx);
    }

    pub(crate) fn set_error(&mut self, error: String, cx: &mut Context<'_, Self>) {
        self.error = Some(error.into());
        cx.notify();
    }

    /// Records an open session's title and activity.
    pub(crate) fn set_live(&mut self, live: LiveSession, cx: &mut Context<'_, Self>) {
        let workspace = self
            .workspaces
            .update(cx, |workspaces, cx| workspaces.index(&live.cwd, cx));
        if self.list.set_live(live, workspace, time::now_unix()) {
            self.rebuild(cx);
        }
    }

    /// Marks a session as started or continued here, so it stays active.
    pub(crate) fn own(&mut self, id: &SessionId, cx: &mut Context<'_, Self>) {
        self.shelves.own(id.as_str());
        self.rebuild(cx);
    }

    /// Drops a session the agent deleted.
    pub(crate) fn remove(&mut self, id: &SessionId, cx: &mut Context<'_, Self>) {
        self.list.remove(id);
        self.shelves.forget(id.as_str());
        if self.active.as_ref() == Some(id) {
            self.active = None;
        }
        self.rebuild(cx);
    }

    /// Forgets every open session, after the agent restarts.
    pub(crate) fn clear_live(&mut self, cx: &mut Context<'_, Self>) {
        self.list.clear_live();
        self.active = None;
        self.rebuild(cx);
    }

    pub(crate) fn set_active(&mut self, id: Option<SessionId>, cx: &mut Context<'_, Self>) {
        if self.active != id {
            self.active = id;
            cx.notify();
        }
    }

    pub(crate) fn set_agent(&mut self, agent: String, cx: &mut Context<'_, Self>) {
        self.agent = Some(agent.into());
        cx.notify();
    }

    fn rebuild(&mut self, cx: &mut Context<'_, Self>) {
        let workspaces = self.workspaces.read(cx);
        self.list.rebuild(&self.shelves, time::now_unix(), |ix| {
            workspaces.get(ix).name.as_ref()
        });
        cx.notify();
    }

    fn toggle(&mut self, which: Shelf, cx: &mut Context<'_, Self>) {
        self.list.toggle(which);
        self.rebuild(cx);
    }

    fn open(&mut self, id: &SessionId, cx: &mut Context<'_, Self>) {
        if let Some(row) = self.list.row(id) {
            cx.emit(SidebarEvent::Open {
                id: row.id.clone(),
                cwd: row.cwd.clone(),
                title: row.title.clone(),
            });
        }
    }

    fn shelve(&mut self, id: &SessionId, action: ShelfAction, cx: &mut Context<'_, Self>) {
        let now = time::now_unix();
        match action {
            ShelfAction::Settle => self.shelves.settle(id.as_str(), now),
            ShelfAction::Snooze(snooze) => self.shelves.snooze(id.as_str(), now + snooze.seconds),
            ShelfAction::Restore => self.shelves.restore(id.as_str()),
        }
        // The row moves away from the pointer.
        self.hover = None;
        self.rebuild(cx);
    }

    fn hover(&mut self, id: &SessionId, cx: &mut Context<'_, Self>) {
        if self.hover.as_ref().is_none_or(|hover| hover.id != *id) {
            self.hover = Some(Hover {
                id: id.clone(),
                snoozing: false,
            });
            cx.notify();
        }
    }

    /// Clears the hover unless another row already took it.
    fn unhover(&mut self, id: &SessionId, cx: &mut Context<'_, Self>) {
        if self.hover.as_ref().is_some_and(|hover| hover.id == *id) {
            self.hover = None;
            cx.notify();
        }
    }

    fn start_snoozing(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(hover) = &mut self.hover {
            hover.snoozing = true;
            cx.notify();
        }
    }

    /// The buttons a hovered row shows in place of its status: Snooze and
    /// Settle, or the snooze durations once Snooze is pressed.
    fn render_actions(&self, row: &Row, cx: &Context<'_, Self>) -> Option<AnyElement> {
        let offered = ShelfAction::offered(row.shelf, row.activity);
        if offered.is_empty() {
            return None;
        }
        let snoozing = self.hover.as_ref().is_some_and(|hover| hover.snoozing);
        let button = |id: &'static str, action: ShelfAction| {
            let row_id = row.id.clone();
            Button::new(id)
                .ghost()
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| this.shelve(&row_id, action, cx)))
        };
        let mut buttons = Vec::new();
        let mut snoozes = offered.iter().filter_map(|action| match action {
            ShelfAction::Snooze(snooze) => Some((*action, *snooze)),
            _ => None,
        });
        if snoozing {
            for (action, snooze) in snoozes {
                buttons.push(
                    button(snooze.id, action)
                        .label(snooze.short)
                        .tooltip(snooze.long)
                        .into_any_element(),
                );
            }
        } else {
            if snoozes.next().is_some() {
                buttons.push(
                    Button::new("snooze")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(Lucide::AlarmClock))
                        .tooltip("Snooze")
                        .on_click(cx.listener(|this, _, _, cx| this.start_snoozing(cx)))
                        .into_any_element(),
                );
            }
            for action in offered {
                let id = match action {
                    ShelfAction::Settle => "settle",
                    ShelfAction::Restore => "restore",
                    ShelfAction::Snooze(_) => continue,
                };
                buttons.push(
                    button(id, *action)
                        .icon(action.icon())
                        .label(action.label())
                        .into_any_element(),
                );
            }
        }
        Some(
            h_flex()
                .flex_shrink_0()
                .h(rems(LINE_REMS))
                // The buttons act on the row; they must not also open it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .children(buttons)
                .into_any_element(),
        )
    }

    /// The items in `range`. Called by [`v_virtual_list`] with only what is
    /// on screen.
    fn render_items(&self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let clock = Clock {
            now: time::now_unix(),
            instant: Instant::now(),
        };
        let view = cx.entity().downgrade();
        let cx: &Context<'_, Self> = cx;
        let workspaces = self.workspaces.read(cx);
        self.list.items()[range]
            .iter()
            .map(|&item| match item {
                Item::Session(row) => {
                    let row = self.list.row_at(row);
                    self.render_row(row, workspaces.get(row.workspace), clock, &view, cx)
                }
                Item::Shelved(row) => {
                    let row = self.list.row_at(row);
                    self.render_shelved(row, workspaces.get(row.workspace), clock, &view, cx)
                }
                Item::Header(which) => self.render_shelf_header(which, cx),
                Item::Gap => div().into_any_element(),
            })
            .collect()
    }

    fn render_row(
        &self,
        row: &Row,
        workspace: &Workspace,
        clock: Clock,
        view: &WeakEntity<Self>,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let state = self.row_state(row);
        let right = state
            .hovered
            .then(|| self.render_actions(row, cx))
            .flatten()
            .unwrap_or_else(|| status(row, clock, theme));
        let inner = v_flex()
            .id(row.element_id.clone())
            .test_support()
            .gap(rems(GAP_REMS))
            .py(rems(PAD_REMS))
            .child(workspace_line(workspace, theme).child(right))
            .child(
                div()
                    .h(rems(TITLE_REMS))
                    .truncate()
                    .text_sm()
                    .when(state.active, StyledExt::font_medium)
                    .child(row.title()),
            )
            .child(branch_line(workspace, theme));
        Self::row_frame(rems(ROW_REMS), row, state, inner, view, cx).into_any_element()
    }

    fn row_state(&self, row: &Row) -> RowState {
        RowState {
            active: self.active.as_ref() == Some(&row.id),
            hovered: self.hover.as_ref().is_some_and(|hover| hover.id == row.id),
        }
    }

    /// What every session row shares: its slot in the list, the highlight,
    /// hover tracking, a click that opens the session and the context menu.
    /// Kit has no list-item element with a fixed height for
    /// `v_virtual_list`, so rows are laid out by hand.
    fn row_frame<E>(
        height: Rems,
        row: &Row,
        state: RowState,
        inner: E,
        view: &WeakEntity<Self>,
        cx: &Context<'_, Self>,
    ) -> impl IntoElement + use<E>
    where
        E: StatefulInteractiveElement + ParentElement + Styled + IntoElement + 'static,
    {
        let theme = cx.theme();
        let accent = theme.sidebar_accent;
        let open_id = row.id.clone();
        let menu = (view.clone(), MenuRow::new(row));
        let (hover_id, hover_view) = (row.id.clone(), view.clone());
        let inner = inner
            // Its accessible name; UI tests read the title from it.
            .aria_label(row.title())
            .size_full()
            .px_2()
            .rounded(theme.radius)
            .cursor_pointer()
            .map(|this| match (state.active, state.hovered) {
                (true, _) => this.bg(accent).text_color(theme.sidebar_accent_foreground),
                (false, true) => this.bg(accent.opacity(0.5)),
                (false, false) => this,
            })
            .on_hover(move |hovered, _, cx| {
                let _ = hover_view.update(cx, |this, cx| {
                    if *hovered {
                        this.hover(&hover_id, cx);
                    } else {
                        this.unhover(&hover_id, cx);
                    }
                });
            })
            .on_click(cx.listener(move |this, _, _, cx| this.open(&open_id, cx)));
        div()
            .h(height)
            .px_2()
            .py(rems(GUTTER_REMS))
            .child(inner.context_menu(move |popup, _, _| {
                let (view, row) = &menu;
                row_menu(popup, view, row)
            }))
    }

    /// A snoozed or settled session on one line: badge, title, and when it
    /// wakes or how old it is. Hovering it swaps that for Restore and, after
    /// a moment, shows the session's details beside the sidebar.
    fn render_shelved(
        &self,
        row: &Row,
        workspace: &Workspace,
        clock: Clock,
        view: &WeakEntity<Self>,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let state = self.row_state(row);
        let wakes = self.shelves.wakes(row.id.as_str());
        let offered = ShelfAction::offered(row.shelf, row.activity);
        let right = if state.hovered && !offered.is_empty() {
            // Icon-only: the row is one line.
            h_flex()
                // The buttons act on the row; they must not also open it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .children(offered.iter().filter_map(|&action| {
                    let id = match action {
                        ShelfAction::Restore => "restore",
                        ShelfAction::Settle => "settle",
                        ShelfAction::Snooze(_) => return None,
                    };
                    let row_id = row.id.clone();
                    Some(
                        Button::new(id)
                            .ghost()
                            .xsmall()
                            .icon(action.icon())
                            .tooltip(action.label())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.shelve(&row_id, action, cx);
                            })),
                    )
                }))
                .into_any_element()
        } else if let (Shelf::Snoozed, Some(wakes)) = (row.shelf, wakes) {
            h_flex()
                .gap_1()
                .text_color(theme.blue)
                .child(Icon::new(Lucide::AlarmClock).xsmall())
                .child(time::relative_age(wakes - clock.now))
                .into_any_element()
        } else {
            status(row, clock, theme)
        };
        let card = Card {
            title: row.title(),
            workspace: workspace.name.clone(),
            initials: workspace.initials.clone(),
            branch: workspace.branch.clone(),
            wakes: wakes
                .filter(|_| row.shelf == Shelf::Snoozed)
                .map(|wakes| format!("Wakes in {}", time::relative_age(wakes - clock.now))),
        };
        let line = h_flex()
            .id(row.element_id.clone())
            .test_support()
            .gap_2()
            .py(rems(SHELVED_PAD_REMS))
            .text_xs()
            // Shelved badges recede until the row is hovered or open.
            .child(
                badge(card.initials.clone())
                    .when(!(state.active || state.hovered), |this| this.opacity(0.4)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(rems(TITLE_REMS))
                    .truncate()
                    .text_sm()
                    .child(card.title.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground)
                    .child(right),
            );
        let trigger = HoverCard::new(row.element_id.clone())
            .anchor(Anchor::LeftCenter)
            .trigger_style(StyleRefinement::default().size_full())
            .ml_3()
            .content(move |_, _, cx| card.render(cx));
        let line = Self::row_frame(rems(SHELVED_ROW_REMS), row, state, line, view, cx);
        div()
            .w_full()
            .h(rems(SHELVED_ROW_REMS))
            .child(trigger.trigger(line))
            .into_any_element()
    }

    /// "Snoozed 1 ⌄": the shelf's name and count; a click opens or folds it.
    /// Kit's `Accordion` owns its items, which here are separate rows of
    /// the virtual list, so the header is laid out by hand.
    fn render_shelf_header(&self, which: Shelf, cx: &Context<'_, Self>) -> AnyElement {
        let id = match which {
            Shelf::Snoozed => "toggle-snoozed",
            Shelf::Settled | Shelf::Active => "toggle-settled",
        };
        let theme = cx.theme();
        let hover = theme.sidebar_accent.opacity(0.5);
        let chevron = if self.list.is_open(which) {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        div()
            .h(rems(HEADER_REMS))
            .px_2()
            .py(rems(GUTTER_REMS))
            .child(
                h_flex()
                    .id(id)
                    .test_support()
                    .size_full()
                    .px_2()
                    .gap_1()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .text_sm()
                    .hover(move |style| style.bg(hover))
                    .child(div().flex_1().child(which.label()))
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(self.list.count(which).to_string()),
                    )
                    .child(
                        Icon::new(chevron)
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(which, cx))),
            )
            .into_any_element()
    }

    fn render_header(cx: &mut Context<'_, Self>) -> impl IntoElement {
        h_flex()
            .flex_shrink_0()
            .h(px(44.))
            .px_3()
            .justify_between()
            .child(div().font_semibold().child("rho"))
            .child(
                Button::new("new-session")
                    .ghost()
                    .small()
                    .icon(Lucide::SquarePen)
                    .tooltip("New session (Ctrl+N)")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::NewSession))),
            )
    }

    fn render_list(&self, window: &Window, cx: &mut Context<'_, Self>) -> AnyElement {
        if self.list.is_empty() {
            let message = if self.list.query().is_empty() {
                "No sessions yet"
            } else {
                "No matching sessions"
            };
            return Empty::new()
                .header(EmptyHeader::new().description(EmptyDescription::new().child(message)))
                .into_any_element();
        }
        // Heights are known, so the list never measures a row.
        let rem = window.rem_size();
        let mut sizes: Vec<_> = self
            .list
            .items()
            .iter()
            .map(|&item| size(px(0.), rem * item_rems(item)))
            .collect();
        // The gap takes whatever height the rows leave, pushing the shelves
        // to the bottom; once they overflow it is gone.
        if let Some(gap) = self.list.items().iter().position(|&item| item == Item::Gap) {
            let rows: Pixels = sizes.iter().map(|size| size.height).sum();
            sizes[gap].height = (self.viewport - rows).max(Pixels::ZERO);
        }
        let view = cx.entity().downgrade();
        // Kit's list doesn't report its height, so a canvas behind it
        // measures it and redraws the sidebar when it changes.
        let measure = canvas(
            move |bounds, _, cx| {
                let _ = view.update(cx, |this, cx| {
                    if this.viewport != bounds.size.height {
                        this.viewport = bounds.size.height;
                        cx.notify();
                    }
                });
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full();
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(measure)
            .child(
                v_virtual_list(
                    cx.entity(),
                    "sessions",
                    Rc::new(sizes),
                    |this, range, _, cx| this.render_items(range, cx),
                )
                .track_scroll(&self.scroll)
                .size_full(),
            )
            .into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .flex_shrink_0()
            .py_2()
            .gap_1()
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div().px_2().child(
                        Alert::error("sessions-error", error)
                            .title("Couldn't load sessions")
                            .small(),
                    ),
                )
            })
            .child(
                h_flex()
                    .px_2()
                    .gap_2()
                    .child(
                        Button::new("open-settings")
                            .ghost()
                            .small()
                            .icon(IconName::Settings)
                            .tooltip("Settings (Ctrl+,)")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(SidebarEvent::OpenSettings);
                            })),
                    )
                    .children(self.agent.clone().map(|agent| {
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(agent)
                    })),
            )
    }
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(Self::render_header(cx))
            .child(
                div().flex_shrink_0().px_2().pb_2().child(
                    Input::new(&self.search)
                        .small()
                        .prefix(Icon::new(IconName::Search).small().text_color(muted))
                        .cleanable(true),
                ),
            )
            .child(self.render_list(window, cx))
            .child(self.render_footer(cx))
    }
}

/// The time, read once per frame for every row's age and timer.
#[derive(Clone, Copy)]
struct Clock {
    /// Unix seconds, for ages.
    now: i64,
    /// For Working and Waiting timers.
    instant: Instant,
}

/// Badge and workspace name; the caller adds the status or actions.
fn workspace_line(workspace: &Workspace, theme: &Theme) -> gpui_kit::Div {
    h_flex()
        .h(rems(LINE_REMS))
        .gap_2()
        .text_xs()
        .child(badge(workspace.initials.clone()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(workspace.name.clone()),
        )
}

/// A workspace's initials, in colors Kit picks from them.
fn badge(initials: SharedString) -> Avatar {
    Avatar::new()
        .name(initials)
        .xsmall()
        // In rems, so it follows the UI scale; square like a workspace tile.
        .size(rems(LINE_REMS))
        .rounded(rems(0.25))
}

/// How a row is lit: open in the pane, under the pointer, or both.
#[derive(Clone, Copy)]
struct RowState {
    active: bool,
    hovered: bool,
}

/// A shelved session's details, shown beside the sidebar while hovered.
/// Built with the row, so the card needs nothing from the view.
struct Card {
    title: SharedString,
    workspace: SharedString,
    initials: SharedString,
    branch: Option<SharedString>,
    wakes: Option<String>,
}

impl Card {
    fn render(&self, cx: &gpui_kit::App) -> impl IntoElement {
        let muted = |icon: Lucide| {
            Icon::new(icon)
                .small()
                .text_color(cx.theme().muted_foreground)
                .into_any_element()
        };
        // Icons stand in for the labels, as in T3 Code.
        let mut details = DescriptionList::horizontal()
            .bordered(false)
            .columns(1)
            // The icon plus a gap: unbordered lists have no padding.
            .label_width(rems(LINE_REMS + 0.75))
            .item(
                badge(self.initials.clone()).into_any_element(),
                self.workspace.clone(),
                1,
            );
        if let Some(branch) = &self.branch {
            details = details.item(muted(Lucide::GitBranch), branch.clone(), 1);
        }
        if let Some(wakes) = &self.wakes {
            details = details.item(muted(Lucide::AlarmClock), wakes.clone(), 1);
        }
        v_flex()
            .id("session-card")
            .test_support()
            .w(rems(20.))
            .gap_1p5()
            .text_sm()
            .child(div().text_sm().font_medium().child(self.title.clone()))
            .child(details)
    }
}

/// The branch, when the workspace is a git repository.
fn branch_line(workspace: &Workspace, theme: &Theme) -> gpui_kit::Div {
    h_flex()
        .h(rems(LINE_REMS))
        .gap_1()
        .min_w_0()
        .text_xs()
        .text_color(theme.muted_foreground)
        .when_some(workspace.branch.clone(), |this, branch| {
            this.child(Icon::new(Lucide::GitBranch).xsmall())
                .child(div().truncate().child(branch))
        })
}

/// "Working 24m", "Waiting 9m", or the age: "3h". Icons are static: an
/// animated one would redraw the whole sidebar every frame.
fn status(row: &Row, clock: Clock, theme: &Theme) -> AnyElement {
    let label = |word: &str, since: Instant| match time::elapsed(
        clock.instant.duration_since(since).as_secs(),
    ) {
        Some(elapsed) => format!("{word} {elapsed}"),
        None => word.to_owned(),
    };
    let busy = |color: Hsla, icon: Icon, text: String| {
        h_flex()
            .flex_shrink_0()
            .gap_1()
            .text_color(color)
            .child(icon.xsmall())
            .child(text)
            .into_any_element()
    };
    match row.activity {
        Activity::Working(since) => busy(
            theme.blue,
            Icon::new(IconName::LoaderCircle),
            label("Working", since),
        ),
        Activity::Waiting(since) => busy(
            theme.warning,
            Icon::new(Lucide::CircleDashed),
            label("Waiting", since),
        ),
        Activity::Idle => div()
            .flex_shrink_0()
            .text_color(theme.muted_foreground)
            .when(row.updated > 0, |this| {
                this.child(time::relative_age(clock.now - row.updated))
            })
            .into_any_element(),
    }
}

/// The right-click menu: the same moves the hover buttons offer, then
/// Delete. A busy session offers neither.
fn row_menu(menu: PopupMenu, view: &WeakEntity<SidebarView>, row: &MenuRow) -> PopupMenu {
    let menu = row.offered.iter().fold(menu, |menu, &action| {
        let (view, id) = (view.clone(), row.id.clone());
        menu.item(
            PopupMenuItem::new(action.label())
                .icon(action.icon())
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.shelve(&id, action, cx));
                }),
        )
    });
    if !row.deletable {
        return menu;
    }
    let (view, id, title) = (view.clone(), row.id.clone(), row.title.clone());
    menu.separator().item(
        PopupMenuItem::new("Delete…")
            .icon(Icon::new(IconName::Delete))
            .on_click(move |_, window, cx| {
                let (view, id, title) = (view.clone(), id.clone(), title.clone());
                window.open_alert_dialog(cx, move |alert, _, cx| {
                    let (view, id) = (view.clone(), id.clone());
                    alert
                        .icon(Icon::new(IconName::Delete).text_color(cx.theme().danger))
                        .title(format!("Delete \"{title}\"?"))
                        .description("rho deletes the saved session. This can't be undone.")
                        .confirm()
                        .ok_text("Delete")
                        .ok_variant(ButtonVariant::Danger)
                        .on_ok(move |_, _, cx| {
                            let id = id.clone();
                            let _ = view.update(cx, |_, cx| cx.emit(SidebarEvent::Delete { id }));
                            true
                        })
                });
            }),
    )
}

/// What a row's menu needs, captured when the row is drawn.
struct MenuRow {
    id: SessionId,
    title: SharedString,
    offered: &'static [ShelfAction],
    /// Busy sessions can't be deleted.
    deletable: bool,
}

impl MenuRow {
    fn new(row: &Row) -> Self {
        Self {
            id: row.id.clone(),
            title: row.title(),
            offered: ShelfAction::offered(row.shelf, row.activity),
            deletable: row.activity.is_idle(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_sessions_offer_no_moves_and_shelves_offer_a_way_back() {
        assert!(ShelfAction::offered(Shelf::Active, Activity::Working(Instant::now())).is_empty());
        assert_eq!(
            ShelfAction::offered(Shelf::Settled, Activity::Idle),
            [ShelfAction::Restore]
        );
        assert!(
            ShelfAction::offered(Shelf::Snoozed, Activity::Idle).contains(&ShelfAction::Restore)
        );
    }
}
