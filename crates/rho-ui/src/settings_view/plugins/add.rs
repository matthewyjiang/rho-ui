//! Add plugin…: a dialog over Settings that installs a plugin on the
//! page's machine, for the user or for the page's project, from a folder
//! there, copied or linked, or from a git repository that machine's `git`
//! fetches. It never holds up the window: a spinner shows while the daemon
//! works, and the dialog may be closed meanwhile; the install goes on, and
//! says so if it fails. When a plugin of the same name is there already,
//! it asks before replacing it.

use std::{
    cell::Cell,
    path::{Path, PathBuf},
};

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    alert::Alert,
    button::{Button, ButtonVariants as _},
    dialog::DialogFooter,
    h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    radio::{Radio, RadioGroup},
    spinner::Spinner,
    v_flex, ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
};
use rho_ui_core::{IfExists, PluginConflict, PluginError, PluginInstallSource, PluginScope};

use crate::{configured::Configured, machines::MachineId, workspaces::tilde};

/// The dialog's width, in rems so it follows the UI scale: room for a
/// GitHub link to a plugin's folder.
const WIDTH_REMS: f32 = 34.;

/// Where the dialog installs: the page's machine and project.
pub(super) struct Target {
    pub(super) machine: MachineId,
    pub(super) workspace: Option<PathBuf>,
    /// The machine, as a sentence names it.
    pub(super) label: String,
    /// Its home, which `~` stands for.
    pub(super) home: Option<PathBuf>,
}

/// What the typed source names.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Typed {
    /// A git repository, which the daemon's machine fetches.
    Url(String),
    /// A folder on the daemon's machine.
    Folder(PathBuf),
}

/// What `text` names, `~` standing for `home`; `None` while it's empty.
///
/// # Errors
///
/// What to type instead, for text that is neither an absolute path nor a
/// URL, as the daemon reads paths on its own machine, not relative to
/// anything here.
fn typed(text: &str, home: Option<&Path>) -> Result<Option<Typed>, &'static str> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.contains("://") || text.starts_with("git@") {
        return Ok(Some(Typed::Url(text.to_owned())));
    }
    if let Some(rest) = text.strip_prefix('~') {
        let Some(home) = home else {
            return Err("This machine's home isn't known yet: type the whole path.");
        };
        let rest = rest.trim_start_matches(['/', '\\']);
        let folder = if rest.is_empty() {
            home.to_path_buf()
        } else {
            // As the daemon's machine names it, whatever this one's
            // separator.
            PathBuf::from(format!("{}/{rest}", home.display()))
        };
        return Ok(Some(Typed::Folder(folder)));
    }
    let drive =
        text.as_bytes().get(1..3) == Some(b":\\") || text.as_bytes().get(1..3) == Some(b":/");
    if text.starts_with(['/', '\\']) || drive {
        return Ok(Some(Typed::Folder(PathBuf::from(text))));
    }
    Err("Type a folder's full path, or a git repository's URL.")
}

/// How a folder is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum How {
    /// A copy, which later edits to the folder don't reach.
    Copy,
    /// A link to it, so new sessions get its edits without installing
    /// again.
    Link,
}

impl Typed {
    fn source(&self, how: How) -> PluginInstallSource {
        match (self, how) {
            (Self::Url(url), _) => PluginInstallSource::Git { url: url.clone() },
            (Self::Folder(path), How::Copy) => PluginInstallSource::Path { path: path.clone() },
            (Self::Folder(path), How::Link) => PluginInstallSource::Link { path: path.clone() },
        }
    }
}

/// Where the dialog stands.
enum Phase {
    /// Being filled in; why the last try failed, if it did.
    Editing(Option<SharedString>),
    /// Sent; the daemon answers once its copy, link or git fetch is done.
    Installing(PluginInstallSource),
    /// A plugin of its name is there already; Replace sends `source`
    /// again, to replace it.
    Exists {
        conflict: PluginConflict,
        source: PluginInstallSource,
    },
}

/// The dialog's content.
struct AddPlugin {
    target: Target,
    configured: Entity<Configured>,
    source: Entity<InputState>,
    scope: PluginScope,
    how: How,
    phase: Phase,
    /// Whether the dialog closed, so a late answer doesn't close another.
    closed: bool,
    _subscription: Subscription,
}

/// Opens the dialog over Settings, the source field focused.
pub(super) fn open(
    target: Target,
    configured: Entity<Configured>,
    window: &mut Window,
    cx: &mut App,
) {
    let input = cx.new(|cx| {
        InputState::new(window, cx).placeholder("/path/to/plugin or https://github.com/owner/repo")
    });
    let view = cx.new(|cx| {
        let subscription = cx.subscribe(&input, |this: &mut AddPlugin, _, event, cx| {
            if let InputEvent::Change = event {
                this.edited(cx);
            }
        });
        AddPlugin {
            target,
            configured,
            source: input.clone(),
            scope: PluginScope::User,
            how: How::Copy,
            phase: Phase::Editing(None),
            closed: false,
            _subscription: subscription,
        }
    });
    let initial_focus = Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if initial_focus.replace(false) {
            input.focus_handle(cx).focus(window, cx);
        }
        let (content, closing, entered) = (view.clone(), view.downgrade(), view.downgrade());
        dialog
            .title("Add plugin")
            .w(window.rem_size() * WIDTH_REMS)
            // Enter confirms the dialog: it sends what was typed, and the
            // dialog stays to say how that went. Replacing takes a click
            // on Replace, not a second Enter.
            .on_ok(move |_, window, cx| {
                let _ = entered.update(cx, |this, cx| {
                    if matches!(this.phase, Phase::Editing(_)) {
                        this.submit(window, cx);
                    }
                });
                false
            })
            .on_close(move |_, _, cx| {
                let _ = closing.update(cx, |this, _| this.closed = true);
            })
            .content(move |body, _, _| body.child(content.clone()))
    });
}

impl AddPlugin {
    /// The source text, or a choice, changed: what was said of the last
    /// try no longer applies.
    fn edited(&mut self, cx: &mut Context<'_, Self>) {
        if matches!(self.phase, Phase::Editing(Some(_)) | Phase::Exists { .. }) {
            self.phase = Phase::Editing(None);
            cx.notify();
        }
    }

    fn typed(&self, cx: &App) -> Result<Option<Typed>, &'static str> {
        typed(&self.source.read(cx).value(), self.target.home.as_deref())
    }

    /// Sends the install, or after [`Phase::Exists`] the replacement.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let (source, if_exists) = match &self.phase {
            Phase::Installing(_) => return,
            Phase::Exists { source, .. } => (source.clone(), IfExists::Replace),
            Phase::Editing(_) => match self.typed(cx) {
                Ok(Some(typed)) => (typed.source(self.how), IfExists::Refuse),
                Ok(None) => return,
                Err(problem) => {
                    self.phase = Phase::Editing(Some(problem.into()));
                    cx.notify();
                    return;
                }
            },
        };
        let (machine, workspace) = (&self.target.machine, self.target.workspace.clone());
        let install = self.configured.update(cx, |configured, cx| {
            let asked = (source.clone(), self.scope);
            configured.install_plugin(machine, workspace, asked, if_exists, cx)
        });
        let Some(install) = install else {
            let problem = format!("Rho on {} isn't connected.", self.target.label);
            self.phase = Phase::Editing(Some(problem.into()));
            cx.notify();
            return;
        };
        self.phase = Phase::Installing(source.clone());
        cx.notify();
        // Kit drops the dialog's content once it closes; the install still
        // answers to it, to say if it failed.
        let view = cx.entity();
        cx.spawn_in(window, async move |_, cx| {
            let installed = install.await;
            let _ = view.update_in(cx, |this, window, cx| {
                match installed {
                    Ok(_) if this.closed => {}
                    Ok(_) => {
                        this.closed = true;
                        window.close_dialog(cx);
                    }
                    Err(PluginError::Exists { conflict, .. }) if !this.closed => {
                        this.phase = Phase::Exists { conflict, source };
                    }
                    Err(error) if this.closed => {
                        // The dialog is gone; say it somewhere.
                        window.push_notification(
                            Notification::error(error.to_string()).title("Couldn't add the plugin"),
                            cx,
                        );
                    }
                    Err(error) => this.phase = Phase::Editing(Some(error.to_string().into())),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.closed = true;
        window.close_dialog(cx);
    }

    /// The choice of whose plugins root it goes in.
    fn scope_choice(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let project = self.target.workspace.as_deref().map(|workspace| {
            format!(
                "This project ({})",
                tilde(workspace, self.target.home.as_deref())
            )
        });
        let selected = match self.scope {
            PluginScope::Project => 1,
            _ => 0,
        };
        let view = cx.entity().downgrade();
        choice(
            "For",
            RadioGroup::horizontal("plugin-scope")
                .selected_index(Some(selected))
                .disabled(self.installing())
                .child(Radio::new("plugin-scope-user").label("Just you"))
                .child(
                    Radio::new("plugin-scope-project")
                        .label(project.clone().unwrap_or_else(|| "This project".to_owned()))
                        .disabled(project.is_none()),
                )
                .on_click(move |ix: &usize, _, cx| {
                    let scope = if *ix == 1 {
                        PluginScope::Project
                    } else {
                        PluginScope::User
                    };
                    let _ = view.update(cx, |this, cx| {
                        this.scope = scope;
                        this.edited(cx);
                        cx.notify();
                    });
                }),
        )
    }

    /// The choice of copying the folder or linking to it.
    fn how_choice(&self, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let selected = match self.how {
            How::Copy => 0,
            How::Link => 1,
        };
        choice(
            "Install",
            RadioGroup::horizontal("plugin-how")
                .selected_index(Some(selected))
                .disabled(self.installing())
                .child(Radio::new("plugin-how-copy").label("A copy"))
                .child(Radio::new("plugin-how-link").label("A link, which picks up edits"))
                .on_click(move |ix: &usize, _, cx| {
                    let how = if *ix == 1 { How::Link } else { How::Copy };
                    let _ = view.update(cx, |this, cx| {
                        this.how = how;
                        this.edited(cx);
                        cx.notify();
                    });
                }),
        )
    }

    fn installing(&self) -> bool {
        matches!(self.phase, Phase::Installing(_))
    }

    /// The line under the source: what it is, why it failed, or how the
    /// install goes.
    fn note(&self, typed: &Result<Option<Typed>, &'static str>, cx: &App) -> impl IntoElement {
        let machine = &self.target.label;
        let theme = cx.theme();
        let (text, color, spinning): (SharedString, _, bool) = match &self.phase {
            Phase::Installing(PluginInstallSource::Git { .. }) => (
                format!("Fetching with git on {machine}…").into(),
                theme.muted_foreground,
                true,
            ),
            Phase::Installing(_) => ("Installing…".into(), theme.muted_foreground, true),
            Phase::Editing(Some(problem)) => (problem.clone(), theme.danger, false),
            Phase::Editing(None) | Phase::Exists { .. } => {
                let text = match typed {
                    Ok(Some(Typed::Url(_))) => format!(
                        "Rho runs git on {machine} to fetch it. A GitHub link may name a \
                         branch and folder."
                    ),
                    Ok(Some(Typed::Folder(_))) => format!("A folder on {machine}."),
                    Ok(None) | Err(_) => format!(
                        "A folder on {machine}, or a git repository's URL, such as a GitHub \
                         link to a plugin's folder."
                    ),
                };
                (text.into(), theme.muted_foreground, false)
            }
        };
        h_flex()
            .id("plugin-add-note")
            .gap_1p5()
            .text_xs()
            .text_color(color)
            .when(spinning, |this| this.child(Spinner::new().xsmall()))
            .child(text)
    }
}

/// A labelled choice in the dialog.
fn choice(label: &'static str, group: RadioGroup) -> impl IntoElement {
    h_flex()
        .gap_3()
        .text_sm()
        .child(div().w(rems(4.)).child(label))
        .child(group)
}

impl Render for AddPlugin {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let typed = self.typed(cx);
        let installing = self.installing();
        let exists = match &self.phase {
            Phase::Exists { conflict, .. } => Some(conflict.clone()),
            _ => None,
        };
        let folder = matches!(typed, Ok(Some(Typed::Folder(_))));
        let view = cx.entity().downgrade();
        let submit = {
            let view = view.clone();
            let button = Button::new("plugin-add-submit")
                .loading(installing)
                .disabled(installing || matches!(typed, Ok(None)))
                .on_click(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| this.submit(window, cx));
                });
            match exists {
                Some(_) => button.danger().label("Replace"),
                None => button.primary().label("Add"),
            }
        };
        let cancel = Button::new("plugin-add-cancel")
            .ghost()
            .label("Cancel")
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| this.cancel(window, cx));
            });
        let replace = exists.map(|conflict| {
            let scope = match self.scope {
                PluginScope::Project => "this project",
                _ => "you",
            };
            // Kit's Alert doesn't register for tests; its wrapper does.
            div()
                .id("plugin-exists")
                .test_support()
                .child(Alert::warning(
                    "plugin-exists-alert",
                    format!(
                        "{} is already installed for {scope}, at {}. Replace it with this one?",
                        conflict.name,
                        tilde(&conflict.path, self.target.home.as_deref())
                    ),
                ))
        });
        v_flex()
            .id("plugin-add")
            .gap_3()
            .child(
                v_flex()
                    .gap_1p5()
                    .child(
                        Input::new(&self.source)
                            .id("plugin-source")
                            .disabled(installing),
                    )
                    .child(self.note(&typed, cx)),
            )
            .child(self.scope_choice(cx))
            .when(folder, |this| this.child(self.how_choice(cx)))
            .children(replace)
            .child(DialogFooter::new().child(cancel).child(submit))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{typed, Typed};

    /// The daemon reads a path on its own machine and never resolves a
    /// relative one, so the dialog sends absolute paths and URLs as typed,
    /// expands `~` to the machine's home, and asks again for anything else.
    #[test]
    fn a_source_is_a_url_or_an_absolute_folder() {
        let home = Some(Path::new("/home/me"));
        let folder = |path: &str| Ok(Some(Typed::Folder(PathBuf::from(path))));
        let url = |url: &str| Ok(Some(Typed::Url(url.to_owned())));
        let cases = [
            ("", home, Ok(None)),
            ("   ", home, Ok(None)),
            ("/srv/plugins/review", home, folder("/srv/plugins/review")),
            (" /srv/review ", home, folder("/srv/review")),
            ("~/code/review", home, folder("/home/me/code/review")),
            ("~", home, folder("/home/me")),
            (r"C:\plugins\review", home, folder(r"C:\plugins\review")),
            (
                "https://github.com/acme/plugins/tree/main/review",
                home,
                url("https://github.com/acme/plugins/tree/main/review"),
            ),
            (
                "git@github.com:acme/review.git",
                home,
                url("git@github.com:acme/review.git"),
            ),
            ("file:///srv/repo", home, url("file:///srv/repo")),
            ("code/review", home, Err("")),
            ("~/code/review", None, Err("")),
        ];
        for (text, home, want) in cases {
            let got = typed(text, home);
            match (&got, &want) {
                (Err(_), Err(_)) => {}
                _ => assert_eq!(got, want, "{text:?}"),
            }
        }
    }
}
