//! Cloning a project's repository onto a machine, into that machine's clone
//! folder (see [`crate::machines::Machine::clone_folder`]). The project
//! list offers it for a project another machine holds, and for a git URL
//! typed in its search. [`clone_here`] clones on this machine, and
//! [`SCRIPT`] on a remote one, over the ssh connection to its daemon; both
//! find the folder by the same rules.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use rho_ui_core::platform;

use gpui_kit::BackgroundExecutor;

use crate::{
    folders::{Deadline, Reader},
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

/// The POSIX `sh` script that clones a repository on a remote machine,
/// given its URL, the clone folder setting, and the folder name as its
/// arguments. It runs from the home directory, so a relative folder is
/// from there; an empty one is `$RHO_HOME/projects`, the rho home found as
/// rho-ui's ssh setup finds it. A folder that already holds a clone of the
/// URL with a commit is taken as it is, so a clone that finished before
/// rho-ui quit can be picked again. One with none yet is refused: a clone
/// may still be running into it, from another draft, or one was cut short,
/// which removing the folder lets the next pick redo. git never prompts:
/// with no terminal, a password or host key question fails with git's
/// message, and ssh runs with `-o BatchMode=yes` (a remote machine's ssh
/// is OpenSSH). It prints a NUL, then the clone's resolved path and a NUL,
/// so whatever a login file prints comes before them.
pub(crate) const SCRIPT: &str = r#"url=$1 folder=$2 name=$3
cd || exit
case $folder in
  "") folder=${RHO_HOME:-$HOME/.rho}/projects ;;
  "~") folder=$HOME ;;
  "~/"*) folder=$HOME/${folder#??} ;;
esac
mkdir -p -- "$folder" && folder=$(cd -P -- "$folder" && pwd -P) || exit
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
"#;

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
    // Relative to `home`, as the script runs from there.
    let folder = match folder {
        "" => home.join(projects),
        _ => home.join(platform::expand_tilde(Path::new(folder), home)),
    };
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
