//! Cloning a project's repository onto a machine, into that machine's clone
//! folder (see [`crate::machines::Machine::clone_folder`]). The project
//! list offers it for a project another machine holds, and for a git URL
//! typed in its search. [`clone_here`] clones on this machine, and
//! [`SCRIPT`] on a remote one, over the ssh connection to its daemon; both
//! find the folder by the same rules. [`find_projects`] lists the folders
//! already in it, which are projects without being added (see
//! [`crate::projects::Projects`]).

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use rho_ui_core::platform;

use gpui_kit::BackgroundExecutor;

use crate::{
    folders::{self, Deadline, Reader},
    local, repo,
};

/// A repository to clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    /// What `git clone` takes.
    pub(crate) url: String,
    /// `owner/name`, as the project list names it; see [`repo::url_name`].
    pub(crate) name: String,
}

impl Source {
    /// The repository at `url`, an origin remote's URL. `None` if it names
    /// none.
    pub(crate) fn new(url: &str) -> Option<Self> {
        Some(Self {
            name: repo::url_name(url)?,
            url: url.to_owned(),
        })
    }

    /// The repository `text`, typed in the project search, names, if it is
    /// a git URL: `https://`, `http://`, `ssh://`, `git://` or `file://`,
    /// or ssh's `user@host:path`. A bare `host:path` is left to be a
    /// search, as it may be one.
    pub(crate) fn typed(text: &str) -> Option<Self> {
        let text = text.trim();
        let schemed = ["https://", "http://", "ssh://", "git://", "file://"]
            .iter()
            .any(|scheme| text.starts_with(scheme));
        let scp = text.split_once(':').is_some_and(|(host, path)| {
            host.contains('@') && !host.contains('/') && !path.is_empty()
        });
        let url = (schemed || scp) && !text.contains(char::is_whitespace);
        url.then(|| Self::new(text)).flatten()
    }

    /// The repository's name without its owner: its folder's name once
    /// cloned.
    pub(crate) fn folder(&self) -> &str {
        repo::without_owner(&self.name)
    }
}

/// The `sh` that turns `$folder`, the clone folder setting, into the folder
/// it names, from the home directory, which it moves to: an empty setting
/// is `$RHO_HOME/projects`, the rho home found as rho-ui's ssh setup finds
/// it, and `~` is the home directory. [`folder_here`] follows the same
/// rules here. A macro, so [`SCRIPT`] and [`FIND_SCRIPT`] can `concat!` it.
macro_rules! resolve_folder {
    () => {
        r#"cd || exit
case $folder in
  "") folder=${RHO_HOME:-$HOME/.rho}/projects ;;
  "~") folder=$HOME ;;
  "~/"*) folder=$HOME/${folder#??} ;;
esac
"#
    };
}

/// The POSIX `sh` script that clones a repository on a remote machine,
/// given its URL, the clone folder setting, and the folder name as its
/// arguments, into the folder [`resolve_folder`] names. A folder that already holds a clone of the
/// URL with a commit is taken as it is, so a clone that finished before
/// rho-ui quit can be picked again. One with none yet is refused: a clone
/// may still be running into it, from another draft, or one was cut short,
/// which removing the folder lets the next pick redo. git never prompts:
/// with no terminal, a password or host key question fails with git's
/// message, and ssh runs with `-o BatchMode=yes` (a remote machine's ssh
/// is OpenSSH). It prints a NUL, then the clone's resolved path and a NUL,
/// so whatever a login file prints comes before them.
pub(crate) const SCRIPT: &str = concat!(
    "url=$1 folder=$2 name=$3\n",
    resolve_folder!(),
    r#"mkdir -p -- "$folder" && folder=$(cd -P -- "$folder" && pwd -P) || exit
dest=$folder/$name
export GIT_TERMINAL_PROMPT=0 GIT_SSH_COMMAND="${GIT_SSH_COMMAND:-ssh} -o BatchMode=yes"
if [ -e "$dest" ]; then
  if [ "$(git -C "$dest" config --get remote.origin.url 2>/dev/null)" != "$url" ]; then
    echo "$dest is already there, and isn't a clone of $url" >&2
    exit 1
  fi
  git -C "$dest" rev-parse -q --verify HEAD >/dev/null || {
    echo "$dest holds a clone of $url with no commit yet: a clone may still be running into it, or one was cut short; if so, remove it to clone again" >&2
    exit 1
  }
else
  git clone --quiet -- "$url" "$dest" </dev/null || exit
fi
printf '\0%s\0' "$dest"
"#
);

/// The POSIX `sh` script that lists the projects in a remote machine's
/// clone folder, given the clone folder setting as its argument: after
/// [`FOUND`], the path of each folder in it, not hidden ones, each ended
/// by a NUL, from the folder resolved as [`SCRIPT`] resolves it, so a
/// clone's path is the same either way. Nothing after [`FOUND`] when the
/// folder isn't there. [`find_here`] does the same here.
pub(crate) const FIND_SCRIPT: &str = concat!(
    "folder=$1\n",
    resolve_folder!(),
    r#"printf 'rho-ui-clones\0'
[ -d "$folder" ] || exit 0
folder=$(cd -P -- "$folder" && pwd -P) || exit
for sub in "$folder"/*; do
  [ -d "$sub" ] && printf '%s\0' "$sub"
done
exit 0
"#
);

/// Printed before [`FIND_SCRIPT`]'s paths, so whatever a login file prints
/// comes first and is skipped.
const FOUND: &[u8] = b"rho-ui-clones\0";

/// The projects in the clone folder `folder`, the setting, on the machine
/// `reader` reads: each folder in it but hidden ones, sorted. Here with
/// [`find_here`] on `executor`, there with [`FIND_SCRIPT`]. None when the
/// folder isn't there yet, as before the first clone.
///
/// # Errors
///
/// Says why, in ssh's words or the file system's, if the machine isn't
/// connected or the folder can't be read.
pub(crate) async fn find_projects(
    reader: Reader,
    folder: String,
    executor: BackgroundExecutor,
) -> Result<Vec<PathBuf>, String> {
    let args = vec![OsString::from(&folder)];
    let here = move || -> Result<Vec<PathBuf>, String> {
        // Without a home there is no clone folder to look in.
        let (Some(home), Some(rho_home)) = (platform::home_dir(), platform::rho_home()) else {
            return Ok(Vec::new());
        };
        find_here(&folder_here(&folder, &home, &rho_home.join("projects")))
    };
    let parse = |output: Vec<u8>| {
        let fields = folders::after(&output, FOUND)
            .and_then(repo::nul_fields)
            .ok_or_else(|| "the project script printed something else".to_owned())?;
        let mut found: Vec<PathBuf> = fields.into_iter().map(platform::path_from_bytes).collect();
        found.sort();
        Ok(found)
    };
    reader
        .query(&executor, here, FIND_SCRIPT, args, Deadline::Script, parse)
        .await
}

/// The folder the clone folder setting `folder` names on this machine, by
/// [`resolve_folder`]'s rules, with `home` as the home directory and
/// `projects` as the folder an empty setting names, from `home` when
/// relative, as the script runs from there.
fn folder_here(folder: &str, home: &Path, projects: &Path) -> PathBuf {
    match folder {
        "" => home.join(projects),
        _ => home.join(platform::expand_tilde(Path::new(folder), home)),
    }
}

/// The projects in `folder` on this machine, as [`FIND_SCRIPT`] lists
/// them, sorted. Does file I/O; call it off the UI thread.
///
/// # Errors
///
/// Says why if `folder` is there but can't be read.
fn find_here(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let couldnt = |error: std::io::Error| format!("Couldn't read {}: {error}", folder.display());
    let folder = match platform::canonicalize(folder) {
        Ok(folder) if folder.is_dir() => folder,
        Ok(_) => return Ok(Vec::new()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(couldnt(error)),
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(&folder)
        .map_err(couldnt)?
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| folder.join(entry.file_name()))
        // Through a symlink too, as `[ -d ]` does.
        .filter(|path| path.is_dir())
        .collect();
    found.sort();
    Ok(found)
}

/// Clones `source` with `reader` into `folder`, the clone folder setting:
/// here with [`clone_here`] on `executor`, there with [`SCRIPT`], with no
/// deadline. Returns the clone's path.
///
/// # Errors
///
/// Says why, in git's or ssh's words, if the machine isn't connected or
/// the clone failed.
pub(crate) async fn clone(
    reader: Reader,
    source: Source,
    folder: String,
    executor: BackgroundExecutor,
) -> Result<PathBuf, String> {
    let name = source.folder().to_owned();
    let args = vec![
        OsString::from(&source.url),
        OsString::from(&folder),
        OsString::from(&name),
    ];
    let here = move || -> Result<PathBuf, String> {
        let no_home = || "There's no home directory to clone into.".to_owned();
        let home = platform::home_dir().ok_or_else(no_home)?;
        // Some whenever there is a home.
        let projects = platform::rho_home().ok_or_else(no_home)?.join("projects");
        clone_here(&source.url, &folder, &name, &home, &projects)
    };
    let parse = |output: Vec<u8>| {
        // The last field is the path, after whatever a login file printed.
        let mut fields = output.split(|&byte| byte == 0).filter(|f| !f.is_empty());
        fields
            .next_back()
            .map(platform::path_from_bytes)
            .ok_or_else(|| "the clone script printed no path".to_owned())
    };
    reader
        .query(&executor, here, SCRIPT, args, Deadline::Never, parse)
        .await
}

/// Clones `url` on this machine into `folder`, the clone folder setting,
/// as `name`, by [`SCRIPT`]'s rules, with `home` as the home directory and
/// `projects` as the folder an empty setting names, from `home` when
/// relative. Returns the clone's path. Blocks for as long as the clone
/// takes; call it off the UI thread.
///
/// # Errors
///
/// Says why, in git's words where it is git's, if the folder can't be
/// made, holds something else, or the clone failed.
fn clone_here(
    url: &str,
    folder: &str,
    name: &str,
    home: &Path,
    projects: &Path,
) -> Result<PathBuf, String> {
    let folder = folder_here(folder, home, projects);
    std::fs::create_dir_all(&folder)
        .map_err(|error| format!("Couldn't make {}: {error}", folder.display()))?;
    let folder = platform::canonicalize(&folder)
        .map_err(|error| format!("Couldn't open {}: {error}", folder.display()))?;
    let dest = folder.join(name);
    if !dest.exists() {
        let mut command = local::git(home, &["clone", "--quiet", "--", url]);
        command.arg(&dest).env("GIT_TERMINAL_PROMPT", "0");
        if let Some(ssh) = batch_ssh(home) {
            command.env("GIT_SSH_COMMAND", ssh);
        }
        local::stdout(&mut command)?;
        return Ok(dest);
    }
    let origin = local::line(&mut local::git(
        &dest,
        &["config", "--get", "remote.origin.url"],
    ))
    .unwrap_or_default();
    if origin != url.as_bytes() {
        return Err(format!(
            "{} is already there, and isn't a clone of {url}",
            dest.display()
        ));
    }
    if !local::succeeds(&mut local::git(
        &dest,
        &["rev-parse", "-q", "--verify", "HEAD"],
    )) {
        return Err(format!(
            "{} holds a clone of {url} with no commit yet: a clone may still be running into \
             it, or one was cut short; if so, remove it to clone again",
            dest.display()
        ));
    }
    Ok(dest)
}

/// The `GIT_SSH_COMMAND` a clone here runs with, so that ssh fails rather
/// than ask for a password or about a host key: the user's own with `-o
/// BatchMode=yes` added, else `ssh -o BatchMode=yes`. `None` when the user
/// chose their ssh with `GIT_SSH` or git's `core.sshCommand`, as read in
/// `dir`, which setting `GIT_SSH_COMMAND` would override: on Windows that
/// is often Windows' own OpenSSH, or `plink`, which takes no `-o`.
/// Their ssh is left as it is; git still can't prompt, with
/// `GIT_TERMINAL_PROMPT=0` and an empty stdin.
fn batch_ssh(dir: &Path) -> Option<OsString> {
    let set = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(mut ssh) = set("GIT_SSH_COMMAND") {
        ssh.push(" -o BatchMode=yes");
        return Some(ssh);
    }
    let chosen = set("GIT_SSH").is_some()
        || local::line(&mut local::git(
            dir,
            &["config", "--get", "core.sshCommand"],
        ))
        .is_ok_and(|ssh| !ssh.is_empty());
    (!chosen).then(|| OsString::from("ssh -o BatchMode=yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_git_urls_are_typed_repositories() {
        let typed = |text| Source::typed(text).map(|source| source.name);
        assert_eq!(
            typed("git@github.com:matthewyjiang/rho.git").as_deref(),
            Some("matthewyjiang/rho")
        );
        assert_eq!(
            typed("https://github.com/matthewyjiang/rho-ui").as_deref(),
            Some("matthewyjiang/rho-ui")
        );
        assert_eq!(typed("ssh://git@host/a/b.git").as_deref(), Some("a/b"));
        for search in [
            "rho",
            "~/code/rho",
            "/srv/rho",
            "host:path",
            "git@host:",
            "a b@c:d",
        ] {
            assert_eq!(typed(search), None, "{search:?} is a search");
        }
    }

    /// Cloning goes into the folder it is given, from home when relative,
    /// the rho home's `projects` when empty, and takes a folder that
    /// already holds a clone of the URL as it is, but not one that holds
    /// something else, nor a clone cut short. `clone` clones a URL into a
    /// folder setting, as a name, from a home directory whose `.rho` is the
    /// rho home, and returns the clone's path if it succeeds.
    fn clones_once_and_refuses_strangers(
        clone: impl Fn(&Path, &str, &str, &str) -> Option<PathBuf>,
    ) {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let git = |args: &[&str]| local::test::git(home, args);
        git(&["init", "-q", "-b", "main", "origin"]);
        git(&["-C", "origin", "commit", "-q", "--allow-empty", "-m", "x"]);
        // git clones a path as it does a URL, and records it as given.
        let url = home.join("origin").display().to_string();
        let run = |folder: &str, name: &str| clone(home, url.as_str(), folder, name);
        // Resolved, as through macOS's /var symlink.
        let resolved = |folder: &str| platform::canonicalize(&home.join(folder)).unwrap();
        let first = run("code", "thing");
        let cloned = resolved("code").join("thing");
        assert_eq!(first, Some(cloned.clone()), "from home");
        assert!(cloned.join(".git").is_dir());
        assert_eq!(run("~/code", "thing"), Some(cloned), "already cloned");
        let first = run("", "thing");
        assert_eq!(
            first,
            Some(resolved(".rho/projects").join("thing")),
            "an empty setting"
        );
        std::fs::create_dir_all(home.join("code/other")).unwrap();
        assert_eq!(run("code", "other"), None, "holds something else");
        // What a clone killed after git wrote its config leaves: the origin
        // set, no commit.
        git(&["init", "-q", "code/partial"]);
        git(&[
            "-C",
            "code/partial",
            "remote",
            "add",
            "origin",
            url.as_str(),
        ]);
        assert_eq!(run("code", "partial"), None, "an incomplete clone");
    }

    /// Finding the projects in a clone folder lists its folders, through a
    /// symlink too, but not hidden ones or files, from the folder resolved
    /// as a clone's path is, and none when the folder isn't there; the
    /// script a remote machine runs lists the same.
    #[test]
    fn finding_projects_lists_the_clone_folders_folders() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let projects = home.join(".rho/projects");
        for dir in ["b", "a", ".hidden"] {
            std::fs::create_dir_all(projects.join(dir)).unwrap();
        }
        std::fs::write(projects.join("file"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(projects.join("a"), projects.join("c")).unwrap();
        let real = platform::canonicalize(&projects).unwrap();
        let mut expected = vec![real.join("a"), real.join("b")];
        if cfg!(unix) {
            expected.push(real.join("c"));
        }
        let here = |folder: &str| find_here(&folder_here(folder, home, &projects));
        assert_eq!(here(""), Ok(expected.clone()));
        assert_eq!(here("~/.rho/projects"), Ok(expected.clone()));
        assert_eq!(here("missing"), Ok(Vec::new()));
        #[cfg(unix)]
        {
            let there = |folder: &str| {
                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(FIND_SCRIPT)
                    .arg("sh")
                    .arg(folder)
                    .env("HOME", home)
                    .env_remove("RHO_HOME")
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{output:?}");
                let fields = repo::nul_fields(folders::after(&output.stdout, FOUND).unwrap());
                let mut found: Vec<PathBuf> = fields
                    .unwrap()
                    .into_iter()
                    .map(platform::path_from_bytes)
                    .collect();
                found.sort();
                found
            };
            assert_eq!(there(""), expected);
            assert_eq!(there("~/.rho/projects"), expected);
            assert_eq!(there("missing"), Vec::<PathBuf>::new());
        }
    }

    #[test]
    fn cloning_here_clones_once_and_refuses_strangers() {
        clones_once_and_refuses_strangers(|home, url, folder, name| {
            clone_here(url, folder, name, home, &home.join(".rho/projects")).ok()
        });
    }

    /// [`SCRIPT`], which clones on a remote machine, follows the same
    /// rules.
    #[cfg(unix)]
    #[test]
    fn the_script_clones_once_and_refuses_strangers() {
        clones_once_and_refuses_strangers(|home, url, folder, name| {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(SCRIPT)
                .arg("sh")
                .args([url, folder, name])
                .env("HOME", home)
                .env_remove("RHO_HOME")
                .output()
                .unwrap();
            output.status.success().then(|| {
                let path = output.stdout.split(|&b| b == 0).rfind(|f| !f.is_empty());
                platform::path_from_bytes(path.unwrap())
            })
        });
    }
}
