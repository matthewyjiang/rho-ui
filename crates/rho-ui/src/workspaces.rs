//! Workspace directories, each on one machine, and what their git
//! repositories say: one table shared by the sidebar rows and the session
//! header, so both show the same workspace and branch. Read from `.git`
//! files on the background executor, then re-read every [`REREAD`] to pick
//! up branch switches. A machine reached through a link, ssh or
//! `rho-ui serve`, has its read with `git` there, over that link, and they
//! show their folder's name until then; in a browser, that is this machine
//! too. A remote machine whose repositories can't be read says why at the
//! foot of the sidebar until a read succeeds. The saved sessions say how
//! recently each was used, which dates the projects in the new-session
//! picker.

use std::{
    cmp::Reverse,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::{App, Context, Entity, SharedString, Task};
use rho_ui_core::{platform, Daemon, Link};

use crate::{
    folders::Reader,
    machines::{MachineId, Place},
    repo::{self, Repo},
};

/// How often branches are re-read. A branch switch shows within this long.
const REREAD: Duration = Duration::from_secs(30);

/// One workspace directory, with its display strings made once.
pub(crate) struct Workspace {
    place: Place,
    repo: Repo,
    pub(crate) name: SharedString,
    /// The name without its owner, as the title bar shows the project;
    /// see [`Self::project_key`].
    pub(crate) short_name: SharedString,
    pub(crate) initials: SharedString,
    pub(crate) branch: Option<SharedString>,
    /// The directory, with its machine's home directory shown as `~`.
    pub(crate) path: SharedString,
    /// Unix seconds of its newest saved session; 0 with none.
    pub(crate) last_used: i64,
    /// Whether it is its machine's scratch root or a folder in it, once
    /// [`Workspaces::set_scratch_root`] says where that is.
    scratch: bool,
}

impl Workspace {
    fn new(place: Place, repo: Repo, home: Option<&Path>) -> Self {
        Self {
            path: tilde(&place.path, home).into(),
            place,
            name: repo.name.clone().into(),
            short_name: repo.short_name().to_owned().into(),
            initials: repo.initials().into(),
            branch: repo.head.as_ref().map(|head| head.name().to_owned().into()),
            repo,
            last_used: 0,
            scratch: false,
        }
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.place.path
    }

    pub(crate) fn machine(&self) -> &MachineId {
        &self.place.machine
    }

    pub(crate) fn place(&self) -> &Place {
        &self.place
    }

    /// Whether it is in its machine's scratch root, where sessions started
    /// in no project work (see [`crate::no_project`]).
    pub(crate) fn is_scratch(&self) -> bool {
        self.scratch
    }

    /// Its repository's origin remote URL, which clones it elsewhere.
    pub(crate) fn origin(&self) -> Option<&str> {
        self.repo.origin.as_deref()
    }

    /// The folder of the project it belongs to, on its machine; see
    /// [`Self::root`].
    pub(crate) fn root_path(&self) -> &Path {
        &self.repo.root
    }

    /// The project it belongs to; see [`Repo::root`]. Its own directory
    /// until its repository has been read, and on another machine.
    pub(crate) fn root(&self) -> Place {
        Place::new(self.place.machine.clone(), self.repo.root.clone())
    }

    /// What makes two checkouts, on any machines, the same project: the
    /// repository's name without its owner, or outside git the folder's
    /// name. Not the owner, as a remote checkout shows only its folder's
    /// name until its repository is read over ssh; so forks of a
    /// repository, and same-named folders, count as one project.
    pub(crate) fn project_key(&self) -> &str {
        &self.short_name
    }

    /// Takes what a fresh read of its repository says.
    fn set_repo(&mut self, repo: Repo) {
        self.name = repo.name.clone().into();
        self.short_name = repo.short_name().to_owned().into();
        self.initials = repo.initials().into();
        self.branch = repo.head.as_ref().map(|head| head.name().to_owned().into());
        self.repo = repo;
    }
}

/// For tests: a workspace at `path` on this machine, read as a folder of
/// the project at `root` named `name`.
#[cfg(test)]
impl Workspace {
    pub(crate) fn local(path: &str, name: &str, root: &str) -> Self {
        let repo = Repo {
            name: name.to_owned(),
            origin: None,
            head: None,
            root: root.into(),
        };
        Self::new(Place::local(path), repo, None)
    }

    /// The same workspace, in its machine's scratch root.
    pub(crate) fn scratch(mut self) -> Self {
        self.scratch = true;
        self
    }
}

/// `cwd` with a leading `home` written as `~`, as a shell shows it.
pub(crate) fn tilde(cwd: &Path, home: Option<&Path>) -> String {
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
    pub(crate) fn new(workspaces: &Entity<Workspaces>, cwd: &Place, cx: &mut App) -> Self {
        let ix = workspaces.update(cx, |workspaces, cx| workspaces.index(cwd, cx));
        Self {
            workspaces: workspaces.clone(),
            ix,
        }
    }

    pub(crate) fn read<'a>(&self, cx: &'a App) -> &'a Workspace {
        self.workspaces.read(cx).get(self.ix)
    }

    /// Its index into the table.
    pub(crate) fn index(&self) -> usize {
        self.ix
    }

    /// The table, to observe for changes.
    pub(crate) fn table(&self) -> &Entity<Workspaces> {
        &self.workspaces
    }
}

/// A machine whose daemon answered through a link: a remote one, or in a
/// browser, this one.
struct Remote {
    machine: MachineId,
    /// Its home directory, as its daemon said.
    home: Option<PathBuf>,
    /// While its daemon is connected: how to read its repositories.
    link: Option<Link>,
    /// Why its repositories couldn't be read, until a read succeeds.
    unread: Option<SharedString>,
}

/// Whether rho-ui reads `machine`'s files itself: this machine's, unless
/// rho-ui runs in a browser, which reaches even this machine only through
/// its link.
pub(crate) fn reads_here(machine: &MachineId) -> bool {
    machine.is_local() && cfg!(not(target_family = "wasm"))
}

/// Every workspace seen so far. Only ever appended to, so callers hold
/// indices instead of paths.
pub(crate) struct Workspaces {
    list: Vec<Workspace>,
    remotes: Vec<Remote>,
    /// Each machine's scratch root, resolved, once found.
    scratch_roots: Vec<Place>,
    /// Workspaces [`Self::index`] added this effect cycle, read together
    /// at its end: a session list of a remote machine names many at once,
    /// and reading each alone would run an ssh script per workspace, ahead
    /// of everything else read there: the connection runs only a few
    /// scripts at once.
    unread: Vec<usize>,
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
            remotes: Vec::new(),
            scratch_roots: Vec::new(),
            unread: Vec::new(),
            _reread: reread,
        }
    }

    /// The home directory of `machine`, which its paths show as `~`.
    pub(crate) fn home(&self, machine: &MachineId) -> Option<PathBuf> {
        match self.remote(machine) {
            Some(linked) => linked.home.clone(),
            None if reads_here(machine) => platform::home_dir(),
            None => None,
        }
    }

    /// How to read files and run `git` on `machine`: over its link while
    /// its daemon is connected through one, else here for this machine.
    pub(crate) fn reader(&self, machine: &MachineId) -> Reader {
        match self.remote(machine) {
            Some(linked) => linked.link.clone().map_or(Reader::Unread, Reader::There),
            None if reads_here(machine) => Reader::Here,
            None => Reader::Unread,
        }
    }

    fn remote(&self, machine: &MachineId) -> Option<&Remote> {
        self.remotes
            .iter()
            .find(|remote| remote.machine == *machine)
    }

    /// Why remote `machine`'s repositories couldn't be read, until a read
    /// succeeds or its connection goes.
    pub(crate) fn unread(&self, machine: &MachineId) -> Option<&SharedString> {
        self.remote(machine)?.unread.as_ref()
    }

    /// Takes what remote `machine`'s newly connected `daemon` says: shows
    /// its workspaces' paths from its home directory, and reads their
    /// repositories over its ssh link.
    pub(crate) fn connected(
        &mut self,
        machine: &MachineId,
        daemon: &Daemon,
        cx: &mut Context<'_, Self>,
    ) {
        let home = daemon.info().home().map(Path::to_path_buf);
        if home.is_some() && self.home(machine) != home {
            for workspace in &mut self.list {
                if workspace.machine() == machine {
                    workspace.path = tilde(workspace.cwd(), home.as_deref()).into();
                }
            }
            cx.notify();
        }
        self.remotes.retain(|remote| remote.machine != *machine);
        self.remotes.push(Remote {
            machine: machine.clone(),
            home,
            link: daemon.link().cloned(),
            unread: None,
        });
        let theirs = (0..self.list.len()).filter(|&ix| self.list[ix].machine() == machine);
        self.read(theirs.collect(), cx);
    }

    /// Stops reading remote `machine`'s repositories, as its daemon's
    /// connection is gone, which the sidebar says instead of why they
    /// couldn't be read. Its workspaces keep what was last read.
    pub(crate) fn disconnected(&mut self, machine: &MachineId) {
        if let Some(remote) = self
            .remotes
            .iter_mut()
            .find(|remote| remote.machine == *machine)
        {
            remote.link = None;
            remote.unread = None;
        }
    }

    /// The index for `cwd`. A new one shows its folder name until its
    /// repository has been read in the background, with the others new
    /// this effect cycle.
    pub(crate) fn index(&mut self, cwd: &Place, cx: &mut Context<'_, Self>) -> usize {
        if let Some(ix) = self
            .list
            .iter()
            .position(|workspace| workspace.place == *cwd)
        {
            return ix;
        }
        let home = self.home(&cwd.machine);
        let mut workspace = Workspace::new(cwd.clone(), repo::folder(&cwd.path), home.as_deref());
        workspace.scratch = self.under_scratch_root(cwd);
        self.list.push(workspace);
        let ix = self.list.len() - 1;
        if self.unread.is_empty() {
            let this = cx.weak_entity();
            cx.defer(move |cx| {
                let _ = this.update(cx, |this, cx| {
                    let unread = std::mem::take(&mut this.unread);
                    this.read(unread, cx);
                });
            });
        }
        self.unread.push(ix);
        ix
    }

    /// Takes `root` as its machine's scratch root, resolved, and marks the
    /// workspaces in it there ([`Workspace::is_scratch`]).
    pub(crate) fn set_scratch_root(&mut self, root: Place, cx: &mut Context<'_, Self>) {
        if self.scratch_root(&root.machine) == Some(root.path.as_path()) {
            return;
        }
        let mut changed = false;
        let theirs = self
            .list
            .iter_mut()
            .filter(|workspace| workspace.place.machine == root.machine);
        for workspace in theirs {
            let scratch = workspace.place.path.starts_with(&root.path);
            changed |= workspace.scratch != scratch;
            workspace.scratch = scratch;
        }
        self.scratch_roots
            .retain(|known| known.machine != root.machine);
        self.scratch_roots.push(root);
        if changed {
            cx.notify();
        }
    }

    /// The scratch root of `machine`, once found.
    pub(crate) fn scratch_root(&self, machine: &MachineId) -> Option<&Path> {
        self.scratch_roots
            .iter()
            .find(|root| root.machine == *machine)
            .map(|root| root.path.as_path())
    }

    /// Whether `place` is its machine's scratch root or in it.
    fn under_scratch_root(&self, place: &Place) -> bool {
        self.scratch_root(&place.machine)
            .is_some_and(|root| place.path.starts_with(root))
    }

    /// The indices for `dirs`, on `machine`, each as [`Self::index`] finds
    /// it.
    pub(crate) fn index_all(
        &mut self,
        machine: &MachineId,
        dirs: &[PathBuf],
        cx: &mut Context<'_, Self>,
    ) -> Vec<usize> {
        dirs.iter()
            .map(|dir| self.index(&Place::new(machine.clone(), dir.clone()), cx))
            .collect()
    }

    /// The workspace at `ix`, from [`Self::index`].
    pub(crate) fn get(&self, ix: usize) -> &Workspace {
        &self.list[ix]
    }

    /// Unix seconds of the newest saved session in project `root`, in any
    /// of its folders or worktrees; 0 with none.
    pub(crate) fn last_used_in(&self, root: &Place) -> i64 {
        self.list
            .iter()
            .filter(|workspace| workspace.root() == *root)
            .map(|workspace| workspace.last_used)
            .max()
            .unwrap_or(0)
    }

    /// Orders the workspaces at `ixs` as projects are listed: most recently
    /// used first ([`Self::last_used_in`]), then by name, then by path.
    pub(crate) fn sort_by_use(&self, ixs: &mut [usize]) {
        ixs.sort_by_cached_key(|&ix| {
            let workspace = self.get(ix);
            (
                Reverse(self.last_used_in(workspace.place())),
                workspace.name.clone(),
                workspace.cwd().to_path_buf(),
            )
        });
    }

    /// The workspace whose directory is one of `places`, if any.
    pub(crate) fn find(&self, places: &[&Place]) -> Option<usize> {
        self.list
            .iter()
            .position(|workspace| places.contains(&&workspace.place))
    }

    /// Dates each of `machine`'s workspaces by its newest saved session,
    /// given as `(workspace, updated)`, and notifies if that changed any.
    /// Other machines' workspaces keep their dates.
    pub(crate) fn set_history(
        &mut self,
        machine: &MachineId,
        sessions: impl IntoIterator<Item = (usize, i64)>,
        cx: &mut Context<'_, Self>,
    ) {
        let mut newest = vec![0_i64; self.list.len()];
        for (ix, updated) in sessions {
            newest[ix] = newest[ix].max(updated);
        }
        let mut changed = false;
        let theirs = self
            .list
            .iter_mut()
            .zip(newest)
            .filter(|(workspace, _)| workspace.machine() == machine);
        for (workspace, last_used) in theirs {
            if workspace.last_used != last_used {
                workspace.last_used = last_used;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// Reads the repositories of `indices` in the background: this
    /// machine's from their files, each connected remote machine's with one
    /// script over ssh. A remote machine not connected waits for
    /// [`Self::connected`].
    fn read(&self, indices: Vec<usize>, cx: &mut Context<'_, Self>) {
        let mut local = Vec::new();
        let mut by_link: Vec<(Link, Vec<usize>)> = Vec::new();
        for ix in indices {
            let machine = self.list[ix].machine();
            if let Some(link) = self.remote(machine).and_then(|remote| remote.link.as_ref()) {
                match by_link.iter_mut().find(|(known, _)| known == link) {
                    Some((_, theirs)) => theirs.push(ix),
                    None => by_link.push((link.clone(), vec![ix])),
                }
            } else if reads_here(machine) {
                local.push(ix);
            }
        }
        if !local.is_empty() {
            let dirs = self.dirs(&local);
            cx.spawn(async move |this, cx| {
                let repos = cx
                    .background_executor()
                    .spawn(async move { dirs.iter().map(|dir| repo::inspect(dir)).collect() })
                    .await;
                let _ = this.update(cx, |this, cx| this.set_repos(&local, repos, cx));
            })
            .detach();
        }
        for (ssh, indices) in by_link {
            let dirs = self.dirs(&indices);
            cx.spawn(async move |this, cx| {
                let read = match ssh.run_script(repo::REMOTE_SCRIPT, &dirs).await {
                    Ok(output) => repo::parse_remote(&dirs, &output)
                        .ok_or_else(|| "The script printed something else.".to_owned()),
                    Err(error) => Err(error.to_string()),
                };
                let _ = this.update(cx, |this, cx| match read {
                    Ok(repos) => {
                        this.set_unread(&ssh, None, cx);
                        this.set_repos(&indices, repos, cx);
                    }
                    Err(why) => this.set_unread(&ssh, Some(why.as_str()), cx),
                });
            })
            .detach();
        }
    }

    fn dirs(&self, indices: &[usize]) -> Vec<PathBuf> {
        indices
            .iter()
            .map(|&ix| self.list[ix].cwd().to_path_buf())
            .collect()
    }

    /// Records why the repositories of the machines `link` reaches couldn't
    /// be read, or with `None` that they were. Each [`REREAD`] tries again,
    /// so stderr hears of a failure only when it changes.
    fn set_unread(&mut self, link: &Link, why: Option<&str>, cx: &mut Context<'_, Self>) {
        let mut changed = false;
        for remote in &mut self.remotes {
            if remote.link.as_ref() == Some(link) && remote.unread.as_deref() != why {
                remote.unread = why.map(|why| SharedString::from(why.to_owned()));
                changed = true;
            }
        }
        if changed {
            if let Some(why) = why {
                say_unread(why);
            }
            cx.notify();
        }
    }

    /// Takes the repositories read for `indices`, and notifies only if one
    /// changed.
    fn set_repos(&mut self, indices: &[usize], repos: Vec<Repo>, cx: &mut Context<'_, Self>) {
        let mut changed = false;
        for (&ix, repo) in indices.iter().zip(repos) {
            let workspace = &mut self.list[ix];
            if workspace.repo != repo {
                workspace.set_repo(repo);
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }
}

#[expect(
    clippy::print_stderr,
    reason = "rho-ui has no log; stderr reaches the terminal or the journal"
)]
fn say_unread(why: &str) {
    eprintln!("rho-ui couldn't read a remote machine's repositories: {why}");
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
