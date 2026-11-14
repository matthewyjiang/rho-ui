//! Workspace directories, each on one machine, and what their git
//! repositories say: one table shared by the sidebar rows and the session
//! header, so both show the same workspace and branch. Read from `.git`
//! files on the background executor, then re-read every [`REREAD`] to pick
//! up branch switches. A remote machine's are read with `git` there, over
//! the ssh connection to its daemon, and show their folder's name until
//! then. The saved sessions say how recently each was used, which dates the
//! projects in the new-session picker.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::{App, Context, Entity, SharedString, Task};
use rho_ui_core::{Daemon, SshLink};

use crate::{
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
    /// The name without its owner, as the title bar shows the project.
    pub(crate) short_name: SharedString,
    pub(crate) initials: SharedString,
    pub(crate) branch: Option<SharedString>,
    /// The directory, with its machine's home directory shown as `~`.
    pub(crate) path: SharedString,
    /// Unix seconds of its newest saved session; 0 with none.
    pub(crate) last_used: i64,
}

impl Workspace {
    fn new(place: Place, repo: Repo, home: Option<&Path>) -> Self {
        Self {
            path: tilde(&place.path, home).into(),
            place,
            name: repo.name.clone().into(),
            short_name: repo.short_name().to_owned().into(),
            initials: repo.initials().into(),
            branch: repo.branch.clone().map(SharedString::from),
            repo,
            last_used: 0,
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

    /// Takes what a fresh read of its repository says.
    fn set_repo(&mut self, repo: Repo) {
        self.name = repo.name.clone().into();
        self.short_name = repo.short_name().to_owned().into();
        self.initials = repo.initials().into();
        self.branch = repo.branch.clone().map(SharedString::from);
        self.repo = repo;
    }
}

/// This machine's home directory, which its paths show as `~`.
pub(crate) fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `cwd` with a leading `home` written as `~`, as a shell shows it.
pub(crate) fn tilde(cwd: &Path, home: Option<&Path>) -> String {
    match home.map(|home| cwd.strip_prefix(home)) {
        Some(Ok(rest)) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(Ok(rest)) => format!("~/{}", rest.display()),
        _ => cwd.display().to_string(),
    }
}

/// The directory `text` names, if it is a path: absolute, `~`, or under
/// `~/`. The reverse of [`tilde`]; whether it exists is the caller's to
/// check, off the UI thread.
pub(crate) fn typed_path(text: &str, home: Option<&Path>) -> Option<PathBuf> {
    if text == "~" {
        return home.map(Path::to_path_buf);
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return home.map(|home| home.join(rest));
    }
    let path = Path::new(text);
    path.is_absolute().then(|| path.to_path_buf())
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

    /// The table, to observe for changes.
    pub(crate) fn table(&self) -> &Entity<Workspaces> {
        &self.workspaces
    }
}

/// A remote machine whose daemon has answered.
struct Remote {
    machine: MachineId,
    /// Its home directory, as its daemon said.
    home: Option<PathBuf>,
    /// While its daemon is connected: how to read its repositories.
    ssh: Option<SshLink>,
}

/// Every workspace seen so far. Only ever appended to, so callers hold
/// indices instead of paths.
pub(crate) struct Workspaces {
    list: Vec<Workspace>,
    remotes: Vec<Remote>,
    /// Workspaces [`Self::index`] added this effect cycle, read together
    /// at its end: a session list of a remote machine names many at once,
    /// and reading each alone would open an ssh session per workspace,
    /// past the 10 that sshd's default `MaxSessions` allows on a master.
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
            unread: Vec::new(),
            _reread: reread,
        }
    }

    /// The home directory of `machine`, which its paths show as `~`.
    pub(crate) fn home(&self, machine: &MachineId) -> Option<PathBuf> {
        if machine.is_local() {
            return home();
        }
        self.remote(machine)?.home.clone()
    }

    fn remote(&self, machine: &MachineId) -> Option<&Remote> {
        self.remotes
            .iter()
            .find(|remote| remote.machine == *machine)
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
            ssh: daemon.ssh().cloned(),
        });
        let theirs = (0..self.list.len()).filter(|&ix| self.list[ix].machine() == machine);
        self.read(theirs.collect(), cx);
    }

    /// Stops reading remote `machine`'s repositories, as its daemon's
    /// connection is gone. Its workspaces keep what was last read.
    pub(crate) fn disconnected(&mut self, machine: &MachineId) {
        if let Some(remote) = self
            .remotes
            .iter_mut()
            .find(|remote| remote.machine == *machine)
        {
            remote.ssh = None;
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
        self.list.push(Workspace::new(
            cwd.clone(),
            repo::folder(&cwd.path),
            home.as_deref(),
        ));
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
        let mut by_link: Vec<(SshLink, Vec<usize>)> = Vec::new();
        for ix in indices {
            let machine = self.list[ix].machine();
            if machine.is_local() {
                local.push(ix);
            } else if let Some(ssh) = self.remote(machine).and_then(|remote| remote.ssh.as_ref()) {
                match by_link.iter_mut().find(|(known, _)| known == ssh) {
                    Some((_, theirs)) => theirs.push(ix),
                    None => by_link.push((ssh.clone(), vec![ix])),
                }
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
                let repos = match ssh.run_script(repo::REMOTE_SCRIPT, &dirs).await {
                    Ok(output) => repo::parse_remote(&dirs, &output),
                    Err(error) => {
                        say_unread(&error.to_string());
                        return;
                    }
                };
                let Some(repos) = repos else {
                    say_unread("the script printed something else");
                    return;
                };
                let _ = this.update(cx, |this, cx| this.set_repos(&indices, repos, cx));
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
    fn typed_paths_are_absolute_or_from_home() {
        let home = Some(Path::new("/home/me"));
        assert_eq!(typed_path("~", home), Some(PathBuf::from("/home/me")));
        assert_eq!(
            typed_path("~/code/rho", home),
            Some(PathBuf::from("/home/me/code/rho"))
        );
        assert_eq!(typed_path("/srv/x", home), Some(PathBuf::from("/srv/x")));
        assert_eq!(typed_path("rho-ui", home), None, "a name is a search");
        assert_eq!(typed_path("~/code", None), None, "no home, no ~");
        let rho = Path::new("/home/me/code/rho");
        assert_eq!(
            typed_path(&tilde(rho, home), home).as_deref(),
            Some(rho),
            "tilde and typed_path undo each other"
        );
    }

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
