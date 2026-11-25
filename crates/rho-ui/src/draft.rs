//! A new session before its first message: a title bar, and a composer with
//! the session's settings. The app opens the session as soon as the draft
//! appears, so the model, reasoning and mode can be set before the first
//! message. rho writes no file for a session until its first prompt, and a
//! draft stops following its session when it goes away unused, which lets
//! the daemon discard it, so an unused New session leaves nothing behind in
//! the sidebar or in rho. One left with a message in its composer stays,
//! session and all: the sidebar lists it at the top until it is sent or
//! discarded.
//!
//! Above the composer, a [`ProjectPicker`] lists the user's projects it can
//! start in, and chooses whether the session works in the project's
//! checkout or in a new git worktree that rho adds for it. A session's
//! directory is fixed when it opens, so a move lets the open session go and
//! opens another in the new directory.
//!
//! The draft's own session always works in the checkout: it holds the
//! settings while the user writes. A worktree is made only when the first
//! message goes: the app then opens a session in a new worktree, gives it
//! the draft's settings and its choice for the sessions it starts, and
//! sends the message there, so choosing New worktree and back leaves no
//! worktree behind.
//!
//! A lead's draft ([`Kind::Lead`]) has no project picker: its session is a
//! lead in the leads root (see [`crate::leads`]), with the lead agent's
//! settings, and the draft shows no folder until that session is open.
//! Once it is, the title bar adds the projects the lead works in to that
//! session ([`AddedDirs`]), as a started lead's does. The first message
//! starts the lead in a folder of its own, named from the message, the
//! same way, with the directories added to the draft's session.

use std::path::PathBuf;

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{alert::Alert, input::InputEvent, v_flex, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    rems, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window,
};
use rho_ui_core::{
    Attached, Checkout, Daemon, DaemonError, Prompt, Session, SessionOptions, SessionUpdate,
    StartedSessionDefaults,
};

use crate::{
    added_dirs::{AddedDirs, DirsAccess, Wording},
    auths::Auths,
    composer::{Composer, ComposerMode, ComposerState},
    leads::Kind,
    machines::{MachineId, Machines, Place, SessionKey},
    project_picker::{PickerEvent, ProjectPicker},
    projects::Projects,
    session_list::{NEW_LEAD, NEW_SESSION},
    session_settings::{SessionSettings, SettingsEvent},
    sign_in::SignInSlot,
    title_bar::TitleBar,
    workspaces::{WorkspaceRef, Workspaces},
};

/// What the draft asks of the app.
pub(crate) enum DraftEvent {
    /// Show `session`, the one the draft opened, and send `prompt` as its
    /// first.
    Start {
        session: Box<Session>,
        prompt: Prompt,
    },
    /// Open a session of its own for the first message, `prompt`: in a new
    /// worktree of the draft's directory, or for a lead's draft, a lead in
    /// a folder named from it. Give it `settings`, the draft session's own
    /// and its choice for the sessions it starts, then show it and send
    /// `prompt` as its first. [`DraftView::launch_failed`] says it
    /// couldn't.
    StartIn {
        settings: Box<DraftSettings>,
        prompt: Prompt,
    },
    /// Open a session in the draft's directory: the first failed, or the
    /// project changed.
    OpenSession,
}

/// A session's draft, or a lead's.
enum DraftKind {
    /// A session's, which `picker` moves between projects and checkouts.
    Session { picker: Entity<ProjectPicker> },
    /// A lead's, which picks no project but adds the user's `projects`
    /// to its session for the lead to work in.
    Lead { projects: Entity<Projects> },
}

/// What a draft's session was set to, for the session that takes over from
/// it: its own settings, its choice for the sessions it starts, and the
/// directories added to it, a lead's projects.
#[derive(Clone)]
pub(crate) struct DraftSettings {
    pub(crate) options: Option<SessionOptions>,
    pub(crate) started: Option<StartedSessionDefaults>,
    pub(crate) added_dirs: Vec<PathBuf>,
}

/// The draft's session.
enum SessionState {
    /// The app is opening it.
    Opening,
    Open {
        daemon: Daemon,
        session: Box<Session>,
        settings: Entity<SessionSettings>,
        /// A lead's projects, in its title bar; a session's draft picks
        /// its project instead.
        added_dirs: Option<Entity<AddedDirs>>,
        /// The sign-in its model's auth profile needs, while it is signed
        /// out.
        sign_in: Box<SignInSlot>,
        _subscription: Subscription,
    },
    /// Opening failed; `DraftView::error` says why.
    Failed,
    /// Handed to the app with the first message.
    Started,
}

pub(crate) struct DraftView {
    kind: DraftKind,
    /// Where the session will work: for a lead's draft, the leads root its
    /// session opened in, with no path before that.
    cwd: Place,
    machines: Entity<Machines>,
    /// Every machine's sign-ins, for the session's.
    auths: Entity<Auths>,
    workspaces: Entity<Workspaces>,
    /// `cwd`'s; `None` for a lead's draft until its session opens.
    workspace: Option<WorkspaceRef>,
    composer: Entity<ComposerState>,
    state: SessionState,
    /// Counts the sessions the draft asked the app to open; only the
    /// latest is wanted.
    request: u64,
    /// The user sent while the session was opening; it goes out once open.
    queued: bool,
    /// The first message went to the app to start in a session of its own
    /// ([`DraftEvent::StartIn`]).
    launching: bool,
    /// Why opening the session, or a settings change, failed.
    error: Option<String>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DraftEvent> for DraftView {}

impl DraftView {
    /// A draft for a session in `cwd`, which the user can move to any of
    /// `projects`, on any of `machines`.
    pub(crate) fn new(
        cwd: Place,
        workspaces: &Entity<Workspaces>,
        projects: &Entity<Projects>,
        machines: &Entity<Machines>,
        auths: Entity<Auths>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let picker = cx
            .new(|cx| ProjectPicker::new(workspaces, projects, machines, cwd.clone(), window, cx));
        let kind = DraftKind::Session { picker };
        Self::of_kind(kind, cwd, workspaces, machines, auths, window, cx)
    }

    /// A lead's draft on `machine`, whose session opens in the leads root
    /// there, and which offers `projects` for the lead to work in; see
    /// [`Self::new`].
    pub(crate) fn new_lead(
        machine: MachineId,
        workspaces: &Entity<Workspaces>,
        projects: &Entity<Projects>,
        machines: &Entity<Machines>,
        auths: Entity<Auths>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let kind = DraftKind::Lead {
            projects: projects.clone(),
        };
        let cwd = Place::new(machine, PathBuf::new());
        Self::of_kind(kind, cwd, workspaces, machines, auths, window, cx)
    }

    /// A draft of `kind` in `cwd`: [`Self::new`] and [`Self::new_lead`].
    fn of_kind(
        kind: DraftKind,
        cwd: Place,
        workspaces: &Entity<Workspaces>,
        machines: &Entity<Machines>,
        auths: Entity<Auths>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let remote = machines.read(cx).remote_label(&cwd.machine);
        let composer = cx.new(|cx| ComposerState::new(remote, window, cx));
        let input = composer.read(cx).input().clone();
        let mut subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } => this.send(cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                },
            ),
            // Attachments added or removed.
            cx.observe(&composer, |_, _, cx| cx.notify()),
            cx.observe(workspaces, |_, _, cx| cx.notify()),
            // An unused session goes with its draft: unfollowed and
            // unsaved, the daemon discards it.
            cx.on_release(|this, cx| {
                let state = std::mem::replace(&mut this.state, SessionState::Started);
                let_go(state, cx);
            }),
        ];
        let workspace = match &kind {
            DraftKind::Session { picker } => {
                subscriptions.push(cx.subscribe_in(picker, window, Self::on_picker_event));
                Some(WorkspaceRef::new(workspaces, &cwd, cx))
            }
            DraftKind::Lead { .. } => {
                input.update(cx, |input, cx| {
                    input.set_placeholder(Kind::Lead.placeholder(), window, cx);
                });
                None
            }
        };
        Self {
            kind,
            cwd,
            machines: machines.clone(),
            auths,
            workspaces: workspaces.clone(),
            workspace,
            composer,
            state: SessionState::Opening,
            request: 0,
            queued: false,
            launching: false,
            error: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn kind(&self) -> Kind {
        match self.kind {
            DraftKind::Session { .. } => Kind::Session,
            DraftKind::Lead { .. } => Kind::Lead,
        }
    }

    /// The project picker, which a lead's draft hasn't.
    fn picker(&self) -> Option<&Entity<ProjectPicker>> {
        match &self.kind {
            DraftKind::Session { picker } => Some(picker),
            DraftKind::Lead { .. } => None,
        }
    }

    fn on_picker_event(
        &mut self,
        _: &Entity<ProjectPicker>,
        event: &PickerEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            PickerEvent::Picked(cwd) => self.move_to(cwd.clone(), window, cx),
            PickerEvent::Dismissed => window.focus(&self.composer.focus_handle(cx), cx),
        }
    }

    pub(crate) fn cwd(&self) -> &Place {
        &self.cwd
    }

    /// The draft's workspace's index into the shared table; `None` for a
    /// lead's draft until its session opens.
    pub(crate) fn workspace_index(&self) -> Option<usize> {
        self.workspace.as_ref().map(WorkspaceRef::index)
    }

    /// Whether the composer holds a message worth coming back to: more than
    /// whitespace, or an attachment.
    pub(crate) fn has_message(&self, cx: &App) -> bool {
        self.composer.read(cx).has_message(cx)
    }

    /// The first line of the message with any text, for the sidebar's row.
    pub(crate) fn title(&self, cx: &App) -> Option<SharedString> {
        let input = self.composer.read(cx).input().read(cx);
        input
            .value()
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| SharedString::from(line.to_owned()))
    }

    pub(crate) fn composer_focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }

    /// The open session, for routing its events here.
    pub(crate) fn session_key(&self) -> Option<SessionKey> {
        match &self.state {
            SessionState::Open { session, .. } => Some(SessionKey::new(
                self.cwd.machine.clone(),
                session.id().clone(),
            )),
            SessionState::Opening | SessionState::Failed | SessionState::Started => None,
        }
    }

    /// The draft's session once open, for UI tests.
    #[cfg(test)]
    pub(crate) fn session(&self) -> Option<&Session> {
        match &self.state {
            SessionState::Open { session, .. } => Some(session),
            SessionState::Opening | SessionState::Failed | SessionState::Started => None,
        }
    }

    /// Where the session will work, for UI tests.
    #[cfg(test)]
    pub(crate) fn checkout(&self, cx: &App) -> Checkout {
        self.picker()
            .map_or(Checkout::Local, |picker| picker.read(cx).checkout(cx))
    }

    /// The project row the keyboard is on, for UI tests.
    #[cfg(test)]
    pub(crate) fn selected_project(&self, cx: &App) -> Option<String> {
        self.picker()?.read(cx).selected_row(cx)
    }

    /// Puts the keyboard in the project search, as Ctrl+Shift+N does.
    pub(crate) fn choose_project(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(picker) = self.picker() {
            picker.update(cx, |picker, cx| picker.focus(window, cx));
        }
    }

    /// Moves the draft to `cwd`. The session opened for the old directory
    /// goes, unfollowed and unsaved, and the app opens one in the new; the
    /// text in the composer stays. A draft already sent stays where it is,
    /// including one whose worktree is still being made.
    fn move_to(&mut self, cwd: Place, window: &mut Window, cx: &mut Context<'_, Self>) {
        if cwd == self.cwd || self.launching || matches!(self.state, SessionState::Started) {
            window.focus(&self.composer.focus_handle(cx), cx);
            return;
        }
        self.workspace = Some(WorkspaceRef::new(&self.workspaces, &cwd, cx));
        let remote = self.machines.read(cx).remote_label(&cwd.machine);
        self.composer
            .update(cx, |composer, cx| composer.set_remote(remote, cx));
        if let Some(picker) = self.picker() {
            picker.update(cx, |picker, cx| picker.set_current(cwd.clone(), window, cx));
        }
        self.cwd = cwd;
        self.replace_session(cx);
        window.focus(&self.composer.focus_handle(cx), cx);
    }

    /// Lets the open session go, unfollowed and unsaved, and asks the app
    /// for one in the draft's directory.
    fn replace_session(&mut self, cx: &mut Context<'_, Self>) {
        let_go(
            std::mem::replace(&mut self.state, SessionState::Opening),
            cx,
        );
        self.error = None;
        self.reopen(cx);
        cx.notify();
    }

    /// The draft's directory, and which of its requests to open a session
    /// there this is, for [`Self::awaits`].
    pub(crate) fn request(&self) -> (Place, u64) {
        (self.cwd.clone(), self.request)
    }

    /// Whether the draft still waits for its first message to start in a
    /// session of its own.
    pub(crate) fn launching(&self) -> bool {
        self.launching
    }

    /// The session of its own couldn't be started; the message stays in
    /// the composer for another try.
    pub(crate) fn launch_failed(&mut self, error: String, cx: &mut Context<'_, Self>) {
        self.launching = false;
        self.error = Some(error);
        cx.notify();
    }

    /// The connection to the draft's machine is gone, and with it the
    /// draft's session and any worktree start under way. The message and a
    /// send waiting on it stay; the app asks for a session again.
    pub(crate) fn disconnected(&mut self, cx: &mut Context<'_, Self>) {
        let_go(
            std::mem::replace(&mut self.state, SessionState::Opening),
            cx,
        );
        self.launching = false;
        self.error = None;
        cx.notify();
    }

    /// The auth profile the draft session's model runs on, once known.
    fn session_auth(&self) -> Option<&str> {
        match &self.state {
            SessionState::Open { session, .. } => session.auth(),
            SessionState::Opening | SessionState::Failed | SessionState::Started => None,
        }
    }

    /// Whether the draft still waits for the answer to `request`. An
    /// earlier request's session, such as one opened before the project
    /// changed, is not wanted: the app leaves it unfollowed, and the daemon
    /// discards it.
    pub(crate) fn awaits(&self, request: u64) -> bool {
        request == self.request && matches!(self.state, SessionState::Opening)
    }

    /// Asks the app for a session in the draft's directory.
    fn reopen(&mut self, cx: &mut Context<'_, Self>) {
        self.request += 1;
        cx.emit(DraftEvent::OpenSession);
    }

    /// Takes the session the app opened in `dir`, and sends a waiting
    /// message. A lead's draft works in `dir`, the leads root, from then on.
    pub(crate) fn open(
        &mut self,
        daemon: Daemon,
        session: Session,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if dir != self.cwd.path {
            self.cwd.path = dir;
            self.workspace = Some(WorkspaceRef::new(&self.workspaces, &self.cwd, cx));
        }
        let settings = cx.new(|cx| {
            let mut settings = SessionSettings::new(
                daemon.clone(),
                session.id().clone(),
                session.options(),
                window,
                cx,
            );
            settings.sync_started(session.started_session_defaults(), cx);
            settings
        });
        let sign_in = Box::new(SignInSlot::new(
            self.auths.clone(),
            self.cwd.machine.clone(),
            Self::session_auth,
            window,
            cx,
        ));
        let subscription = cx.subscribe_in(&settings, window, Self::on_settings_event);
        let added_dirs = match &self.kind {
            DraftKind::Lead { projects } => {
                let workspace = WorkspaceRef::new(&self.workspaces, &self.cwd, cx);
                Some(AddedDirs::of_session(
                    &daemon,
                    &session,
                    &workspace,
                    DirsAccess::Add(projects.clone()),
                    Wording::from(Kind::Lead),
                    window,
                    cx,
                ))
            }
            DraftKind::Session { .. } => None,
        };
        self.state = SessionState::Open {
            daemon,
            session: Box::new(session),
            settings,
            added_dirs,
            sign_in,
            _subscription: subscription,
        };
        self.error = None;
        if std::mem::take(&mut self.queued) {
            self.send(cx);
        }
        cx.notify();
    }

    /// The session couldn't be opened. The text stays; Send tries again.
    pub(crate) fn fail(&mut self, error: String, cx: &mut Context<'_, Self>) {
        self.state = SessionState::Failed;
        self.queued = false;
        self.error = Some(error);
        cx.notify();
    }

    /// Records the answer to the open session's attach. A session nothing
    /// follows would be discarded under the draft, so a failure fails it.
    pub(crate) fn attached(
        &mut self,
        attached: Result<Attached, DaemonError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match (attached, &mut self.state) {
            (
                Ok(attached),
                SessionState::Open {
                    session,
                    settings,
                    added_dirs,
                    ..
                },
            ) => {
                session.attached(attached);
                settings.update(cx, |settings, cx| {
                    settings.sync(session.options(), window, cx);
                    settings.sync_started(session.started_session_defaults(), cx);
                });
                if let Some(added_dirs) = added_dirs {
                    added_dirs.update(cx, |dirs, cx| dirs.sync(session.added_dirs(), cx));
                }
                self.composer.update(cx, |composer, cx| {
                    composer.set_commands(session.commands(), cx);
                });
            }
            (Err(error), _) => self.fail(error.to_string(), cx),
            (Ok(_), _) => {}
        }
    }

    /// Applies an event the daemon sent for the open session.
    pub(crate) fn apply(
        &mut self,
        update: SessionUpdate,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let SessionState::Open {
            session,
            settings,
            added_dirs,
            ..
        } = &mut self.state
        {
            session.apply(update);
            settings.update(cx, |settings, cx| {
                settings.sync(session.options(), window, cx);
                settings.sync_started(session.started_session_defaults(), cx);
            });
            if let Some(added_dirs) = added_dirs {
                added_dirs.update(cx, |dirs, cx| dirs.sync(session.added_dirs(), cx));
            }
            self.composer.update(cx, |composer, cx| {
                composer.set_commands(session.commands(), cx);
            });
        }
    }

    fn on_settings_event(
        &mut self,
        _: &Entity<SessionSettings>,
        event: &SettingsEvent,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            SettingsEvent::Changed(options) => {
                self.error = None;
                if let SessionState::Open { session, .. } = &mut self.state {
                    session.set_options(options.clone());
                }
            }
            SettingsEvent::Failed(message) => self.error = Some(message.clone()),
        }
        cx.notify();
    }

    fn send(&mut self, cx: &mut Context<'_, Self>) {
        if self.queued || self.launching {
            return;
        }
        let prompt = self.composer.read(cx).prompt(cx);
        if prompt.is_empty() {
            return;
        }
        // In the project's checkout, the draft's own session takes it; in a
        // new worktree, or as a lead, a session of its own.
        let own = self
            .picker()
            .is_some_and(|picker| picker.read(cx).checkout(cx) == Checkout::Local);
        match (
            std::mem::replace(&mut self.state, SessionState::Started),
            own,
        ) {
            (SessionState::Open { session, .. }, true) => {
                cx.emit(DraftEvent::Start { session, prompt });
            }
            // The draft's session stays, with its settings, until the
            // new session takes over.
            (state @ SessionState::Open { .. }, false) => {
                if let SessionState::Open { session, .. } = &state {
                    let settings = Box::new(DraftSettings {
                        options: session.options().cloned(),
                        started: session.started_session_defaults().cloned(),
                        added_dirs: session.added_dirs().to_vec(),
                    });
                    self.launching = true;
                    self.error = None;
                    cx.emit(DraftEvent::StartIn { settings, prompt });
                }
                self.state = state;
            }
            (SessionState::Opening, _) => {
                self.state = SessionState::Opening;
                self.queued = true;
            }
            (SessionState::Failed, _) => {
                self.state = SessionState::Opening;
                self.queued = true;
                self.error = None;
                self.reopen(cx);
            }
            (SessionState::Started, _) => {}
        }
        cx.notify();
    }
}

#[cfg(test)]
impl DraftView {
    /// The message the composer holds, attachments and all, for UI tests.
    pub(crate) fn composer_prompt(&self, cx: &App) -> Prompt {
        self.composer.read(cx).prompt(cx)
    }
}

/// Lets an unused draft's session go: unfollowed and unsaved, the daemon
/// discards it.
fn let_go(state: SessionState, cx: &mut App) {
    if let SessionState::Open {
        daemon, session, ..
    } = state
    {
        cx.spawn(async move |_| {
            let _ = daemon.detach(session.id()).await;
        })
        .detach();
    }
}

impl Render for DraftView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let mode = if self.queued || self.launching || matches!(self.state, SessionState::Started) {
            ComposerMode::Starting
        } else {
            ComposerMode::Send
        };
        let (settings, added_dirs, sign_in) = match &self.state {
            SessionState::Open {
                settings,
                added_dirs,
                sign_in,
                session,
                ..
            } => (
                Some(settings),
                added_dirs.clone(),
                sign_in.card(session.auth(), cx),
            ),
            SessionState::Opening | SessionState::Failed | SessionState::Started => {
                (None, None, None)
            }
        };
        let mut composer = Composer::new(
            &self.composer,
            mode,
            move |_, cx| {
                let _ = view.update(cx, DraftView::send);
            },
            |_, _| {},
        );
        if let Some(settings) = settings {
            composer = composer
                .leading(SessionSettings::render_mode(settings, cx))
                .trailing(SessionSettings::render_panel_button(settings, cx));
        }
        let title = match self.kind() {
            Kind::Session => NEW_SESSION,
            Kind::Lead => NEW_LEAD,
        };
        let workspace = self.workspace.as_ref().map(|workspace| workspace.read(cx));
        // A lead's projects, at the right, where a session's title bar has
        // its added directories.
        let title_bar = TitleBar::new(workspace, title).trailing(added_dirs);
        v_flex()
            .id("draft-view")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(title_bar)
            .child(
                // The project list, centered in the space above the
                // composer, at its width; a lead picks none.
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .justify_center()
                    .w_full()
                    .max_w(rems(48.))
                    .mx_auto()
                    .px_4()
                    .pt_4()
                    .pb_4()
                    .children(self.picker().cloned()),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .w_full()
                    .max_w(rems(48.))
                    .mx_auto()
                    .gap_2()
                    .px_4()
                    .pb_3()
                    .when_some(self.error.clone(), |this, error| {
                        this.child(
                            Alert::error("draft-error", SharedString::from(error))
                                .title("Couldn't start the session")
                                .small(),
                        )
                    })
                    .children(sign_in)
                    .child(composer),
            )
    }
}
