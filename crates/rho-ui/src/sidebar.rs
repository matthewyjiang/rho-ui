//! The session list, drawn: one scrolling list of every machine's active
//! sessions, pinned first and the rest ranked as an inbox (see
//! [`crate::session_list`]), then the Snoozed and Settled shelves, held at the
//! bottom until opened shelves fill the list. Drafts of new sessions the
//! user left with a message lead it, saying Draft; hovering one offers
//! Discard. An active row shows the
//! workspace, the session's status or age, its title, its branch, and with
//! more than one machine, the machine's label; hovering it swaps the status
//! for Snooze and Settle, or Unpin. Pinned sessions lead the list, and a
//! turn that finished off screen shows Done until its session is opened.
//! Right-click a row for its menu, which also renames it in place. A shelf
//! opens from its header into one-line rows, its newest 25 at first, and
//! Show more under them adds 25 more at a time. Hovering a row shows its
//! details in a card. The filter beside the
//! search hides machines or keeps to chosen projects, and the footer says
//! which machines aren't connected, with what would fix it (Retry, a
//! restart of a daemon too old for rho-ui or of another build, Start for
//! one the user stopped), and why any remote machine's repositories
//! couldn't be read.
//!
//! Drag the open sidebar's right edge to resize it, and double-click the
//! edge for its default width. The sidebar collapses to a rail of buttons;
//! see [`rail`].
//!
//! The app draws this view as a cached view, so a hover or scroll here
//! redraws only the sidebar, and [`v_virtual_list`] builds only the rows on
//! screen.
//! The data lives in [`SessionList`] and [`Workspaces`], built when it
//! changes, never per frame.

use std::{ops::Range, path::PathBuf, rc::Rc, time::Duration};

use web_time::Instant;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    alert::Alert,
    button::{Button, ButtonVariant, ButtonVariants as _},
    description_list::DescriptionList,
    empty::{Empty, EmptyDescription, EmptyHeader},
    h_flex,
    hover_card::HoverCard,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    resizable::{resize_handle, HandleEdge},
    v_flex, v_virtual_list, ActiveTheme as _, Icon, IconName, InteractiveElementExt as _,
    Sizable as _, StyledExt, Theme, VirtualListScrollHandle, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    canvas, div, px, rems, size, Anchor, AnyElement, App, AppContext as _, Axis, ClipboardItem,
    Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable as _, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Rems,
    Render, ScrollStrategy, SharedString, StatefulInteractiveElement, StyleRefinement, Styled,
    Subscription, Task, WeakEntity, Window,
};
use rho_ui_core::{Job, SessionSummary};

use crate::{
    app_update::{self, AppUpdate},
    badge::Badge,
    machines::{self, DaemonAction, MachineId, MachineStatus, Machines, Place, SessionKey},
    projects::Projects,
    pull_requests::{PrLinks, PrSummary, PullRequests, Rollup, Target, Watch},
    rho_install::{render_offer, Installs},
    right_panel::state_look,
    session_list::{
        Activity, DraftRow, Item, JobSession, LiveSession, Row, SavedSession, SessionList,
        SHELF_PAGE,
    },
    shelf::{Shelf, Shelves},
    store::StateDb,
    time,
    title_bar::TOP_ROW_REMS,
    unseen::Unseen,
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
const ROW_REMS: f32 =
    2. * LINE_REMS + TITLE_REMS + 2. * GAP_REMS + 2. * PAD_REMS + 2. * GUTTER_REMS;

/// Inside a one-line shelved row's highlight, above and below its title.
const SHELVED_PAD_REMS: f32 = 0.5;

/// A snoozed or settled row's height.
const SHELVED_ROW_REMS: f32 = TITLE_REMS + 2. * SHELVED_PAD_REMS + 2. * GUTTER_REMS;

/// The most a machine's label takes in a row, so a long one leaves the
/// title and branch room: about a quarter of the open sidebar's default
/// 18 rem.
const MACHINE_LABEL_REMS: f32 = 5.;

/// A shelf header's height.
const HEADER_REMS: f32 = 2.5;

/// The sidebar's top row, open or collapsed, so the toggle sits at the same
/// height in both, and as tall as the main pane's title bar.
fn top_row() -> Rems {
    rems(TOP_ROW_REMS)
}

mod filter;
mod rail;

pub(crate) use filter::SavedFilter;
pub(crate) use rail::SidebarLayout;
use rail::{toggle_button, Layout};

/// Each item's height. The gap's comes from the list's height instead; see
/// [`SidebarView::render_list`].
fn item_rems(item: Item) -> f32 {
    match item {
        Item::Draft(_) | Item::Session(_) => ROW_REMS,
        Item::Shelved(_) | Item::More(_) => SHELVED_ROW_REMS,
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
    /// The label in the context menu's Snooze submenu.
    long: &'static str,
    seconds: i64,
}

const SNOOZES: [Snooze; 3] = [
    Snooze {
        id: "snooze-1h",
        short: "1h",
        long: "1 hour",
        seconds: 3600,
    },
    Snooze {
        id: "snooze-4h",
        short: "4h",
        long: "4 hours",
        seconds: 4 * 3600,
    },
    Snooze {
        id: "snooze-1d",
        short: "1d",
        long: "1 day",
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
    /// nothing: it shows as active until its turn ends. Nor does a pinned
    /// one, which stays active until unpinned.
    fn for_row(row: &Row) -> &'static [ShelfAction] {
        if row.pinned.is_some() {
            return &[];
        }
        Self::offered(row.shelf, row.activity)
    }

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

    fn label(self) -> SharedString {
        match self {
            ShelfAction::Settle => "Settle".into(),
            ShelfAction::Snooze(snooze) => format!("Snooze {}", snooze.long).into(),
            ShelfAction::Restore => "Restore".into(),
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
        id: SessionKey,
        cwd: PathBuf,
        title: Option<SharedString>,
    },
    NewSession,
    /// Show a listed draft.
    OpenDraft(EntityId),
    /// Drop a listed draft, message and all.
    DiscardDraft(EntityId),
    /// Delete a saved session; the user has confirmed.
    Delete {
        id: SessionKey,
    },
    /// Save `title` as the session's title. The row shows it already.
    Rename {
        id: SessionKey,
        title: SharedString,
    },
    OpenSettings,
    /// Collapse the sidebar to its rail, or open it again; see
    /// [`SidebarView::toggle_collapsed`].
    ToggleCollapsed,
    /// The open sidebar went back to its default width; lay it out again.
    Resized,
}

/// What a drag of the open sidebar's edge carries. The drag draws nothing:
/// the sidebar itself follows the pointer.
#[derive(Clone, Copy)]
pub(crate) struct ResizeSidebar;

impl Render for ResizeSidebar {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        gpui_kit::Empty
    }
}

/// The row under the mouse, which shows its actions instead of its status.
struct Hover {
    id: SessionKey,
    /// The snooze button was pressed: show the durations.
    snoozing: bool,
}

/// A row whose title is being edited in place. Enter or leaving the box
/// saves it; Escape doesn't.
struct Renaming {
    id: SessionKey,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// What the sidebar remembers between runs, each part loaded from the
/// state database: settled and snoozed sessions, the ones Done and not yet
/// seen, whether it is collapsed, and its filter.
pub(crate) struct SidebarState {
    pub(crate) shelves: Shelves,
    pub(crate) unseen: Unseen,
    pub(crate) layout: SidebarLayout,
    pub(crate) filter: SavedFilter,
}

impl SidebarState {
    pub(crate) fn load(db: &StateDb, now: i64) -> Self {
        Self {
            shelves: Shelves::load(db, now),
            unseen: Unseen::load(db),
            layout: SidebarLayout::load(db),
            filter: SavedFilter::load(db),
        }
    }
}

pub(crate) struct SidebarView {
    list: SessionList,
    workspaces: Entity<Workspaces>,
    machines: Entity<Machines>,
    /// Each machine's rho, which the footer offers to install or update.
    installs: Entity<Installs>,
    /// A newer rho-ui, which the footer offers to install.
    app_update: Entity<AppUpdate>,
    projects: Entity<Projects>,
    /// Each active row's PR, which its branch line marks.
    prs: Entity<PullRequests>,
    shelves: Shelves,
    layout: SidebarLayout,
    saved_filter: SavedFilter,
    active: Option<SessionKey>,
    /// The listed draft on screen.
    active_draft: Option<EntityId>,
    hover: Option<Hover>,
    /// The draft row under the mouse, which offers Discard.
    draft_hover: Option<EntityId>,
    renaming: Option<Renaming>,
    search: Entity<InputState>,
    scroll: VirtualListScrollHandle,
    /// The list's height in the last frame, which the gap above the
    /// shelves fills up to.
    viewport: Pixels,
    _subscriptions: [Subscription; 8],
    _tick: Task<()>,
}

/// The app's models the sidebar reads.
pub(crate) struct SidebarModels {
    pub(crate) workspaces: Entity<Workspaces>,
    /// Labels each row's machine.
    pub(crate) machines: Entity<Machines>,
    /// What each machine's rho needs, which the footer offers.
    pub(crate) installs: Entity<Installs>,
    /// A newer rho-ui, which the footer offers.
    pub(crate) app_update: Entity<AppUpdate>,
    /// What the filter offers.
    pub(crate) projects: Entity<Projects>,
    /// Each active row's PR.
    pub(crate) prs: Entity<PullRequests>,
}

impl EventEmitter<SidebarEvent> for SidebarView {}

impl SidebarView {
    /// The sidebar over `models`, as `state` last left it.
    pub(crate) fn new(
        models: SidebarModels,
        state: SidebarState,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let SidebarModels {
            workspaces,
            machines,
            installs,
            app_update,
            projects,
            prs,
        } = models;
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
            // Labels changed, or a machine came or went.
            cx.observe(&machines, |_, _, cx| cx.notify()),
            cx.observe(&projects, |this, projects, cx| {
                let roots = projects.read(cx).roots().to_vec();
                this.prune_filter(|filter| filter.keep_projects(&roots));
                this.rebuild(cx);
            }),
            cx.observe(&installs, |_, _, cx| cx.notify()),
            cx.observe(&app_update, |_, _, cx| cx.notify()),
            cx.observe(&prs, |_, _, cx| cx.notify()),
            // A PR linked or unlinked: the row follows another.
            cx.observe_global::<PrLinks>(Self::rebuild),
        ];
        let tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            if this.update(cx, Self::rebuild).is_err() {
                break;
            }
        });
        let mut list = SessionList::new(state.unseen);
        list.set_filter(state.filter.filter().clone());
        Self {
            list,
            workspaces,
            machines,
            installs,
            app_update,
            projects,
            prs,
            shelves: state.shelves,
            layout: state.layout,
            saved_filter: state.filter,
            active: None,
            active_draft: None,
            hover: None,
            draft_hover: None,
            renaming: None,
            search,
            scroll: VirtualListScrollHandle::new(),
            viewport: Pixels::ZERO,
            _subscriptions: subscriptions,
            _tick: tick,
        }
    }

    /// Replaces `machine`'s saved sessions with a fresh `list_sessions`
    /// result.
    pub(crate) fn set_saved(
        &mut self,
        machine: &MachineId,
        sessions: Vec<SessionSummary>,
        cx: &mut Context<'_, Self>,
    ) {
        let saved = self.workspaces.update(cx, |workspaces, cx| {
            let saved: Vec<SavedSession> = sessions
                .into_iter()
                .map(|session| SavedSession {
                    workspace: workspaces.index(&Place::new(machine.clone(), session.cwd()), cx),
                    id: SessionKey::new(machine.clone(), session.id().clone()),
                    cwd: session.cwd().to_path_buf(),
                    title: session.title().map(|title| title.to_owned().into()),
                    updated: i64::try_from(session.updated_at()).unwrap_or(i64::MAX),
                })
                .collect();
            workspaces.set_history(
                machine,
                saved
                    .iter()
                    .map(|session| (session.workspace, session.updated)),
                cx,
            );
            saved
        });
        self.list.set_saved(machine, saved);
        self.rebuild(cx);
    }

    /// Lists `drafts` above the sessions, in their order, with `active`
    /// on screen.
    pub(crate) fn set_drafts(
        &mut self,
        drafts: Vec<DraftRow>,
        active: Option<EntityId>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.active_draft != active {
            self.active_draft = active;
            cx.notify();
        }
        if self.list.set_drafts(drafts) {
            self.rebuild(cx);
        }
    }

    /// Drops everything of `machine`, gone from the settings.
    pub(crate) fn remove_machine(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        self.list.remove_machine(machine);
        self.prune_filter(|filter| filter.forget_machine(machine));
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.machine == *machine)
        {
            self.active = None;
        }
        self.rebuild(cx);
    }

    /// Replaces the sessions with work under way in `machine`'s daemon.
    pub(crate) fn set_jobs(
        &mut self,
        machine: &MachineId,
        jobs: &[Job],
        cx: &mut Context<'_, Self>,
    ) {
        let jobs = self.workspaces.update(cx, |workspaces, cx| {
            jobs.iter()
                .map(|job| JobSession {
                    workspace: workspaces.index(&Place::new(machine.clone(), job.cwd()), cx),
                    id: SessionKey::new(machine.clone(), job.session().clone()),
                    cwd: job.cwd().to_path_buf(),
                    title: job.title().map(|title| title.to_owned().into()),
                    activity: job.activity(),
                    turn_elapsed: job.turn_elapsed(),
                })
                .collect()
        });
        self.list
            .set_jobs(machine, jobs, Instant::now(), time::now_unix());
        self.mark_active_seen();
        self.rebuild(cx);
    }

    /// Records an open session's title and activity.
    pub(crate) fn set_live(&mut self, live: LiveSession, cx: &mut Context<'_, Self>) {
        let place = Place::new(live.id.machine.clone(), live.cwd.clone());
        let workspace = self
            .workspaces
            .update(cx, |workspaces, cx| workspaces.index(&place, cx));
        if self.list.set_live(live, workspace, time::now_unix()) {
            self.mark_active_seen();
            self.rebuild(cx);
        }
    }

    /// Forgets what an open session's view says of it, once the app stops
    /// following the session: its row follows the daemon's jobs again, until
    /// the view follows it again and catches up.
    pub(crate) fn forget_live(&mut self, id: &SessionKey, cx: &mut Context<'_, Self>) {
        self.list.forget_live(id);
        self.rebuild(cx);
    }

    /// Marks a session as started or continued here, so it stays active.
    pub(crate) fn own(&mut self, id: &SessionKey, cx: &mut Context<'_, Self>) {
        self.shelves.own(id);
        self.rebuild(cx);
    }

    /// Drops a session that was deleted.
    pub(crate) fn remove(&mut self, id: &SessionKey, cx: &mut Context<'_, Self>) {
        self.list.remove(id);
        self.shelves.forget(id);
        if self.active.as_ref() == Some(id) {
            self.active = None;
        }
        self.rebuild(cx);
    }

    /// Forgets every open session and job of `machine`, once its
    /// connection is gone.
    pub(crate) fn clear_live(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        self.list.clear_live(machine);
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.machine == *machine)
        {
            self.active = None;
        }
        self.rebuild(cx);
    }

    pub(crate) fn set_active(&mut self, id: Option<SessionKey>, cx: &mut Context<'_, Self>) {
        if self.active != id {
            self.active = id;
            if self.mark_active_seen() {
                self.rebuild(cx);
            } else {
                cx.notify();
            }
        }
    }

    /// Session `id`'s title, once it has one.
    pub(crate) fn session_title(&self, id: &SessionKey) -> Option<SharedString> {
        self.list.row(id)?.title.clone()
    }

    /// The session on screen has no Done to show: its turn ended in view.
    /// Returns whether it had one, so the list needs a rebuild.
    fn mark_active_seen(&mut self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| self.list.mark_seen(active))
    }

    /// How wide the app lays the sidebar out.
    pub(crate) fn width(&self) -> Rems {
        self.layout.width()
    }

    /// Makes the open sidebar `width` rems wide, held to the `room` the
    /// row leaves it, as a drag of its edge goes.
    pub(crate) fn resize(&mut self, width: f32, room: f32) {
        self.layout.resize(width, room);
    }

    /// Saves the width a drag of the edge left.
    pub(crate) fn save_width(&self) {
        self.layout.save_width();
    }

    /// The search's, which the collapsed sidebar doesn't show.
    pub(crate) fn search_focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.read(cx).focus_handle(cx)
    }

    /// Collapses the sidebar to its rail, or opens it again, and remembers
    /// which for the next run. Returns whether it is collapsed now. The
    /// search and scroll stay as they were.
    pub(crate) fn toggle_collapsed(&mut self, cx: &mut Context<'_, Self>) -> bool {
        self.layout.toggle();
        // No row is under the pointer once the rows are gone.
        self.hover = None;
        self.draft_hover = None;
        cx.notify();
        self.layout.current() == Layout::Rail
    }

    fn rebuild(&mut self, cx: &mut Context<'_, Self>) {
        let workspaces = self.workspaces.read(cx);
        self.list.rebuild(&self.shelves, time::now_unix(), |ix| {
            let workspace = workspaces.get(ix);
            (workspace.name.as_ref(), workspace.root_path())
        });
        // The active rows shown have their PRs read.
        let links = cx.global::<PrLinks>();
        let listed = self
            .list
            .items()
            .iter()
            .filter_map(|item| match *item {
                Item::Session(ix) => Some(self.list.row_at(ix)),
                _ => None,
            })
            .filter_map(|row| {
                let workspace = workspaces.get(row.workspace);
                let target = Target::of(&row.id, workspace, links).ok()?;
                Some(Watch {
                    machine: row.id.machine.clone(),
                    target,
                })
            })
            .collect();
        self.prs.update(cx, |prs, cx| prs.set_listed(listed, cx));
        cx.notify();
    }

    /// The PR of `row`'s session, once found.
    fn pr_of<'a>(
        &self,
        row: &Row,
        workspace: &Workspace,
        cx: &'a gpui_kit::App,
    ) -> Option<&'a PrSummary> {
        let target = Target::of(&row.id, workspace, cx.global::<PrLinks>()).ok()?;
        self.prs.read(cx).pr(&target)
    }

    fn toggle(&mut self, which: Shelf, cx: &mut Context<'_, Self>) {
        self.list.toggle(which);
        self.rebuild(cx);
    }

    fn show_more(&mut self, which: Shelf, cx: &mut Context<'_, Self>) {
        self.list.show_more(which);
        self.rebuild(cx);
    }

    fn open(&mut self, id: &SessionKey, cx: &mut Context<'_, Self>) {
        if let Some(row) = self.list.row(id) {
            cx.emit(SidebarEvent::Open {
                id: row.id.clone(),
                cwd: row.cwd.clone(),
                title: row.title.clone(),
            });
        }
    }

    fn shelve(&mut self, id: &SessionKey, action: ShelfAction, cx: &mut Context<'_, Self>) {
        let now = time::now_unix();
        match action {
            ShelfAction::Settle => self.shelves.settle(id, now),
            ShelfAction::Snooze(snooze) => self.shelves.snooze(id, now + snooze.seconds),
            ShelfAction::Restore => self.shelves.restore(id),
        }
        // The row moves away from the pointer.
        self.hover = None;
        self.rebuild(cx);
    }

    fn set_pinned(&mut self, id: &SessionKey, pin: Pin, cx: &mut Context<'_, Self>) {
        match pin {
            Pin::Pin => self.shelves.pin(id, time::now_unix()),
            Pin::Unpin => self.shelves.unpin(id),
        }
        self.hover = None;
        self.rebuild(cx);
    }

    /// Turns `id`'s title into a text box holding it, all selected.
    fn start_rename(&mut self, id: &SessionKey, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(row) = self.list.row(id) else {
            return;
        };
        let title = row.title.clone().unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        input.update(cx, |input, cx| {
            input.select_all(window, cx);
            input.focus(window, cx);
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event, _, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.finish_rename(cx),
            InputEvent::Change | InputEvent::Focus => {}
        });
        self.renaming = Some(Renaming {
            id: id.clone(),
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Shows the edited title and asks the app to save it, unless it is
    /// blank or unchanged.
    fn finish_rename(&mut self, cx: &mut Context<'_, Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        let title: SharedString = renaming.input.read(cx).value().trim().to_owned().into();
        let changed = self
            .list
            .row(&renaming.id)
            .is_some_and(|row| row.title.as_ref() != Some(&title));
        if !title.is_empty() && changed {
            // Shown at once only when the daemon can take it; otherwise the
            // app says the machine isn't connected and the title stays.
            let connected = matches!(
                self.machines.read(cx).status(&renaming.id.machine),
                Some(MachineStatus::Ready { .. })
            );
            if connected {
                self.list.rename(&renaming.id, title.clone());
                self.rebuild(cx);
            }
            cx.emit(SidebarEvent::Rename {
                id: renaming.id,
                title,
            });
        }
        cx.notify();
    }

    fn cancel_rename(&mut self, cx: &mut Context<'_, Self>) {
        if self.renaming.take().is_some() {
            cx.notify();
        }
    }

    /// The text box in place of `row`'s title, while it is being renamed.
    fn rename_box(&self, row: &Row, cx: &Context<'_, Self>) -> Option<AnyElement> {
        let renaming = self
            .renaming
            .as_ref()
            .filter(|renaming| renaming.id == row.id)?;
        Some(
            div()
                .flex_1()
                .min_w_0()
                // A click in the box must not also open the session.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.cancel_rename(cx);
                        cx.stop_propagation();
                    }
                }))
                .child(Input::new(&renaming.input).xsmall())
                .into_any_element(),
        )
    }

    fn hover(&mut self, id: &SessionKey, cx: &mut Context<'_, Self>) {
        if self.hover.as_ref().is_none_or(|hover| hover.id != *id) {
            self.hover = Some(Hover {
                id: id.clone(),
                snoozing: false,
            });
            cx.notify();
        }
    }

    /// Clears the hover unless another row already took it.
    fn unhover(&mut self, id: &SessionKey, cx: &mut Context<'_, Self>) {
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
        if row.pinned.is_some() {
            let row_id = row.id.clone();
            return Some(
                Button::new("unpin")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::PinOff))
                    .tooltip("Unpin")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_pinned(&row_id, Pin::Unpin, cx);
                    }))
                    .into_any_element(),
            );
        }
        let offered = ShelfAction::for_row(row);
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
                        .tooltip(action.label())
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
    fn render_items(
        &self,
        range: Range<usize>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let clock = Clock {
            now: time::now_unix(),
            instant: Instant::now(),
        };
        let view = cx.entity().downgrade();
        let cx: &Context<'_, Self> = cx;
        let workspaces = self.workspaces.read(cx);
        let machines = self.machines.read(cx);
        // With one machine, a label says nothing.
        let label = |row: &Row| machines.tag(&row.id.machine);
        let age_width = age_width(window);
        self.list.items()[range]
            .iter()
            .map(|&item| match item {
                Item::Draft(ix) => {
                    let draft = self.list.draft_at(ix);
                    let workspace = workspaces.get(draft.workspace);
                    let label = machines.tag(&draft.cwd.machine);
                    self.render_draft(draft, workspace, label, cx)
                }
                Item::Session(row) => {
                    let row = self.list.row_at(row);
                    let workspace = workspaces.get(row.workspace);
                    self.render_row(row, workspace, label(row), clock, &view, cx)
                }
                Item::Shelved(row) => {
                    let row = self.list.row_at(row);
                    let workspace = workspaces.get(row.workspace);
                    self.render_shelved(row, workspace, label(row), age_width, clock, cx)
                }
                Item::Header(which) => self.render_shelf_header(which, cx),
                Item::More(which) => self.render_show_more(which, cx),
                Item::Gap => div().into_any_element(),
            })
            .collect()
    }

    fn render_row(
        &self,
        row: &Row,
        workspace: &Workspace,
        machine: Option<SharedString>,
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
            .child(self.rename_box(row, cx).unwrap_or_else(|| {
                div()
                    .h(rems(TITLE_REMS))
                    .truncate()
                    .text_sm()
                    .when(state.active, StyledExt::font_medium)
                    .child(row.title())
                    .into_any_element()
            }))
            .child(branch_line(
                workspace,
                self.pr_of(row, workspace, cx).map(|pr| {
                    pr_mark(format!("{}-pr", row.element_id).into(), pr, theme).into_any_element()
                }),
                machine.map(|label| (format!("{}-machine", row.element_id).into(), label)),
                theme,
            ));
        Self::row_frame(rems(ROW_REMS), row, state, inner, view, cx).into_any_element()
    }

    /// A draft, laid out as an active session: workspace, Draft or on
    /// hover Discard, the first line of its message, and branch. A click
    /// shows it again; its menu discards it.
    fn render_draft(
        &self,
        draft: &DraftRow,
        workspace: &Workspace,
        machine: Option<SharedString>,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let id = draft.id;
        let state = RowState {
            active: self.active_draft == Some(id),
            hovered: self.draft_hover == Some(id),
        };
        let right = if state.hovered {
            Button::new("discard-draft")
                .ghost()
                .xsmall()
                .icon(Icon::new(Lucide::Trash))
                .tooltip("Discard draft")
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::DiscardDraft(id))))
                .into_any_element()
        } else {
            div()
                .flex_shrink_0()
                .text_color(theme.muted_foreground)
                .child("Draft")
                .into_any_element()
        };
        let title = draft.title();
        let view = cx.weak_entity();
        let inner = v_flex()
            .id(draft.element_id.clone())
            .test_support()
            .aria_label(title.clone())
            .gap(rems(GAP_REMS))
            .py(rems(PAD_REMS))
            .child(workspace_line(workspace, theme).child(right))
            .child(
                div()
                    .h(rems(TITLE_REMS))
                    .truncate()
                    .text_sm()
                    .when(state.active, StyledExt::font_medium)
                    .child(title),
            )
            .child(branch_line(
                workspace,
                None,
                machine.map(|label| (format!("{}-machine", draft.element_id).into(), label)),
                theme,
            ))
            .size_full()
            .px_2()
            .rounded(theme.radius)
            .cursor_pointer()
            .map(|this| lit(this, state, theme))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let hover = match (*hovered, this.draft_hover) {
                    (true, _) => Some(id),
                    // Unless another row already took it.
                    (false, Some(other)) if other != id => Some(other),
                    (false, _) => None,
                };
                if this.draft_hover != hover {
                    this.draft_hover = hover;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::OpenDraft(id))))
            .context_menu(move |menu, _, _| {
                menu.item(
                    PopupMenuItem::new("Discard draft")
                        .icon(Icon::new(Lucide::Trash))
                        .on_click(on_view(&view, move |_, _, cx| {
                            cx.emit(SidebarEvent::DiscardDraft(id));
                        })),
                )
            });
        div()
            .h(rems(ROW_REMS))
            .px_2()
            .py(rems(GUTTER_REMS))
            .child(inner)
            .into_any_element()
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
            .map(|this| lit(this, state, theme))
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
            .child(inner.context_menu(move |popup, window, cx| {
                let (view, row) = &menu;
                row_menu(popup, view, row, window, cx)
            }))
    }

    /// A snoozed or settled session on one line: badge, title, and when it
    /// wakes or how old it is, in a column `age_width` wide so titles end
    /// together. Hovering it swaps that for Restore and, after a moment,
    /// shows the session's details, machine included, beside the sidebar.
    fn render_shelved(
        &self,
        row: &Row,
        workspace: &Workspace,
        machine: Option<SharedString>,
        age_width: Pixels,
        clock: Clock,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let state = self.row_state(row);
        let wakes = self.shelves.wakes(&row.id);
        let offered = ShelfAction::for_row(row);
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
            machine,
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
            .child(self.rename_box(row, cx).unwrap_or_else(|| {
                div()
                    .flex_1()
                    .min_w_0()
                    .h(rems(TITLE_REMS))
                    .truncate()
                    .text_sm()
                    .child(card.title.clone())
                    .into_any_element()
            }))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .min_w(age_width)
                    .justify_end()
                    .text_color(theme.muted_foreground)
                    .child(right),
            );
        let trigger = HoverCard::new(row.element_id.clone())
            .anchor(Anchor::LeftCenter)
            .trigger_style(StyleRefinement::default().size_full())
            .ml_3()
            .content(move |_, _, cx| card.render(cx));
        let view = cx.weak_entity();
        let line = Self::row_frame(rems(SHELVED_ROW_REMS), row, state, line, &view, cx);
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

    /// "Show 25 more" under a shelf that holds back sessions, or as many as
    /// it holds back when fewer.
    fn render_show_more(&self, which: Shelf, cx: &Context<'_, Self>) -> AnyElement {
        let id = match which {
            Shelf::Snoozed => "more-snoozed",
            Shelf::Settled | Shelf::Active => "more-settled",
        };
        let more = self.list.held_back(which).min(SHELF_PAGE);
        div()
            .h(rems(SHELVED_ROW_REMS))
            .px_2()
            .py(rems(GUTTER_REMS))
            .child(
                Button::new(id)
                    .ghost()
                    .small()
                    .size_full()
                    .text_color(cx.theme().muted_foreground)
                    .label(format!("Show {more} more"))
                    .on_click(cx.listener(move |this, _, _, cx| this.show_more(which, cx))),
            )
            .into_any_element()
    }

    /// The toggle, "Rho", and New session at the far end.
    fn render_header(cx: &Context<'_, Self>) -> impl IntoElement {
        h_flex()
            .flex_shrink_0()
            .h(top_row())
            .px_3()
            .gap_1()
            .child(toggle_button(Layout::Open, cx))
            .child(div().flex_1().font_semibold().child("Rho"))
            .child(new_session_button(cx))
    }

    fn render_list(&self, window: &Window, cx: &mut Context<'_, Self>) -> AnyElement {
        if self.list.is_empty() {
            let connecting = self
                .machines
                .read(cx)
                .troubles()
                .any(|(_, status, _)| *status == MachineStatus::Connecting);
            let message = if !self.list.query().is_empty() || !self.list.filter().is_empty() {
                "No matching sessions"
            } else if connecting {
                "Connecting…"
            } else {
                "No sessions yet"
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
                    |this, range, window, cx| this.render_items(range, window, cx),
                )
                .track_scroll(&self.scroll)
                .size_full(),
            )
            .into_any_element()
    }

    /// A newer rho-ui, then what each machine's rho needs: installing,
    /// updating, or a restart into the newer one its daemon waits to run.
    /// Without a session on screen, the main pane offers this machine's
    /// install itself.
    fn render_offers(&self, cx: &App) -> Vec<AnyElement> {
        let machines = self.machines.read(cx);
        let installs = self.installs.read(cx);
        let app = self.app_update.read(cx).offer().map(|offer| {
            let offer = app_update::render_offer(&self.app_update, offer, cx);
            div().px_3().child(offer).into_any_element()
        });
        app.into_iter()
            .chain(machines.iter().filter_map(|machine| {
                let offer = installs.offer(&machine.id, cx)?;
                if machine.id.is_local()
                    && self.active.is_none()
                    && offer.replaces_connection_error()
                {
                    return None;
                }
                let tag = machines.tag(&machine.id);
                let offer = render_offer(&self.installs, &machine.id, tag.as_deref(), &offer, cx);
                Some(div().px_3().child(offer).into_any_element())
            }))
            .collect()
    }

    /// What isn't right with how `machine`'s connection is doing, `status`,
    /// with the buttons that would fix it; nothing for a daemon that works.
    /// `label` names the machine, and `on` is " on {label}" where machines
    /// need telling apart, else empty. `restart_runs` is the rho installed
    /// there when it isn't the version the daemon runs, which only then a
    /// restart would change.
    fn render_status(
        &self,
        machines: &Machines,
        machine: &MachineId,
        (label, on): (&str, &str),
        status: &MachineStatus,
        restart_runs: Option<&str>,
        muted: Hsla,
    ) -> Option<AnyElement> {
        let ask = |id: &str, label: &'static str, icon: Lucide, action: DaemonAction| {
            let entity = self.machines.clone();
            Button::new(SharedString::from(format!("{id}-{machine}")))
                .outline()
                .xsmall()
                .icon(icon)
                .label(label)
                .on_click(move |_, _, cx| machines::ask(&entity, action.clone(), cx))
        };
        let retry = || {
            let action = DaemonAction::Connect(machine.clone());
            ask("retry", "Retry", Lucide::RotateCw, action)
        };
        let restart = || {
            let action = DaemonAction::Restart(machine.clone());
            ask("restart-daemon", "Restart…", Lucide::RotateCw, action)
        };
        let alert_with = |alert: Alert, buttons: Vec<Button>| {
            v_flex()
                .px_2()
                .gap_1()
                .child(alert.small())
                .child(h_flex().justify_end().gap_1().children(buttons))
                .into_any_element()
        };
        let shown = match status {
            MachineStatus::Ready {
                another_build: false,
                ..
            } => return None,
            MachineStatus::Connecting => h_flex()
                .px_3()
                .gap_1()
                .text_xs()
                .text_color(muted)
                .child(Icon::new(Lucide::CircleDashed).xsmall())
                .child(format!("Connecting to {label}…"))
                .into_any_element(),
            MachineStatus::Ready {
                version,
                another_build: true,
                ..
            } => {
                let why = skew_note(version, restart_runs);
                let buttons = restart_runs.map(|_| restart()).into_iter().collect();
                alert_with(
                    Alert::warning(SharedString::from(format!("machine-skew-{machine}")), why)
                        .title(format!("Rho{on} is another build")),
                    buttons,
                )
            }
            MachineStatus::OlderDaemon { .. } => alert_with(
                Alert::error(
                    SharedString::from(format!("machine-error-{machine}")),
                    machines::OLDER_DAEMON,
                )
                .title(format!("Rho{on} is older than rho-ui")),
                if machines.restarts_older(machine) {
                    vec![retry(), restart()]
                } else {
                    vec![retry()]
                },
            ),
            MachineStatus::OlderRho => alert_with(
                Alert::error(
                    SharedString::from(format!("machine-error-{machine}")),
                    machines::OLDER_RHO,
                )
                .title(format!("Rho{on} is older than rho-ui")),
                vec![retry()],
            ),
            MachineStatus::Failed(why) => alert_with(
                Alert::error(
                    SharedString::from(format!("machine-error-{machine}")),
                    why.clone(),
                )
                .title(format!("Can't reach {label}")),
                vec![retry()],
            ),
            MachineStatus::Stopped => h_flex()
                .px_3()
                .gap_2()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Rho{on} is stopped")),
                )
                .child(ask(
                    "start-daemon",
                    "Start",
                    Lucide::Play,
                    DaemonAction::Connect(machine.clone()),
                ))
                .into_any_element(),
        };
        Some(shown)
    }

    /// What isn't right with each machine, then why any remote machine's
    /// repositories couldn't be read, then what each rho needs, then
    /// Settings and the local rho's version.
    fn render_footer(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let machines = self.machines.read(cx);
        let version = match machines.status(&MachineId::local()) {
            Some(MachineStatus::Ready { version, .. }) => Some(format!("Rho {version}")),
            _ => None,
        };
        let installs = self.installs.read(cx);
        let offers = self.render_offers(cx);
        let mut troubles: Vec<AnyElement> = Vec::new();
        for (machine, status, refused) in machines.troubles() {
            let label = machines.label(machine);
            let on = machines
                .tag(machine)
                .map_or_default(|_| format!(" on {label}"));
            for (listing, error) in refused {
                troubles.push(footer_alert(
                    format!("{}-error-{machine}", listing.name()),
                    format!("{}{on}", listing.failed()),
                    error.clone(),
                ));
            }
            let offers_install = installs
                .offer(machine, cx)
                .is_some_and(|offer| offer.replaces_connection_error());
            // Without rho there is nothing to reach: its offer says so.
            if !offers_install {
                let named = (label.as_ref(), on.as_str());
                let restart_runs = match status {
                    MachineStatus::Ready { version, .. } => installs.restart_runs(machine, version),
                    _ => None,
                };
                troubles.extend(self.render_status(
                    machines,
                    machine,
                    named,
                    status,
                    restart_runs,
                    muted,
                ));
            }
        }
        let workspaces = self.workspaces.read(cx);
        for machine in machines.iter() {
            let Some(why) = workspaces.unread(&machine.id) else {
                continue;
            };
            let on = machines
                .tag(&machine.id)
                .map_or_default(|label| format!(" on {label}"));
            troubles.push(footer_alert(
                format!("repos-error-{}", machine.id),
                format!("Couldn't read repositories{on}"),
                why.clone(),
            ));
        }
        v_flex()
            .flex_shrink_0()
            .py_2()
            .gap_1()
            .children(troubles)
            .children(offers)
            .child(
                // The toggle's inset, so Settings stays put in the rail.
                h_flex().px_3().gap_2().child(settings_button(cx)).children(
                    version.map(|version| div().text_xs().text_color(muted).child(version)),
                ),
            )
    }
}

/// What the footer says of a daemon of another build than rho-ui's, which
/// runs rho `version`: builds have no order, so either side may be the
/// older one. `restart_runs` is the rho installed there, if a restart would
/// run another version.
fn skew_note(version: &str, restart_runs: Option<&str>) -> String {
    match restart_runs {
        Some(installed) => format!(
            "Requests may fail. Its daemon runs Rho {version}, but Rho {installed} is installed \
             there: restarting it runs that."
        ),
        None => "Requests may fail. Update whichever of Rho and rho-ui is older, then restart it."
            .to_owned(),
    }
}

/// A small error in the footer, with element ID `id`.
fn footer_alert(id: String, title: String, why: SharedString) -> AnyElement {
    div()
        .px_2()
        .child(
            Alert::error(SharedString::from(id), why)
                .title(title)
                .small(),
        )
        .into_any_element()
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let frame = v_flex()
            .id("sidebar")
            .test_support()
            .relative()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border);
        if self.layout.current() == Layout::Rail {
            return frame.child(self.render_rail(cx));
        }
        frame
            .child(Self::render_header(cx))
            .child(
                // The header's padding, so the filter lines up under New session.
                h_flex()
                    .flex_shrink_0()
                    .px_3()
                    .pb_2()
                    .gap_1()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search)
                                .small()
                                .prefix(Icon::new(IconName::Search).small().text_color(muted))
                                .cleanable(true),
                        ),
                    )
                    .child(filter::button(self, cx)),
            )
            .child(self.render_list(window, cx))
            .child(self.render_footer(cx))
            // [`crate::app::RhoApp`] follows the drag, as it lays out the
            // row the sidebar sits in.
            .child(
                resize_handle::<ResizeSidebar, ResizeSidebar>("sidebar-edge", Axis::Horizontal)
                    .inside(HandleEdge::Trailing)
                    .on_drag(ResizeSidebar, |drag, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| *drag)
                    })
                    .on_double_click(cx.listener(|this, _, _, cx| {
                        this.layout.reset_width();
                        cx.emit(SidebarEvent::Resized);
                    })),
            )
    }
}

fn new_session_button(cx: &Context<'_, SidebarView>) -> Button {
    Button::new("new-session")
        .ghost()
        .small()
        .icon(Lucide::SquarePen)
        .accessibility_label("New session")
        .tooltip_with_action("New session", &crate::NewSession, Some("RhoApp"))
        .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::NewSession)))
}

fn settings_button(cx: &Context<'_, SidebarView>) -> Button {
    Button::new("open-settings")
        .ghost()
        .small()
        .icon(IconName::Settings)
        .accessibility_label("Settings")
        .tooltip_with_action("Settings", &crate::OpenSettings, Some("RhoApp"))
        .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::OpenSettings)))
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

/// A workspace's badge, a line of the sidebar's text tall.
fn badge(initials: SharedString) -> Badge {
    Badge::new(initials, rems(LINE_REMS))
}

/// The widest age [`time::relative_age`] gives below a year, in the rows'
/// font at `text_xs`: a shelved row's age column, so every title ends at
/// one place. Wider content, such as a snooze's alarm, widens its own row.
/// Measured, not a constant: the font is the user's choice.
fn age_width(window: &Window) -> Pixels {
    let style = window.text_style();
    let size = rems(0.75).to_pixels(window.rem_size());
    ["now", "00m", "00w"]
        .into_iter()
        .map(|age| {
            let run = style.to_run(age.len());
            window
                .text_system()
                .shape_line(SharedString::new_static(age), size, &[run], None)
                .width
                // Rounded up, so the widest age fits.
                .ceil()
        })
        .max()
        .unwrap_or_default()
}

/// How a row is lit: open in the pane, under the pointer, or both.
#[derive(Clone, Copy)]
struct RowState {
    active: bool,
    hovered: bool,
}

/// Lights a row as `state` says.
fn lit<E: Styled>(row: E, state: RowState, theme: &Theme) -> E {
    let accent = theme.sidebar_accent;
    match (state.active, state.hovered) {
        (true, _) => row.bg(accent).text_color(theme.sidebar_accent_foreground),
        (false, true) => row.bg(accent.opacity(0.5)),
        (false, false) => row,
    }
}

/// A shelved session's details, shown beside the sidebar while hovered.
/// Built with the row, so the card needs nothing from the view.
struct Card {
    title: SharedString,
    workspace: SharedString,
    initials: SharedString,
    branch: Option<SharedString>,
    machine: Option<SharedString>,
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
        if let Some(machine) = &self.machine {
            details = details.item(muted(Lucide::Server), machine.clone(), 1);
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

/// The branch, when the workspace is a git repository, then the session's
/// `pr` mark, and at the right the machine's label, when there is more than
/// one machine, as its element ID and the label.
fn branch_line(
    workspace: &Workspace,
    pr: Option<AnyElement>,
    machine: Option<(SharedString, SharedString)>,
    theme: &Theme,
) -> gpui_kit::Div {
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
        .children(pr)
        .child(div().flex_1())
        .when_some(machine, |this, (id, label)| {
            this.child(Icon::new(Lucide::Server).xsmall().flex_shrink_0())
                .child(
                    div()
                        .id(id)
                        .test_support()
                        .aria_label(label.clone())
                        .flex_shrink_0()
                        .max_w(rems(MACHINE_LABEL_REMS))
                        .truncate()
                        .child(label),
                )
        })
}

/// A row's PR: its state's icon and its number, with a red dot while its
/// checks fail. `id` is its element ID.
fn pr_mark(id: SharedString, pr: &PrSummary, theme: &Theme) -> impl IntoElement {
    let (_, icon, color) = state_look(pr.state);
    // As the surface's tag colors its text.
    let color = color.scale(if theme.is_dark() { 300 } else { 600 });
    let failing = pr.change.rollup == Rollup::Failing;
    h_flex()
        .id(id)
        .test_support()
        .aria_label(SharedString::from(format!(
            "#{}{}",
            pr.id.number,
            if failing { ", checks failing" } else { "" }
        )))
        .flex_shrink_0()
        .gap_0p5()
        .child(Icon::new(icon).xsmall().text_color(color))
        .child(format!("#{}", pr.id.number))
        .when(failing, |this| {
            this.child(div().size_1p5().rounded_full().bg(theme.red))
        })
}

/// "Working 24m", "Needs you 9m", "Waiting" on background work, "Done", or the age: "3h", after a pin on a
/// pinned row. Icons are static: an animated one would redraw the whole
/// sidebar every frame. So Working gets a symmetric dashed circle, not a
/// spinner frozen mid-turn.
fn status(row: &Row, clock: Clock, theme: &Theme) -> AnyElement {
    let status = activity(row, clock, theme);
    if row.pinned.is_none() {
        return status;
    }
    h_flex()
        .flex_shrink_0()
        .gap_1()
        .child(
            Icon::new(Lucide::Pin)
                .xsmall()
                .text_color(theme.muted_foreground),
        )
        .child(status)
        .into_any_element()
}

/// What [`status`] says of the session's activity.
fn activity(row: &Row, clock: Clock, theme: &Theme) -> AnyElement {
    let label = |word: &str, since: Instant| match time::elapsed(
        clock.instant.duration_since(since).as_secs(),
    ) {
        Some(elapsed) => format!("{word} {elapsed}"),
        None => word.to_owned(),
    };
    let busy = |color: Hsla, icon: Icon, text: String| {
        h_flex()
            .id(SharedString::from(format!("{}-busy", row.element_id)))
            .test_support()
            .aria_label(text.clone())
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
            Icon::new(Lucide::CircleDashed),
            label("Working", since),
        ),
        Activity::Waiting(since) => busy(
            theme.warning,
            Icon::new(Lucide::CircleAlert),
            label("Needs you", since),
        ),
        Activity::Background => h_flex()
            .id(SharedString::from(format!("{}-background", row.element_id)))
            .test_support()
            .aria_label("Waiting on background work")
            .flex_shrink_0()
            .gap_1()
            .text_color(theme.muted_foreground)
            .child(Icon::new(Lucide::Hourglass).xsmall())
            .child("Waiting")
            .into_any_element(),
        Activity::Idle if row.done => h_flex()
            .id(SharedString::from(format!("{}-done", row.element_id)))
            .test_support()
            .aria_label("Done")
            .flex_shrink_0()
            .gap_1()
            .text_color(theme.success)
            .child(Icon::new(Lucide::CircleCheck).xsmall())
            .child("Done")
            .into_any_element(),
        Activity::Idle => div()
            .flex_shrink_0()
            .text_color(theme.muted_foreground)
            .when(row.updated > 0, |this| {
                this.child(time::relative_age(clock.now - row.updated))
            })
            .into_any_element(),
    }
}

/// Pinning a session, or unpinning it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pin {
    Pin,
    Unpin,
}

/// The right-click menu: Rename, Pin, the moves the hover buttons offer,
/// Copy, and Delete, which a busy session can't do.
fn row_menu(
    menu: PopupMenu,
    view: &WeakEntity<SidebarView>,
    row: &MenuRow,
    window: &mut Window,
    cx: &mut Context<'_, PopupMenu>,
) -> PopupMenu {
    let (pin, pin_icon, pin_label) = if row.pinned {
        (Pin::Unpin, Lucide::PinOff, "Unpin")
    } else {
        (Pin::Pin, Lucide::Pin, "Pin")
    };
    let id = row.id.clone();
    let mut menu = menu
        .item(
            PopupMenuItem::new("Rename…")
                .icon(Icon::new(Lucide::Pencil))
                .on_click(on_view(view, move |this, window, cx| {
                    this.start_rename(&id, window, cx);
                })),
        )
        .item({
            let id = row.id.clone();
            PopupMenuItem::new(pin_label)
                .icon(Icon::new(pin_icon))
                .on_click(on_view(view, move |this, _, cx| {
                    this.set_pinned(&id, pin, cx);
                }))
        });
    menu = snooze_menu(menu, view, row, window, cx);
    for &action in row.offered {
        if !matches!(action, ShelfAction::Snooze(_)) {
            let id = row.id.clone();
            menu = menu.item(
                PopupMenuItem::new(action.label())
                    .icon(action.icon())
                    .on_click(on_view(view, move |this, _, cx| {
                        this.shelve(&id, action, cx);
                    })),
            );
        }
    }
    copy_menu(menu, row, window, cx)
        .separator()
        .item(delete_item(view, row))
}

/// A menu item's click handler that runs `f` on the sidebar.
fn on_view(
    view: &WeakEntity<SidebarView>,
    f: impl Fn(&mut SidebarView, &mut Window, &mut Context<'_, SidebarView>) + 'static,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |_, window, cx| {
        let _ = view.update(cx, |this, cx| f(this, window, cx));
    }
}

/// The Snooze submenu, when the row offers snoozes.
fn snooze_menu(
    menu: PopupMenu,
    view: &WeakEntity<SidebarView>,
    row: &MenuRow,
    window: &mut Window,
    cx: &mut Context<'_, PopupMenu>,
) -> PopupMenu {
    let snoozes: Vec<(ShelfAction, &'static Snooze)> = row
        .offered
        .iter()
        .filter_map(|&action| match action {
            ShelfAction::Snooze(snooze) => Some((action, snooze)),
            ShelfAction::Settle | ShelfAction::Restore => None,
        })
        .collect();
    if snoozes.is_empty() {
        return menu;
    }
    let (view, id) = (view.clone(), row.id.clone());
    menu.submenu_with_icon(
        Some(Icon::new(Lucide::AlarmClock)),
        "Snooze",
        window,
        cx,
        move |menu, _, _| {
            snoozes.iter().fold(menu, |menu, &(action, snooze)| {
                let id = id.clone();
                menu.item(PopupMenuItem::new(snooze.long).on_click(on_view(
                    &view,
                    move |this, _, cx| {
                        this.shelve(&id, action, cx);
                    },
                )))
            })
        },
    )
}

/// The Copy submenu: the session's ID, the command that resumes it, and its
/// folder's path.
fn copy_menu(
    menu: PopupMenu,
    row: &MenuRow,
    window: &mut Window,
    cx: &mut Context<'_, PopupMenu>,
) -> PopupMenu {
    let copies = [
        ("Session ID", row.id.id.to_string()),
        ("Resume command", format!("rho -R {}", row.id.id)),
        ("Folder path", row.cwd.display().to_string()),
    ];
    menu.submenu_with_icon(
        Some(Icon::new(Lucide::Copy)),
        "Copy",
        window,
        cx,
        move |menu, _, _| {
            copies.iter().fold(menu, |menu, (label, text)| {
                let text = text.clone();
                menu.item(PopupMenuItem::new(*label).on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                }))
            })
        },
    )
}

/// Delete…, which asks first, and is greyed out while the session is busy.
fn delete_item(view: &WeakEntity<SidebarView>, row: &MenuRow) -> PopupMenuItem {
    let (view, id, title) = (view.clone(), row.id.clone(), row.title.clone());
    PopupMenuItem::new("Delete…")
        .icon(Icon::new(Lucide::Trash))
        .disabled(row.busy)
        .on_click(move |_, window, cx| {
            let (view, id, title) = (view.clone(), id.clone(), title.clone());
            window.open_alert_dialog(cx, move |alert, _, cx| {
                let (view, id) = (view.clone(), id.clone());
                alert
                    .icon(Icon::new(Lucide::Trash).text_color(cx.theme().danger))
                    .title(format!("Delete \"{title}\"?"))
                    .description("Rho deletes the saved session. This can't be undone.")
                    .confirm()
                    .ok_text("Delete")
                    .ok_variant(ButtonVariant::Danger)
                    .on_ok(move |_, _, cx| {
                        let id = id.clone();
                        let _ = view.update(cx, |_, cx| cx.emit(SidebarEvent::Delete { id }));
                        true
                    })
            });
        })
}

/// What a row's menu needs, captured when the row is drawn.
struct MenuRow {
    id: SessionKey,
    title: SharedString,
    cwd: PathBuf,
    offered: &'static [ShelfAction],
    pinned: bool,
    /// Busy sessions can't be deleted.
    busy: bool,
}

impl MenuRow {
    fn new(row: &Row) -> Self {
        Self {
            id: row.id.clone(),
            title: row.title(),
            cwd: row.cwd.clone(),
            offered: ShelfAction::for_row(row),
            pinned: row.pinned.is_some(),
            busy: !row.activity.is_idle(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_sessions_offer_no_moves_and_shelves_offer_a_way_back() {
        assert_eq!(
            ShelfAction::offered(Shelf::Active, Activity::Working(Instant::now())),
            []
        );
        assert_eq!(
            ShelfAction::offered(Shelf::Settled, Activity::Idle),
            [ShelfAction::Restore]
        );
        assert!(
            ShelfAction::offered(Shelf::Snoozed, Activity::Idle).contains(&ShelfAction::Restore)
        );
    }
}
