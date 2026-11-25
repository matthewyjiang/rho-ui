//! How a draft's session opens: a plain session, in the draft's directory
//! or a new worktree of it, or a lead, in a folder made for it (see
//! [`crate::leads`]).

use std::path::{Path, PathBuf};

use gpui_kit::App;
use rho_ui_core::{Checkout, Daemon, Prompt, SessionSetup};

use super::RhoApp;
use crate::{
    draft::DraftView,
    leads::{Folder, Kind, Leads},
};

/// How [`Opening::open`] opens a session.
pub(super) enum Opening {
    /// A plain session, in the draft's directory checked out so.
    Plain(Checkout),
    /// A lead, in the folder of the leads root that [`Leads`] makes, whose
    /// tools reach `added_dirs` too: the projects its draft picked.
    Lead {
        leads: Leads,
        folder: Folder,
        added_dirs: Vec<PathBuf>,
    },
}

impl Opening {
    /// Opens a session on `daemon` for a draft that works in `cwd`, and
    /// returns it with the directory it was opened in: `cwd` for a plain
    /// session, whose worktree its attach says, or the lead's folder. A
    /// lead's folder that its lead didn't open in goes again.
    ///
    /// # Errors
    ///
    /// Says why, in the file system's, ssh's or the daemon's words, if the
    /// machine isn't connected, a lead's folder can't be made, or the
    /// daemon won't open the session, as a rho without the lead agent
    /// won't open a lead, or one with a directory that no longer exists.
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
            Opening::Lead {
                leads,
                folder,
                added_dirs,
            } => {
                let dir = leads.make(folder).await?;
                match daemon.new_lead(&dir, added_dirs).await {
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
    /// that was a lead's own folder and it is still empty, as when its lead
    /// didn't start, so a retry takes its name again. The leads root and a
    /// plain session's directory stay.
    pub(super) async fn unmake(&self, dir: &Path) {
        if let Opening::Lead {
            leads,
            folder: Folder::Lead(_),
            ..
        } = self
        {
            leads.unmake(dir).await;
        }
    }

    /// The same opening, for a lead whose tools reach `dirs` too. A plain
    /// session's draft picks no directories, so its opening takes none.
    pub(super) fn with_added_dirs(self, dirs: Vec<PathBuf>) -> Self {
        match self {
            Opening::Lead { leads, folder, .. } => Opening::Lead {
                leads,
                folder,
                added_dirs: dirs,
            },
            plain @ Opening::Plain(_) => plain,
        }
    }
}

impl RhoApp {
    /// How `draft`'s session opens: with no `first` message, the session
    /// the draft holds, in its directory or the leads root; with one, a
    /// session of its own, in a new worktree or a lead's folder named from
    /// it.
    pub(super) fn opening(&self, draft: &DraftView, first: Option<&Prompt>, cx: &App) -> Opening {
        match (draft.kind(), first) {
            (Kind::Session, None) => Opening::Plain(Checkout::Local),
            (Kind::Session, Some(_)) => Opening::Plain(Checkout::NewWorktree),
            (Kind::Lead, first) => {
                let reader = self.workspaces.read(cx).reader(&draft.cwd().machine);
                let executor = cx.background_executor().clone();
                let leads = Leads::new(reader, self.launch.rho_home.clone(), executor);
                Opening::Lead {
                    leads,
                    folder: first.map_or(Folder::Root, |first| Folder::named_from(first.text())),
                    added_dirs: Vec::new(),
                }
            }
        }
    }
}
