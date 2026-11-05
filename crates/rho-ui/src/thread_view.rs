//! One thread: a header with its title and project, the transcript,
//! approvals, and the composer with the session settings. Owns the session's
//! [`Thread`] state and every request the user makes in it.

use std::{collections::VecDeque, path::PathBuf, rc::Rc, time::Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    alert::Alert,
    empty::{Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia},
    h_flex,
    input::{InputEvent, TextareaState},
    message_scroller::MessageScroller,
    spinner::Spinner,
    v_flex, ActiveTheme as _, Icon, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, rems, AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription, Window,
};
use rho_ui_core::{
    Agent, AgentError, Entry, PermissionRequest, RunState, SessionId, SessionSetup, Thread,
    ThreadUpdate,
};

use crate::{
    approval::ApprovalCard,
    composer::ComposerMode,
    composer::{self, Composer},
    projects::{Project, ProjectRef},
    session_settings::{Availability, SessionSettings, SettingsEvent},
    thread_list::{Activity, LiveThread, NEW_THREAD, UNTITLED},
    transcript::{ActionHandler, Transcript},
    StopTurn,
};

/// What the app needs to hear from a thread.
pub(crate) enum ThreadEvent {
    /// Title or activity changed; see [`ThreadView::live`].
    LiveChanged,
    /// The agent accepted a prompt from here, so the thread is rho-ui's.
    Prompted,
    /// The saved session changed (a prompt was accepted or a turn ended);
    /// reload the session list.
    Saved,
}

/// Whether the session is usable yet.
pub(crate) enum Loading {
    /// A new session, or a resumed one whose history has arrived.
    Ready,
    /// `resume_session` is replaying history.
    Resuming,
    /// `resume_session` failed.
    Failed(String),
}

pub(crate) struct ThreadView {
    agent: Agent,
    thread: Thread,
    cwd: PathBuf,
    /// The project and branch, for the header.
    project: ProjectRef,
    loading: Loading,
    transcript: Transcript,
    composer: Entity<TextareaState>,
    settings: Entity<SessionSettings>,
    /// Waiting approvals, oldest first. Dropping one answers "cancelled".
    approvals: VecDeque<PermissionRequest>,
    approval_focus: FocusHandle,
    /// A prompt is on its way to the agent.
    sending: bool,
    /// The first prompt was accepted, so the session is saved.
    prompted: bool,
    /// When the running turn started.
    busy_since: Option<Instant>,
    /// When the oldest waiting approval arrived.
    waiting_since: Option<Instant>,
    /// Title and activity as last reported in [`ThreadEvent::LiveChanged`].
    reported: (String, Activity),
    /// A request failed outside a turn, such as a settings change.
    error: Option<String>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ThreadEvent> for ThreadView {}

impl ThreadView {
    /// A resumed session starts as [`Loading::Resuming`] until
    /// [`Self::finish_loading`].
    pub(crate) fn new(
        agent: Agent,
        thread: Thread,
        cwd: PathBuf,
        project: ProjectRef,
        loading: Loading,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let transcript = Transcript::new(cx);
        let composer = composer::new_state(window, cx);
        let settings = cx.new(|cx| {
            SessionSettings::new(
                agent.clone(),
                thread.id().clone(),
                thread.config(),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe_in(&settings, window, Self::on_settings_event),
            // The jump-to-latest button follows the scroll position.
            cx.observe(transcript.scroller(), |_, _, cx| cx.notify()),
            // The header shows the branch, which can change.
            cx.observe(project.table(), |_, _, cx| cx.notify()),
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
            agent,
            thread,
            cwd,
            project,
            // A resumed session is already saved.
            prompted: matches!(loading, Loading::Resuming),
            loading,
            transcript,
            composer,
            settings,
            approvals: VecDeque::new(),
            approval_focus: cx.focus_handle(),
            sending: false,
            busy_since: None,
            waiting_since: None,
            reported: (String::new(), Activity::Idle),
            error: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        view.transcript.sync(view.thread.entries(), cx);
        view.sync_settings(window, cx);
        view.reported = (view.title(), view.activity());
        view
    }

    pub(crate) fn id(&self) -> &SessionId {
        self.thread.id()
    }

    /// What the sidebar shows for this thread.
    pub(crate) fn live(&self) -> LiveThread {
        LiveThread {
            id: self.thread.id().clone(),
            cwd: self.cwd.clone(),
            title: self.name().map(SharedString::from),
            placeholder: self.placeholder(),
            activity: self.activity(),
        }
    }

    /// The agent's title, or until it sends one (rho names a session after
    /// its first turn), the first prompt, as rho lists an untitled session.
    fn name(&self) -> Option<String> {
        self.thread
            .title()
            .map(str::to_owned)
            .or_else(|| first_prompt(&self.thread))
    }

    fn title(&self) -> String {
        self.name().unwrap_or_else(|| self.placeholder().to_owned())
    }

    /// The title until the agent gives one: a saved thread is untitled.
    fn placeholder(&self) -> &'static str {
        if self.prompted {
            UNTITLED
        } else {
            NEW_THREAD
        }
    }

    pub(crate) fn composer_focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }

    fn is_busy(&self) -> bool {
        self.sending || self.thread.state().is_busy()
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
        update: ThreadUpdate,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // The thread names the entry it changed; only its row is remeasured.
        // A new entry gets its row in `after_change`.
        if let Some(ix) = self.thread.apply(update) {
            self.transcript.changed(self.thread.entries(), ix, cx);
        }
        self.after_change(window, cx);
    }

    /// Records the result of `resume_session`.
    pub(crate) fn finish_loading(
        &mut self,
        result: Result<SessionSetup, AgentError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match result {
            Ok(setup) => {
                self.thread.set_setup(setup);
                self.loading = Loading::Ready;
            }
            Err(error) => self.loading = Loading::Failed(error.to_string()),
        }
        self.after_change(window, cx);
    }

    pub(crate) fn push_approval(
        &mut self,
        request: PermissionRequest,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let first = self.approvals.is_empty();
        self.approvals.push_back(request);
        // Move the keyboard to the default choice if the user is in this
        // thread; never steal focus from another one.
        if first && self.focus_handle.contains_focused(window, cx) {
            window.focus(&self.approval_focus, cx);
        }
        self.after_change(window, cx);
    }

    /// Brings the scroller, settings, timers and sidebar up to date with
    /// `thread`.
    fn after_change(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.transcript.sync(self.thread.entries(), cx);
        self.sync_settings(window, cx);
        let now = Instant::now();
        let busy = self.is_busy();
        match (busy, self.busy_since) {
            (true, None) => {
                self.busy_since = Some(now);
                self.transcript.begin_turn(cx);
            }
            (false, Some(since)) => {
                self.busy_since = None;
                self.transcript
                    .end_turn(now - since, self.thread.entries(), cx);
                cx.emit(ThreadEvent::Saved);
            }
            _ => {}
        }
        self.waiting_since = match (self.approvals.is_empty(), self.waiting_since) {
            (true, _) => None,
            (false, since) => Some(since.unwrap_or(now)),
        };
        let activity = self.activity();
        let title = self.title();
        if self.reported.0 != title || self.reported.1 != activity {
            self.reported = (title, activity);
            cx.emit(ThreadEvent::LiveChanged);
        }
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.is_busy() || !matches!(self.loading, Loading::Ready) {
            return;
        }
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.submit(text, window, cx);
    }

    /// Sends `text` as the next prompt. A failed send puts it back in the
    /// composer.
    pub(crate) fn submit(&mut self, text: String, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.sending = true;
        self.transcript.expect_turn();
        self.error = None;
        self.after_change(window, cx);

        let agent = self.agent.clone();
        let id = self.thread.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = agent.prompt(&id, &text).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match result {
                    Ok(()) => {
                        cx.emit(ThreadEvent::Prompted);
                        if !this.prompted {
                            this.prompted = true;
                            cx.emit(ThreadEvent::Saved);
                        }
                    }
                    Err(error) => {
                        this.thread.fail(error.to_string());
                        // Give the prompt back so it isn't lost.
                        if this.composer.read(cx).value().is_empty() {
                            this.composer
                                .update(cx, |state, cx| state.set_value(text, window, cx));
                        }
                    }
                }
                this.after_change(window, cx);
            });
        })
        .detach();
    }

    fn stop(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.is_busy() {
            return;
        }
        // Pending approvals answer "cancelled" as they drop.
        self.approvals.clear();
        if let Err(error) = self.agent.cancel(self.thread.id()) {
            self.error = Some(format!("Couldn't stop the turn: {error}"));
        }
        self.after_change(window, cx);
    }

    fn choose(&mut self, choice: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(request) = self.approvals.pop_front() {
            request.choose(choice);
        }
        if self.approvals.is_empty() {
            window.focus(&self.composer.focus_handle(cx), cx);
        } else {
            window.focus(&self.approval_focus, cx);
        }
        self.after_change(window, cx);
    }

    /// Shows the thread's settings, changeable once the session is open.
    fn sync_settings(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let allowed = if matches!(self.loading, Loading::Ready) {
            Availability::Enabled
        } else {
            Availability::Disabled
        };
        self.settings.update(cx, |settings, cx| {
            settings.sync(self.thread.config(), window, cx);
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
                self.thread.set_config(options.clone());
            }
            SettingsEvent::Failed(message) => self.error = Some(message.clone()),
        }
        self.after_change(window, cx);
    }

    fn render_transcript(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        if self.thread.entries().is_empty() {
            let placeholder = Empty::new();
            return match &self.loading {
                Loading::Resuming => placeholder.header(
                    EmptyHeader::new()
                        .media(EmptyMedia::new().child(Spinner::new()))
                        .description(EmptyDescription::new().child("Loading thread…")),
                ),
                Loading::Failed(message) => placeholder.content(
                    EmptyContent::new().max_w(rems(36.)).child(
                        Alert::error("resume-error", SharedString::from(message.clone()))
                            .title("Couldn't open this thread"),
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
                    this.transcript.act(action, this.thread.entries(), cx);
                    cx.notify();
                });
            })
        };
        div()
            .flex_1()
            .min_h_0()
            .child(
                MessageScroller::new(
                    SharedString::from(format!("transcript-{}", self.thread.id())),
                    self.transcript.scroller().clone(),
                    move |ix, _, cx| match view.upgrade() {
                        Some(view) => {
                            let this = view.read(cx);
                            this.transcript
                                .render_row(ix, this.thread.entries(), &act, cx)
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
        let failure = match self.thread.state() {
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
                    Alert::warning("thread-error", SharedString::from(message))
                        .small()
                        .on_close(cx.listener(|this, _, _, cx| {
                            this.error = None;
                            cx.notify();
                        })),
                )
            })
            .when_some(self.approvals.front(), |this, request| {
                let view = view.clone();
                this.child(ApprovalCard::new(
                    request,
                    self.approvals.len(),
                    self.approval_focus.clone(),
                    move |choice, window, cx| {
                        let _ = view.update(cx, |this, cx| this.choose(choice, window, cx));
                    },
                ))
            })
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

    /// "Context 25% · $0.01", once the agent reports usage.
    fn usage_label(&self) -> Option<String> {
        let usage = self.thread.usage()?;
        let mut parts = Vec::new();
        if usage.size() > 0 {
            let percent = usage.used().saturating_mul(100) / usage.size();
            parts.push(format!("Context {percent}%"));
        }
        if let Some((amount, currency)) = usage.cost() {
            parts.push(if currency == "USD" {
                format!("${amount:.2}")
            } else {
                format!("{amount:.2} {currency}")
            });
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

#[cfg(test)]
impl ThreadView {
    /// The session state behind this view, for UI tests.
    pub(crate) fn thread(&self) -> &Thread {
        &self.thread
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

impl Render for ThreadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        v_flex()
            .id("thread-view")
            .key_context("ThreadView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &StopTurn, window, cx| this.stop(window, cx)))
            .size_full()
            .child(header(self.title(), self.project.read(cx), cx))
            .child(self.render_transcript(cx))
            .child(self.render_footer(cx))
    }
}

/// A thread's title, with its project and branch under it. Shared with the
/// new-thread draft.
pub(crate) fn header(title: String, project: &Project, cx: &App) -> impl IntoElement {
    let details = h_flex()
        .gap_1()
        .min_w_0()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(div().truncate().child(project.name.clone()))
        .when_some(project.branch.clone(), |this, branch| {
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
                .id("thread-title")
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
fn first_prompt(thread: &Thread) -> Option<String> {
    let text = thread.entries().iter().find_map(|entry| match entry {
        Entry::User(message) => Some(message.text()),
        Entry::Agent(_) | Entry::Thought(_) | Entry::Tool(_) | Entry::Notice(_) => None,
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
