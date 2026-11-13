//! One session: a header with its title and workspace, the transcript,
//! approvals, and the composer with the session settings. Owns the session's
//! [`Session`] state and every request the user makes in it.
//!
//! While a turn runs, whoever started it, the composer steers it: Enter adds
//! the text to the turn's next model request, and it waits above the box
//! until the model reads it. A message the turn can't take, as one sent
//! just as the daemon began a turn of its own to deliver background work,
//! waits above the box too, and goes as a prompt once that turn ends.
//!
//! The session's background work is listed above the box, with a button to
//! stop each piece that runs.

use std::{path::PathBuf, rc::Rc, time::Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    alert::Alert,
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia},
    h_flex,
    input::{InputEvent, TextareaState},
    message_scroller::MessageScroller,
    spinner::Spinner,
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, rems, AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription, Window,
};
use rho_ui_core::{
    Attached, BackgroundId, Daemon, DaemonError, Entry, RunState, Session, SessionId,
    SessionUpdate, SteerId, TurnId, Unsteered,
};

use crate::{
    approval::ApprovalCard,
    background::BackgroundList,
    composer::ComposerMode,
    composer::{self, Composer},
    session_list::{Activity, LiveSession, NEW_SESSION, UNTITLED},
    session_settings::{Availability, SessionSettings, SettingsEvent},
    transcript::{ActionHandler, Transcript},
    workspaces::{Workspace, WorkspaceRef},
    StopTurn,
};

/// What the app needs to hear from a session.
pub(crate) enum SessionEvent {
    /// Title or activity changed; see [`SessionView::live`].
    LiveChanged,
    /// The daemon took a prompt or a steer from here, so the session is
    /// rho-ui's.
    Prompted,
    /// The saved session changed (a prompt was accepted or a turn ended);
    /// reload the session list.
    Saved,
}

/// Whether the session is usable yet.
pub(crate) enum Loading {
    /// A new session, or a saved one the daemon has attached.
    Ready,
    /// The attach to a saved session has not answered yet.
    Attaching,
    /// The attach failed.
    Failed(String),
}

/// A message held until the turn that could not take it ends.
struct Queued {
    text: String,
    /// The turn that had ended last when it was sent. Once another turn
    /// ended and none runs, it goes.
    after: Option<TurnId>,
}

pub(crate) struct SessionView {
    daemon: Daemon,
    session: Session,
    cwd: PathBuf,
    /// The workspace and branch, for the header.
    workspace: WorkspaceRef,
    loading: Loading,
    transcript: Transcript,
    composer: Entity<TextareaState>,
    settings: Entity<SessionSettings>,
    approval_focus: FocusHandle,
    /// A prompt is on its way to the daemon.
    sending: bool,
    /// A message the session's turn could not take; see [`Self::hold`].
    queued: Option<Queued>,
    /// Background work the user asked to stop that still runs.
    stopping: Vec<BackgroundId>,
    /// The first prompt was accepted, so the session is saved.
    prompted: bool,
    /// When the running turn started.
    busy_since: Option<Instant>,
    /// When the turn under way started, for a session attached mid-turn:
    /// the daemon says how long it had run.
    turn_began: Option<Instant>,
    /// When the oldest open request arrived.
    waiting_since: Option<Instant>,
    /// Title and activity as last reported in [`SessionEvent::LiveChanged`].
    reported: (String, Activity),
    /// A request failed outside a turn, such as a settings change.
    error: Option<String>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SessionEvent> for SessionView {}

impl SessionView {
    /// A saved session starts as [`Loading::Attaching`] until
    /// [`Self::attached`].
    pub(crate) fn new(
        daemon: Daemon,
        session: Session,
        cwd: PathBuf,
        workspace: WorkspaceRef,
        loading: Loading,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let transcript = Transcript::new(cx);
        let composer = composer::new_state(window, cx);
        let settings = cx.new(|cx| {
            SessionSettings::new(
                daemon.clone(),
                session.id().clone(),
                session.config(),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe_in(&settings, window, Self::on_settings_event),
            // The jump-to-latest button follows the scroll position.
            cx.observe(transcript.scroller(), |_, _, cx| cx.notify()),
            // The header shows the branch, which can change.
            cx.observe(workspace.table(), |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                },
            ),
        ];
        let mut view = Self {
            daemon,
            session,
            cwd,
            workspace,
            // A resumed session is already saved.
            prompted: matches!(loading, Loading::Attaching),
            loading,
            transcript,
            composer,
            settings,
            approval_focus: cx.focus_handle(),
            sending: false,
            queued: None,
            stopping: Vec::new(),
            busy_since: None,
            turn_began: None,
            waiting_since: None,
            reported: (String::new(), Activity::Idle),
            error: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        view.transcript.sync(view.session.entries(), cx);
        view.sync_settings(window, cx);
        view.reported = (view.title(), view.activity());
        view
    }

    pub(crate) fn id(&self) -> &SessionId {
        self.session.id()
    }

    /// Whether the attach failed, so the view shows only why.
    pub(crate) fn failed_to_attach(&self) -> bool {
        matches!(self.loading, Loading::Failed(_))
    }

    /// What the sidebar shows for this session.
    pub(crate) fn live(&self) -> LiveSession {
        LiveSession {
            id: self.session.id().clone(),
            cwd: self.cwd.clone(),
            title: self.name().map(SharedString::from),
            placeholder: self.placeholder(),
            activity: self.activity(),
        }
    }

    /// rho's title, or until it sends one (rho names a session after its
    /// first turn), the first prompt, as rho lists an untitled session.
    fn name(&self) -> Option<String> {
        self.session
            .title()
            .map(str::to_owned)
            .or_else(|| first_prompt(&self.session))
    }

    fn title(&self) -> String {
        self.name().unwrap_or_else(|| self.placeholder().to_owned())
    }

    /// The title until rho gives one: a saved session is untitled.
    fn placeholder(&self) -> &'static str {
        if self.prompted {
            UNTITLED
        } else {
            NEW_SESSION
        }
    }

    pub(crate) fn composer_focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }

    fn is_busy(&self) -> bool {
        self.sending || self.session.state().is_busy()
    }

    fn activity(&self) -> Activity {
        match (self.waiting_since, self.busy_since) {
            (Some(since), _) => Activity::Waiting(since),
            (None, Some(since)) => Activity::Working(since),
            (None, None) => Activity::Idle,
        }
    }

    pub(crate) fn apply(
        &mut self,
        update: SessionUpdate,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let asked = !self.session.inputs().is_empty();
        // The session names the entry it changed; only its row is remeasured.
        // A new entry gets its row in `after_change`.
        if let Some(ix) = self.session.apply(update) {
            self.transcript.changed(self.session.entries(), ix, cx);
        }
        // Move the keyboard to the default answer if the user is in this
        // session; never steal focus from another one, nor from a message
        // being typed, whose Enter would then approve instead of send.
        let asks = !self.session.inputs().is_empty();
        let typing = !self.composer.read(cx).value().is_empty();
        if asks && !asked && !typing && self.focus_handle.contains_focused(window, cx) {
            window.focus(&self.approval_focus, cx);
        }
        // A turn that ended before reading a steer dropped it.
        let dropped = self.session.take_dropped_steers();
        if !dropped.is_empty() {
            self.give_back(&dropped.join("\n"), window, cx);
        }
        self.after_change(window, cx);
    }

    /// Records the answer to the attach.
    pub(crate) fn attached(
        &mut self,
        result: Result<Attached, DaemonError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match result {
            Ok(attached) => {
                self.turn_began = attached
                    .turn_elapsed()
                    .and_then(|elapsed| Instant::now().checked_sub(elapsed));
                self.session.attached(attached);
                self.loading = Loading::Ready;
            }
            Err(error) => self.loading = Loading::Failed(error.to_string()),
        }
        self.after_change(window, cx);
    }

    /// Puts `text` back in the composer, above anything typed since.
    fn give_back(&self, text: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        let typed = self.composer.read(cx).value();
        let text = if typed.is_empty() {
            text.to_owned()
        } else {
            format!("{text}\n{typed}")
        };
        self.composer
            .update(cx, |state, cx| state.set_value(text, window, cx));
    }

    /// Brings the scroller, settings, timers and sidebar up to date with
    /// `session`.
    fn after_change(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.transcript.sync(self.session.entries(), cx);
        self.sync_settings(window, cx);
        // Until the attachment has caught up, the session's state is the
        // conversation's past: no turn starts or ends in it now.
        if self.session.is_replaying() {
            cx.notify();
            return;
        }
        let now = Instant::now();
        let busy = self.is_busy();
        match (busy, self.busy_since) {
            // Attached with a turn under way: it is the newest one.
            (true, None) if self.turn_began.is_some() => {
                self.busy_since = self.turn_began.take();
                self.transcript.resume_turn(cx);
            }
            // A turn began, from here or from another client.
            (true, None) => {
                self.busy_since = Some(now);
                self.transcript.expect_turn();
            }
            (false, Some(since)) => {
                self.busy_since = None;
                self.transcript
                    .end_turn(now - since, self.session.entries(), cx);
                cx.emit(SessionEvent::Saved);
            }
            _ => {}
        }
        self.waiting_since = match (self.session.inputs().is_empty(), self.waiting_since) {
            (true, _) => None,
            (false, since) => Some(since.unwrap_or(now)),
        };
        // A stop that took leaves the work no longer running.
        let work = self.session.background();
        self.stopping
            .retain(|id| work.iter().any(|work| work.id() == id && work.can_stop()));
        let activity = self.activity();
        let title = self.title();
        if self.reported.0 != title || self.reported.1 != activity {
            self.reported = (title, activity);
            cx.emit(SessionEvent::LiveChanged);
        }
        cx.notify();
        self.send_queued(window, cx);
    }

    /// Sends the composer's text: as a prompt, or while a turn runs, as a
    /// steer for it.
    fn send(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.sending || !matches!(self.loading, Loading::Ready) {
            return;
        }
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        if self.session.state().is_busy() {
            self.steer(text, window, cx);
        } else {
            self.submit(text, window, cx);
        }
    }

    /// Sends `text` as the next prompt. A failed send puts it back in the
    /// composer.
    pub(crate) fn submit(&mut self, text: String, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.sending = true;
        self.error = None;
        self.after_change(window, cx);

        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        let ended = self.session.ended_turn().cloned();
        cx.spawn_in(window, async move |this, cx| {
            let result = daemon.prompt(&id, &text).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match result {
                    Ok(turn) => {
                        this.session.turn_began(&turn);
                        cx.emit(SessionEvent::Prompted);
                        if !this.prompted {
                            this.prompted = true;
                            cx.emit(SessionEvent::Saved);
                        }
                    }
                    Err(DaemonError::Busy(_)) => this.hold(text, ended, window, cx),
                    Err(error) => {
                        // A turn another client began meanwhile is not
                        // what failed.
                        if this.session.state().is_busy() {
                            this.error = Some(format!("Couldn't send the message: {error}"));
                        } else {
                            this.session.fail(error.to_string());
                        }
                        // Give the prompt back so it isn't lost.
                        this.give_back(&text, window, cx);
                    }
                }
                this.after_change(window, cx);
            });
        })
        .detach();
    }

    /// Adds `text` to the running turn. It shows above the composer until
    /// the model reads it; a refused steer goes back in the composer.
    fn steer(&mut self, text: String, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.error = None;
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        let ended = self.session.ended_turn().cloned();
        cx.spawn_in(window, async move |this, cx| {
            let result = daemon.steer(&id, &text).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(steer) => {
                        this.session.steer_sent(steer, text);
                        // The turn may have ended before this answer came.
                        let dropped = this.session.take_dropped_steers();
                        if !dropped.is_empty() {
                            this.give_back(&dropped.join("\n"), window, cx);
                        }
                        // Continued from here, so the session is rho-ui's.
                        cx.emit(SessionEvent::Prompted);
                    }
                    // The turn is past its last model request, or ended.
                    Err(DaemonError::Busy(_)) => this.hold(text, ended, window, cx),
                    Err(error) => {
                        this.error = Some(format!("Couldn't steer the turn: {error}"));
                        this.give_back(&text, window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Holds `text`, which the session's turn could not take, and sends it
    /// as a prompt once that turn ends. `ended` is the turn that had ended
    /// last when `text` was sent: the refusing turn is a later one, which
    /// may already have ended, or whose events may not have arrived yet.
    fn hold(
        &mut self,
        text: String,
        ended: Option<TurnId>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.queued = Some(match self.queued.take() {
            Some(queued) => Queued {
                text: format!("{}\n{text}", queued.text),
                after: queued.after,
            },
            None => Queued { text, after: ended },
        });
        cx.notify();
        self.send_queued(window, cx);
    }

    /// Sends the held message once a turn ended after it was sent, and
    /// none runs.
    fn send_queued(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let due = self.queued.as_ref().is_some_and(|queued| {
            !self.is_busy() && self.session.ended_turn() != queued.after.as_ref()
        });
        if due {
            if let Some(queued) = self.queued.take() {
                self.submit(queued.text, window, cx);
            }
        }
    }

    /// Takes back the held message, into the composer.
    fn unqueue(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(queued) = self.queued.take() {
            self.give_back(&queued.text, window, cx);
        }
        cx.notify();
    }

    /// Asks the daemon to stop background work `work`. Its row says so
    /// until the work ends.
    fn stop_background(
        &mut self,
        work: BackgroundId,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.stopping.contains(&work) {
            return;
        }
        self.stopping.push(work.clone());
        cx.notify();
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = daemon.stop_background(&id, &work).await {
                let _ = this.update(cx, |this, cx| {
                    this.stopping.retain(|asked| *asked != work);
                    this.error = Some(format!("Couldn't stop it: {error}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Takes back a steer the model has not read, into the composer.
    fn unsteer(&mut self, steer: SteerId, window: &mut Window, cx: &mut Context<'_, Self>) {
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = daemon.unsteer(&id, &steer).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Unsteered::Withdrawn) => {
                        if let Some(text) = this.session.withdraw_steer(&steer) {
                            this.give_back(&text, window, cx);
                        }
                    }
                    // The model has it; it joins the conversation shortly.
                    Ok(Unsteered::Kept) => {}
                    Err(error) => {
                        this.error = Some(format!("Couldn't take the message back: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn stop(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.is_busy() {
            return;
        }
        // The turn withdraws its open requests as it ends.
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = daemon.cancel(&id).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("Couldn't stop the turn: {error}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Answers the oldest open request with its choice `choice`. The
    /// request leaves the view at once, so it can't be answered twice, and
    /// comes back if the daemon didn't take the answer.
    fn choose(&mut self, choice: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        let answer = self.session.inputs().first().and_then(|request| {
            let choice = request.choice(choice)?.clone();
            Some((request.id(), choice))
        });
        let Some((request, choice)) = answer else {
            return;
        };
        let Some(asked) = self.session.close_input(request) else {
            return;
        };
        if self.session.inputs().is_empty() {
            window.focus(&self.composer.focus_handle(cx), cx);
        } else {
            window.focus(&self.approval_focus, cx);
        }
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = daemon.answer(&id, request, &choice).await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.session.open_input(asked);
                    this.error = Some(format!("Couldn't send the answer: {error}"));
                    this.after_change(window, cx);
                });
            }
        })
        .detach();
        self.after_change(window, cx);
    }

    /// Shows the session's settings, changeable once the session is open.
    fn sync_settings(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let allowed = if matches!(self.loading, Loading::Ready) {
            Availability::Enabled
        } else {
            Availability::Disabled
        };
        self.settings.update(cx, |settings, cx| {
            settings.sync(self.session.config(), window, cx);
            settings.set_allowed(allowed, cx);
        });
    }

    fn on_settings_event(
        &mut self,
        _: &Entity<SessionSettings>,
        event: &SettingsEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            SettingsEvent::Changed(options) => {
                self.error = None;
                self.session.set_config(options.clone());
            }
            SettingsEvent::Failed(message) => self.error = Some(message.clone()),
        }
        self.after_change(window, cx);
    }

    fn render_transcript(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        if self.session.entries().is_empty() {
            let placeholder = Empty::new();
            return match &self.loading {
                Loading::Attaching => placeholder.header(
                    EmptyHeader::new()
                        .media(EmptyMedia::new().child(Spinner::new()))
                        .description(EmptyDescription::new().child("Loading session…")),
                ),
                Loading::Failed(message) => placeholder.content(
                    EmptyContent::new().max_w(rems(36.)).child(
                        Alert::error("attach-error", SharedString::from(message.clone()))
                            .title("Couldn't open this session"),
                    ),
                ),
                Loading::Ready => placeholder.header(
                    EmptyHeader::new()
                        .description(EmptyDescription::new().child("Send a message to start.")),
                ),
            }
            .into_any_element();
        }

        let view = cx.entity().downgrade();
        let act: ActionHandler = {
            let view = view.clone();
            Rc::new(move |action, _, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.transcript.act(action, this.session.entries(), cx);
                    cx.notify();
                });
            })
        };
        div()
            .flex_1()
            .min_h_0()
            .child(
                MessageScroller::new(
                    SharedString::from(format!("transcript-{}", self.session.id())),
                    self.transcript.scroller().clone(),
                    move |ix, _, cx| match view.upgrade() {
                        Some(view) => {
                            let this = view.read(cx);
                            this.transcript
                                .render_row(ix, this.session.entries(), &act, cx)
                        }
                        None => div().into_any_element(),
                    },
                )
                .with_list_style(StyleRefinement::default().px_4())
                // Kit pads every row but the last by 32px. The transcript
                // spaces its own rows, so a folded row can take no space.
                .with_row_style(StyleRefinement::default().pb_0())
                .with_bottom_fade(cx.theme().background)
                .size_full(),
            )
            .into_any_element()
    }

    /// Errors, approvals and the composer, under the transcript.
    fn render_footer(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let failure = match self.session.state() {
            RunState::Failed(message) => Some(message.clone()),
            _ => None,
        };
        let busy = self.is_busy();
        let view = cx.entity().downgrade();
        let stop_view = view.clone();
        let send_view = view.clone();
        v_flex()
            .flex_shrink_0()
            .w_full()
            .max_w(rems(48.))
            .mx_auto()
            .gap_2()
            .px_4()
            .pb_3()
            .when_some(failure, |this, message| {
                this.child(
                    Alert::error("turn-error", SharedString::from(message))
                        .title("The turn failed")
                        .small(),
                )
            })
            .when_some(self.error.clone(), |this, message| {
                this.child(
                    Alert::warning("session-error", SharedString::from(message))
                        .small()
                        .on_close(cx.listener(|this, _, _, cx| {
                            this.error = None;
                            cx.notify();
                        })),
                )
            })
            .when_some(self.session.inputs().first(), |this, request| {
                let view = view.clone();
                this.child(ApprovalCard::new(
                    request,
                    self.session.inputs().len(),
                    self.approval_focus.clone(),
                    move |choice, window, cx| {
                        let _ = view.update(cx, |this, cx| this.choose(choice, window, cx));
                    },
                ))
            })
            .children(self.render_background(cx))
            .children(self.render_steers(cx))
            .children(self.render_queued(cx))
            .child(
                Composer::new(
                    &self.composer,
                    if busy {
                        ComposerMode::Stop
                    } else {
                        ComposerMode::Send
                    },
                    move |window, cx| {
                        let _ = send_view.update(cx, |this, cx| this.send(window, cx));
                    },
                    move |window, cx| {
                        let _ = stop_view.update(cx, |this, cx| this.stop(window, cx));
                    },
                )
                .leading(SessionSettings::render_modes(&self.settings, cx))
                .trailing(SessionSettings::render_panel_button(&self.settings, cx))
                .meta(self.usage_label()),
            )
    }

    /// The steers the turn has taken that the model has not read yet,
    /// each with a button to take it back.
    fn render_steers(&self, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let steers = self.session.pending_steers().enumerate();
        steers
            .map(|(ix, (steer, text))| {
                let steer = steer.clone();
                h_flex()
                    .id(SharedString::from(format!("steer-{ix}")))
                    .test_support()
                    .aria_label(format!("Queued: {text}"))
                    .gap_2()
                    .px_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(Lucide::CornerDownRight).xsmall())
                    .child(div().flex_1().min_w_0().truncate().child(text.to_owned()))
                    .child(
                        Button::new(SharedString::from(format!("unsteer-{ix}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Take it back")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.unsteer(steer.clone(), window, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect()
    }

    fn render_background(&self, cx: &mut Context<'_, Self>) -> Option<BackgroundList> {
        let view = cx.entity().downgrade();
        BackgroundList::new(
            self.session.background(),
            &self.stopping,
            move |work, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.stop_background(work.clone(), window, cx);
                });
            },
        )
    }

    /// The message held for when the turn ends, with a button to take it
    /// back.
    fn render_queued(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let text = &self.queued.as_ref()?.text;
        let row = h_flex()
            .id("queued")
            .test_support()
            .aria_label(format!("Sends after this turn: {text}"))
            .gap_2()
            .px_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(Lucide::Clock).xsmall())
            .child(div().flex_1().min_w_0().truncate().child(text.clone()))
            .child(div().text_xs().child("After this turn"))
            .child(
                Button::new("unqueue")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip("Take it back")
                    .on_click(cx.listener(|this, _, window, cx| this.unqueue(window, cx))),
            );
        Some(row.into_any_element())
    }

    /// "Context 25% · $0.01", once the daemon reports usage.
    fn usage_label(&self) -> Option<String> {
        let usage = self.session.usage()?;
        let mut parts = Vec::new();
        if let Some(size) = usage.size().filter(|size| *size > 0) {
            let percent = usage.used().saturating_mul(100) / size;
            parts.push(format!("Context {percent}%"));
        }
        if let Some(cost) = usage.cost_usd() {
            parts.push(format!("${cost:.2}"));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

#[cfg(test)]
impl SessionView {
    /// The session state behind this view, for UI tests.
    pub(crate) fn session(&self) -> &Session {
        &self.session
    }

    /// What the composer holds, for UI tests.
    pub(crate) fn composer_text(&self, cx: &App) -> String {
        self.composer.read(cx).value().to_string()
    }

    /// The message held for when the turn ends, for UI tests.
    pub(crate) fn queued(&self) -> Option<&str> {
        self.queued.as_ref().map(|queued| queued.text.as_str())
    }

    /// The transcript, for UI tests.
    pub(crate) fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// How far down the transcript is scrolled, in pixels from the top of
    /// the first row on screen, for UI tests.
    pub(crate) fn scroll_top(&self, cx: &App) -> (usize, f32) {
        let top = self.transcript.scroller().read(cx).logical_scroll_top();
        (top.item_ix, f32::from(top.offset_in_item))
    }
}

impl Render for SessionView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .id("session-view")
            .key_context("SessionView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &StopTurn, window, cx| this.stop(window, cx)))
            .size_full()
            .child(header(self.title(), self.workspace.read(cx), cx))
            .child(self.render_transcript(cx))
            .child(self.render_footer(cx))
    }
}

/// A session's title, with its workspace and branch under it. Shared with the
/// new-session draft.
pub(crate) fn header(title: String, workspace: &Workspace, cx: &App) -> impl IntoElement {
    let details = h_flex()
        .gap_1()
        .min_w_0()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(div().truncate().child(workspace.name.clone()))
        .when_some(workspace.branch.clone(), |this, branch| {
            this.child(Icon::new(Lucide::GitBranch).xsmall().ml_2())
                .child(div().truncate().child(branch))
        });
    v_flex()
        .flex_shrink_0()
        .min_h(px(44.))
        .justify_center()
        .px_4()
        .py_1()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .id("session-title")
                .test_support()
                .aria_label(title.clone())
                .truncate()
                .font_medium()
                .child(title),
        )
        .child(details)
}

/// Longest first-prompt title, in characters: rho's `FALLBACK_TITLE_CHARS`,
/// so the header matches the title rho lists for an untitled session.
const PROMPT_TITLE_CHARS: usize = 80;

/// The first prompt's text on one line, cut to [`PROMPT_TITLE_CHARS`].
fn first_prompt(session: &Session) -> Option<String> {
    let text = session.entries().iter().find_map(|entry| match entry {
        Entry::User(message) => Some(message.text()),
        Entry::Agent(_) | Entry::Thought(_) | Entry::Tool(_) | Entry::Notice(_) | Entry::Wake => {
            None
        }
    })?;
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    if line.chars().count() <= PROMPT_TITLE_CHARS {
        return Some(line);
    }
    let mut cut: String = line.chars().take(PROMPT_TITLE_CHARS - 1).collect();
    cut.push('…');
    Some(cut)
}
