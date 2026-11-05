//! Workspace directories and what their git repositories say: one table
//! shared by the sidebar rows and the thread header, so both show the same
//! project and branch. Read from `.git` files on the background executor,
//! then re-read every [`REREAD`] to pick up branch switches.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::{App, Context, Entity, SharedString, Task};

use crate::repo::{self, Repo};

/// How often branches are re-read. A branch switch shows within this long.
const REREAD: Duration = Duration::from_secs(30);

/// One workspace directory, with its display strings made once.
pub(crate) struct Project {
    cwd: PathBuf,
    repo: Repo,
    pub(crate) name: SharedString,
    pub(crate) initials: SharedString,
    pub(crate) branch: Option<SharedString>,
}

impl Project {
    fn new(cwd: PathBuf, repo: Repo) -> Self {
        Self {
            cwd,
            name: repo.name.clone().into(),
            initials: repo.initials().into(),
            branch: repo.branch.clone().map(SharedString::from),
            repo,
        }
    }
}

/// One project in a shared [`Projects`] table, for a view that shows it.
#[derive(Clone)]
pub(crate) struct ProjectRef {
    projects: Entity<Projects>,
    ix: usize,
}

impl ProjectRef {
    /// The project for `cwd`, adding it to `projects` if new.
    pub(crate) fn new(projects: &Entity<Projects>, cwd: &Path, cx: &mut App) -> Self {
        let ix = projects.update(cx, |projects, cx| projects.index(cwd, cx));
        Self {
            projects: projects.clone(),
            ix,
        }
    }

    pub(crate) fn read<'a>(&self, cx: &'a App) -> &'a Project {
        self.projects.read(cx).get(self.ix)
    }

    /// The table, to observe for changes.
    pub(crate) fn table(&self) -> &Entity<Projects> {
        &self.projects
    }
}

/// Every project seen so far. Only ever appended to, so callers hold
/// indices instead of paths.
pub(crate) struct Projects {
    list: Vec<Project>,
    _reread: Task<()>,
}

impl Projects {
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
        if let Some(ix) = self.list.iter().position(|project| project.cwd == cwd) {
            return ix;
        }
        self.list
            .push(Project::new(cwd.to_path_buf(), repo::folder(cwd)));
        let ix = self.list.len() - 1;
        self.read(vec![ix], cx);
        ix
    }

    /// The project at `ix`, from [`Self::index`].
    pub(crate) fn get(&self, ix: usize) -> &Project {
        &self.list[ix]
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
                    let project = &mut this.list[ix];
                    if project.repo != repo {
                        *project = Project::new(std::mem::take(&mut project.cwd), repo);
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
