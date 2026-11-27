//! How a draft's session opens: a plain session, in the draft's directory
//! or a new worktree of it, or a session in no project, a lead or a
//! scratch session, in a folder made for it (see [`crate::no_project`]).
//! Each machine's scratch root, which tells the sessions in it, is learned
//! as a connection starts, if it is there, and whenever a scratch session
//! opens.

use std::path::{Path, PathBuf};

use gpui_kit::{App, Context};
use rho_ui_core::{Checkout, Daemon, Prompt, SessionSetup};

use super::RhoApp;
use crate::{
    draft::DraftView,
    machines::{MachineId, Place},
    no_project::{Folder, FolderMaker, Root},
};

/// How [`Opening::open`] opens a session.
pub(super) enum Opening {
    /// A plain session, in the draft's directory checked out so.
    Plain(Checkout),
    /// A lead, in the folder `maker` makes in the leads root, whose tools
    /// reach `added_dirs` too: the projects its draft picked.
    Lead {
        maker: FolderMaker,
        folder: Folder,
        added_dirs: Vec<PathBuf>,
    },
    /// A plain session in no project, in the checkout of the folder `maker`
    /// makes in the scratch root.
    Scratch { maker: FolderMaker, folder: Folder },
}

impl Opening {
    /// Opens a session on `daemon` for a draft that works in `cwd`, and
    /// returns it with the directory it was opened in: `cwd` for a plain
    /// session, whose worktree its attach says, or the folder made for it.
    /// A folder its session didn't open in goes again.
    ///
    /// # Errors
    ///
    /// Says why, in the file system's, ssh's or the daemon's words, if the
    /// machine isn't connected, a folder can't be made, or the daemon won't
    /// open the session, as a rho without the lead agent won't open a lead,
    /// or one with a directory that no longer exists.
    pub(super) async fn open(
        &self,
        daemon: &Daemon,
        cwd: &Path,
    ) -> Result<(SessionSetup, PathBuf), String> {
        let (dir, opened) = match self {
            Opening::Plain(checkout) => {
                return daemon
                    .new_session(cwd, *checkout)
                    .await
                    .map(|setup| (setup, cwd.to_path_buf()))
                    .map_err(|error| error.to_string());
            }
            Opening::Lead {
                maker,
                folder,
                added_dirs,
            } => {
                let dir = maker.make(folder).await?;
                let opened = daemon.new_lead(&dir, added_dirs).await;
                (dir, opened)
            }
            Opening::Scratch { maker, folder } => {
                let dir = maker.make(folder).await?;
                let opened = daemon.new_session(&dir, Checkout::Local).await;
                (dir, opened)
            }
        };
        match opened {
            Ok(setup) => Ok((setup, dir)),
            Err(error) => {
                self.unmake(&dir).await;
                Err(error.to_string())
            }
        }
    }

    /// Removes `dir`, the directory [`Self::open`] opened a session in, if
    /// that was a folder made for the session and it is still empty, as
    /// when its session didn't start, so a retry takes its name again. A
    /// root and a plain session's directory stay.
    pub(super) async fn unmake(&self, dir: &Path) {
        match self {
            Opening::Lead {
                maker,
                folder: Folder::Named(_),
                ..
            }
            | Opening::Scratch {
                maker,
                folder: Folder::Named(_),
            } => maker.unmake(dir).await,
            Opening::Plain(_) | Opening::Lead { .. } | Opening::Scratch { .. } => {}
        }
    }

    /// The scratch root, resolved, when [`Self::open`] opened a scratch
    /// session in `dir`: `dir` itself for a draft's, else its parent.
    fn scratch_root<'a>(&self, dir: &'a Path) -> Option<&'a Path> {
        match self {
            Opening::Scratch {
                folder: Folder::Root,
                ..
            } => Some(dir),
            Opening::Scratch { .. } => dir.parent(),
            Opening::Plain(_) | Opening::Lead { .. } => None,
        }
    }

    /// The same opening, for a lead whose tools reach `dirs` too. A
    /// session's draft picks no directories, so its opening takes none.
    pub(super) fn with_added_dirs(self, dirs: Vec<PathBuf>) -> Self {
        match self {
            Opening::Lead { maker, folder, .. } => Opening::Lead {
                maker,
                folder,
                added_dirs: dirs,
            },
            other @ (Opening::Plain(_) | Opening::Scratch { .. }) => other,
        }
    }
}

impl RhoApp {
    /// How `draft`'s session opens: with no `first` message, the session
    /// the draft holds, in its directory or, in no project, its root; with
    /// one, a session of its own, in a new worktree or, in no project, a
    /// folder named from it.
    pub(super) fn opening(&self, draft: &DraftView, first: Option<&Prompt>, cx: &App) -> Opening {
        let Some(root) = draft.no_project_root(cx) else {
            return Opening::Plain(match first {
                None => Checkout::Local,
                Some(_) => Checkout::NewWorktree,
            });
        };
        let maker = self.folder_maker(root, &draft.cwd().machine, cx);
        let folder = first.map_or(Folder::Root, |first| Folder::named_from(root, first.text()));
        match root {
            Root::Leads => Opening::Lead {
                maker,
                folder,
                added_dirs: Vec::new(),
            },
            Root::Scratch => Opening::Scratch { maker, folder },
        }
    }

    /// Takes the scratch root `opening` shows in `dir`, the directory it
    /// opened a session in on `machine`, so that session is in no project
    /// even if [`Self::find_scratch_root`] didn't find the root.
    pub(super) fn opened_in(
        &self,
        opening: &Opening,
        machine: &MachineId,
        dir: &Path,
        cx: &mut App,
    ) {
        if let Some(root) = opening.scratch_root(dir) {
            let root = Place::new(machine.clone(), root);
            self.workspaces
                .update(cx, |workspaces, cx| workspaces.set_scratch_root(root, cx));
        }
    }

    /// Finds `machine`'s scratch root and tells the workspaces where it
    /// is, so the sessions in it, from earlier runs, are in no project.
    /// It makes nothing: connecting writes nothing to the machine, and a
    /// root that isn't there holds no session. Best effort: until a scratch
    /// session opens there, a root that can't be read leaves them in their
    /// folders' projects.
    pub(super) fn find_scratch_root(&self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let maker = self.folder_maker(Root::Scratch, machine, cx);
        let (workspaces, machine) = (self.workspaces.clone(), machine.clone());
        cx.spawn(async move |_, cx| {
            let Ok(Some(root)) = maker.find_root().await else {
                return;
            };
            workspaces.update(cx, |workspaces, cx| {
                workspaces.set_scratch_root(Place::new(machine, root), cx);
            });
        })
        .detach();
    }

    /// `machine`'s `root`, to make folders in.
    fn folder_maker(&self, root: Root, machine: &MachineId, cx: &App) -> FolderMaker {
        let reader = self.workspaces.read(cx).reader(machine);
        let executor = cx.background_executor().clone();
        FolderMaker::new(root, reader, self.launch.rho_home.clone(), executor)
    }
}
