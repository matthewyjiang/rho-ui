//! Workspace directories and what their git repositories say: one table
//! shared by the sidebar rows and the session header, so both show the same
//! workspace and branch. Read from `.git` files on the background executor,
//! then re-read every [`REREAD`] to pick up branch switches. The saved
//! sessions say how recently each was used, which orders the new-session
//! project picker.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::{App, Context, Entity, SharedString, Task};

use crate::repo::{self, Repo};

/// How often branches are re-read. A branch switch shows within this long.
const REREAD: Duration = Duration::from_secs(30);

/// One workspace directory, with its display strings made once.
pub(crate) struct Workspace {
    cwd: PathBuf,
    repo: Repo,
    pub(crate) name: SharedString,
    pub(crate) initials: SharedString,
    pub(crate) branch: Option<SharedString>,
    /// The directory, with the home directory shown as `~`.
    pub(crate) path: SharedString,
    /// Unix seconds of its newest saved session; 0 with none.
    pub(crate) last_used: i64,
    /// How many saved sessions run in it.
    pub(crate) sessions: usize,
}

impl Workspace {
    fn new(cwd: PathBuf, repo: Repo) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self {
            path: tilde(&cwd, home.as_deref()).into(),
            cwd,
            name: repo.name.clone().into(),
            initials: repo.initials().into(),
            branch: repo.branch.clone().map(SharedString::from),
            repo,
            last_used: 0,
            sessions: 0,
        }
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Takes what a fresh read of its repository says.
    fn set_repo(&mut self, repo: Repo) {
        self.name = repo.name.clone().into();
        self.initials = repo.initials().into();
        self.branch = repo.branch.clone().map(SharedString::from);
        self.repo = repo;
    }
}

/// `cwd` with a leading `home` written as `~`, as a shell shows it.
fn tilde(cwd: &Path, home: Option<&Path>) -> String {
    match home.map(|home| cwd.strip_prefix(home)) {
        Some(Ok(rest)) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(Ok(rest)) => format!("~/{}", rest.display()),
        _ => cwd.display().to_string(),
    }
}

/// One workspace in a shared [`Workspaces`] table, for a view that shows it.
#[derive(Clone)]
pub(crate) struct WorkspaceRef {
    workspaces: Entity<Workspaces>,
    ix: usize,
}

impl WorkspaceRef {
    /// The workspace for `cwd`, adding it to `workspaces` if new.
    pub(crate) fn new(workspaces: &Entity<Workspaces>, cwd: &Path, cx: &mut App) -> Self {
        let ix = workspaces.update(cx, |workspaces, cx| workspaces.index(cwd, cx));
        Self {
            workspaces: workspaces.clone(),
            ix,
        }
    }

    pub(crate) fn read<'a>(&self, cx: &'a App) -> &'a Workspace {
        self.workspaces.read(cx).get(self.ix)
    }

    /// The table, to observe for changes.
    pub(crate) fn table(&self) -> &Entity<Workspaces> {
        &self.workspaces
    }
}

/// Every workspace seen so far. Only ever appended to, so callers hold
/// indices instead of paths.
pub(crate) struct Workspaces {
    list: Vec<Workspace>,
    _reread: Task<()>,
}

impl Workspaces {
    pub(crate) fn new(cx: &mut Context<'_, Self>) -> Self {
        let reread = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REREAD).await;
            let all = this.update(cx, |this, cx| this.read((0..this.list.len()).collect(), cx));
            if all.is_err() {
                break;
            }
        });
        Self {
            list: Vec::new(),
            _reread: reread,
        }
    }

    /// The index for `cwd`. A new one shows its folder name until its
    /// repository has been read in the background.
    pub(crate) fn index(&mut self, cwd: &Path, cx: &mut Context<'_, Self>) -> usize {
        if let Some(ix) = self.list.iter().position(|workspace| workspace.cwd == cwd) {
            return ix;
        }
        self.list
            .push(Workspace::new(cwd.to_path_buf(), repo::folder(cwd)));
        let ix = self.list.len() - 1;
        self.read(vec![ix], cx);
        ix
    }

    /// The workspace at `ix`, from [`Self::index`].
    pub(crate) fn get(&self, ix: usize) -> &Workspace {
        &self.list[ix]
    }

    /// Every workspace, the most recently used first; never-used ones follow
    /// by name.
    pub(crate) fn recent(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.list.len()).collect();
        order.sort_by(|&a, &b| {
            let (a, b) = (&self.list[a], &self.list[b]);
            b.last_used
                .cmp(&a.last_used)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.cwd.cmp(&b.cwd))
        });
        order
    }

    /// Counts the saved sessions, given as `(workspace, updated)`, and
    /// notifies if that changed any workspace's use.
    pub(crate) fn set_history(
        &mut self,
        sessions: impl IntoIterator<Item = (usize, i64)>,
        cx: &mut Context<'_, Self>,
    ) {
        let mut uses = vec![(0_i64, 0_usize); self.list.len()];
        for (ix, updated) in sessions {
            let (last_used, count) = &mut uses[ix];
            *last_used = (*last_used).max(updated);
            *count += 1;
        }
        let mut changed = false;
        for (workspace, (last_used, sessions)) in self.list.iter_mut().zip(uses) {
            if (workspace.last_used, workspace.sessions) != (last_used, sessions) {
                workspace.last_used = last_used;
                workspace.sessions = sessions;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// Reads the repositories of `indices` in the background, and notifies
    /// only if one changed.
    fn read(&self, indices: Vec<usize>, cx: &mut Context<'_, Self>) {
        let jobs: Vec<(usize, PathBuf)> = indices
            .into_iter()
            .map(|ix| (ix, self.list[ix].cwd.clone()))
            .collect();
        cx.spawn(async move |this, cx| {
            let repos = cx
                .background_executor()
                .spawn(async move {
                    jobs.into_iter()
                        .map(|(ix, cwd)| (ix, repo::inspect(&cwd)))
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let mut changed = false;
                for (ix, repo) in repos {
                    let workspace = &mut this.list[ix];
                    if workspace.repo != repo {
                        workspace.set_repo(repo);
                        changed = true;
                    }
                }
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_shows_as_tilde() {
        let home = Path::new("/home/me");
        assert_eq!(
            tilde(Path::new("/home/me/code/rho"), Some(home)),
            "~/code/rho"
        );
        assert_eq!(tilde(home, Some(home)), "~");
        assert_eq!(tilde(Path::new("/home/meg"), Some(home)), "/home/meg");
        assert_eq!(tilde(Path::new("/srv/x"), None), "/srv/x");
    }
}
