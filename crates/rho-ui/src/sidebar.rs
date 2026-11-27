//! The session list, drawn: one scrolling list of every machine's active
//! sessions, the leads first under a header of their own, pinned sessions
//! first in each part and the rest ranked as an inbox (see
//! [`crate::session_list`]), then the Snoozed and Settled shelves, held at
//! the bottom until opened shelves fill the list. Drafts of new sessions
//! the user left with a message come before them all, saying Draft;
//! hovering one offers Discard. An active row shows the workspace, the
//! session's status or age, its title, its branch, and with more than one
//! machine, the machine's label; hovering it swaps the status for Snooze
//! and Settle, or Unpin. A turn that finished off screen, or on screen
//! while the window was in the background, shows Done until its session is
//! seen.
//! Right-click a row for its menu, which also renames it in place. A shelf
//! opens from its header into one-line rows, its newest 25 at first, and
//! Show more under them adds 25 more at a time. A session another session
//! started is drawn indented under it, behind a line that counts them and
//! opens on a click, or by itself when one of them comes on screen, or
//! while the search or the filter shows some of them and not their starter.
//! Hovering a row shows its details in a card. The filter beside the
//! search hides machines or keeps to chosen projects, shown as chips under
//! the search while it does (see [`filter`]), and the footer says
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
use rho_ui_core::{Job, NewSessionMark, SessionMark, SessionSummary};

use crate::{
    app_update::{self, AppUpdate},
    badge::Badge,
    leads::Kind,
    machines::{self, DaemonAction, MachineId, MachineStatus, Machines, Place, SessionKey},
    pr_mark::{mark_label, pr_mark},
    project_stack::{self, session_projects, session_workspaces, Naming, ProjectStack},
    projects::Projects,
    pull_requests::{Focus, PullRequests},
    rho_install::{render_offer, Installs},
    session_list::{
        DraftRow, Family, Fold, FoldLine, Item, JobSession, LiveSession, Pressing, Row,
        SavedSession, SessionList, Status, SHELF_PAGE,
    },
    settings::{self, Settings},
    shelf::{self, Shelf},
    store::StateDb,
    time,
    title_bar::TOP_ROW_REMS,
    unseen::Unseen,
    window_chrome::{self, DragArea, MovesWindow as _},
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

/// The line under a session that started others: one line of text, padded
/// as the lines of a row are apart.
const FOLD_REMS: f32 = LINE_REMS + 2. * GAP_REMS + 2. * GUTTER_REMS;

/// How far the sessions a session started, and the line over them, sit in
/// from its row: about a badge's width.
const NEST_REMS: f32 = 1.25;

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
        Item::Header(_) | Item::Leads | Item::Sessions => HEADER_REMS,
        Item::Fold(_) => FOLD_REMS,
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
    seconds: u64,
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
    /// nothing: it shows as active until its turn ends, as does a session
    /// whose family is busy. Nor does a pinned one, which stays active
    /// until unpinned, one a session started, which moves with that one, or
    /// one its daemon has saved nothing of, which it keeps no mark for.
    fn for_row(row: &Row) -> &'static [ShelfAction] {
        if row.pinned.is_some() || row.family == Family::Started || row.busy() || !row.saved {
            return &[];
        }
        Self::offered(row.shelf)
    }

    fn offered(shelf: Shelf) -> &'static [ShelfAction] {
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

    /// The mark that makes the move at `now`, which the daemon is asked to
    /// save. A restore keeps the session rather than clearing its mark, so
    /// the daemon doesn't settle it again for a pull request that merged
    /// before.
    fn mark(self, now: u64) -> NewSessionMark {
        match self {
            ShelfAction::Settle => NewSessionMark::Done,
            ShelfAction::Snooze(snooze) => NewSessionMark::Snoozed {
                until: now.saturating_add(snooze.seconds),
            },
            ShelfAction::Restore => NewSessionMark::Kept,
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
        kind: Kind,
    },
    NewSession,
    /// Show a draft for a new lead.
    NewLead,
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
    /// Save each of `marks`, a session's mark, for sessions of one family,
    /// in their daemon, as one move. Their rows show them already when the
    /// machine is connected.
    Mark {
        marks: Vec<(SessionKey, NewSessionMark)>,
    },
    OpenSettings,
    /// Search every session's conversation for `query`, the sidebar
    /// search's text.
    SearchSessions {
        query: SharedString,
    },
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
/// state database: the sessions Done and not yet seen, whether it is
/// collapsed, and its filter. Pinned, settled and snoozed sessions are their
/// daemons' to keep (see [`crate::shelf`]).
pub(crate) struct SidebarState {
    pub(crate) unseen: Unseen,
    pub(crate) layout: SidebarLayout,
    pub(crate) filter: SavedFilter,
}

impl SidebarState {
    pub(crate) fn load(db: &StateDb) -> Self {
        Self {
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
    /// Each active row's PR, which its branch line marks, and whether its
    /// agent watches one, which its status says.
    prs: Entity<PullRequests>,
    layout: SidebarLayout,
    saved_filter: SavedFilter,
    /// Whether the window has the focus, as its last change said: the
    /// session on screen counts as seen only while it does.
    focus: Focus,
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
    _subscriptions: [Subscription; 7],
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
                let projects = projects.read(cx);
                this.prune_filter(|filter| filter.keep_projects(|root| projects.may_list(root)));
                this.rebuild(cx);
            }),
            cx.observe(&installs, |_, _, cx| cx.notify()),
            cx.observe(&app_update, |_, _, cx| cx.notify()),
            // A set of PRs answered or changed: the marks follow, and the
            // rows whose agents took up a watch or ended one move.
            cx.observe(&prs, |this, _, cx| this.rebuild(cx)),
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
            layout: state.layout,
            saved_filter: state.filter,
            focus: Focus::Focused,
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
                .map(|session| {
                    let id = SessionKey::new(machine.clone(), session.id().clone());
                    SavedSession {
                        added: workspaces.index_all(machine, session.added_dirs(), cx),
                        workspace: workspaces
                            .index(&Place::new(machine.clone(), session.cwd()), cx),
                        id,
                        cwd: session.cwd().to_path_buf(),
                        title: session.title().map(|title| title.to_owned().into()),
                        updated: i64::try_from(session.updated_at()).unwrap_or(i64::MAX),
                        parent: session
                            .parent()
                            .map(|parent| SessionKey::new(machine.clone(), parent.clone())),
                        kind: session.agent().map_or(Kind::Session, Kind::of),
                        mark: session.mark(),
                    }
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
                    parent: job
                        .parent()
                        .map(|parent| SessionKey::new(machine.clone(), parent.clone())),
                })
                .collect()
        });
        self.list
            .set_jobs(machine, jobs, Instant::now(), time::now_unix());
        self.mark_active_seen();
        self.rebuild(cx);
    }

    /// Records an open session's title, activity and added directories.
    pub(crate) fn set_live(&mut self, live: LiveSession, cx: &mut Context<'_, Self>) {
        let place = Place::new(live.id.machine.clone(), live.cwd.clone());
        let (workspace, added) = self.workspaces.update(cx, |workspaces, cx| {
            let added = (live.added_dirs.as_deref())
                .map(|dirs| workspaces.index_all(&place.machine, dirs, cx));
            (workspaces.index(&place, cx), added)
        });
        if self.list.set_live(live, workspace, added, time::now_unix()) {
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

    /// Drops a session that was deleted.
    pub(crate) fn remove(&mut self, id: &SessionKey, cx: &mut Context<'_, Self>) {
        self.list.remove(id);
        self.rebuild(cx);
    }

    /// Forgets every open session and job of `machine`, once its
    /// connection is gone.
    pub(crate) fn clear_live(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        self.list.clear_live(machine);
        self.rebuild(cx);
    }

    /// Puts session `id` on screen, opening the fold it is under and
    /// showing it there while it is on screen.
    pub(crate) fn set_active(&mut self, id: Option<&SessionKey>, cx: &mut Context<'_, Self>) {
        if self.list.on_screen() != id {
            self.list.set_on_screen(id);
            // Opening a session is seeing it, focus or not.
            self.list.mark_on_screen_seen();
            // Its PR is listed, whichever shelf it is on.
            self.rebuild(cx);
        }
    }

    /// Session `id`'s title, once it has one.
    pub(crate) fn session_title(&self, id: &SessionKey) -> Option<SharedString> {
        self.list.row(id)?.title.clone()
    }

    /// The rows of the sessions `starter` started itself, in the list's
    /// order, whatever the search and the filter show.
    pub(crate) fn started_by<'a>(
        &'a self,
        starter: &'a SessionKey,
    ) -> impl Iterator<Item = &'a Row> + 'a {
        self.list.started_by(starter)
    }

    /// The session on screen has no Done to show while the window has the
    /// focus: its turn ended in view. While it doesn't, the user may not
    /// have seen it, so its row says Done until the focus comes back
    /// ([`Self::set_focus`]). The list needs a rebuild after it.
    fn mark_active_seen(&mut self) {
        if self.focus == Focus::Focused {
            self.list.mark_on_screen_seen();
        }
    }

    /// The window gained the focus or lost it. Back in it, the user sees
    /// the session on screen.
    pub(crate) fn set_focus(&mut self, focus: Focus, cx: &mut Context<'_, Self>) {
        if self.focus != focus {
            self.focus = focus;
            self.mark_active_seen();
            self.rebuild(cx);
        }
    }

    /// Every listed session, whatever the search and the filter show.
    pub(crate) fn rows(&self) -> &[Row] {
        self.list.rows()
    }

    /// How wide the app lays the sidebar out: on macOS, never too narrow
    /// for the traffic lights over it.
    pub(crate) fn width(&self, cx: &App) -> Rems {
        rems(
            self.layout
                .width()
                .0
                .max(window_chrome::sidebar_least_rems(cx)),
        )
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

    /// Whether it is collapsed to its rail.
    pub(crate) fn is_collapsed(&self) -> bool {
        self.layout.current() == Layout::Rail
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
        self.is_collapsed()
    }

    fn rebuild(&mut self, cx: &mut Context<'_, Self>) {
        let workspaces = self.workspaces.read(cx);
        let projects = self.projects.read(cx);
        let watching = self.prs.read(cx).watching();
        let named = |kind: Kind, own: Option<usize>, added: &[usize]| {
            let added = added.iter().map(|&ix| workspaces.get(ix));
            let own = own.map(|ix| workspaces.get(ix));
            session_workspaces(kind, own, added, Some(projects))
                .into_iter()
                .map(|workspace| (workspace.name.as_ref(), workspace.root_path()))
                .collect()
        };
        self.list.rebuild(time::now_unix(), &watching, named);
        // Every session on the active shelf the filter shows has its PRs
        // listed by its daemon, under a closed fold too, as what it watches
        // ranks its family; so has every row drawn there, a starter shown
        // for its sessions included, and the session on screen, for its
        // title bar.
        let drawn = self.list.items().iter().filter_map(|item| match *item {
            Item::Session(ix) => Some(self.list.row_at(ix).id.clone()),
            _ => None,
        });
        let active = self.list.on_active_shelf(named);
        let mut listed: Vec<SessionKey> = Vec::new();
        for id in active
            .into_iter()
            .chain(drawn)
            .chain(self.list.on_screen().cloned())
        {
            if !listed.contains(&id) {
                listed.push(id);
            }
        }
        self.prs.update(cx, |prs, cx| prs.set_listed(listed, cx));
        cx.notify();
    }

    fn toggle(&mut self, which: Shelf, cx: &mut Context<'_, Self>) {
        self.list.toggle(which);
        self.rebuild(cx);
    }

    fn show_more(&mut self, which: Shelf, cx: &mut Context<'_, Self>) {
        self.list.show_more(which);
        self.rebuild(cx);
    }

    fn toggle_fold(&mut self, starter: &SessionKey, cx: &mut Context<'_, Self>) {
        self.list.toggle_fold(starter);
        self.rebuild(cx);
    }

    /// Asks the app to open listed session `id`, as a click on its row
    /// does, and says whether it is listed: one that isn't can't open.
    pub(crate) fn open(&mut self, id: &SessionKey, cx: &mut Context<'_, Self>) -> bool {
        let Some(row) = self.list.row(id) else {
            return false;
        };
        cx.emit(SidebarEvent::Open {
            id: row.id.clone(),
            cwd: row.cwd.clone(),
            title: row.title.clone(),
            kind: row.kind,
        });
        true
    }

    fn shelve(&mut self, id: &SessionKey, action: ShelfAction, cx: &mut Context<'_, Self>) {
        // A family moves as one.
        let mark = action.mark(now());
        let family = self.list.family(id);
        self.mark(
            family.into_iter().map(|id| (id, mark.clone())).collect(),
            cx,
        );
    }

    /// Pins session `id` or unpins it. Pinning a snoozed family also brings
    /// back the sessions snoozed with it, as Restore does
    /// ([`SessionList::pinned_with`]).
    fn set_pinned(&mut self, id: &SessionKey, pin: Pin, cx: &mut Context<'_, Self>) {
        let kept = match pin {
            Pin::Pin => self.list.pinned_with(id),
            Pin::Unpin => Vec::new(),
        };
        let kept = kept.into_iter().map(|id| (id, NewSessionMark::Kept));
        self.mark(
            std::iter::once((id.clone(), pin.mark()))
                .chain(kept)
                .collect(),
            cx,
        );
    }

    /// Asks the app to save `marks`, each a session's mark, for sessions of
    /// one family, in their daemon; each is one it has saved, as a row
    /// offers no move otherwise and [`SessionList::family`] leaves the
    /// others out. They show at once, stamped with this machine's time
    /// until the daemon's own comes, only when the daemon can take them, as
    /// a rename is; the daemon's next list replaces them either way, so a
    /// mark it refused goes.
    fn mark(&mut self, marks: Vec<(SessionKey, NewSessionMark)>, cx: &mut Context<'_, Self>) {
        let machines = self.machines.read(cx);
        let connected = marks.first().is_some_and(|(id, _)| {
            matches!(
                machines.status(&id.machine),
                Some(MachineStatus::Ready { .. })
            )
        });
        if connected {
            let mut moved = false;
            for (id, mark) in &marks {
                if let Some(shown) = SessionMark::stamped(mark, now()) {
                    moved |= self.list.set_mark(id, shown);
                }
            }
            if moved {
                // The row moves away from the pointer.
                self.hover = None;
                self.rebuild(cx);
            }
        }
        cx.emit(SidebarEvent::Mark { marks });
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
                    let workspace = draft.workspace.map(|ix| workspaces.get(ix));
                    let projects = self.projects_of(draft.kind, draft.workspace, &draft.added, cx);
                    let label = machines.tag(&draft.cwd.machine);
                    self.render_draft(draft, workspace, &projects, label, cx)
                }
                Item::Session(row) => {
                    let row = self.list.row_at(row);
                    self.render_row(row, label(row), clock, &view, cx)
                }
                Item::Shelved(row) => {
                    let row = self.list.row_at(row);
                    self.render_shelved(row, label(row), age_width, clock, cx)
                }
                Item::Header(which) => self.render_shelf_header(which, cx),
                Item::Leads => Self::render_kind_header("leads-header", "Leads"),
                Item::Sessions => Self::render_kind_header("sessions-header", "Sessions"),
                Item::Fold(line) => self.render_fold(line, cx),
                Item::More(which) => self.render_show_more(which, cx),
                Item::Gap => div().into_any_element(),
            })
            .collect()
    }

    /// The projects a session of `kind` works in, with `own` and `added`
    /// its workspaces' indices; see [`session_projects`].
    fn projects_of<'a>(
        &self,
        kind: Kind,
        own: Option<usize>,
        added: &[usize],
        cx: &'a App,
    ) -> Vec<&'a Workspace> {
        let workspaces = self.workspaces.read(cx);
        let added = added.iter().map(|&ix| workspaces.get(ix));
        let own = own.map(|ix| workspaces.get(ix));
        session_projects(kind, own, added, Some(self.projects.read(cx)))
    }

    fn render_row(
        &self,
        row: &Row,
        machine: Option<SharedString>,
        clock: Clock,
        view: &WeakEntity<Self>,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let workspace = self.workspaces.read(cx).get(row.workspace);
        let projects = &self.projects_of(row.kind, Some(row.workspace), &row.added, cx);
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
            .child(
                project_line(&row.element_id, projects, surface(state, theme), theme).child(right),
            )
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
                workspace.branch.clone(),
                self.prs.read(cx).mark(&row.id).map(|mark| {
                    pr_mark(&mark, theme)
                        // Apart from the branch before it.
                        .when(workspace.branch.is_some(), Styled::ml_1)
                        .id(SharedString::from(format!("{}-pr", row.element_id)))
                        .test_support()
                        .aria_label(mark_label(&mark))
                        .flex_shrink_0()
                        .into_any_element()
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
        workspace: Option<&Workspace>,
        projects: &[&Workspace],
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
            .child(
                project_line(&draft.element_id, projects, surface(state, theme), theme)
                    .child(right),
            )
            .child(
                div()
                    .h(rems(TITLE_REMS))
                    .truncate()
                    .text_sm()
                    .when(state.active, StyledExt::font_medium)
                    .child(title),
            )
            .child(branch_line(
                workspace.and_then(|workspace| workspace.branch.clone()),
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
            active: self.list.on_screen() == Some(&row.id),
            hovered: self.hover.as_ref().is_some_and(|hover| hover.id == row.id),
        }
    }

    /// What every session row shares: its slot in the list, indented under
    /// the session that started it, the highlight, hover tracking, a click
    /// that opens the session and the context menu. Kit has no list-item
    /// element with a fixed height for `v_virtual_list`, so rows are laid
    /// out by hand.
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
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open(&open_id, cx);
            }));
        div()
            .h(height)
            .px_2()
            .when(row.family == Family::Started, |this| {
                this.pl(rems(NEST_REMS))
            })
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
        machine: Option<SharedString>,
        age_width: Pixels,
        clock: Clock,
        cx: &Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let workspace = self.workspaces.read(cx).get(row.workspace);
        let projects = &self.projects_of(row.kind, Some(row.workspace), &row.added, cx);
        let state = self.row_state(row);
        let wakes = shelf::wakes(row.mark);
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
            projects: projects
                .iter()
                .map(|project| (project.initials.clone(), project.name.clone()))
                .collect(),
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
                ProjectStack::new(
                    format!("{}-projects", row.element_id),
                    projects,
                    rems(LINE_REMS),
                )
                .on(surface(state, theme))
                // The row's card lists them all.
                .untipped()
                .when(!(state.active || state.hovered), ProjectStack::receded),
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

    /// "Snoozed 1 ⌄": the shelf's name and how many families it holds, a
    /// session and those it started counting once; a click opens or folds
    /// it.
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

    /// The header over the active leads, or over the active sessions that
    /// follow them, laid out as a shelf header is, but it neither folds
    /// nor counts.
    fn render_kind_header(id: &'static str, label: &'static str) -> AnyElement {
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
                    .text_sm()
                    .child(label),
            )
            .into_any_element()
    }

    /// "⌄ 3 sessions · 1 needs you": the line under a session that started
    /// others, indented as they are, with the most pressing thing of its
    /// family: how many of them wait for the user, else are Done (never
    /// under a lead, which reads them itself), else work. A click opens or closes the fold, unless the search or the
    /// filter holds it open, which leaves out the chevron. Kit's
    /// `Collapsible` owns its content, which here is separate rows of the
    /// virtual list, so the line is laid out by hand, as a shelf header is.
    fn render_fold(&self, line: FoldLine, cx: &Context<'_, Self>) -> AnyElement {
        let theme = cx.theme();
        let starter = self.list.row_at(line.starter);
        let chevron = match line.fold {
            Fold::Open => Some(IconName::ChevronDown),
            Fold::Closed => Some(IconName::ChevronRight),
            Fold::Showing => None,
        };
        let count = match line.sessions {
            1 => "1 session".to_owned(),
            sessions => format!("{sessions} sessions"),
        };
        // As their rows say it, in their colors.
        let pressing = line.pressing.map(|pressing| match pressing {
            Pressing::Waiting(1) => (theme.warning, "1 needs you".to_owned()),
            Pressing::Waiting(waiting) => (theme.warning, format!("{waiting} need you")),
            Pressing::Done(done) => (theme.success, format!("{done} done")),
            Pressing::Working(working) => (theme.blue, format!("{working} working")),
        });
        let label = match &pressing {
            Some((_, pressing)) => format!("{count} · {pressing}"),
            None => count.clone(),
        };
        let hover = theme.sidebar_accent.opacity(0.5);
        let key = starter.id.clone();
        div()
            .h(rems(FOLD_REMS))
            .px_2()
            .pl(rems(NEST_REMS))
            .py(rems(GUTTER_REMS))
            .child(
                h_flex()
                    .id(SharedString::from(format!("{}-fold", starter.element_id)))
                    .test_support()
                    .aria_label(label)
                    .size_full()
                    .px_2()
                    .gap_1()
                    .rounded(theme.radius)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .when_some(chevron, |this, chevron| {
                        this.cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .child(Icon::new(chevron).xsmall())
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(&key, cx)))
                    })
                    .child(count)
                    .when_some(pressing, |this, (color, pressing)| {
                        this.child("·")
                            .child(div().text_color(color).child(pressing))
                    }),
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

    /// The toggle, "Rho", and New session at the far end; on macOS, the
    /// traffic lights first. "Rho" moves the window, as a title does.
    fn render_header(window: &Window, cx: &Context<'_, Self>) -> impl IntoElement {
        let row = h_flex().flex_shrink_0().h(top_row()).pr_3().gap_1();
        match window_chrome::traffic_light_room(window, cx) {
            Some(room) => row.child(room),
            None => row.pl_3(),
        }
        .child(toggle_button(Layout::Open, cx))
        .child(
            div()
                .id("sidebar-title")
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .font_semibold()
                .moves_window(DragArea::SidebarTitle, window, cx)
                .child("Rho"),
        )
        .child(new_lead_button(cx))
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
                    && self.list.on_screen().is_none()
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
        // The alert registered as `{kind}-{machine}`, which Kit's Alert
        // isn't, with `buttons` under it.
        let alert_with = |kind: &str, alert: Alert, buttons: Vec<Button>| {
            let id = SharedString::from(format!("{kind}-{machine}"));
            v_flex()
                .px_2()
                .gap_1()
                .child(div().id(id).test_support().child(alert.small()))
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
                let buttons = restart_runs.map(|_| restart()).into_iter().collect();
                let why = skew_note(version, restart_runs);
                alert_with("skew", skew_alert(machine, on, why), buttons)
            }
            MachineStatus::OlderDaemon { .. } => alert_with(
                "older-daemon",
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
                "older-rho",
                Alert::error(
                    SharedString::from(format!("machine-error-{machine}")),
                    machines::OLDER_RHO,
                )
                .title(format!("Rho{on} is older than rho-ui")),
                vec![retry()],
            ),
            MachineStatus::Failed(why) => alert_with(
                "unreachable",
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
        let warn_other_builds = cx.global::<Settings>().warn_other_builds;
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
            let unwarned = !warn_other_builds
                && matches!(
                    status,
                    MachineStatus::Ready {
                        another_build: true,
                        ..
                    }
                );
            if !offers_install && !unwarned {
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

/// The warning that `machine`'s daemon is another build, saying `why`; `on`
/// is as in [`SidebarView::render_status`]. Its X turns the warning off for
/// every machine, until Settings → About turns it back on.
fn skew_alert(machine: &MachineId, on: &str, why: String) -> Alert {
    Alert::warning(SharedString::from(format!("machine-skew-{machine}")), why)
        .title(format!("Rho{on} is another build"))
        .on_close(|_, _, cx| {
            settings::update(cx, |settings| settings.warn_other_builds = false);
        })
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
            return frame.child(self.render_rail(window, cx));
        }
        frame
            .child(Self::render_header(window, cx))
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
                    .child(filter::button(self, window, cx)),
            )
            .children(filter::chips(self, cx))
            .when(!self.list.query().is_empty(), |frame| {
                frame.child(
                    div()
                        .flex_shrink_0()
                        .px_3()
                        .pb_2()
                        .child(search_sessions_button(self.list.query(), cx)),
                )
            })
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

/// Hands the search's `query` on to the search of every session's
/// conversation, as the sidebar's search matches only titles and names.
fn search_sessions_button(query: &str, cx: &Context<'_, SidebarView>) -> Button {
    let query = SharedString::from(query.to_owned());
    Button::new("search-sessions")
        .ghost()
        .small()
        .w_full()
        .icon(IconName::Search)
        .label(format!("Search conversations for “{query}”"))
        .tooltip_with_action("Search sessions", &crate::SearchSessions, Some("RhoApp"))
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.emit(SidebarEvent::SearchSessions {
                query: query.clone(),
            });
        }))
}

fn new_lead_button(cx: &Context<'_, SidebarView>) -> Button {
    Button::new("new-lead")
        .ghost()
        .small()
        .icon(Lucide::Network)
        .accessibility_label("New lead")
        .tooltip("New lead")
        .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::NewLead)))
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

/// The session's projects' badges and names, those of the row
/// `element_id`, on a row of `surface`'s color; the caller adds the status
/// or actions.
fn project_line(
    element_id: &SharedString,
    projects: &[&Workspace],
    surface: Hsla,
    theme: &Theme,
) -> gpui_kit::Div {
    let names = project_stack::names(projects, Naming::Full);
    h_flex()
        .h(rems(LINE_REMS))
        .gap_2()
        .text_xs()
        .child(
            ProjectStack::new(format!("{element_id}-projects"), projects, rems(LINE_REMS))
                .on(surface),
        )
        .child(
            div()
                .id(SharedString::from(format!("{element_id}-project-names")))
                .test_support()
                .aria_label(names.clone())
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(names),
        )
}

/// The color under a row's content as `state` lights it: what a project
/// stack rings its badges with.
fn surface(state: RowState, theme: &Theme) -> Hsla {
    row_bg(state, theme).map_or(theme.sidebar, |bg| theme.sidebar.blend(bg))
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
    let row = match row_bg(state, theme) {
        Some(bg) => row.bg(bg),
        None => row,
    };
    if state.active {
        row.text_color(theme.sidebar_accent_foreground)
    } else {
        row
    }
}

/// The background `state` lights a row with, over the sidebar's; `None`
/// for a row at rest.
fn row_bg(state: RowState, theme: &Theme) -> Option<Hsla> {
    let accent = theme.sidebar_accent;
    match (state.active, state.hovered) {
        (true, _) => Some(accent),
        (false, true) => Some(accent.opacity(0.5)),
        (false, false) => None,
    }
}

/// A shelved session's details, shown beside the sidebar while hovered.
/// Built with the row, so the card needs nothing from the view.
struct Card {
    title: SharedString,
    /// Each project's initials and name.
    projects: Vec<(SharedString, SharedString)>,
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
            .label_width(rems(LINE_REMS + 0.75));
        // Each project on a line of its own: the card has the room.
        for (initials, name) in &self.projects {
            details = details.item(badge(initials.clone()).into_any_element(), name.clone(), 1);
        }
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
    branch: Option<SharedString>,
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
        .when_some(branch, |this, branch| {
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

/// "Working 24m", "Needs you 9m", "Waiting" on background work, "Watching"
/// while its agent watches a pull request, "Done", or the age: "3h", after
/// a pin on a pinned row. Icons are static: an animated one would redraw
/// the whole sidebar every frame. So Working gets a symmetric dashed
/// circle, not a spinner frozen mid-turn.
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
    let Some(status) = row.status() else {
        return div()
            .flex_shrink_0()
            .text_color(theme.muted_foreground)
            .when(row.updated > 0, |this| {
                this.child(time::relative_age(clock.now - row.updated))
            })
            .into_any_element();
    };
    let word = status.word();
    match status {
        Status::Working(since) => busy(
            theme.blue,
            Icon::new(Lucide::CircleDashed),
            label(word, since),
        ),
        Status::NeedsYou(since) => busy(
            theme.warning,
            Icon::new(Lucide::CircleAlert),
            label(word, since),
        ),
        Status::Background => h_flex()
            .id(SharedString::from(format!("{}-background", row.element_id)))
            .test_support()
            .aria_label("Waiting on background work")
            .flex_shrink_0()
            .gap_1()
            .text_color(theme.muted_foreground)
            .child(Icon::new(Lucide::Hourglass).xsmall())
            .child(word)
            .into_any_element(),
        // Its agent wakes when the PR changes: no turn runs, but it isn't
        // done. The row's mark below says which PR.
        Status::Watching => h_flex()
            .id(SharedString::from(format!("{}-watching", row.element_id)))
            .test_support()
            .aria_label("Watching a pull request")
            .flex_shrink_0()
            .gap_1()
            .text_color(theme.muted_foreground)
            .child(Icon::new(Lucide::Eye).xsmall())
            .child(word)
            .into_any_element(),
        Status::Done => h_flex()
            .id(SharedString::from(format!("{}-done", row.element_id)))
            .test_support()
            .aria_label(word)
            .flex_shrink_0()
            .gap_1()
            .text_color(theme.success)
            .child(Icon::new(Lucide::CircleCheck).xsmall())
            .child(word)
            .into_any_element(),
    }
}

/// Pinning a session, or unpinning it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pin {
    Pin,
    Unpin,
}

impl Pin {
    /// As [`ShelfAction::mark`]. An unpinned session is kept, on the
    /// active list for a while.
    fn mark(self) -> NewSessionMark {
        match self {
            Pin::Pin => NewSessionMark::Pinned,
            Pin::Unpin => NewSessionMark::Kept,
        }
    }
}

/// This machine's time, in Unix seconds: what a snooze counts from, and
/// what a mark made here is shown with until its daemon stamps its own.
fn now() -> u64 {
    u64::try_from(time::now_unix()).unwrap_or(0)
}

/// The right-click menu: Rename, Pin unless the session moves with the one
/// that started it, the moves the hover buttons offer,
/// Copy, and Delete, which a busy session can't do, nor one whose family is
/// busy.
fn row_menu(
    menu: PopupMenu,
    view: &WeakEntity<SidebarView>,
    row: &MenuRow,
    window: &mut Window,
    cx: &mut Context<'_, PopupMenu>,
) -> PopupMenu {
    let id = row.id.clone();
    let mut menu = menu.item(
        PopupMenuItem::new("Rename…")
            .icon(Icon::new(Lucide::Pencil))
            .on_click(on_view(view, move |this, window, cx| {
                this.start_rename(&id, window, cx);
            })),
    );
    if let Some(pin) = row.pin {
        let (pin_icon, pin_label) = match pin {
            Pin::Unpin => (Lucide::PinOff, "Unpin"),
            Pin::Pin => (Lucide::Pin, "Pin"),
        };
        let id = row.id.clone();
        menu = menu.item(
            PopupMenuItem::new(pin_label)
                .icon(Icon::new(pin_icon))
                .on_click(on_view(view, move |this, _, cx| {
                    this.set_pinned(&id, pin, cx);
                })),
        );
    }
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

/// Delete…, which asks first, and is greyed out while the session is busy,
/// or for a starter, while any of its family is.
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
    /// What the menu offers: a session a session started has no pin of its
    /// own, as it moves with that one, and one its daemon has saved nothing
    /// of can't be pinned, as the daemon keeps no mark for it.
    pin: Option<Pin>,
    /// Busy sessions can't be deleted, nor a starter while any of its
    /// family is busy: as with moves, it waits for them.
    busy: bool,
}

impl MenuRow {
    fn new(row: &Row) -> Self {
        Self {
            id: row.id.clone(),
            title: row.title(),
            cwd: row.cwd.clone(),
            offered: ShelfAction::for_row(row),
            pin: match (row.family, row.pinned) {
                (Family::Started, _) => None,
                (Family::Top(_), Some(_)) => Some(Pin::Unpin),
                (Family::Top(_), None) => row.saved.then_some(Pin::Pin),
            },
            busy: row.busy(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::Path};

    use super::*;
    use crate::session_list::Activity;

    /// A busy session offers no moves, nor a starter at rest while a
    /// session it started works, as its family stays active until then;
    /// nor does a session the daemon has saved nothing of, or a pin, as the
    /// daemon would refuse its mark. Shelves offer a way back.
    #[test]
    fn busy_and_unsaved_sessions_offer_no_moves_and_shelves_offer_a_way_back() {
        let session = |id: &str, parent: Option<&str>| SavedSession {
            id: SessionKey::local(id),
            cwd: PathBuf::from("/w"),
            title: None,
            updated: 1,
            mark: None,
            workspace: 0,
            added: Vec::new(),
            parent: parent.map(SessionKey::local),
            kind: Kind::Session,
        };
        let mut list = SessionList::default();
        let sessions = vec![
            session("busy", None),
            session("w1", Some("lead")),
            session("lead", None),
            session("idle", None),
        ];
        list.set_saved(&MachineId::local(), sessions);
        let working = Activity::Working(Instant::now());
        // "fresh" is a session of this run that has had no prompt.
        for (id, activity) in [
            ("busy", working),
            ("w1", working),
            ("fresh", Activity::Idle),
        ] {
            let live = LiveSession {
                id: SessionKey::local(id),
                cwd: PathBuf::from("/w"),
                title: None,
                placeholder: "",
                activity,
                kind: Some(Kind::Session),
                added_dirs: None,
            };
            list.set_live(live, 0, None, 1);
        }
        list.rebuild(1, &HashSet::new(), |_, _, _: &[usize]| {
            vec![("w", Path::new("/w"))]
        });
        for id in ["busy", "lead", "fresh"] {
            let row = list.row(&SessionKey::local(id)).expect("listed");
            assert_eq!(ShelfAction::for_row(row), [], "{id}");
        }
        let menu = |id: &str| MenuRow::new(list.row(&SessionKey::local(id)).expect("listed"));
        assert_eq!(menu("fresh").pin, None);
        assert_eq!(
            (menu("idle").offered, menu("idle").pin),
            (&ShelfAction::ON_ACTIVE[..], Some(Pin::Pin)),
            "a saved session at rest offers them all"
        );
        assert_eq!(ShelfAction::offered(Shelf::Settled), [ShelfAction::Restore]);
        assert!(ShelfAction::offered(Shelf::Snoozed).contains(&ShelfAction::Restore));
    }
}
