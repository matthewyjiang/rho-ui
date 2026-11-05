//! One thread: header with settings, transcript, approvals, composer and
//! status bar. Owns the session's [`Thread`] state and every request the user
//! makes in it.

use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui_kit::component::{
    alert::Alert,
    h_flex,
    input::{InputEvent, TextareaState},
    message_scroller::{MessageScroller, MessageScrollerState},
    spinner::Spinner,
    status_bar::StatusBar,
    v_flex, ActiveTheme as _, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StyleRefinement, Styled as _, Subscription, Window,
};
use rho_ui_core::{
    Agent, AgentError, ConfigValue, Entry, PermissionRequest, RunState, SessionSetup, Thread,
    ThreadUpdate, ToolContent, ToolStatus,
};

use crate::{
    approval::ApprovalCard,
    composer::ComposerMode,
    composer::{self, Composer},
    config_bar::{Availability, ConfigBar},
    sidebar::Attention,
    transcript::{self, Disclosure, RowContext, ToggleHandler},
    StopTurn,
};

/// What the app needs to hear from a thread.
pub(crate) enum ThreadEvent {
    /// Title or attention changed; redraw the sidebar.
    SidebarChanged,
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

/// What decides a transcript row's height, to tell which rows to remeasure.
#[derive(PartialEq)]
enum RowStamp {
    Message(usize),
    Tool {
        status: ToolStatus,
        title: usize,
        content: usize,
    },
    Notice,
}

impl RowStamp {
    fn of(entry: &Entry) -> Self {
        match entry {
            Entry::User(message) | Entry::Agent(message) | Entry::Thought(message) => {
                Self::Message(message.text().len())
            }
            Entry::Tool(tool) => Self::Tool {
                status: tool.status(),
                title: tool.title().len(),
                content: tool
                    .content()
                    .iter()
                    .map(|content| match content {
                        ToolContent::Text(text) => text.len(),
                        ToolContent::Diff { paths, patch } => {
                            paths.len() + patch.as_ref().map_or(0, String::len)
                        }
                    })
                    .sum(),
            },
            Entry::Notice(_) => Self::Notice,
        }
    }
}

pub(crate) struct ThreadView {
    agent: Agent,
    thread: Thread,
    cwd: PathBuf,
    loading: Loading,
    scroller: Entity<MessageScrollerState>,
    /// One stamp per entry, as the scroller last measured them.
    row_stamps: Vec<RowStamp>,
    /// Row keys of thoughts and tool cards the user opened.
    expanded: HashSet<String>,
    composer: Entity<TextareaState>,
    config_bar: ConfigBar,
    /// `set_config` calls in flight.
    config_pending: usize,
    /// Waiting approvals, oldest first. Dropping one answers "cancelled".
    approvals: VecDeque<PermissionRequest>,
    approval_focus: FocusHandle,
    /// A prompt is on its way to the agent.
    sending: bool,
    /// The first prompt was accepted, so the session is saved.
    prompted: bool,
    was_busy: bool,
    sidebar_snapshot: (Option<String>, Attention),
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
        loading: Loading,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let composer = composer::new_state(window, cx);
        let subscriptions = vec![
            // The jump-to-latest button follows the scroll position.
            cx.observe(&scroller, |_, _, cx| cx.notify()),
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
            // A resumed session is already saved.
            prompted: matches!(loading, Loading::Resuming),
            loading,
            scroller,
            row_stamps: Vec::new(),
            expanded: HashSet::new(),
            composer,
            config_bar: ConfigBar::default(),
            config_pending: 0,
            approvals: VecDeque::new(),
            approval_focus: cx.focus_handle(),
            sending: false,
            was_busy: false,
            sidebar_snapshot: (None, Attention::None),
            error: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        view.sync_rows(cx);
        view.config_bar.sync(view.thread.config(), window, cx);
        view.sidebar_snapshot = view.sidebar_state();
        view
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The agent's title for this session, once it has one.
    pub(crate) fn live_title(&self) -> Option<&str> {
        self.thread.title()
    }

    pub(crate) fn title(&self) -> &str {
        self.thread.title().unwrap_or("New thread")
    }

    pub(crate) fn composer_focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }

    fn is_busy(&self) -> bool {
        self.sending || self.thread.state().is_busy()
    }

    pub(crate) fn attention(&self) -> Attention {
        if !self.approvals.is_empty() {
            Attention::Approval(self.approvals.len())
        } else if self.is_busy() {
            Attention::Busy
        } else {
            Attention::None
        }
    }

    fn sidebar_state(&self) -> (Option<String>, Attention) {
        (self.thread.title().map(str::to_owned), self.attention())
    }

    pub(crate) fn apply(
        &mut self,
        update: ThreadUpdate,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.thread.apply(update);
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

    /// Brings the scroller, settings, and sidebar up to date with `thread`.
    fn after_change(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.sync_rows(cx);
        if !self.config_bar.is_synced(self.thread.config()) {
            self.config_bar.sync(self.thread.config(), window, cx);
        }
        let busy = self.is_busy();
        if self.was_busy && !busy {
            cx.emit(ThreadEvent::Saved);
        }
        self.was_busy = busy;
        let snapshot = self.sidebar_state();
        if snapshot != self.sidebar_snapshot {
            self.sidebar_snapshot = snapshot;
            cx.emit(ThreadEvent::SidebarChanged);
        }
        cx.notify();
    }

    /// Tells the scroller about new rows and rows whose height may have
    /// changed. Entries are only ever appended or updated in place.
    fn sync_rows(&mut self, cx: &mut Context<'_, Self>) {
        let stamps: Vec<RowStamp> = self.thread.entries().iter().map(RowStamp::of).collect();
        let old = std::mem::take(&mut self.row_stamps);
        self.scroller.update(cx, |scroller, cx| {
            if stamps.len() < old.len() {
                scroller.reset(stamps.len(), cx);
                return;
            }
            for (ix, (old, new)) in old.iter().zip(&stamps).enumerate() {
                if old != new {
                    scroller.remeasure_items(ix..ix + 1, cx);
                }
            }
            if stamps.len() > old.len() {
                scroller.append(stamps.len() - old.len(), cx);
            }
        });
        self.row_stamps = stamps;
    }

    fn toggle_expanded(&mut self, key: &str, cx: &mut Context<'_, Self>) {
        if !self.expanded.remove(key) {
            self.expanded.insert(key.to_owned());
        }
        let row = self
            .thread
            .entries()
            .iter()
            .enumerate()
            .position(|(ix, entry)| transcript::row_key(entry, ix) == key);
        if let Some(ix) = row {
            self.scroller.update(cx, |scroller, cx| {
                scroller.remeasure_items(ix..ix + 1, cx);
            });
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
        self.sending = true;
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

    /// Sends a settings change. The agent answers with every option, which
    /// replaces the controls; on failure they snap back.
    pub(crate) fn request_config(
        &mut self,
        config_id: String,
        value: ConfigValue,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.config_bar.show_requested(&config_id, &value);
        self.config_pending += 1;
        self.error = None;
        cx.notify();

        let agent = self.agent.clone();
        let id = self.thread.id().clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = agent.set_config(&id, &config_id, value).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.config_pending -= 1;
                match result {
                    Ok(options) => this.thread.set_config(options),
                    Err(error) => {
                        let name = this
                            .config_bar
                            .name_of(&config_id)
                            .unwrap_or_else(|| config_id.clone().into());
                        this.error =
                            Some(format!("Couldn't change {}: {error}", name.to_lowercase()));
                    }
                }
                // Always resync: success shows the new values, failure
                // reverts the control the user touched.
                this.config_bar.sync(this.thread.config(), window, cx);
                this.after_change(window, cx);
            });
        })
        .detach();
    }

    fn render_header(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        h_flex()
            .flex_shrink_0()
            .gap_3()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_medium()
                            .child(self.title().to_owned()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.cwd.display().to_string()),
                    ),
            )
            .child(self.config_bar.render(
                if matches!(self.loading, Loading::Ready) && self.config_pending == 0 {
                    Availability::Enabled
                } else {
                    Availability::Disabled
                },
                cx,
            ))
    }

    fn render_transcript(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        if self.thread.entries().is_empty() {
            let placeholder = v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground);
            return match &self.loading {
                Loading::Resuming => placeholder.child(Spinner::new()).child("Loading thread…"),
                Loading::Failed(message) => placeholder.child(
                    div().max_w(rems(36.)).child(
                        Alert::error("resume-error", SharedString::from(message.clone()))
                            .title("Couldn't open this thread"),
                    ),
                ),
                Loading::Ready => placeholder.child("Send a message to start."),
            }
            .into_any_element();
        }

        let view = cx.entity().downgrade();
        let row_view = view;
        div()
            .flex_1()
            .min_h_0()
            .child(
                MessageScroller::new(
                    SharedString::from(format!("transcript-{}", self.thread.id())),
                    self.scroller.clone(),
                    move |ix, _, cx| {
                        let Some(view) = row_view.upgrade() else {
                            return div().into_any_element();
                        };
                        let this = view.read(cx);
                        let entries = this.thread.entries();
                        let Some(entry) = entries.get(ix).cloned() else {
                            return div().into_any_element();
                        };
                        let key = transcript::row_key(&entry, ix);
                        let disclosure = if this.expanded.contains(key.as_ref()) {
                            Disclosure::Expanded
                        } else {
                            Disclosure::Collapsed
                        };
                        let live = this.is_busy() && ix + 1 == entries.len();
                        let on_toggle: ToggleHandler = {
                            let view = view.downgrade();
                            let key = key.clone();
                            Rc::new(move |_, cx| {
                                let _ = view.update(cx, |this, cx| this.toggle_expanded(&key, cx));
                            })
                        };
                        let row = transcript::render_row(
                            entry,
                            RowContext {
                                key,
                                disclosure,
                                live,
                                on_toggle,
                            },
                            cx,
                        );
                        // A readable measure on wide windows.
                        div()
                            .w_full()
                            .max_w(rems(48.))
                            .mx_auto()
                            .child(row)
                            .into_any_element()
                    },
                )
                .with_row_style(StyleRefinement::default().pb_4())
                .with_list_style(StyleRefinement::default().px_4())
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
            .child(Composer::new(
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
            ))
    }

    fn render_status_bar(&self) -> impl IntoElement {
        let state = match (&self.loading, self.thread.state()) {
            (Loading::Resuming, _) => "Loading",
            (Loading::Failed(_), _) => "Couldn't open",
            _ if self.sending => "Sending",
            (_, RunState::Idle) => "Ready",
            (_, RunState::Running) => "Working",
            (_, RunState::NeedsAction) => "Waiting for approval",
            (_, RunState::Cancelled) => "Stopped",
            (_, RunState::Failed(_)) => "Failed",
        };
        let usage = self.thread.usage().map(|usage| {
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
            parts.join(" · ")
        });
        let info = self.agent.info();
        StatusBar::new()
            .left(
                h_flex()
                    .gap_1()
                    .when(self.is_busy(), |this| this.child(Spinner::new().xsmall()))
                    .child(state),
            )
            .right(
                h_flex()
                    .gap_3()
                    .when_some(usage, gpui_kit::ParentElement::child)
                    .child(format!("{} {}", info.name(), info.version())),
            )
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
            .child(self.render_header(cx))
            .child(self.render_transcript(cx))
            .child(self.render_footer(cx))
            .child(self.render_status_bar())
    }
}
