//! A new thread before its first message: a header, and a composer with the
//! session's settings. The app opens the session as soon as the draft
//! appears, so the model, reasoning and mode can be set before the first
//! message. rho writes no file for a session until its first prompt, and a
//! draft closes its session when it goes away unused, so an unused New
//! thread leaves nothing behind in the sidebar or in rho.

use std::path::{Path, PathBuf};

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    alert::Alert,
    empty::{Empty, EmptyDescription, EmptyHeader},
    input::{InputEvent, TextareaState},
    v_flex, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    rems, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window,
};
use rho_ui_core::{Agent, SessionId, Thread, ThreadUpdate};

use crate::{
    composer::{self, Composer, ComposerMode},
    projects::ProjectRef,
    session_settings::{SessionSettings, SettingsEvent},
    thread_list::NEW_THREAD,
    thread_view::header,
};

/// What the draft asks of the app.
pub(crate) enum DraftEvent {
    /// Open `thread`, the draft's session, and send `text` as its first
    /// prompt.
    Start { thread: Box<Thread>, text: String },
    /// The session couldn't be opened; try again.
    Retry,
}

/// The draft's session.
enum Session {
    /// The app is opening it.
    Opening,
    Open {
        agent: Agent,
        thread: Box<Thread>,
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
    project: ProjectRef,
    composer: Entity<TextareaState>,
    session: Session,
    /// The user sent while the session was opening; it goes out once open.
    queued: bool,
    /// Why opening the session, or a settings change, failed.
    error: Option<String>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DraftEvent> for DraftView {}

impl DraftView {
    pub(crate) fn new(
        cwd: PathBuf,
        project: ProjectRef,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let composer = composer::new_state(window, cx);
        let subscriptions = vec![
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } => this.send(cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                },
            ),
            cx.observe(project.table(), |_, _, cx| cx.notify()),
            // An unused session is freed with its draft.
            cx.on_release(|this, cx| {
                if let Session::Open { agent, thread, .. } = &this.session {
                    let (agent, id) = (agent.clone(), thread.id().clone());
                    cx.spawn(async move |_| {
                        let _ = agent.close_session(&id).await;
                    })
                    .detach();
                }
            }),
        ];
        Self {
            cwd,
            project,
            composer,
            session: Session::Opening,
            queued: false,
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

    /// The open session's ID, for routing its updates here.
    pub(crate) fn session_id(&self) -> Option<&SessionId> {
        match &self.session {
            Session::Open { thread, .. } => Some(thread.id()),
            Session::Opening | Session::Failed | Session::Started => None,
        }
    }

    /// Takes the session the app opened, and sends a waiting message.
    pub(crate) fn open(
        &mut self,
        agent: Agent,
        thread: Thread,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let settings = cx.new(|cx| {
            SessionSettings::new(
                agent.clone(),
                thread.id().clone(),
                thread.config(),
                window,
                cx,
            )
        });
        let subscription = cx.subscribe_in(&settings, window, Self::on_settings_event);
        self.session = Session::Open {
            agent,
            thread: Box::new(thread),
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
        self.session = Session::Failed;
        self.queued = false;
        self.error = Some(error);
        cx.notify();
    }

    /// Applies an update the agent sent for the open session.
    pub(crate) fn apply(
        &mut self,
        update: ThreadUpdate,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Session::Open {
            thread, settings, ..
        } = &mut self.session
        {
            thread.apply(update);
            settings.update(cx, |settings, cx| {
                settings.sync(thread.config(), window, cx);
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
                if let Session::Open { thread, .. } = &mut self.session {
                    thread.set_config(options.clone());
                }
            }
            SettingsEvent::Failed(message) => self.error = Some(message.clone()),
        }
        cx.notify();
    }

    fn send(&mut self, cx: &mut Context<'_, Self>) {
        if self.queued {
            return;
        }
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        match std::mem::replace(&mut self.session, Session::Started) {
            Session::Open { thread, .. } => cx.emit(DraftEvent::Start { thread, text }),
            Session::Opening => {
                self.session = Session::Opening;
                self.queued = true;
            }
            Session::Failed => {
                self.session = Session::Opening;
                self.queued = true;
                self.error = None;
                cx.emit(DraftEvent::Retry);
            }
            Session::Started => {}
        }
        cx.notify();
    }
}

impl Render for DraftView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let mode = if self.queued || matches!(self.session, Session::Started) {
            ComposerMode::Starting
        } else {
            ComposerMode::Send
        };
        let settings = match &self.session {
            Session::Open { settings, .. } => Some(settings),
            Session::Opening | Session::Failed | Session::Started => None,
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
            .child(header(NEW_THREAD.to_owned(), self.project.read(cx), cx))
            .child(
                Empty::new().header(
                    EmptyHeader::new()
                        .description(EmptyDescription::new().child("Send a message to start.")),
                ),
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
                                .title("Couldn't start the thread")
                                .small(),
                        )
                    })
                    .child(composer),
            )
    }
}
