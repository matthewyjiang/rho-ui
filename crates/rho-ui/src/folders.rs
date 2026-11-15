//! What a path typed in the project search names on one machine: the
//! project of the folder typed, if it is there, and the folders the path
//! can go on to, so the search browses folders on any machine. This
//! machine's are read from its files; a remote machine's with
//! [`REMOTE_SCRIPT`], over the ssh connection to its daemon. Also
//! [`Reader`], which every feature that reads a machine goes through, and
//! the file read that the Files surface and pictures share.

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::BackgroundExecutor;
use rho_ui_core::{platform, DaemonError, SshLink};

use crate::repo;

/// How long a remote lookup waits for the next key before it runs. Kit
/// drops a search's task when the next one starts, so a burst of keys runs
/// one ssh script, not one a key: each takes two ssh sessions on the
/// shared master, past sshd's default `MaxSessions` of 10 if a dozen ran
/// at once. 150 ms is longer than the gap between keys at 80 words a
/// minute (about 400 keys a minute, 150 ms apart) and short enough to read
/// as instant.
const REMOTE_PAUSE: Duration = Duration::from_millis(150);

/// A typed path, split where the user is typing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Typed {
    /// The folder the path names.
    pub(crate) path: PathBuf,
    /// The folder whose subfolders it can go on to: the path itself after
    /// a trailing `/`, else its parent.
    pub(crate) browse: PathBuf,
    /// What follows the last `/`, which those subfolders' names start with.
    pub(crate) prefix: String,
}

impl Typed {
    /// The path `text` names, if it is one: absolute, `~`, or under `~/`,
    /// with `home` as the machine's home directory. Whether it exists is
    /// for [`look`] to find out.
    pub(crate) fn parse(text: &str, home: Option<&Path>) -> Option<Self> {
        let path = if text == "~" {
            home?.to_path_buf()
        } else if let Some(rest) = text.strip_prefix("~/") {
            home?.join(rest)
        } else {
            Some(Path::new(text))
                .filter(|path| path.is_absolute())?
                .to_path_buf()
        };
        let at_folder = text.ends_with('/') || text == "~" || path.parent().is_none();
        if at_folder {
            return Some(Self {
                browse: path.clone(),
                path,
                prefix: String::new(),
            });
        }
        let browse = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
        let prefix = path
            .file_name()
            .map_or_default(|name| name.to_string_lossy().into_owned());
        Some(Self {
            path,
            browse,
            prefix,
        })
    }

    /// Whether the path can go on to `folder`: it is in the browse folder,
    /// and its name starts with the prefix, ignoring case. A hidden folder
    /// only when the prefix starts with `.`.
    pub(crate) fn leads_to(&self, folder: &Path) -> bool {
        folder.parent() == Some(&self.browse)
            && folder.file_name().is_some_and(|name| self.matches(name))
    }

    fn matches(&self, name: &OsStr) -> bool {
        let (name, prefix) = (
            name.to_string_lossy().to_lowercase(),
            self.prefix.to_lowercase(),
        );
        (!name.starts_with('.') || prefix.starts_with('.')) && name.starts_with(&prefix)
    }
}

/// The project of a typed folder that is there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    /// Its folder (see [`repo::Repo::root`]), resolved.
    pub(crate) root: PathBuf,
    /// The folder as typed, when that spelling differs, as through a
    /// symlink: the project list may hold either.
    pub(crate) alias: Option<PathBuf>,
}

impl Found {
    /// The project at `root` of a folder typed as `typed`, which resolves
    /// to `real`. Typed at the project's own folder, the alias is its
    /// spelling as typed; typed below it, git gives no other.
    fn new(root: PathBuf, real: &Path, typed: &Path) -> Self {
        let alias =
            (real == root.as_path() && typed != root.as_path()).then(|| typed.to_path_buf());
        Self { root, alias }
    }

    /// Each spelling of its folder.
    pub(crate) fn spellings(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.root.as_path()).chain(self.alias.as_deref())
    }
}

/// What a [`Typed`] path names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lookup {
    /// The project of the folder typed, if it is there.
    pub(crate) found: Option<Found>,
    /// The subfolders the path can go on to (see [`Typed::leads_to`]),
    /// sorted by name.
    pub(crate) next: Vec<PathBuf>,
}

/// Where rho-ui reads a machine's files and runs `git`. A feature that
/// reads a machine has two ways to, which [`Self::query`] picks between:
/// here, with the files themselves and `git` run as a program (see
/// [`crate::local`]); on a remote machine, which is Unix, with a POSIX `sh`
/// script over its ssh link, whose output it parses. Both give the feature
/// the same value.
pub(crate) enum Reader {
    /// From this machine's files.
    Here,
    /// Over the ssh connection to a remote machine's daemon.
    There(SshLink),
    /// Not at all: the remote machine isn't connected.
    Unread,
}

/// How long a remote machine's script in [`Reader::query`] may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Deadline {
    /// [`SshLink::run_script`]'s, for a script that reads.
    Script,
    /// None, for a script whose time the network and the work set, such as
    /// a `git clone` (see [`SshLink::run_long_script`]).
    Never,
}

/// Why a [`Reader::Unread`] machine can't be read.
const UNREAD: &str = "The machine isn't connected.";

impl Reader {
    /// Finds something out on the machine: here by calling `here` on
    /// `executor`; there by running the POSIX `sh` script `script` with
    /// `args` as its positional parameters, within `deadline`, and reading
    /// what it printed with `parse` on `executor`.
    ///
    /// # Errors
    ///
    /// Says why, in ssh's words or those of `here` or `parse`, if the
    /// machine isn't connected, the script fails, or either of those does.
    pub(crate) async fn query<T: Send + 'static>(
        self,
        executor: &BackgroundExecutor,
        here: impl FnOnce() -> Result<T, String> + Send + 'static,
        script: &'static str,
        args: Vec<OsString>,
        deadline: Deadline,
        parse: impl FnOnce(Vec<u8>) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        match self {
            Reader::Here => executor.spawn(async move { here() }).await,
            Reader::There(ssh) => {
                let output = match deadline {
                    Deadline::Script => ssh.run_script(script, &args).await,
                    Deadline::Never => ssh.run_long_script(script, &args).await,
                }
                .map_err(|error| error.to_string())?;
                executor.spawn(async move { parse(output) }).await
            }
            Reader::Unread => Err(UNREAD.to_owned()),
        }
    }
}

/// The POSIX `sh` script that prints the file at the path given as its
/// second argument, from the folder given as its first: [`OPENED`], then
/// as many of its bytes as the third argument says. [`GONE`] if it isn't a
/// file. It fails if the folder isn't one. Read with [`parse_read`];
/// [`read_here`] does the same here.
pub(crate) const READ_SCRIPT: &str = r#"unset CDPATH
cd -- "$1" || exit
if [ -f "$2" ]; then
  printf 'rho-ui-file\0'
  head -c "$3" -- "$2"
else
  printf 'rho-ui-file-gone\0'
fi
"#;

/// Printed before a file's bytes, so whatever a login file prints comes
/// first and is skipped.
const OPENED: &[u8] = b"rho-ui-file\0";

/// Printed instead when the file isn't there.
const GONE: &[u8] = b"rho-ui-file-gone\0";

/// What [`Reader::read_file`] found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FileRead {
    /// The file's bytes, no more than the limit asked.
    Bytes(Vec<u8>),
    /// It is larger than the limit asked.
    TooLarge,
    /// It isn't there, or isn't a file.
    Gone,
}

impl FileRead {
    /// What a read that stopped after `limit + 1` bytes of a file found.
    fn within(bytes: Vec<u8>, limit: usize) -> Self {
        if bytes.len() > limit {
            Self::TooLarge
        } else {
            Self::Bytes(bytes)
        }
    }
}

impl Reader {
    /// Reads the file at `path`, from `folder` when relative, if it holds
    /// at most `limit` bytes: here with [`read_here`], there with
    /// [`READ_SCRIPT`].
    ///
    /// # Errors
    ///
    /// As [`Self::query`]: if the machine isn't connected, `folder` isn't
    /// a folder, the file can't be read, or the script printed something
    /// else.
    pub(crate) async fn read_file(
        self,
        folder: &Path,
        path: &str,
        limit: usize,
        executor: &BackgroundExecutor,
    ) -> Result<FileRead, String> {
        let args = vec![
            folder.as_os_str().to_owned(),
            OsString::from(path),
            (limit + 1).to_string().into(),
        ];
        let (folder, path) = (folder.to_path_buf(), path.to_owned());
        self.query(
            executor,
            move || read_here(&folder, &path, limit),
            READ_SCRIPT,
            args,
            Deadline::Script,
            move |output| parse_read(output, limit),
        )
        .await
    }
}

/// Reads the file at `path`, from `folder` when relative, on this machine,
/// as [`READ_SCRIPT`] does: no more than `limit + 1` of its bytes, or
/// [`FileRead::Gone`] if it isn't a file. Blocks; call it off the UI
/// thread.
///
/// # Errors
///
/// Says why if `folder` isn't a folder, or the file can't be read.
pub(crate) fn read_here(folder: &Path, path: &str, limit: usize) -> Result<FileRead, String> {
    use std::io::Read as _;

    if !folder.is_dir() {
        return Err(format!("{} isn't a folder.", folder.display()));
    }
    let path = folder.join(path);
    if !path.is_file() {
        return Ok(FileRead::Gone);
    }
    let couldnt = |error: std::io::Error| format!("Couldn't read {}: {error}", path.display());
    let file = std::fs::File::open(&path).map_err(couldnt)?;
    let mut bytes = Vec::new();
    let most = u64::try_from(limit).map_or(u64::MAX, |limit| limit.saturating_add(1));
    file.take(most).read_to_end(&mut bytes).map_err(couldnt)?;
    Ok(FileRead::within(bytes, limit))
}

/// The file [`READ_SCRIPT`] printed, told to stop after `limit + 1` bytes.
///
/// # Errors
///
/// If it printed something else.
pub(crate) fn parse_read(mut output: Vec<u8>, limit: usize) -> Result<FileRead, String> {
    if let Some(at) = find(&output, OPENED) {
        output.drain(..at + OPENED.len());
        return Ok(FileRead::within(output, limit));
    }
    match find(&output, GONE) {
        Some(_) => Ok(FileRead::Gone),
        None => Err("The file script printed something else.".to_owned()),
    }
}

/// Where `marker` first shows in `output`.
pub(crate) fn find(output: &[u8], marker: &[u8]) -> Option<usize> {
    output
        .windows(marker.len())
        .position(|window| window == marker)
}

/// What `output` holds after the first `marker`, which a script prints
/// before its fields so that whatever a login file prints is skipped.
pub(crate) fn after<'a>(output: &'a [u8], marker: &[u8]) -> Option<&'a [u8]> {
    let at = find(output, marker)?;
    output.get(at + marker.len()..)
}

/// Looks `typed` up with `reader`, doing file I/O on `executor`.
///
/// # Errors
///
/// Fails if the machine isn't connected, or its script can't run there or
/// prints something else.
pub(crate) async fn look(
    reader: Reader,
    typed: Typed,
    executor: BackgroundExecutor,
) -> Result<Lookup, DaemonError> {
    match reader {
        Reader::Here => Ok(executor.spawn(async move { look_here(&typed) }).await),
        Reader::There(ssh) => {
            executor.timer(REMOTE_PAUSE).await;
            look_there(&ssh, &typed).await
        }
        Reader::Unread => Err(DaemonError::Unreachable("it isn't connected".into())),
    }
}

/// Looks `typed` up on this machine. Does file I/O; call it off the UI
/// thread.
fn look_here(typed: &Typed) -> Lookup {
    let found = match platform::canonicalize(&typed.path) {
        Ok(dir) if dir.is_dir() => Some(Found::new(repo::inspect(&dir).root, &dir, &typed.path)),
        _ => None,
    };
    let names = std::fs::read_dir(&typed.browse)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name());
    Lookup {
        found,
        next: next(typed, names),
    }
}

/// Looks `typed` up on a remote machine, with [`REMOTE_SCRIPT`] over `ssh`.
///
/// # Errors
///
/// Fails if the script can't run there, or prints something else.
async fn look_there(ssh: &SshLink, typed: &Typed) -> Result<Lookup, DaemonError> {
    let output = ssh
        .run_script(REMOTE_SCRIPT, &[&typed.path, &typed.browse])
        .await?;
    parse_remote(typed, &output)
        .ok_or_else(|| DaemonError::Unreachable("the folder script printed something else".into()))
}

/// The POSIX `sh` script that looks a typed path up on a remote machine,
/// given the path and the folder to browse as its arguments. After
/// [`LOOKED`], it prints four fields, each ended by a NUL: `y` if the path
/// is a folder, else `n`; its resolved path; and as `git` says, its work
/// tree root and common git dir, empty outside a repository. Then it
/// prints the name of each subfolder of the second argument, each ended by
/// a NUL. Read with [`parse_remote`].
pub(crate) const REMOTE_SCRIPT: &str = r#"unset CDPATH
printf 'rho-ui-folder\0'
if [ -d "$1" ] && real=$(cd "$1" && pwd -P); then
  if top=$(git -C "$1" rev-parse --show-toplevel 2>/dev/null); then
    common=$(cd "$1" && cd "$(git rev-parse --git-common-dir)" && pwd -P)
  else
    top= common=
  fi
  printf 'y\0%s\0%s\0%s\0' "$real" "$top" "$common"
else
  printf 'n\0\0\0\0'
fi
for sub in "$2"/* "$2"/.*; do
  [ -d "$sub" ] || continue
  name=${sub##*/}
  case $name in .|..) continue ;; esac
  printf '%s\0' "$name"
done
"#;

/// Printed before [`REMOTE_SCRIPT`]'s fields, so whatever a login file
/// prints comes first and is skipped.
const LOOKED: &[u8] = b"rho-ui-folder\0";

/// What [`REMOTE_SCRIPT`] printed for `typed`. `None` if it printed
/// something else.
fn parse_remote(typed: &Typed, output: &[u8]) -> Option<Lookup> {
    let fields = repo::nul_fields(after(output, LOOKED)?)?;
    let ([is_dir, real, top, common], names) = fields.split_first_chunk::<4>()?;
    let path = platform::path_from_bytes;
    let found = match *is_dir {
        b"y" => {
            let real = path(real);
            let root = if top.is_empty() || common.is_empty() {
                real.clone()
            } else {
                repo::project_root(path(top), &path(common))
            };
            Some(Found::new(root, &real, &typed.path))
        }
        b"n" => None,
        _ => return None,
    };
    let names = names
        .iter()
        .map(|name| platform::path_from_bytes(name).into_os_string());
    Some(Lookup {
        found,
        next: next(typed, names),
    })
}

/// The subfolders of `typed`'s browse folder among `names` that it can go
/// on to, sorted by name, ignoring case.
fn next(typed: &Typed, names: impl Iterator<Item = OsString>) -> Vec<PathBuf> {
    let mut names: Vec<(String, OsString)> = names
        .filter(|name| typed.matches(name))
        .map(|name| (name.to_string_lossy().to_lowercase(), name))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|(_, name)| typed.browse.join(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(
        windows,
        ignore = "a remote machine's POSIX path doesn't parse on a Windows host: \
                  `Typed` uses this machine's path rules"
    )]
    fn a_typed_path_browses_its_folder_after_a_slash_else_its_parent() {
        let home = Some(Path::new("/home/me"));
        let parse = |text| Typed::parse(text, home).map(|t| (t.path, t.browse, t.prefix));
        let typed = |path: &str, browse: &str, prefix: &str| {
            Some((path.into(), browse.into(), prefix.to_owned()))
        };
        assert_eq!(parse("~/co"), typed("/home/me/co", "/home/me", "co"));
        assert_eq!(
            parse("~/code/"),
            typed("/home/me/code/", "/home/me/code", "")
        );
        assert_eq!(parse("~"), typed("/home/me", "/home/me", ""), "~ is home");
        assert_eq!(parse("/srv/x"), typed("/srv/x", "/srv", "x"));
        assert_eq!(parse("/"), typed("/", "/", ""));
        assert_eq!(parse("rho-ui"), None, "a name is a search");
        assert_eq!(Typed::parse("~/code", None), None, "no home, no ~");
    }

    /// [`REMOTE_SCRIPT`], run with this machine's `sh` and `git`, names
    /// what reading the files here does: a subfolder of a checkout, a
    /// linked worktree, a plain folder, a symlink to one and to a checkout,
    /// a prefix, a hidden folder, and a missing path.
    // The script runs on remote machines, which are Unix.
    #[cfg(unix)]
    #[test]
    fn the_remote_script_agrees_with_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let dir = platform::canonicalize(dir.path()).unwrap();
        let git = |args: &[&str]| crate::local::test::git(&dir, args);
        git(&["init", "-q", "-b", "main", "project"]);
        git(&["-C", "project", "commit", "-q", "--allow-empty", "-m", "x"]);
        git(&[
            "-C", "project", "worktree", "add", "-q", "-b", "feat", "../wt",
        ]);
        let (main, plain) = (dir.join("project"), dir.join("Plain"));
        std::fs::create_dir_all(main.join("docs")).unwrap();
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join("project-notes"), "a file").unwrap();
        std::os::unix::fs::symlink(&plain, dir.join("link")).unwrap();
        std::os::unix::fs::symlink(&main, dir.join("plink")).unwrap();
        let at = |rest: &str| Typed::parse(&format!("{}/{rest}", dir.display()), None).unwrap();
        let cases = [
            at("project/docs"),
            at("wt"),
            at("Plain/"),
            at("link/"),
            at("plink"),
            at("p"),
            at(".h"),
            at("gone"),
        ];
        let looked: Vec<Lookup> = cases.iter().map(look_here).collect();
        for (typed, local) in cases.iter().zip(&looked) {
            let output = crate::local::test::sh(REMOTE_SCRIPT, &[&typed.path, &typed.browse])
                .expect("the script runs");
            // As a login file that prints would have it.
            let output = [b"Welcome!\n".as_slice(), output.as_slice()].concat();
            let remote = parse_remote(typed, &output).expect("the script's fields");
            assert_eq!(&remote, local, "{typed:?}");
        }
        // What the cases name, so agreeing on nothing can't pass.
        let found = |root: &Path, alias: Option<&str>| {
            Some(Found {
                root: root.to_path_buf(),
                alias: alias.map(|alias| dir.join(alias)),
            })
        };
        assert_eq!(looked[0].found, found(&main, None));
        assert_eq!(
            looked[1].found,
            found(&main, None),
            "a worktree is its repo's"
        );
        assert_eq!(looked[2].found, found(&plain, None));
        assert_eq!(looked[3].found, found(&plain, Some("link")));
        assert_eq!(looked[4].found, found(&main, Some("plink")));
        assert_eq!(looked[5].found, None, "no folder p");
        assert_eq!(looked[5].next, [plain, dir.join("plink"), main]);
        assert_eq!(looked[6].next, [dir.join(".hidden")], "hidden after a dot");
        assert!(looked[7].found.is_none() && looked[7].next.is_empty());
    }
}
