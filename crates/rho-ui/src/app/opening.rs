//! How a draft's session opens: a plain session, in the draft's directory
//! or a new worktree of it, or a session in no project, a lead or a
//! scratch session, in a folder made for it (see [`crate::no_project`]).

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
    /// A session in no project, in the folder `maker` makes in its root: a
    /// lead in the leads root, whose tools reach `added_dirs` too, the
    /// projects its draft picked; or in the scratch root, a plain session
    /// in its folder's checkout.
    Made {
        maker: FolderMaker,
        folder: Folder,
        added_dirs: Vec<PathBuf>,
    },
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
        match self {
            Opening::Plain(checkout) => daemon
                .new_session(cwd, *checkout)
                .await
                .map(|setup| (setup, cwd.to_path_buf()))
                .map_err(|error| error.to_string()),
            Opening::Made {
                maker,
                added_dirs,
                folder,
            } => {
                let dir = maker.make(folder).await?;
                let opened = match maker.root() {
                    Root::Leads => daemon.new_lead(&dir, added_dirs).await,
                    Root::Scratch => daemon.new_session(&dir, Checkout::Local).await,
                };
                match opened {
                    Ok(setup) => Ok((setup, dir)),
                    Err(error) => {
                        self.unmake(&dir).await;
                        Err(error.to_string())
                    }
                }
            }
        }
    }

    /// Removes `dir`, the directory [`Self::open`] opened a session in, if
    /// that was a folder made for the session and it is still empty, as
    /// when its session didn't start, so a retry takes its name again. A
    /// root and a plain session's directory stay.
    pub(super) async fn unmake(&self, dir: &Path) {
        if let Opening::Made {
            maker,
            folder: Folder::Named(_),
            ..
        } = self
        {
            maker.unmake(dir).await;
        }
    }

    /// The same opening, for a lead whose tools reach `dirs` too. A plain
    /// session's draft picks no directories, so its opening takes none.
    pub(super) fn with_added_dirs(self, dirs: Vec<PathBuf>) -> Self {
        match self {
            Opening::Made { maker, folder, .. } => Opening::Made {
                maker,
                folder,
                added_dirs: dirs,
            },
            plain @ Opening::Plain(_) => plain,
        }
    }
}

impl RhoApp {
    /// How `draft`'s session opens: with no `first` message, the session
    /// the draft holds, in its directory or, in no project, its root; with
    /// one, a session of its own, in a new worktree or, in no project, a
    /// folder named from it.
    pub(super) fn opening(&self, draft: &DraftView, first: Option<&Prompt>, cx: &App) -> Opening {
        match (draft.no_project_root(cx), first) {
            (None, None) => Opening::Plain(Checkout::Local),
            (None, Some(_)) => Opening::Plain(Checkout::NewWorktree),
            (Some(root), first) => Opening::Made {
                maker: self.folder_maker(root, &draft.cwd().machine, cx),
                folder: first.map_or(Folder::Root, |first| Folder::named_from(root, first.text())),
                added_dirs: Vec::new(),
            },
        }
    }

    /// Makes `machine`'s scratch root, if missing, and tells the workspaces
    /// where it is, so the sessions in it are in no project. Best effort: a
    /// root that can't be made leaves them in their folders' projects, and
    /// a scratch draft says why when it can't open.
    pub(super) fn find_scratch_root(&self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let maker = self.folder_maker(Root::Scratch, machine, cx);
        let (workspaces, machine) = (self.workspaces.clone(), machine.clone());
        cx.spawn(async move |_, cx| {
            let Ok(root) = maker.make(&Folder::Root).await else {
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
