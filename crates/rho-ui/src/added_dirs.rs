//! The directories a session's tools reach besides its workspace, behind a
//! button in the session's title bar. The button counts them and opens
//! their list, with a box to add one: an absolute or `~/` path, by the
//! rules of the session's machine. rho saves each with the session and
//! sends every client that follows it the session's options, which hold
//! the list ([`rho_ui_core::Session::added_dirs`]), so this shows what the
//! daemon says. An add sent while a turn runs waits for the turn to end,
//! as a settings change does. A subagent's session only lists them.

use std::path::PathBuf;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    popover::Popover,
    v_flex, ActiveTheme as _, Sizable as _, StyledExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, Anchor, AnyElement, App, AppContext as _, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window,
};
use rho_ui_core::{Daemon, SessionId};

use crate::{
    folders::{PathStyle, Typed},
    machines::Place,
    workspaces::{tilde, Workspaces},
};

/// Whether the user can add a directory to the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirsAccess {
    /// The user's own session.
    Add,
    /// A subagent's session, which only its run changes.
    ListOnly,
}

/// One session's added directories, and the box that adds one.
pub(crate) struct AddedDirs {
    daemon: Daemon,
    session: SessionId,
    /// The session's workspace, on its machine.
    cwd: Place,
    /// Where the machine's home is, which `~` stands for.
    workspaces: Entity<Workspaces>,
    /// As the session last listed them.
    dirs: Vec<PathBuf>,
    /// `None` in a subagent's session, which only its run changes.
    form: Option<AddForm>,
}

/// The box that adds a directory.
struct AddForm {
    input: Entity<InputState>,
    /// Why the last add failed.
    error: Option<SharedString>,
    /// An add is on its way to the daemon.
    adding: bool,
    _events: Subscription,
}

impl AddedDirs {
    pub(crate) fn new(
        daemon: Daemon,
        session: SessionId,
        cwd: Place,
        workspaces: Entity<Workspaces>,
        access: DirsAccess,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let form = (access == DirsAccess::Add).then(|| {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder("Add a directory: /path or ~/path")
            });
            let events = cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.add(window, cx),
                    InputEvent::Change => {
                        if let Some(form) = &mut this.form {
                            form.error = None;
                        }
                        cx.notify();
                    }
                    _ => {}
                },
            );
            AddForm {
                input,
                error: None,
                adding: false,
                _events: events,
            }
        });
        Self {
            daemon,
            session,
            cwd,
            workspaces,
            dirs: Vec::new(),
            form,
        }
    }

    /// Sends adds to `daemon` from now on, as once the session's machine
    /// connected again.
    pub(crate) fn set_daemon(&mut self, daemon: Daemon) {
        self.daemon = daemon;
    }

    /// Shows `dirs`, the session's list as the daemon last sent it.
    pub(crate) fn sync(&mut self, dirs: &[PathBuf], cx: &mut Context<'_, Self>) {
        if self.dirs != dirs {
            self.dirs = dirs.to_vec();
            cx.notify();
        }
    }

    /// Whether the box holds nothing typed and no add is on its way, so
    /// letting the session go loses nothing.
    pub(crate) fn is_settled(&self, cx: &App) -> bool {
        self.form
            .as_ref()
            .is_none_or(|form| !form.adding && form.input.read(cx).value().trim().is_empty())
    }

    /// Sends the typed path to the daemon, or says why it isn't one.
    fn add(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(form) = &mut self.form else {
            return;
        };
        let text = form.input.read(cx).value().trim().to_owned();
        if form.adding || text.is_empty() {
            return;
        }
        let machine = &self.cwd.machine;
        let home = self.workspaces.read(cx).home(machine);
        let Some(typed) = Typed::parse(&text, home.as_deref(), PathStyle::of(machine)) else {
            form.error = Some("Type an absolute path, or one under ~/".into());
            cx.notify();
            return;
        };
        form.adding = true;
        form.error = None;
        cx.notify();
        let (daemon, session) = (self.daemon.clone(), self.session.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = daemon.add_directory(&session, &typed.path).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(form) = &mut this.form else {
                    return;
                };
                form.adding = false;
                match result {
                    // The list itself comes with the session's options.
                    Ok(()) => form
                        .input
                        .update(cx, |input, cx| input.set_value("", window, cx)),
                    Err(error) => form.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The popover's list and box, read from `this` as it is now.
    fn render_content(this: &WeakEntity<Self>, cx: &App) -> AnyElement {
        let Some(this_entity) = this.upgrade() else {
            return div().into_any_element();
        };
        let dirs = this_entity.read(cx);
        let theme = cx.theme();
        let home = dirs.workspaces.read(cx).home(&dirs.cwd.machine);
        let list = v_flex()
            .id("added-dirs-list")
            .max_h(rems(16.))
            .overflow_y_scroll()
            .gap_1()
            .when(dirs.dirs.is_empty(), |list| {
                list.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Its tools reach only its workspace."),
                )
            })
            .children(dirs.dirs.iter().enumerate().map(|(ix, dir)| {
                div()
                    .id(SharedString::from(format!("added-dir-{ix}")))
                    .test_support()
                    .text_sm()
                    .truncate()
                    .child(tilde(dir, home.as_deref()))
            }));
        v_flex()
            .w(rems(26.))
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("Directories beside the workspace"),
            )
            .child(list)
            .when_some(dirs.form.as_ref(), |content, form| {
                let add = this.clone();
                content
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("added-dirs-input")
                                    .test_support()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&form.input).small()),
                            )
                            .child(
                                Button::new("added-dirs-add")
                                    .primary()
                                    .small()
                                    .label("Add")
                                    .loading(form.adding)
                                    .on_click(move |_, window, cx| {
                                        let _ = add.update(cx, |this, cx| this.add(window, cx));
                                    }),
                            ),
                    )
                    .when_some(form.error.clone(), |content, why| {
                        content.child(
                            div()
                                .id("added-dirs-error")
                                .test_support()
                                .text_xs()
                                .text_color(theme.red)
                                .child(why),
                        )
                    })
            })
            .into_any_element()
    }
}

impl Render for AddedDirs {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        // A subagent's session with none has nothing to show.
        if self.form.is_none() && self.dirs.is_empty() {
            return div().into_any_element();
        }
        let this = cx.entity().downgrade();
        let focus = self.form.as_ref().map(|form| form.input.focus_handle(cx));
        Popover::new("added-dirs")
            .anchor(Anchor::TopRight)
            .when_some(focus, |popover, focus| popover.track_focus(&focus))
            .trigger(
                Button::new("added-dirs-button")
                    .ghost()
                    .xsmall()
                    .icon(Lucide::FolderPlus)
                    .when(!self.dirs.is_empty(), |button| {
                        button.label(self.dirs.len().to_string())
                    })
                    .tooltip("Directories beside the workspace"),
            )
            .content(move |_, _, cx| Self::render_content(&this, cx))
            .into_any_element()
    }
}
