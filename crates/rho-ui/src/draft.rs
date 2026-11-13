//! A new session before its first message: a header, and a composer with the
//! session's settings. The app opens the session as soon as the draft
//! appears, so the model, reasoning and mode can be set before the first
//! message. rho writes no file for a session until its first prompt, and a
//! draft stops following its session when it goes away unused, which lets
//! the daemon discard it, so an unused New session leaves nothing behind in
//! the sidebar or in rho.
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
//! the draft's settings, and sends the message there, so choosing New
//! worktree and back leaves no worktree behind.

use std::path::{Path, PathBuf};

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{alert::Alert, input::InputEvent, v_flex, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    rems, AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window,
};
use rho_ui_core::{
    Attached, Checkout, ConfigOption, Daemon, DaemonError, Prompt, Session, SessionId,
    SessionUpdate,
};

use crate::{
    composer::{Composer, ComposerMode, ComposerState},
    project_picker::{PickerEvent, ProjectPicker},
    projects::Projects,
    session_list::NEW_SESSION,
    session_settings::{SessionSettings, SettingsEvent},
    session_view::header,
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
    /// Open a session in a new worktree of the draft's directory, give it
    /// `config`, the draft session's settings, then show it and send
    /// `prompt` as its first. [`DraftView::launch_failed`] says it couldn't.
    StartInWorktree {
        config: Vec<ConfigOption>,
        prompt: Prompt,
    },
    /// Open a session in the draft's directory: the first failed, or the
    /// project changed.
    OpenSession,
}

/// The draft's session.
enum SessionState {
    /// The app is opening it.
    Opening,
    Open {
        daemon: Daemon,
        session: Box<Session>,
        settings: Entity<SessionSettings>,
        _subscription: Subscription,
    },
    /// Opening failed; `DraftView::error` says why.
    Failed,
    /// Handed to the app with the first message.
    Started,
}

pub(crate) struct DraftView {
    cwd: PathBuf,
    workspace: WorkspaceRef,
    picker: Entity<ProjectPicker>,
    composer: Entity<ComposerState>,
    state: SessionState,
    /// Counts the sessions the draft asked the app to open; only the
    /// latest is wanted.
    request: u64,
    /// The user sent while the session was opening; it goes out once open.
    queued: bool,
    /// The first message went to the app to start in a new worktree.
    launching: bool,
    /// Why opening the session, or a settings change, failed.
    error: Option<String>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DraftEvent> for DraftView {}

impl DraftView {
    /// A draft for a session in `cwd`, which the user can move to any of
    /// `projects`.
    pub(crate) fn new(
        cwd: PathBuf,
        workspaces: &Entity<Workspaces>,
        projects: &Entity<Projects>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let composer = cx.new(|cx| ComposerState::new(window, cx));
        let input = composer.read(cx).input().clone();
        let workspace = WorkspaceRef::new(workspaces, &cwd, cx);
        let picker = cx.new(|cx| ProjectPicker::new(workspaces, projects, cwd.clone(), window, cx));
        let subscriptions = vec![
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
            cx.observe(workspace.table(), |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &picker,
                window,
                |this, _, event: &PickerEvent, window, cx| match event {
                    PickerEvent::Picked(cwd) => this.move_to(cwd.clone(), window, cx),
                    PickerEvent::Dismissed => window.focus(&this.composer.focus_handle(cx), cx),
                },
            ),
            // An unused session goes with its draft: unfollowed and
            // unsaved, the daemon discards it.
            cx.on_release(|this, cx| {
                let state = std::mem::replace(&mut this.state, SessionState::Started);
                let_go(state, cx);
            }),
        ];
        Self {
            cwd,
            workspace,
            picker,
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

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn composer_focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }

    /// The open session's ID, for routing its events here.
    pub(crate) fn session_id(&self) -> Option<&SessionId> {
        match &self.state {
            SessionState::Open { session, .. } => Some(session.id()),
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
        self.picker.read(cx).checkout()
    }

    /// The project row the keyboard is on, for UI tests.
    #[cfg(test)]
    pub(crate) fn selected_project(&self, cx: &App) -> Option<String> {
        self.picker.read(cx).selected_row(cx)
    }

    /// Puts the keyboard in the project search, as Ctrl+Shift+N does.
    pub(crate) fn choose_project(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.picker
            .update(cx, |picker, cx| picker.focus(window, cx));
    }

    /// Moves the draft to `cwd`. The session opened for the old directory
    /// goes, unfollowed and unsaved, and the app opens one in the new; the
    /// text in the composer stays.
    fn move_to(&mut self, cwd: PathBuf, window: &mut Window, cx: &mut Context<'_, Self>) {
        if cwd == self.cwd || matches!(self.state, SessionState::Started) {
            window.focus(&self.composer.focus_handle(cx), cx);
            return;
        }
        self.workspace = WorkspaceRef::new(self.workspace.table(), &cwd, cx);
        self.picker
            .update(cx, |picker, cx| picker.set_current(cwd.clone(), window, cx));
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
    pub(crate) fn request(&self) -> (PathBuf, u64) {
        (self.cwd.clone(), self.request)
    }

    /// Whether the draft still waits for its first message to start in a
    /// new worktree.
    pub(crate) fn launching(&self) -> bool {
        self.launching
    }

    /// The new worktree's session couldn't be started; the message stays in
    /// the composer for another try.
    pub(crate) fn launch_failed(&mut self, error: String, cx: &mut Context<'_, Self>) {
        self.launching = false;
        self.error = Some(error);
        cx.notify();
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

    /// Takes the session the app opened, and sends a waiting message.
    pub(crate) fn open(
        &mut self,
        daemon: Daemon,
        session: Session,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let settings = cx.new(|cx| {
            SessionSettings::new(
                daemon.clone(),
                session.id().clone(),
                session.config(),
                window,
                cx,
            )
        });
        let subscription = cx.subscribe_in(&settings, window, Self::on_settings_event);
        self.state = SessionState::Open {
            daemon,
            session: Box::new(session),
            settings,
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
                    session, settings, ..
                },
            ) => {
                session.attached(attached);
                settings.update(cx, |settings, cx| {
                    settings.sync(session.config(), window, cx);
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
            session, settings, ..
        } = &mut self.state
        {
            session.apply(update);
            settings.update(cx, |settings, cx| {
                settings.sync(session.config(), window, cx);
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
                    session.set_config(options.clone());
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
        let checkout = self.picker.read(cx).checkout();
        match (
            std::mem::replace(&mut self.state, SessionState::Started),
            checkout,
        ) {
            (SessionState::Open { session, .. }, Checkout::Local) => {
                cx.emit(DraftEvent::Start { session, prompt });
            }
            // The draft's session stays, with its settings, until the
            // worktree's session takes over.
            (state @ SessionState::Open { .. }, Checkout::NewWorktree) => {
                if let SessionState::Open { session, .. } = &state {
                    let config = session.config().to_vec();
                    self.launching = true;
                    self.error = None;
                    cx.emit(DraftEvent::StartInWorktree { config, prompt });
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
        let settings = match &self.state {
            SessionState::Open { settings, .. } => Some(settings),
            SessionState::Opening | SessionState::Failed | SessionState::Started => None,
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
                .leading(SessionSettings::render_modes(settings, cx))
                .trailing(SessionSettings::render_panel_button(settings, cx));
        }
        v_flex()
            .id("draft-view")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(header(
                NEW_SESSION.to_owned(),
                self.workspace.read(cx),
                None::<AnyElement>,
                cx,
            ))
            .child(
                // The project list, centered in the space above the
                // composer, at its width.
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
                    .child(self.picker.clone()),
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
                    .child(composer),
            )
    }
}
