//! One session: a title bar naming its project and the sessions above it,
//! the transcript, what a turn asks (approvals and questions), and the
//! composer with the session settings.
//! Owns the session's [`Session`] state and every request the user makes in
//! it.
//!
//! A subagent's session opens from its row in the background work, nested
//! under the session that started it. Only the subagent writes to it, so
//! its view only watches: no composer, no approvals (the subagent's
//! questions go to its parent), and no stopping its work.
//!
//! While a turn runs, whoever started it, the composer steers it: Enter adds
//! the text to the turn's next model request, and it waits above the box
//! until the model reads it. A message the turn can't take, as one sent
//! just as the daemon began a turn of its own to deliver background work,
//! waits above the box too, and goes as a prompt once that turn ends. So
//! does a message with attachments, as a steer carries only text, and any
//! message after one waiting, which would otherwise overtake it.
//!
//! The session's running background work is listed above the box, with a
//! button to stop each piece; what ended is behind a button in the title
//! bar. The view records the work as it starts and ends, and a session
//! opened again after a restart lists what ended from that record (see
//! [`crate::background_log`]).

use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    alert::Alert,
    button::{Button, ButtonVariants as _},
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia},
    h_flex,
    input::InputEvent,
    message_scroller::MessageScroller,
    spinner::Spinner,
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription, Task, WeakEntity,
    Window,
};
use rho_ui_core::{
    surface::{SurfaceAction, SurfaceStore},
    Answer, Attached, Attachment, BackgroundId, BackgroundStatus, Daemon, DaemonError, Entry,
    InputId, JobActivity, Prompt, Run, RunState, Session, SessionId, SessionUpdate, SteerId,
    SurfaceActionError, TurnId, Unsteered,
};

use crate::{
    auths::Auths,
    background::{BackgroundHistory, BackgroundList, BackgroundSurface, OpenHandler},
    background_log::{BackgroundLog, Recorded},
    composer::{Composer, ComposerMode, ComposerState},
    input_card::{InputCard, InputCardEvent, Queue},
    machines::{Place, SessionKey},
    pictures::{PictureRead, Pictures},
    plan::PlanList,
    session_list::{Activity, LiveSession, NEW_SESSION, UNTITLED},
    session_settings::{Availability, SessionSettings, SettingsEvent},
    sign_in::SignInSlot,
    surface_view::{Standing, SurfaceEvent, SurfacePanel},
    title_bar::TitleBar,
    transcript::{ActionHandler, RowContent, Transcript},
    usage::UsageRing,
    work::Disclosure,
    workspaces::WorkspaceRef,
    StopTurn,
};

/// How often a running turn's header redraws: it counts whole seconds.
const TICK: Duration = Duration::from_secs(1);

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
    /// Open the session of a subagent this one started, nested under it.
    OpenSubagent {
        session: SessionId,
        /// The subagent's task, its title until its session says.
        title: SharedString,
    },
    /// Show a session above this one, from the title bar.
    Show(WeakEntity<SessionView>),
    /// Start a new session in this one's project, from the title bar.
    NewSession,
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

pub(crate) struct SessionView {
    daemon: Daemon,
    session: Session,
    /// The session's directory, on its machine.
    cwd: Place,
    /// The workspace and branch, for the header.
    workspace: WorkspaceRef,
    /// For a subagent's session, the session that started it: the view
    /// then only watches.
    parent: Option<WeakEntity<SessionView>>,
    /// The daemon stopped sending an observed session's events, as when a
    /// subagent's run ends: what it shows is all there is until it is
    /// observed again.
    unloaded: bool,
    loading: Loading,
    transcript: Transcript,
    composer: Entity<ComposerState>,
    settings: Entity<SessionSettings>,
    /// The sign-in its model's auth profile needs, while it is signed out;
    /// see [`Self::with_sign_in`].
    sign_in: Option<SignInSlot>,
    /// The oldest open request, while there is one, and its events.
    card: Option<(Entity<InputCard>, Subscription)>,
    /// A prompt is on its way to the daemon.
    sending: bool,
    /// Background work the user asked to stop that still runs.
    stopping: Vec<BackgroundId>,
    /// The subagent and workflow runs the session started, as the daemon's
    /// job list last said; see [`Self::set_runs`].
    runs: Vec<Run>,
    /// What the background log holds of the session's work.
    recorded: Recorded,
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
    /// The surfaces the session's `ui` calls showed: its A2UI processor.
    /// It goes with the view, which a session that fell behind replaces.
    surfaces: SurfaceStore,
    /// How many of the session's entries [`Self::sync_surfaces`] has
    /// looked at for surfaces.
    surfaces_seen: usize,
    /// The surfaces' events.
    surface_subscriptions: Vec<Subscription>,
    /// Whether the agent's todo list above the composer is open.
    plan: Disclosure,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
    /// For a subagent's session, redraws the title bar as the sessions
    /// above it are renamed.
    _above: Vec<Subscription>,
    /// Redraws "Working for" while a turn runs.
    _tick: Task<()>,
}

impl EventEmitter<SessionEvent> for SessionView {}

impl SessionView {
    /// A saved session starts as [`Loading::Attaching`] until
    /// [`Self::attached`].
    pub(crate) fn new(
        daemon: Daemon,
        mut session: Session,
        workspace: WorkspaceRef,
        remote: Option<SharedString>,
        loading: Loading,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let cwd = workspace.read(cx).place().clone();
        let pictures = cx.new(|_| Pictures::new(cwd.clone(), workspace.table().clone()));
        let transcript = Transcript::new(pictures, cx);
        // A session about to attach may have run background work in an
        // earlier run of the app, which the daemon may no longer list.
        let recorded = if matches!(loading, Loading::Attaching) {
            let key = SessionKey::new(cwd.machine.clone(), session.id().clone());
            cx.global::<BackgroundLog>().remember(&key, &mut session)
        } else {
            Recorded::default()
        };
        let composer = cx.new(|cx| ComposerState::new(remote, window, cx));
        let input = composer.read(cx).input().clone();
        let settings = cx.new(|cx| {
            SessionSettings::new(
                daemon.clone(),
                session.id().clone(),
                session.config(),
                window,
                cx,
            )
        });
        let subscriptions =
            vec![
                cx.subscribe_in(&settings, window, Self::on_settings_event),
                // The jump-to-latest button follows the scroll position.
                cx.observe(transcript.scroller(), |_, _, cx| cx.notify()),
                // A picture a message shows was read.
                cx.subscribe(transcript.pictures(), |this, _, read: &PictureRead, cx| {
                    this.transcript.picture_read(read, cx);
                    cx.notify();
                }),
                // The title bar shows the project, which can change.
                cx.observe(workspace.table(), |_, _, cx| cx.notify()),
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
                        InputEvent::Change => cx.notify(),
                        _ => {}
                    }
                }),
                // Attachments added or removed.
                cx.observe(&composer, |_, _, cx| cx.notify()),
            ];
        let mut view = Self {
            daemon,
            session,
            cwd,
            workspace,
            parent: None,
            unloaded: false,
            // A resumed session is already saved.
            prompted: matches!(loading, Loading::Attaching),
            loading,
            transcript,
            composer,
            settings,
            sign_in: None,
            card: None,
            sending: false,
            stopping: Vec::new(),
            runs: Vec::new(),
            recorded,
            busy_since: None,
            turn_began: None,
            waiting_since: None,
            reported: (String::new(), Activity::Idle),
            error: None,
            surfaces: SurfaceStore::default(),
            surfaces_seen: 0,
            surface_subscriptions: Vec::new(),
            plan: Disclosure::default(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
            _above: Vec::new(),
            _tick: cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(TICK).await;
                let tick = this.update(cx, |this, cx| {
                    if this.busy_since.is_some() {
                        cx.notify();
                    }
                });
                if tick.is_err() {
                    break;
                }
            }),
        };
        view.transcript.sync(view.session.entries(), cx);
        view.sync_settings(window, cx);
        view.reported = (view.title(), view.activity());
        view
    }

    /// The session of a subagent that `parent` started, whose observe has
    /// not answered yet. It works where its parent does.
    pub(crate) fn subagent(
        daemon: Daemon,
        session: Session,
        parent: &Entity<SessionView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let (workspace, remote) = {
            let parent = parent.read(cx);
            (parent.workspace.clone(), parent.composer.read(cx).remote())
        };
        // The title bar names every session above this one.
        let mut renamed = Vec::new();
        let mut above = Some(parent.clone());
        while let Some(session) = above {
            renamed.push(cx.subscribe(&session, |_, _, event: &SessionEvent, cx| {
                if matches!(event, SessionEvent::LiveChanged) {
                    cx.notify();
                }
            }));
            above = session.read(cx).parent();
        }
        let view = Self::new(
            daemon,
            session,
            workspace,
            remote,
            Loading::Attaching,
            window,
            cx,
        );
        Self {
            parent: Some(parent.downgrade()),
            _above: renamed,
            ..view
        }
    }

    /// Offers the sign-in the session's model needs, above the composer,
    /// while its auth profile is signed out on its machine, as `auths`
    /// says. A subagent's session has no composer, so it gets none.
    pub(crate) fn with_sign_in(
        mut self,
        auths: Entity<Auths>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let machine = self.cwd.machine.clone();
        self.sign_in = Some(SignInSlot::new(
            auths,
            machine,
            Self::model_auth,
            window,
            cx,
        ));
        self
    }

    /// The auth profile the session's model runs on, once known.
    fn model_auth(&self) -> Option<&str> {
        self.session.auth()
    }

    /// The session, and the machine it is on.
    pub(crate) fn key(&self) -> SessionKey {
        SessionKey::new(self.cwd.machine.clone(), self.session.id().clone())
    }

    /// The session that started this one, for a subagent's session whose
    /// parent is still open.
    pub(crate) fn parent(&self) -> Option<Entity<SessionView>> {
        self.parent.as_ref().and_then(WeakEntity::upgrade)
    }

    /// Whether this is a subagent's session, which the view only watches.
    pub(crate) fn is_subagent(&self) -> bool {
        self.parent.is_some()
    }

    /// The session at the top of this one's nesting: itself, unless it is
    /// a subagent's.
    pub(crate) fn root_id(&self, cx: &App) -> SessionKey {
        match self.parent() {
            Some(parent) => parent.read(cx).root_id(cx),
            None => self.key(),
        }
    }

    /// Whether `ancestor` started this session, or one above it.
    pub(crate) fn descends_from(&self, ancestor: &Entity<SessionView>, cx: &App) -> bool {
        self.parent().is_some_and(|parent| {
            parent == *ancestor || parent.read(cx).descends_from(ancestor, cx)
        })
    }

    /// The daemon stopped sending the session's events; see
    /// [`Self::is_unloaded`].
    pub(crate) fn unload(&mut self) {
        self.unloaded = true;
    }

    /// Whether the daemon stopped sending this observed session's events,
    /// so what it shows may be out of date: observe it again to read it
    /// afresh.
    pub(crate) fn is_unloaded(&self) -> bool {
        self.unloaded
    }

    /// Whether the attach failed, so the view shows only why.
    pub(crate) fn failed_to_attach(&self) -> bool {
        matches!(self.loading, Loading::Failed(_))
    }

    /// What the sidebar shows for this session.
    pub(crate) fn live(&self) -> LiveSession {
        LiveSession {
            id: self.key(),
            cwd: self.cwd.path.clone(),
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

    /// Where the keyboard goes when the session is shown: its composer, or
    /// for a subagent's, which has none, the view.
    pub(crate) fn focus_target(&self, cx: &App) -> FocusHandle {
        if self.is_subagent() {
            self.focus_handle.clone()
        } else {
            self.composer.focus_handle(cx)
        }
    }

    fn is_busy(&self) -> bool {
        self.sending || self.session.state().is_busy()
    }

    fn activity(&self) -> Activity {
        let running = || {
            self.session
                .background()
                .iter()
                .any(|work| work.status() == BackgroundStatus::Running)
        };
        match (self.waiting_since, self.busy_since) {
            (Some(since), _) => Activity::Waiting(since),
            (None, Some(since)) => Activity::Working(since),
            (None, None) if running() => Activity::Background,
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
        // A turn that ended before reading a steer dropped it.
        let dropped = self.session.take_dropped_steers();
        if !dropped.is_empty() {
            self.give_back(dropped.join("\n").into(), window, cx);
        }
        self.after_change(window, cx);
        if !asked {
            self.focus_new_card(window, cx);
        }
    }

    /// Records the answer to the attach.
    pub(crate) fn attached(
        &mut self,
        result: Result<Attached, DaemonError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let asked = !self.session.inputs().is_empty();
        match result {
            Ok(attached) => {
                // A turn this client began meanwhile is already timed.
                self.turn_began = attached
                    .turn_elapsed()
                    .filter(|_| self.busy_since.is_none())
                    .and_then(|elapsed| Instant::now().checked_sub(elapsed));
                self.session.attached(attached);
                self.loading = Loading::Ready;
            }
            Err(error) => self.loading = Loading::Failed(error.to_string()),
        }
        self.after_change(window, cx);
        // A question asked before the attach answered arrives with it.
        if !asked {
            self.focus_new_card(window, cx);
        }
    }

    /// Moves the keyboard into a card that just opened if the user is in
    /// this session; never steals focus from another one, nor from a
    /// message being typed, whose Enter would then answer instead of send.
    fn focus_new_card(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let typing = !self.composer.read(cx).is_empty(cx);
        // `contains_focused` reads the last painted frame, which a session
        // opened this frame isn't in yet, so its focused composer counts too.
        let here = self.focus_handle.contains_focused(window, cx)
            || self.focus_target(cx).is_focused(window);
        if !typing && here {
            self.focus_card(window, cx);
        }
    }

    /// Puts `prompt` back in the composer, before anything typed or pasted
    /// since.
    fn give_back(&self, prompt: Prompt, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.composer
            .update(cx, |composer, cx| composer.give_back(prompt, window, cx));
    }

    /// Brings the scroller, settings, timers and sidebar up to date with
    /// `session`.
    fn after_change(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(from) = self.session.take_removed() {
            self.transcript.truncate(from, cx);
            self.surfaces_seen = self.surfaces_seen.min(from);
        }
        self.transcript.sync(self.session.entries(), cx);
        self.sync_surfaces(window, cx);
        self.transcript.set_delay(self.session.delay());
        self.sync_settings(window, cx);
        self.sync_card(window, cx);
        // Until the attachment has caught up, the session's state is the
        // conversation's past: no turn starts or ends in it now.
        if self.session.is_replaying() {
            cx.notify();
            return;
        }
        let now = Instant::now();
        let busy = self.is_busy();
        match (busy, self.busy_since, self.turn_began) {
            // Attached with a turn under way: it is the newest one.
            (true, None, Some(since)) => {
                self.turn_began = None;
                self.busy_since = Some(since);
                self.transcript.resume_turn(since, cx);
            }
            // A turn began, from here or from another client.
            (true, None, None) => {
                self.busy_since = Some(now);
                self.transcript.expect_turn(now);
            }
            (false, Some(_), _) => {
                self.busy_since = None;
                self.transcript.end_turn(self.session.entries(), cx);
                // A sign-out elsewhere fails the next turn: read whether
                // the model's profile is still signed in.
                if matches!(self.session.state(), RunState::Failed(_)) {
                    if let Some(slot) = &self.sign_in {
                        slot.refresh(cx);
                    }
                }
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
        let key = self.key();
        cx.global::<BackgroundLog>()
            .record(&key, &self.session, &mut self.recorded);
        let activity = self.activity();
        let title = self.title();
        if self.reported.0 != title || self.reported.1 != activity {
            self.reported = (title, activity);
            cx.emit(SessionEvent::LiveChanged);
        }
        cx.notify();
        self.send_held(window, cx);
    }

    /// Sends the composer's message: as a prompt, or while a turn runs, as
    /// a steer for it. A message with attachments can't steer, so it waits
    /// for the turn to end, and so does any message after one waiting.
    fn send(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.sending || !matches!(self.loading, Loading::Ready) {
            return;
        }
        if self.composer.read(cx).prompt(cx).is_empty() {
            return;
        }
        let prompt = self
            .composer
            .update(cx, |composer, cx| composer.take(window, cx));
        if !self.session.state().is_busy() {
            self.submit(prompt, window, cx);
        } else if prompt.attachments().is_empty() && self.session.held().is_none() {
            self.steer(prompt.into_parts().0, window, cx);
        } else {
            // After a held message, a steer would reach the model first.
            let ended = self.session.ended_turn().cloned();
            self.hold(prompt, ended, window, cx);
        }
    }

    /// Sends `prompt` as the next prompt. A failed send puts it back in the
    /// composer.
    pub(crate) fn submit(
        &mut self,
        prompt: Prompt,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.sending = true;
        self.error = None;
        // A new session's attach may still be on its way, as when the first
        // message goes at once; `after_change` then takes the state for
        // history. The turn this prompt starts is the next one all the
        // same: its echo can't come before it is sent.
        if self.session.is_replaying() && self.busy_since.is_none() {
            let now = Instant::now();
            self.busy_since = Some(now);
            self.transcript.expect_turn(now);
        }
        self.after_change(window, cx);

        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        let ended = self.session.ended_turn().cloned();
        cx.spawn_in(window, async move |this, cx| {
            let result = daemon.prompt(&id, &prompt).await;
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
                    Err(DaemonError::Busy(error)) => {
                        this.refused_busy(prompt, ended, error, window, cx);
                    }
                    Err(error) => {
                        // A turn another client began meanwhile is not
                        // what failed.
                        if this.session.state().is_busy() {
                            this.error = Some(format!("Couldn't send the message: {error}"));
                        } else {
                            this.session.fail(error.to_string());
                        }
                        // Give the prompt back so it isn't lost.
                        this.give_back(prompt, window, cx);
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
                            this.give_back(dropped.join("\n").into(), window, cx);
                        }
                        // Continued from here, so the session is rho-ui's.
                        cx.emit(SessionEvent::Prompted);
                    }
                    // The turn is past its last model request, or ended.
                    Err(DaemonError::Busy(error)) => {
                        this.refused_busy(text.into(), ended, error, window, cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("Couldn't steer the turn: {error}"));
                        this.give_back(text.into(), window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `prompt` was refused as busy, the daemon saying `why`. A turn this
    /// view knows of, under way or ended since `prompt` was sent, explains
    /// it: `prompt` waits for that turn. Otherwise either the turn's events
    /// have yet to arrive, or the session is busy with no turn at all, as
    /// while it is deleted; the daemon's job list tells which.
    fn refused_busy(
        &mut self,
        prompt: Prompt,
        sent_after: Option<TurnId>,
        why: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.session.has_turn_since(sent_after.as_ref()) {
            self.hold(prompt, sent_after, window, cx);
            return;
        }
        let daemon = self.daemon.clone();
        cx.spawn_in(window, async move |this, cx| {
            let jobs = daemon.jobs().await;
            let _ = this.update_in(cx, |this, window, cx| {
                let id = this.session.id();
                // Only a turn under way or waiting for a person refuses one.
                let listed = jobs.is_ok_and(|jobs| {
                    jobs.active
                        .iter()
                        .any(|job| job.session() == id && job.activity() != JobActivity::Background)
                });
                // The turn's events may have come in meanwhile.
                if listed || this.session.has_turn_since(sent_after.as_ref()) {
                    this.hold(prompt, sent_after, window, cx);
                } else {
                    this.error = Some(format!("Couldn't send the message: {why}"));
                    this.give_back(prompt, window, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Holds `prompt`, which the session's turn could not take, to send as
    /// a prompt once that turn ends; see [`Session::hold`].
    fn hold(
        &mut self,
        prompt: Prompt,
        sent_after: Option<TurnId>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.session.hold(prompt, sent_after);
        cx.notify();
        self.send_held(window, cx);
    }

    /// Sends the held message as a prompt once it is due.
    fn send_held(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.sending {
            return;
        }
        if let Some(prompt) = self.session.take_due() {
            self.submit(prompt, window, cx);
        }
    }

    /// Takes back the held message, into the composer.
    fn take_back_held(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(prompt) = self.session.take_held() {
            self.give_back(prompt, window, cx);
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
                    // Work that ended meanwhile needs no stop; the list
                    // already says how it ended.
                    let work_list = this.session.background();
                    if work_list
                        .iter()
                        .any(|listed| *listed.id() == work && listed.can_stop())
                    {
                        this.error = Some(format!("Couldn't stop it: {error}"));
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Takes the session's runs from a listing of the daemon's jobs, which
    /// says what its background list lacks: where a queued run waits, why
    /// a failed one failed, and a workflow's agent nodes.
    pub(crate) fn set_runs(&mut self, runs: Vec<Run>, cx: &mut Context<'_, Self>) {
        if self.runs != runs {
            self.runs = runs;
            cx.notify();
        }
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
                            this.give_back(text.into(), window, cx);
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
        // Only its parent stops a subagent's run.
        if !self.is_busy() || self.is_subagent() {
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

    /// Shows the oldest open request in the card, keeping what the user
    /// filled in while it is the same request. A subagent's session shows
    /// none: its questions go to its parent.
    fn sync_card(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let inputs = if self.is_subagent() {
            &[]
        } else {
            self.session.inputs()
        };
        let queue = Queue::of(inputs);
        let shown = self.card.as_ref().map(|(card, _)| card.clone());
        match (inputs.first(), &shown) {
            (Some(request), Some(card)) if card.read(cx).id() == request.id() => {
                card.update(cx, |card, cx| card.set_request(request, queue, cx));
                return;
            }
            (None, None) => return,
            _ => {}
        }
        // Another request takes the card: the keyboard follows it, if it
        // was in the card.
        let focused = shown.is_some_and(|card| card.read(cx).contains_focus(window, cx));
        self.card = inputs.first().cloned().map(|request| {
            let card = cx.new(|cx| InputCard::new(request, queue, window, cx));
            let subscription = cx.subscribe_in(&card, window, Self::on_card_event);
            (card, subscription)
        });
        if focused && self.card.is_some() {
            self.focus_card(window, cx);
        } else if focused {
            window.focus(&self.composer.focus_handle(cx), cx);
        }
    }

    /// Moves the keyboard into the card, if one shows.
    fn focus_card(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some((card, _)) = &self.card {
            card.update(cx, |card, cx| card.focus(window, cx));
        }
    }

    fn on_card_event(
        &mut self,
        card: &Entity<InputCard>,
        event: &InputCardEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let card = card.downgrade();
        match event {
            InputCardEvent::Answer(request, answer) => {
                // The keyboard goes back to the composer now, not when the
                // daemon answers, which may be after the user moved on.
                // With more requests waiting, it stays to follow the next.
                if self.session.inputs().len() <= 1 {
                    window.focus(&self.composer.focus_handle(cx), cx);
                }
                self.answer(*request, answer.clone(), card, window, cx);
            }
            InputCardEvent::Hold(request) => self.hold_input(*request, card, window, cx),
        }
    }

    /// Sends `answer` to open request `request`, whose card takes no other
    /// answer meanwhile. Once the daemon takes it, the request closes; if
    /// not, the card takes an answer again, filled in as it was.
    fn answer(
        &mut self,
        request: InputId,
        answer: Answer,
        card: WeakEntity<InputCard>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            let sent = daemon.answer(&id, request, &answer).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = sent {
                    let _ = card.update(cx, InputCard::answer_failed);
                    this.error = Some(format!("Couldn't send the answer: {error}"));
                    cx.notify();
                    return;
                }
                this.session.close_input(request);
                this.after_change(window, cx);
            });
        })
        .detach();
    }

    /// Stops request `request`'s timeout, as the user started on it. The
    /// daemon's word that it is held drops the countdown.
    fn hold_input(
        &mut self,
        request: InputId,
        card: WeakEntity<InputCard>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = daemon.hold(&id, request).await {
                let _ = card.update(cx, |card, _| card.hold_failed());
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("Couldn't stop the question's timeout: {error}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Brings the surfaces up to date with the `ui` calls that arrived
    /// since the last call: a call's messages may create, change, move or
    /// delete any surface, so every panel takes its surface as it stands.
    fn sync_surfaces(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let mut arrived = Vec::new();
        for entry in self.session.entries().iter().skip(self.surfaces_seen) {
            if let Entry::Surface(shown) = entry {
                if self.surfaces.show(shown) {
                    arrived.push(shown.surface().to_owned());
                }
            }
        }
        self.surfaces_seen = self.session.entries().len();
        if arrived.is_empty() {
            return;
        }
        for surface in arrived {
            if self.transcript.surface(&surface).is_none() {
                let panel = cx.new(|_| SurfacePanel::new(surface.clone()));
                self.surface_subscriptions.push(cx.subscribe_in(
                    &panel,
                    window,
                    Self::on_surface_event,
                ));
                // A panel that redraws may have changed height: a check's
                // message came or went, or another tab shows.
                let changed = surface.clone();
                self.surface_subscriptions
                    .push(cx.observe(&panel, move |this, _, cx| {
                        this.transcript
                            .surface_changed(&changed, this.session.entries(), cx);
                    }));
                self.transcript.set_surface(surface, panel);
            }
        }
        let shown: Vec<String> = self
            .session
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                Entry::Surface(shown) => Some(shown.surface().to_owned()),
                _ => None,
            })
            .collect();
        for surface in shown {
            self.show_surface(&surface, window, cx);
        }
    }

    /// Gives surface `surface`'s panel the surface as the store has it.
    fn show_surface(&mut self, surface: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(panel) = self.transcript.surface(surface).cloned() else {
            return;
        };
        let store = &self.surfaces;
        let (tree, drawn_under, standing) = (
            store.tree(surface),
            store.drawn_under(surface).map(str::to_owned),
            if store.is_stale(surface) {
                Standing::Stale
            } else {
                Standing::Current
            },
        );
        panel.update(cx, |panel, cx| {
            panel.show(tree, drawn_under.as_deref(), standing, window, cx);
        });
    }

    fn on_surface_event(
        &mut self,
        panel: &Entity<SurfacePanel>,
        event: &SurfaceEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let surface = panel.read(cx).surface().to_owned();
        match event {
            SurfaceEvent::Write { binding, value } => {
                self.surfaces.write(&surface, binding, value.clone());
            }
            SurfaceEvent::Act { target, action } => {
                let acted = self.surfaces.act(&surface, target, action);
                for url in &acted.open {
                    cx.open_url(url);
                }
                if let Some(error) = acted.error {
                    self.error = Some(format!("Couldn't run the form's action: {error}"));
                }
                if let Some(action) = acted.send {
                    // The keyboard goes back to the composer, where the
                    // reply is followed, as after an answered question.
                    window.focus(&self.composer.focus_handle(cx), cx);
                    self.send_surface_action(action, panel.downgrade(), window, cx);
                }
            }
        }
        // The edit, or what the action's function wrote, may change what
        // any control of the surface shows and which checks pass.
        self.show_surface(&surface, window, cx);
        cx.notify();
    }

    /// Sends `action` to the session. A session that no longer has the
    /// surface refuses it: the surface then says it is out of date, and
    /// takes no more.
    fn send_surface_action(
        &mut self,
        action: SurfaceAction,
        panel: WeakEntity<SurfacePanel>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let daemon = self.daemon.clone();
        let id = self.session.id().clone();
        let _ = panel.update(cx, SurfacePanel::sending);
        cx.spawn_in(window, async move |this, cx| {
            let sent = daemon.surface_action(&id, &action).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let _ = panel.update(cx, SurfacePanel::sent);
                match sent {
                    Ok(()) => {}
                    Err(SurfaceActionError::Stale(_)) => {
                        this.surfaces.mark_stale(action.surface());
                        this.show_surface(action.surface(), window, cx);
                    }
                    Err(SurfaceActionError::Failed(error)) => {
                        this.error = Some(format!("Couldn't send the form's action: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Shows the session's settings, changeable once the session is open,
    /// and offers its commands and skills in the composer.
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
        self.composer.update(cx, |composer, cx| {
            composer.set_commands(self.session.commands(), cx);
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
        let content: RowContent = {
            let view = view.clone();
            let act = Rc::clone(&act);
            Rc::new(move |ix, _, cx| match view.upgrade() {
                Some(view) => {
                    let this = view.read(cx);
                    this.transcript
                        .render_content(ix, this.session.entries(), &act, cx)
                }
                None => div().into_any_element(),
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
                            this.transcript.render_row(
                                ix,
                                this.session.entries(),
                                &act,
                                &content,
                                cx,
                            )
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

    /// Errors, approvals and the composer, under the transcript. A
    /// subagent's session shows only how its run failed and its own
    /// running work.
    fn render_footer(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let failure = match self.session.state() {
            RunState::Failed(message) => Some(message.clone()),
            _ => None,
        };
        let footer = v_flex()
            .flex_shrink_0()
            .w_full()
            .max_w(rems(48.))
            .mx_auto()
            .gap_2()
            .px_4()
            .pb_3()
            // Each alert is wrapped so UI tests can find it: Kit's Alert
            // does not register its ID with them.
            .when_some(failure, |this, message| {
                this.child(
                    div().id("turn-error").test_support().child(
                        Alert::error("turn-error-alert", SharedString::from(message))
                            .title("The turn failed")
                            .small(),
                    ),
                )
            });
        if self.is_subagent() {
            return footer
                .children(self.render_background(cx))
                .into_any_element();
        }
        let busy = self.is_busy();
        let send_view = cx.entity().downgrade();
        let stop_view = send_view.clone();
        footer
            .when_some(self.error.clone(), |this, message| {
                this.child(
                    div().id("session-error").test_support().child(
                        Alert::warning("session-error-alert", SharedString::from(message))
                            .small()
                            .on_close(cx.listener(|this, _, _, cx| {
                                this.error = None;
                                cx.notify();
                            })),
                    ),
                )
            })
            .children(
                self.sign_in
                    .as_ref()
                    .and_then(|slot| slot.card(self.session.auth(), cx)),
            )
            .when_some(self.card.as_ref(), |this, (card, _)| {
                this.child(card.clone())
            })
            .children(self.render_background(cx))
            .children(PlanList::new(self.session.plan(), self.plan, {
                let view = cx.entity().downgrade();
                move |_, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.plan.toggle();
                        cx.notify();
                    });
                }
            }))
            .children(self.render_steers(cx))
            .children(self.render_held(cx))
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
                .usage(self.session.usage().cloned().map(UsageRing::new)),
            )
            .into_any_element()
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

    /// The running background work; a subagent's session can't stop its
    /// own.
    fn render_background(&self, cx: &mut Context<'_, Self>) -> Option<BackgroundList> {
        let list = BackgroundList::new(
            self.session.background(),
            &self.runs,
            Self::open_handler(cx.entity().downgrade()),
        )?;
        if self.is_subagent() {
            return Some(list);
        }
        let view = cx.entity().downgrade();
        Some(list.stoppable(&self.stopping, move |work, window, cx| {
            let _ = view.update(cx, |this, cx| {
                this.stop_background(work.clone(), window, cx);
            });
        }))
    }

    /// The session's background work, running and ended, for the right
    /// panel's Background surface. A subagent's session can't stop its own.
    pub(crate) fn background_surface(view: &Entity<Self>, cx: &App) -> BackgroundSurface {
        let this = view.read(cx);
        let surface = BackgroundSurface::new(
            this.session.background(),
            this.session.ended_background(),
            &this.runs,
            Self::open_handler(view.downgrade()),
        );
        if this.is_subagent() {
            return surface;
        }
        let view = view.downgrade();
        surface.stoppable(&this.stopping, move |work, window, cx| {
            let _ = view.update(cx, |this, cx| {
                this.stop_background(work.clone(), window, cx);
            });
        })
    }

    /// Opens a subagent's session from its row in the background work.
    fn open_handler(view: WeakEntity<Self>) -> OpenHandler {
        Rc::new(move |session, title, _, cx| {
            let _ = view.update(cx, |_, cx| {
                cx.emit(SessionEvent::OpenSubagent {
                    session: session.clone(),
                    title: title.clone(),
                });
            });
        })
    }

    /// The project, which starts a new session in it, the sessions above
    /// this one, each a link to it, and this one's title, with its ended background work at the right.
    fn render_title_bar(&self, cx: &mut Context<'_, Self>) -> TitleBar {
        let mut above = Vec::new();
        let mut next = self.parent();
        while let Some(view) = next {
            next = view.read(cx).parent();
            above.push(view);
        }
        let mut bar = TitleBar::new(self.workspace.read(cx), self.title());
        let this = cx.entity().downgrade();
        bar = bar.on_project({
            let this = this.clone();
            move |_, cx| {
                let _ = this.update(cx, |_, cx| cx.emit(SessionEvent::NewSession));
            }
        });
        for view in above.into_iter().rev() {
            let (this, target) = (this.clone(), view.downgrade());
            bar = bar.parent(view.read(cx).title(), move |_, cx| {
                let _ = this.update(cx, |_, cx| cx.emit(SessionEvent::Show(target.clone())));
            });
        }
        bar.trailing(BackgroundHistory::new(
            self.session.ended_background(),
            &self.runs,
            Self::open_handler(cx.entity().downgrade()),
        ))
    }

    /// The message held for when the turn ends, with a button to take it
    /// back.
    fn render_held(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let text = held_summary(self.session.held()?);
        let row = h_flex()
            .id("held")
            .test_support()
            .aria_label(format!("Sends after this turn: {text}"))
            .gap_2()
            .px_3()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(Lucide::Clock).xsmall())
            .child(div().flex_1().min_w_0().truncate().child(text))
            .child(div().text_xs().child("After this turn"))
            .child(
                Button::new("take-back-held")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip("Take it back")
                    .on_click(cx.listener(|this, _, window, cx| this.take_back_held(window, cx))),
            );
        Some(row.into_any_element())
    }
}

#[cfg(test)]
impl SessionView {
    /// The composer's focus, for UI tests.
    pub(crate) fn composer_focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }

    /// The session state behind this view, for UI tests.
    pub(crate) fn session(&self) -> &Session {
        &self.session
    }

    /// What the composer holds, for UI tests.
    pub(crate) fn composer_text(&self, cx: &App) -> String {
        self.composer.read(cx).input().read(cx).value().to_string()
    }

    /// The message the composer holds, attachments and all, for UI tests.
    pub(crate) fn composer_prompt(&self, cx: &App) -> Prompt {
        self.composer.read(cx).prompt(cx)
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
            .child(self.render_title_bar(cx))
            .child(self.render_transcript(cx))
            .child(self.render_footer(cx))
    }
}

/// A held message on one line: its text, and what is attached to it, such
/// as "look · 1 image, 2 files".
fn held_summary(held: &Prompt) -> String {
    let attachments = held.attachments();
    let images = attachments
        .iter()
        .filter(|attachment| matches!(attachment, Attachment::Image(_)))
        .count();
    let count = |count: usize, one: &str| match count {
        0 => None,
        1 => Some(format!("1 {one}")),
        count => Some(format!("{count} {one}s")),
    };
    let attached = [
        count(images, "image"),
        count(attachments.len() - images, "file"),
    ];
    let attached: Vec<_> = attached.into_iter().flatten().collect();
    match (held.text().is_empty(), attached.is_empty()) {
        (_, true) => held.text().to_owned(),
        (true, false) => attached.join(", "),
        (false, false) => format!("{} · {}", held.text(), attached.join(", ")),
    }
}

/// Longest first-prompt title, in characters: rho's `FALLBACK_TITLE_CHARS`,
/// so the header matches the title rho lists for an untitled session.
const PROMPT_TITLE_CHARS: usize = 80;

/// The first prompt's text on one line, cut to [`PROMPT_TITLE_CHARS`]. It
/// runs on every daemon event, so it reads only as far into the prompt as
/// the title needs: one character more tells whether to cut.
fn first_prompt(session: &Session) -> Option<String> {
    let text = session.entries().iter().find_map(|entry| match entry {
        Entry::User(message) => Some(message.text()),
        Entry::Agent(_)
        | Entry::Thought(_)
        | Entry::Tool(_)
        | Entry::Notice(_)
        | Entry::Surface(_)
        | Entry::Wake => None,
    })?;
    let mut line: String = text
        .split_whitespace()
        .enumerate()
        .flat_map(|(n, word)| (n > 0).then_some(' ').into_iter().chain(word.chars()))
        .take(PROMPT_TITLE_CHARS + 1)
        .collect();
    if line.is_empty() {
        return None;
    }
    if line.chars().count() > PROMPT_TITLE_CHARS {
        let end = line
            .char_indices()
            .nth(PROMPT_TITLE_CHARS - 1)
            .map_or(line.len(), |(at, _)| at);
        line.truncate(end);
        line.push('…');
    }
    Some(line)
}
