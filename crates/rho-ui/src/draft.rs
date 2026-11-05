//! A new thread before its first message: a header and a composer. rho-ui
//! asks the agent for a session only when that message is sent, so an
//! unused New thread leaves nothing behind in the sidebar or in rho.

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
    rems, App, Context, Entity, EventEmitter, FocusHandle, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
};

use crate::{
    composer::{self, Composer, ComposerMode},
    projects::ProjectRef,
    thread_list::NEW_THREAD,
    thread_view::header,
};

/// The first message, to start a session with.
pub(crate) struct Start {
    pub(crate) text: String,
}

pub(crate) struct DraftView {
    cwd: PathBuf,
    project: ProjectRef,
    composer: Entity<TextareaState>,
    /// The session is being made; the text waits in the composer.
    starting: bool,
    /// Why the last start failed.
    error: Option<String>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Start> for DraftView {}

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
        ];
        Self {
            cwd,
            project,
            composer,
            starting: false,
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

    fn send(&mut self, cx: &mut Context<'_, Self>) {
        if self.starting {
            return;
        }
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.starting = true;
        self.error = None;
        cx.emit(Start { text });
        cx.notify();
    }

    /// The session couldn't be made; the text stays for another try.
    pub(crate) fn fail(&mut self, error: String, cx: &mut Context<'_, Self>) {
        self.starting = false;
        self.error = Some(error);
        cx.notify();
    }
}

impl Render for DraftView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let mode = if self.starting {
            ComposerMode::Starting
        } else {
            ComposerMode::Send
        };
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
                    .child(Composer::new(
                        &self.composer,
                        mode,
                        move |_, cx| {
                            let _ = view.update(cx, DraftView::send);
                        },
                        |_, _| {},
                    )),
            )
    }
}
