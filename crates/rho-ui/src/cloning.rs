//! Cloning a project's repository onto a machine, into that machine's clone
//! folder (see [`crate::machines::Machine::clone_folder`]). The project
//! list offers it for a project another machine holds, and for a git URL
//! typed in its search. The same [`SCRIPT`] runs on this machine and, over
//! the ssh connection to its daemon, on a remote one, so both find the
//! folder by the same rules.

use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt as _,
    path::PathBuf,
    process::{Command, Stdio},
};

use gpui_kit::BackgroundExecutor;

use crate::{folders::Reader, repo};

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
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }
}

/// The POSIX `sh` script that clones a repository, given its URL, the
/// clone folder setting, and the folder name as its arguments. It runs
/// from the home directory, so a relative folder is from there; an empty
/// one is `$RHO_HOME/projects`, the rho home found as rho-ui's ssh setup
/// finds it. A folder that already holds a clone of the URL is taken as
/// it is, so a clone cut short by rho-ui quitting can be picked again.
/// git never prompts: with no terminal, a password or host key question
/// fails with git's message. It prints a NUL, then the clone's resolved
/// path and a NUL, so whatever a login file prints comes before them.
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
else
  git clone --quiet -- "$url" "$dest" </dev/null || exit
fi
printf '\0%s\0' "$dest"
"#;

/// Clones `source` with `reader` into `folder`, the clone folder setting,
/// running a process on `executor` on this machine. Returns the clone's
/// path.
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
    let args = [source.url.clone(), folder, source.folder().to_owned()];
    let output = match reader {
        Reader::Here => executor.spawn(async move { run_here(&args) }).await?,
        Reader::There(ssh) => ssh
            .run_long_script(SCRIPT, &args)
            .await
            .map_err(|error| error.to_string())?,
        Reader::Unread => return Err("it isn't connected".to_owned()),
    };
    // The last field is the path, after whatever a login file printed.
    let mut fields = output.split(|&byte| byte == 0).filter(|f| !f.is_empty());
    fields
        .next_back()
        .map(|path| PathBuf::from(OsString::from_vec(path.to_vec())))
        .ok_or_else(|| "the clone script printed no path".to_owned())
}

/// Runs [`SCRIPT`] on this machine with `args`. Blocks until git is done;
/// call it off the UI thread.
fn run_here(args: &[String]) -> Result<Vec<u8>, String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(SCRIPT)
        .arg("sh")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("couldn't run sh: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(match stderr.trim() {
            "" => format!("the clone ended with {}", output.status),
            why => why.to_owned(),
        })
    }
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

    /// The script clones into the folder it is given, from home when
    /// relative, and takes a folder that already holds a clone of the URL
    /// as it is, but not one that holds something else.
    #[test]
    fn the_script_clones_once_and_refuses_strangers() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main", "origin"]);
        git(&["-C", "origin", "commit", "-q", "--allow-empty", "-m", "x"]);
        let url = format!("file://{}/origin", dir.display());
        let run = |folder: &str, name: &str| {
            let output = Command::new("sh")
                .arg("-c")
                .arg(SCRIPT)
                .arg("sh")
                .args([&url, folder, name])
                .env("HOME", &dir)
                .output()
                .unwrap();
            output.status.success().then(|| {
                let path = output.stdout.split(|&b| b == 0).rfind(|f| !f.is_empty());
                PathBuf::from(OsString::from_vec(path.unwrap().to_vec()))
            })
        };
        let cloned = dir.join("code/thing");
        assert_eq!(run("code", "thing"), Some(cloned.clone()), "from home");
        assert!(cloned.join(".git").is_dir());
        assert_eq!(run("~/code", "thing"), Some(cloned), "already cloned");
        std::fs::create_dir_all(dir.join("code/other")).unwrap();
        assert_eq!(run("code", "other"), None, "holds something else");
    }
}
