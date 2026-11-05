//! The thread list, drawn: one flat list, newest first. Each row shows the
//! project, the thread's status or age, its title, and its branch. Hovering
//! a row swaps its status for Snooze and Settle; settled and snoozed threads
//! have their own lists, opened from the bottom of the sidebar.
//!
//! The app draws this view as a cached view, so a hover or scroll here
//! redraws only the sidebar, and [`uniform_list`] builds only the rows on
//! screen. The data lives in [`ThreadList`] and [`Projects`], built when it
//! changes, never per frame.

use std::{
    ops::Range,
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt, Theme,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, uniform_list, AnyElement, AppContext as _, Context, Entity, EventEmitter, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, ScrollStrategy,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    UniformListScrollHandle, WeakEntity, Window,
};
use rho_ui_core::{SessionId, SessionSummary};

use crate::{
    projects::{Project, Projects, PALETTE_SIZE},
    shelf::{Shelf, Shelves},
    thread_list::{Activity, LiveThread, Row, SavedThread, ThreadList},
    time,
};

/// Every row's height. [`uniform_list`] needs one height for all rows; this
/// fits three lines (12, 14 and 12 px text) with padding.
pub(crate) const ROW_HEIGHT: f32 = 64.;

/// How often ages, timers and snooze wake-ups refresh. They show whole
/// minutes, so 30 s keeps them current.
const TICK: Duration = Duration::from_secs(30);

/// How long a thread can be snoozed for.
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

/// Moving a thread between shelves.
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

    /// What a row offers, inline and in its menu. A busy thread offers
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
    /// Open a thread. `title` is the saved one, if any.
    Open {
        id: SessionId,
        cwd: PathBuf,
        title: Option<SharedString>,
    },
    NewThread,
}

/// The row under the mouse, which shows its actions instead of its status.
struct Hover {
    id: SessionId,
    /// The snooze button was pressed: show the durations.
    snoozing: bool,
}

pub(crate) struct SidebarView {
    list: ThreadList,
    projects: Entity<Projects>,
    shelves: Shelves,
    active: Option<SessionId>,
    hover: Option<Hover>,
    creating: bool,
    error: Option<SharedString>,
    /// The agent's name and version, for the footer.
    agent: Option<SharedString>,
    search: Entity<InputState>,
    scroll: UniformListScrollHandle,
    _subscriptions: [Subscription; 2],
    _tick: Task<()>,
}

impl EventEmitter<SidebarEvent> for SidebarView {}

impl SidebarView {
    pub(crate) fn new(
        projects: Entity<Projects>,
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
            // Project names are searchable.
            cx.observe(&projects, |this, _, cx| this.rebuild(cx)),
        ];
        let tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            if this.update(cx, Self::rebuild).is_err() {
                break;
            }
        });
        Self {
            list: ThreadList::default(),
            projects,
            shelves,
            active: None,
            hover: None,
            creating: false,
            error: None,
            agent: None,
            search,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
            _tick: tick,
        }
    }

    /// Replaces the saved threads with a fresh `list_sessions` result.
    pub(crate) fn set_saved(&mut self, sessions: Vec<SessionSummary>, cx: &mut Context<'_, Self>) {
        let saved = self.projects.update(cx, |projects, cx| {
            sessions
                .into_iter()
                .map(|session| SavedThread {
                    project: projects.index(session.cwd(), cx),
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

    /// Records an open thread's title and activity.
    pub(crate) fn set_live(&mut self, live: LiveThread, cx: &mut Context<'_, Self>) {
        let project = self
            .projects
            .update(cx, |projects, cx| projects.index(&live.cwd, cx));
        if self.list.set_live(live, project, time::now_unix()) {
            self.rebuild(cx);
        }
    }

    /// Forgets every open thread, after the agent restarts.
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

    /// `starting` counts new threads still being made.
    pub(crate) fn set_creating(&mut self, starting: usize, cx: &mut Context<'_, Self>) {
        let creating = starting > 0;
        if self.creating != creating {
            self.creating = creating;
            cx.notify();
        }
    }

    pub(crate) fn set_agent(&mut self, agent: String, cx: &mut Context<'_, Self>) {
        self.agent = Some(agent.into());
        cx.notify();
    }

    fn rebuild(&mut self, cx: &mut Context<'_, Self>) {
        let projects = self.projects.read(cx);
        self.list.rebuild(&self.shelves, time::now_unix(), |ix| {
            projects.get(ix).name.as_ref()
        });
        cx.notify();
    }

    fn show(&mut self, list: Shelf, cx: &mut Context<'_, Self>) {
        self.list.show(list);
        self.hover = None;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
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
                .h(px(16.))
                // The buttons act on the row; they must not also open it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .children(buttons)
                .into_any_element(),
        )
    }

    /// The rows in `range` of the shown list. Called by [`uniform_list`]
    /// with only what is on screen.
    fn render_rows(&self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let clock = Clock {
            now: time::now_unix(),
            instant: Instant::now(),
        };
        let view = cx.entity().downgrade();
        let cx: &Context<'_, Self> = cx;
        let projects = self.projects.read(cx);
        range
            .filter_map(|ix| self.list.shown(ix))
            .map(|row| self.render_row(row, projects.get(row.project), clock, &view, cx))
            .collect()
    }

    fn render_row(
        &self,
        row: &Row,
        project: &Project,
        clock: Clock,
        view: &WeakEntity<Self>,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let active = self.active.as_ref() == Some(&row.id);
        let hovered = self.hover.as_ref().is_some_and(|hover| hover.id == row.id);
        let right = hovered
            .then(|| self.render_actions(row, cx))
            .flatten()
            .unwrap_or_else(|| status(row, clock, theme));
        let accent = theme.sidebar_accent;
        let open_id = row.id.clone();
        let hover_id = row.id.clone();
        let menu = (
            view.clone(),
            row.id.clone(),
            ShelfAction::offered(row.shelf, row.activity),
        );
        div()
            .h(px(ROW_HEIGHT))
            .px_2()
            .py(px(1.))
            .child(
                v_flex()
                    .id(row.element_id.clone())
                    .test_support()
                    .size_full()
                    .justify_center()
                    .gap(px(2.))
                    .px_2()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .map(|this| match (active, hovered) {
                        (true, _) => this.bg(accent).text_color(theme.sidebar_accent_foreground),
                        (false, true) => this.bg(accent.opacity(0.5)),
                        (false, false) => this,
                    })
                    .child(project_line(project, theme).child(right))
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .when(active, StyledExt::font_medium)
                            .child(row.title()),
                    )
                    .child(branch_line(project, theme))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.hover(&hover_id, cx);
                        } else {
                            this.unhover(&hover_id, cx);
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.open(&open_id, cx)))
                    .context_menu(move |popup, _, _| {
                        let (view, id, offered) = &menu;
                        row_menu(popup, view, id, offered)
                    }),
            )
            .into_any_element()
    }

    fn render_header(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        h_flex()
            .flex_shrink_0()
            .h(px(44.))
            .px_3()
            .justify_between()
            .child(div().font_semibold().child("rho"))
            .child(
                Button::new("new-thread")
                    .ghost()
                    .small()
                    .icon(Lucide::SquarePen)
                    .loading(self.creating)
                    .tooltip("New thread (Ctrl+N)")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::NewThread))),
            )
    }

    fn render_list(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        if self.list.is_empty() {
            let message = match (self.list.query().is_empty(), self.list.list()) {
                (false, _) => "No matching threads",
                (true, Shelf::Active) => "No threads yet",
                (true, Shelf::Snoozed) => "Nothing snoozed",
                (true, Shelf::Settled) => "Nothing settled",
            };
            return div()
                .flex_1()
                .px_4()
                .py_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message)
                .into_any_element();
        }
        uniform_list(
            "threads",
            self.list.len(),
            cx.processor(|this, range, _, cx| this.render_rows(range, cx)),
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .into_any_element()
    }

    /// "Snoozed 3 >" or "Settled 191 >", under the active list.
    fn shelf_entry(shelf: Shelf, count: usize, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let id = match shelf {
            Shelf::Snoozed => "show-snoozed",
            Shelf::Settled | Shelf::Active => "show-settled",
        };
        let theme = cx.theme();
        let hover = theme.sidebar_accent.opacity(0.5);
        h_flex()
            .id(id)
            .test_support()
            .mx_2()
            .px_2()
            .py_1()
            .gap_1()
            .rounded(theme.radius)
            .cursor_pointer()
            .text_sm()
            .hover(move |style| style.bg(hover))
            .child(div().flex_1().child(shelf.label()))
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(count.to_string()),
            )
            .child(
                Icon::new(IconName::ChevronRight)
                    .xsmall()
                    .text_color(theme.muted_foreground),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.show(shelf, cx)))
    }

    fn render_footer(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let entries: Vec<AnyElement> = match self.list.list() {
            Shelf::Active => [Shelf::Snoozed, Shelf::Settled]
                .into_iter()
                .map(|shelf| (shelf, self.list.shelf_count(shelf)))
                .filter(|(_, count)| *count > 0)
                .map(|(shelf, count)| Self::shelf_entry(shelf, count, cx).into_any_element())
                .collect(),
            Shelf::Snoozed | Shelf::Settled => Vec::new(),
        };
        let theme = cx.theme();
        v_flex()
            .flex_shrink_0()
            .py_2()
            .gap_1()
            .children(entries)
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_4()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(format!("Couldn't load threads: {error}")),
                )
            })
            .when_some(self.agent.clone(), |this, agent| {
                this.child(
                    div()
                        .px_4()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(agent),
                )
            })
    }
}

impl Render for SidebarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let showing = self.list.list();
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(self.render_header(cx))
            .child(
                div().flex_shrink_0().px_2().pb_2().child(
                    Input::new(&self.search)
                        .small()
                        .prefix(Icon::new(IconName::Search).small().text_color(muted))
                        .cleanable(true),
                ),
            )
            .when(showing != Shelf::Active, |this| {
                this.child(
                    div().flex_shrink_0().px_2().pb_1().child(
                        Button::new("show-active")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronLeft)
                            .label(showing.label())
                            .on_click(cx.listener(|this, _, _, cx| this.show(Shelf::Active, cx))),
                    ),
                )
            })
            .child(self.render_list(cx))
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

/// Badge and project name; the caller adds the status or actions.
fn project_line(project: &Project, theme: &Theme) -> gpui_kit::Div {
    let color = palette(theme)[project.hue];
    h_flex()
        .gap_2()
        .text_xs()
        .child(
            div()
                .flex_shrink_0()
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .bg(color.opacity(0.2))
                .text_color(color)
                .text_size(px(8.))
                .font_bold()
                .child(project.initials.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(project.name.clone()),
        )
}

/// The branch, when the workspace is a git repository.
fn branch_line(project: &Project, theme: &Theme) -> gpui_kit::Div {
    h_flex()
        .gap_1()
        .min_w_0()
        .text_xs()
        .text_color(theme.muted_foreground)
        .when_some(project.branch.clone(), |this, branch| {
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

/// Badge colors, one per project hue.
fn palette(theme: &Theme) -> [Hsla; PALETTE_SIZE] {
    [
        theme.blue,
        theme.magenta,
        theme.green,
        theme.yellow,
        theme.cyan,
        theme.red,
    ]
}

/// The right-click menu: the same moves the hover buttons offer.
fn row_menu(
    menu: PopupMenu,
    view: &WeakEntity<SidebarView>,
    id: &SessionId,
    offered: &[ShelfAction],
) -> PopupMenu {
    offered.iter().fold(menu, |menu, &action| {
        let (view, id) = (view.clone(), id.clone());
        menu.item(
            PopupMenuItem::new(action.label())
                .icon(action.icon())
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.shelve(&id, action, cx));
                }),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_threads_offer_no_moves_and_shelves_offer_a_way_back() {
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
