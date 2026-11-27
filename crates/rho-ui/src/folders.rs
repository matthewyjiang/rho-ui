//! A machine's folders, as the project search and the folder chooser read
//! them: what a path typed in the search names ([`look`]: the project of
//! the folder typed, if it is there, and the folders the path can go on
//! to), a folder's subfolders with their git checkouts ([`list`]), and the
//! project a chosen folder belongs to ([`resolve`]). A path follows its
//! machine's rules ([`PathStyle`]): this machine's, Windows' on Windows,
//! and a remote machine's POSIX ones whatever this machine is. This
//! machine's folders are read from its files; a remote machine's with
//! [`REMOTE_SCRIPT`], one run per folder, over the ssh connection to its
//! daemon. Also [`Reader`], which every feature that reads a machine goes
//! through, and the file read that the Files surface and pictures share.

use std::{
    ffi::{OsStr, OsString},
    path::{Component, Path, PathBuf},
    time::Duration,
};

use gpui_kit::BackgroundExecutor;
use rho_ui_core::{platform, DaemonError, Link};

use crate::{machines::MachineId, repo};

/// How long a remote lookup waits for the next key before it runs. Kit
/// drops a search's task when the next one starts, so a burst of keys runs
/// one ssh script, not one a key: each takes one of the few sessions the
/// shared connection runs scripts in at once, which everything else read
/// on the machine waits for too. 150 ms is longer than the gap between
/// keys at 80 words a minute (about 400 keys a minute, 150 ms apart) and
/// short enough to read as instant.
pub(crate) const REMOTE_PAUSE: Duration = Duration::from_millis(150);

/// The rules a machine's paths follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathStyle {
    /// This machine's, as `std::path` has them: on Windows, `\` or `/`
    /// between names, and absolute from a drive's root.
    Native,
    /// A remote machine's, which is Unix, whatever this machine is: `/`
    /// between names, and absolute from `/`.
    Posix,
}

impl PathStyle {
    /// The rules `machine`'s paths follow: this platform's where rho-ui
    /// reads them itself, else POSIX, as a machine reached through a link
    /// is Unix. In a browser that is this machine too, whose paths std
    /// can't read there (it finds no path absolute).
    pub(crate) fn of(machine: &MachineId) -> Self {
        if crate::workspaces::reads_here(machine) {
            Self::Native
        } else {
            Self::Posix
        }
    }

    fn is_separator(self, c: char) -> bool {
        match self {
            Self::Native => std::path::is_separator(c),
            Self::Posix => c == '/',
        }
    }

    fn is_absolute(self, text: &str) -> bool {
        match self {
            Self::Native => Path::new(text).is_absolute(),
            Self::Posix => text.starts_with('/'),
        }
    }

    /// `name`, which may hold separators, in `folder`.
    pub(crate) fn join(self, folder: &Path, name: &OsStr) -> PathBuf {
        match self {
            Self::Native => folder.join(name),
            Self::Posix => {
                let mut bytes = platform::path_to_bytes(folder.as_os_str());
                if !bytes.ends_with(b"/") {
                    bytes.push(b'/');
                }
                bytes.extend(platform::path_to_bytes(name));
                platform::path_from_bytes(&bytes)
            }
        }
    }

    /// `path` without `.` names, or separators repeated or at its end; on
    /// Windows, this machine's with `\` between names.
    fn tidy(self, path: &Path) -> PathBuf {
        match self {
            Self::Native => path.components().collect(),
            Self::Posix => {
                let bytes = platform::path_to_bytes(path.as_os_str());
                let names: Vec<&[u8]> = bytes
                    .split(|&byte| byte == b'/')
                    .filter(|name| !matches!(*name, b"" | b"."))
                    .collect();
                let mut tidy = if bytes.starts_with(b"/") {
                    vec![b'/']
                } else {
                    Vec::new()
                };
                tidy.extend(names.join(&b'/'));
                platform::path_from_bytes(&tidy)
            }
        }
    }

    /// The folder `path` is in, and its last name; `None` at a root, or
    /// after a last `..`, which names a folder but not by its name (as
    /// `std::path`'s `file_name` has it), so the path is browsed itself.
    fn split(self, path: &Path) -> Option<(PathBuf, OsString)> {
        match self {
            Self::Native => Some((path.parent()?.to_path_buf(), path.file_name()?.to_owned())),
            Self::Posix => {
                let bytes = platform::path_to_bytes(path.as_os_str());
                let at = bytes.iter().rposition(|&byte| byte == b'/')?;
                let (folder, name) = bytes.split_at(at);
                let name = name.get(1..).filter(|name| !matches!(*name, b"" | b".."))?;
                let folder: &[u8] = if folder.is_empty() { b"/" } else { folder };
                Some((
                    platform::path_from_bytes(folder),
                    platform::path_from_bytes(name).into_os_string(),
                ))
            }
        }
    }

    /// The folder `path` is in; `None` at a root.
    pub(crate) fn parent(self, path: &Path) -> Option<PathBuf> {
        self.split(path).map(|(parent, _)| parent)
    }

    /// `text` split after its last separator, where a path bar takes what
    /// comes before as folders to go to and what follows as a name still
    /// being typed; `None` without a separator.
    pub(crate) fn split_typed(self, text: &str) -> Option<(&str, &str)> {
        let at = text.rfind(|c: char| self.is_separator(c))?;
        // A separator is one byte in either style.
        Some(text.split_at(at + 1))
    }

    /// The folder `text` names from `from`: itself when absolute, under
    /// `home` after `~` and a separator, else under `from`. `.` and `..`
    /// go by name, not through symlinks, as a file chooser's path bar takes
    /// them, and `..` stops at the root. `None` for `~` with no home.
    pub(crate) fn resolve(self, from: &Path, home: Option<&Path>, text: &str) -> Option<PathBuf> {
        let under_home = text
            .strip_prefix('~')
            .filter(|rest| rest.is_empty() || rest.starts_with(|c: char| self.is_separator(c)));
        match self {
            Self::Native => {
                let joined = match under_home {
                    Some(rest) => {
                        home?.join(rest.trim_start_matches(|c: char| self.is_separator(c)))
                    }
                    None => from.join(text),
                };
                let mut path = PathBuf::new();
                for part in joined.components() {
                    match part {
                        Component::ParentDir => {
                            path.pop();
                        }
                        Component::CurDir => {}
                        part => path.push(part),
                    }
                }
                Some(path)
            }
            Self::Posix => {
                let (start, rest) = match under_home {
                    Some(rest) => (home?.to_path_buf(), rest),
                    None if text.starts_with('/') => (PathBuf::from("/"), text),
                    None => (from.to_path_buf(), text),
                };
                Some(rest.split('/').fold(start, |at, name| match name {
                    "" | "." => at,
                    ".." => self.parent(&at).unwrap_or(at),
                    name => self.join(&at, OsStr::new(name)),
                }))
            }
        }
    }

    /// The folders from the top of `path` down to it, each with the name a
    /// path bar shows: `home` as `~`, with nothing above it, and a root as
    /// itself.
    pub(crate) fn crumbs(self, path: &Path, home: Option<&Path>) -> Vec<(String, PathBuf)> {
        let mut crumbs = Vec::new();
        let mut at = path.to_path_buf();
        loop {
            if home == Some(at.as_path()) {
                crumbs.push(("~".to_owned(), at));
                break;
            }
            let Some((parent, name)) = self.split(&at) else {
                crumbs.push((at.display().to_string(), at));
                break;
            };
            crumbs.push((name.to_string_lossy().into_owned(), at));
            at = parent;
        }
        crumbs.reverse();
        crumbs
    }
}

/// Whether a folder named `name` matches `prefix` as a path being typed
/// does: its name starts with it, ignoring case. A hidden folder only when
/// the prefix starts with `.`.
pub(crate) fn matches_prefix(name: &OsStr, prefix: &str) -> bool {
    let (name, prefix) = (name.to_string_lossy().to_lowercase(), prefix.to_lowercase());
    (!name.starts_with('.') || prefix.starts_with('.')) && name.starts_with(&prefix)
}

/// A typed path, split where the user is typing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Typed {
    /// The folder the path names.
    pub(crate) path: PathBuf,
    /// The folder whose subfolders it can go on to: the path itself after
    /// a trailing separator, else its parent.
    pub(crate) browse: PathBuf,
    /// What follows the last separator, which those subfolders' names
    /// start with.
    pub(crate) prefix: String,
    /// The rules of the machine it is on.
    style: PathStyle,
}

impl Typed {
    /// The path `text` names on a machine whose paths follow `style`, if
    /// it is one: absolute, `~`, or under `~` and a separator (`~/`, and
    /// on Windows here `~\` too), with `home` as the machine's home
    /// directory. Whether it exists is for [`look`] to find out.
    pub(crate) fn parse(text: &str, home: Option<&Path>, style: PathStyle) -> Option<Self> {
        let under_home = text
            .strip_prefix('~')
            .and_then(|rest| rest.strip_prefix(|c: char| style.is_separator(c)));
        let path = if text == "~" {
            home?.to_path_buf()
        } else if let Some(rest) = under_home {
            style.join(home?, OsStr::new(rest))
        } else if style.is_absolute(text) {
            PathBuf::from(text)
        } else {
            return None;
        };
        let path = style.tidy(&path);
        let at_folder = text == "~" || text.ends_with(|c: char| style.is_separator(c));
        let split = if at_folder { None } else { style.split(&path) };
        match split {
            Some((browse, name)) => Some(Self {
                path,
                browse,
                prefix: name.to_string_lossy().into_owned(),
                style,
            }),
            None => Some(Self {
                browse: path.clone(),
                path,
                prefix: String::new(),
                style,
            }),
        }
    }

    /// Whether the path can go on to `folder`: it is in the browse folder,
    /// and its name starts with the prefix, ignoring case. A hidden folder
    /// only when the prefix starts with `.`.
    pub(crate) fn leads_to(&self, folder: &Path) -> bool {
        self.style
            .split(folder)
            .is_some_and(|(parent, name)| parent == self.browse && self.matches(&name))
    }

    fn matches(&self, name: &OsStr) -> bool {
        matches_prefix(name, &self.prefix)
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
#[derive(Clone)]
pub(crate) enum Reader {
    /// From this machine's files.
    Here,
    /// Through a remote machine's link: ssh, or `rho-ui serve`.
    There(Link),
    /// Not at all: the remote machine isn't connected.
    Unread,
}

/// How long a remote machine's script in [`Reader::query`] may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Deadline {
    /// [`Link::run_script`]'s, for a script that reads.
    Script,
    /// None, for a script whose time the network and the work set, such as
    /// a `git clone` (see [`Link::run_long_script`]).
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

/// The path a script printed last, as a field ended by a NUL, after
/// whatever a login file printed.
pub(crate) fn last_path(output: &[u8]) -> Option<PathBuf> {
    output
        .split(|&byte| byte == 0)
        .rfind(|field| !field.is_empty())
        .map(platform::path_from_bytes)
}

/// What a folder holds, as [`list`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listing {
    /// The folder with its symlinks resolved, if that could be read.
    /// Projects are kept resolved, so this tells a subfolder that is one
    /// already.
    pub(crate) real: Option<PathBuf>,
    /// Its subfolders, sorted by name ignoring case.
    pub(crate) subfolders: Vec<Subfolder>,
}

/// A subfolder in a [`Listing`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Subfolder {
    pub(crate) name: OsString,
    /// Whether it is a git checkout's root, and what it has checked out.
    pub(crate) git: Option<GitRoot>,
}

/// A git checkout's root: a folder with a `.git` folder, or a `.git` file
/// that points to one, as a linked worktree or a submodule has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitRoot {
    /// What it has checked out, when its `HEAD` could be read.
    pub(crate) head: Option<repo::Head>,
}

/// Why a folder's subfolders couldn't be listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unlisted {
    Gone,
    NotFolder,
    Denied,
    /// Another reason, in the system's words. Only this machine gives one:
    /// a remote machine's script tells only the others.
    Other(String),
}

impl std::fmt::Display for Unlisted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Gone => "it isn't there",
            Self::NotFolder => "it isn't a folder",
            Self::Denied => "permission denied",
            Self::Other(why) => why,
        })
    }
}

/// What one run of [`REMOTE_SCRIPT`], or [`look_here`], found.
#[derive(Debug, PartialEq, Eq)]
struct Read {
    /// The first path asked, when it is a folder.
    folder: Option<Resolved>,
    /// The second, when asked.
    listed: Option<Result<Listing, Unlisted>>,
}

/// A folder that is there.
#[derive(Debug, PartialEq, Eq)]
struct Resolved {
    /// Its path, with symlinks resolved.
    real: PathBuf,
    /// Its project's folder (see [`repo::Repo::root`]), resolved.
    root: PathBuf,
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
    pause(&reader, &executor).await;
    let read = read(
        reader,
        Some(typed.path.clone()),
        Some(typed.browse.clone()),
        &executor,
    )
    .await?;
    Ok(Lookup::new(&typed, read))
}

impl Lookup {
    /// What `typed` names, from what was read of its path and browse
    /// folder. The search lists no subfolders it can't read.
    fn new(typed: &Typed, read: Read) -> Self {
        let subfolders = match read.listed {
            Some(Ok(listing)) => listing.subfolders,
            Some(Err(_)) | None => Vec::new(),
        };
        Self {
            found: read
                .folder
                .map(|folder| Found::new(folder.root, &folder.real, &typed.path)),
            next: subfolders
                .into_iter()
                .filter(|subfolder| typed.matches(&subfolder.name))
                .map(|subfolder| typed.style.join(&typed.browse, &subfolder.name))
                .collect(),
        }
    }
}

/// Lists `folder`'s subfolders with `reader`, or says why they can't be,
/// doing file I/O on `executor`. On a remote machine, one script lists
/// them all, git roots and branches too, after [`REMOTE_PAUSE`]: a folder
/// chooser drops the listing it no longer shows, so a burst of moves runs
/// one script.
///
/// # Errors
///
/// Fails if the machine isn't connected, or its script can't run there or
/// prints something else.
pub(crate) async fn list(
    reader: Reader,
    folder: PathBuf,
    executor: BackgroundExecutor,
) -> Result<Result<Listing, Unlisted>, DaemonError> {
    pause(&reader, &executor).await;
    // Nothing to resolve: what it is in git costs most of the script.
    let read = read(reader, None, Some(folder), &executor).await?;
    read.listed.ok_or_else(printed_something_else)
}

/// The project of `folder` with `reader`, as adding it finds it: its
/// repository's main checkout, or the folder, resolved. `None` if it isn't
/// a folder.
///
/// # Errors
///
/// Fails if the machine isn't connected, or its script can't run there or
/// prints something else.
pub(crate) async fn resolve(
    reader: Reader,
    folder: PathBuf,
    executor: BackgroundExecutor,
) -> Result<Option<Found>, DaemonError> {
    let read = read(reader, Some(folder.clone()), None, &executor).await?;
    Ok(read
        .folder
        .map(|found| Found::new(found.root, &found.real, &folder)))
}

/// Waits [`REMOTE_PAUSE`] before a remote machine's lookup, for the next
/// key or move, which drops this one.
async fn pause(reader: &Reader, executor: &BackgroundExecutor) {
    if matches!(reader, Reader::There(_)) {
        executor.timer(REMOTE_PAUSE).await;
    }
}

/// Resolves `path` and lists `browse`'s subfolders, each if asked, with
/// `reader`: here on `executor`, there with [`REMOTE_SCRIPT`].
async fn read(
    reader: Reader,
    path: Option<PathBuf>,
    browse: Option<PathBuf>,
    executor: &BackgroundExecutor,
) -> Result<Read, DaemonError> {
    match reader {
        Reader::Here => Ok(executor
            .spawn(async move { look_here(path.as_deref(), browse.as_deref()) })
            .await),
        Reader::There(link) => {
            // The script takes an empty argument as not asked.
            let arg =
                |path: Option<PathBuf>| path.map_or_else(OsString::new, PathBuf::into_os_string);
            let output = link
                .run_script(REMOTE_SCRIPT, &[arg(path), arg(browse)])
                .await?;
            parse_remote(&output).ok_or_else(printed_something_else)
        }
        Reader::Unread => Err(DaemonError::Unreachable("it isn't connected".into())),
    }
}

fn printed_something_else() -> DaemonError {
    DaemonError::Unreachable("the folder script printed something else".into())
}

/// Resolves `path` and lists `browse`'s subfolders, each if asked, on
/// this machine, as [`REMOTE_SCRIPT`] does on another. Does file I/O;
/// call it off the UI thread.
fn look_here(path: Option<&Path>, browse: Option<&Path>) -> Read {
    let folder = match path.map(platform::canonicalize) {
        Some(Ok(real)) if real.is_dir() => Some(Resolved {
            root: repo::inspect(&real).root,
            real,
        }),
        _ => None,
    };
    Read {
        folder,
        listed: browse.map(list_here),
    }
}

/// `folder`'s subfolders on this machine.
fn list_here(folder: &Path) -> Result<Listing, Unlisted> {
    use std::io::ErrorKind;

    match std::fs::metadata(folder) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Err(Unlisted::Gone),
        Ok(metadata) if !metadata.is_dir() => return Err(Unlisted::NotFolder),
        _ => {}
    }
    let entries = std::fs::read_dir(folder).map_err(|error| match error.kind() {
        ErrorKind::NotFound => Unlisted::Gone,
        ErrorKind::PermissionDenied => Unlisted::Denied,
        _ => Unlisted::Other(error.to_string()),
    })?;
    let subfolders = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            Some(Subfolder {
                git: git_root(&path),
                name: path.file_name()?.to_owned(),
            })
        });
    Ok(Listing {
        subfolders: sorted(subfolders),
        real: platform::canonicalize(folder).ok(),
    })
}

/// Whether `folder` is a git checkout's root, from its `.git` alone, as
/// [`REMOTE_SCRIPT`] tells it.
fn git_root(folder: &Path) -> Option<GitRoot> {
    let git_dir = repo::git_dir_at(folder)?;
    let head = repo::read_small(&git_dir.join("HEAD"));
    Some(GitRoot {
        head: head.as_deref().and_then(repo::parse_head),
    })
}

/// `subfolders`, sorted by name, ignoring case.
fn sorted(subfolders: impl Iterator<Item = Subfolder>) -> Vec<Subfolder> {
    let mut subfolders: Vec<(String, Subfolder)> = subfolders
        .map(|subfolder| (subfolder.name.to_string_lossy().to_lowercase(), subfolder))
        .collect();
    subfolders.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.name.cmp(&y.name)));
    subfolders
        .into_iter()
        .map(|(_, subfolder)| subfolder)
        .collect()
}

/// The POSIX `sh` script that looks a folder up on a remote machine, given
/// a path to resolve and a folder to list, each empty when not asked, as
/// its arguments. After [`LOOKED`], it prints four fields, each ended by a
/// NUL: `y` if the path is a folder, else `n`; its resolved path; and as
/// `git` says, its work tree root and common git dir, empty outside a
/// repository. Then, if asked to list a folder, it prints why it can't
/// (`gone`, `file` or `denied`), or `listed`, its resolved path (empty if
/// it can't be entered), and three fields for each subfolder: its name;
/// `y` if it has a `.git` folder, or a `.git` file that points to one,
/// else `n`; and the first line of that git dir's `HEAD`, empty if it
/// can't be read. Only `sh` builtins run per subfolder, so a folder of
/// hundreds lists in one quick script. Read with [`parse_remote`].
pub(crate) const REMOTE_SCRIPT: &str = r#"unset CDPATH
printf 'rho-ui-folder\0'
if [ -n "$1" ] && [ -d "$1" ] && real=$(cd "$1" && pwd -P); then
  if top=$(git -C "$1" rev-parse --show-toplevel 2>/dev/null); then
    common=$(cd "$1" && cd "$(git rev-parse --git-common-dir)" && pwd -P)
  else
    top= common=
  fi
  printf 'y\0%s\0%s\0%s\0' "$real" "$top" "$common"
else
  printf 'n\0\0\0\0'
fi
[ -n "$2" ] || exit 0
if [ ! -e "$2" ]; then
  printf 'gone\0'
elif [ ! -d "$2" ]; then
  printf 'file\0'
elif [ ! -r "$2" ]; then
  printf 'denied\0'
else
  here=$(cd "$2" && pwd -P) || here=
  printf 'listed\0%s\0' "$here"
  for sub in "$2"/* "$2"/.*; do
    [ -d "$sub" ] || continue
    name=${sub##*/}
    case $name in .|..) continue ;; esac
    gitdir=
    if [ -d "$sub/.git" ]; then
      gitdir=$sub/.git
    elif [ -f "$sub/.git" ]; then
      pointer=
      { IFS= read -r pointer < "$sub/.git"; } 2>/dev/null
      case $pointer in
        gitdir:*)
          gitdir=${pointer#gitdir:}
          gitdir=${gitdir#"${gitdir%%[! ]*}"}
          case $gitdir in /*) ;; *) gitdir=$sub/$gitdir ;; esac
          ;;
      esac
    fi
    if [ -n "$gitdir" ]; then
      head=
      [ -f "$gitdir/HEAD" ] && { IFS= read -r head < "$gitdir/HEAD"; } 2>/dev/null
      printf '%s\0y\0%s\0' "$name" "$head"
    else
      printf '%s\0n\0\0' "$name"
    fi
  done
fi
"#;

/// Printed before [`REMOTE_SCRIPT`]'s fields, so whatever a login file
/// prints comes first and is skipped.
const LOOKED: &[u8] = b"rho-ui-folder\0";

/// What [`REMOTE_SCRIPT`] printed. `None` if it printed something else.
fn parse_remote(output: &[u8]) -> Option<Read> {
    let fields = repo::nul_fields(after(output, LOOKED)?)?;
    let ([is_dir, real, top, common], rest) = fields.split_first_chunk::<4>()?;
    let path = platform::path_from_bytes;
    let folder = match *is_dir {
        b"y" => {
            let real = path(real);
            let root = if top.is_empty() || common.is_empty() {
                real.clone()
            } else {
                repo::project_root(path(top), &path(common))
            };
            Some(Resolved { real, root })
        }
        b"n" => None,
        _ => return None,
    };
    let listed = match rest.split_first() {
        None => None,
        Some((status, subfolders)) => Some(match *status {
            b"gone" => Err(Unlisted::Gone),
            b"file" => Err(Unlisted::NotFolder),
            b"denied" => Err(Unlisted::Denied),
            b"listed" => {
                let (here, subfolders) = subfolders.split_first()?;
                let (facts, []) = subfolders.as_chunks::<3>() else {
                    return None;
                };
                let subfolders = facts
                    .iter()
                    .map(|[name, git, head]| {
                        let git = match *git {
                            b"y" => Some(GitRoot {
                                head: repo::parse_head(&String::from_utf8_lossy(head)),
                            }),
                            b"n" => None,
                            _ => return None,
                        };
                        let name = platform::path_from_bytes(name).into_os_string();
                        Some(Subfolder { name, git })
                    })
                    .collect::<Option<Vec<_>>>()?;
                Ok(Listing {
                    real: (!here.is_empty()).then(|| platform::path_from_bytes(here)),
                    subfolders: sorted(subfolders.into_iter()),
                })
            }
            _ => return None,
        }),
    };
    Some(Read { folder, listed })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A typed path follows its machine's rules, a remote machine's POSIX
    /// ones on any host, and browses its folder after a separator, else
    /// its parent.
    #[test]
    fn a_typed_path_browses_its_folder_after_a_slash_else_its_parent() {
        type Case = (
            &'static str,
            Option<(&'static str, &'static str, &'static str)>,
        );
        let check = |cases: &[Case], home: &str, style| {
            for &(text, want) in cases {
                let got = Typed::parse(text, Some(Path::new(home)), style).map(|typed| {
                    let shown = |path: &Path| path.display().to_string();
                    (shown(&typed.path), shown(&typed.browse), typed.prefix)
                });
                let want = want.map(|(path, browse, prefix)| {
                    (path.to_owned(), browse.to_owned(), prefix.to_owned())
                });
                assert_eq!(got, want, "{text:?} as {style:?}");
            }
        };
        let remote: &[Case] = &[
            ("~/co", Some(("/home/me/co", "/home/me", "co"))),
            ("~/code/", Some(("/home/me/code", "/home/me/code", ""))),
            ("~", Some(("/home/me", "/home/me", ""))),
            ("/srv/x", Some(("/srv/x", "/srv", "x"))),
            ("/srv//x/./", Some(("/srv/x", "/srv/x", ""))),
            // A last `..` browses the folder it names, as `std::path` has it.
            ("/srv/app/..", Some(("/srv/app/..", "/srv/app/..", ""))),
            ("/", Some(("/", "/", ""))),
            ("rho-ui", None),
            (r"C:\code", None),
            (r"~\x", None),
        ];
        check(remote, "/home/me", PathStyle::Posix);
        #[cfg(windows)]
        let (home, here): (&str, &[Case]) = (
            r"C:\Users\me",
            &[
                (r"C:\code\", Some((r"C:\code", r"C:\code", ""))),
                ("C:/code", Some((r"C:\code", r"C:\", "code"))),
                (r"C:\code\..", Some((r"C:\code\..", r"C:\code\..", ""))),
                (r"~\x", Some((r"C:\Users\me\x", r"C:\Users\me", "x"))),
                ("~/x", Some((r"C:\Users\me\x", r"C:\Users\me", "x"))),
                ("/srv/x", None),
                ("rho-ui", None),
            ],
        );
        #[cfg(not(windows))]
        let (home, here): (&str, &[Case]) = (
            "/home/me",
            &[
                ("~/co", Some(("/home/me/co", "/home/me", "co"))),
                ("/srv/app/..", Some(("/srv/app/..", "/srv/app/..", ""))),
                ("/", Some(("/", "/", ""))),
                (r"C:\code", None),
                (r"~\x", None),
            ],
        );
        check(here, home, PathStyle::Native);
        assert_eq!(
            Typed::parse("~/code", None, PathStyle::Posix),
            None,
            "no home, no ~"
        );
    }

    /// A path bar takes typed folders from the folder shown, from `~` or
    /// from the root, by name, with `..` stopping at the root; and shows a
    /// folder's crumbs from `~` when under home, else from the root. A
    /// remote machine's POSIX rules hold on any host.
    #[test]
    fn a_path_bar_resolves_typed_folders_by_name() {
        type Case = (&'static str, Option<&'static str>);
        let check = |cases: &[Case], from: &str, home: &str, style: PathStyle| {
            for &(text, want) in cases {
                let got = style.resolve(Path::new(from), Some(Path::new(home)), text);
                assert_eq!(got, want.map(PathBuf::from), "{text:?} as {style:?}");
            }
        };
        let remote: &[Case] = &[
            ("src/", Some("/srv/app/src")),
            ("src//lib/./", Some("/srv/app/src/lib")),
            ("../", Some("/srv")),
            ("../../../../", Some("/")),
            ("/etc/", Some("/etc")),
            ("~/", Some("/home/me")),
            ("~/code/../x/", Some("/home/me/x")),
            (r"C:\x\", Some(r"/srv/app/C:\x\")),
        ];
        check(remote, "/srv/app", "/home/me", PathStyle::Posix);
        assert_eq!(
            PathStyle::Posix.resolve(Path::new("/"), None, "~/"),
            None,
            "no home, no ~"
        );
        #[cfg(not(windows))]
        check(
            &[("../b/", Some("/srv/b")), ("~/", Some("/home/me"))],
            "/srv/app",
            "/home/me",
            PathStyle::Native,
        );
        #[cfg(windows)]
        check(
            &[
                (r"..\b\", Some(r"C:\srv\b")),
                ("D:/x/", Some(r"D:\x")),
                (r"~\", Some(r"C:\Users\me")),
                (r"..\..\..\", Some(r"C:\")),
            ],
            r"C:\srv\app",
            r"C:\Users\me",
            PathStyle::Native,
        );

        let crumbs = |path: &str, home: &str| -> Vec<String> {
            PathStyle::Posix
                .crumbs(Path::new(path), Some(Path::new(home)))
                .into_iter()
                .map(|(name, _)| name)
                .collect()
        };
        assert_eq!(
            crumbs("/home/me/code/app", "/home/me"),
            ["~", "code", "app"]
        );
        assert_eq!(crumbs("/srv/app", "/home/me"), ["/", "srv", "app"]);
        assert_eq!(crumbs("/", "/home/me"), ["/"]);
        assert_eq!(
            PathStyle::Posix.split_typed("~/code/ap"),
            Some(("~/code/", "ap")),
        );
    }

    /// Folders for [`REMOTE_SCRIPT`] to agree on with the files: a
    /// checkout with a subfolder, a linked worktree of it, a plain folder,
    /// symlinks to both, a hidden folder, a file, a `.git` file pointing to
    /// the checkout's by a relative path, a detached checkout, and a folder
    /// no one can read.
    // The script runs on remote machines, which are Unix.
    #[cfg(unix)]
    struct Folders {
        _temp: tempfile::TempDir,
        dir: PathBuf,
        /// Whether the unreadable folder is: root reads any folder.
        unreadable: bool,
    }

    #[cfg(unix)]
    impl Folders {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            let temp = tempfile::tempdir().unwrap();
            let dir = platform::canonicalize(temp.path()).unwrap();
            let git = |args: &[&str]| crate::local::test::git(&dir, args);
            git(&["init", "-q", "-b", "main", "project"]);
            git(&["-C", "project", "commit", "-q", "--allow-empty", "-m", "x"]);
            git(&[
                "-C", "project", "worktree", "add", "-q", "-b", "feat", "../wt",
            ]);
            std::fs::create_dir_all(dir.join("project/docs")).unwrap();
            std::fs::create_dir_all(dir.join("Plain")).unwrap();
            std::fs::create_dir_all(dir.join(".hidden")).unwrap();
            std::fs::write(dir.join("project-notes"), "a file").unwrap();
            std::os::unix::fs::symlink(dir.join("Plain"), dir.join("link")).unwrap();
            std::os::unix::fs::symlink(dir.join("project"), dir.join("plink")).unwrap();
            std::fs::create_dir_all(dir.join("rel")).unwrap();
            std::fs::write(dir.join("rel/.git"), "gitdir: ../project/.git\n").unwrap();
            std::fs::create_dir_all(dir.join("det/.git")).unwrap();
            std::fs::write(
                dir.join("det/.git/HEAD"),
                format!("{}\n", "ab12".repeat(10)),
            )
            .unwrap();
            // A HEAD no one writes to, which reading would wait on forever.
            std::fs::create_dir_all(dir.join("fifo/.git")).unwrap();
            let status = std::process::Command::new("mkfifo")
                .arg(dir.join("fifo/.git/HEAD"))
                .status()
                .unwrap();
            assert!(status.success(), "mkfifo");
            let denied = dir.join("denied");
            std::fs::create_dir_all(&denied).unwrap();
            std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).unwrap();
            let unreadable = std::fs::read_dir(&denied).is_err();
            Self {
                _temp: temp,
                dir,
                unreadable,
            }
        }

        /// What the files say of `path` and `browse`, once the script,
        /// after what a login file that talks prints, says the same.
        fn read(path: Option<&Path>, browse: Option<&Path>) -> Read {
            let local = look_here(path, browse);
            let path = path.map(Path::to_path_buf).unwrap_or_default();
            let browse = browse.map(Path::to_path_buf).unwrap_or_default();
            let output =
                crate::local::test::sh(REMOTE_SCRIPT, &[&path, &browse]).expect("the script runs");
            let output = [b"Welcome!\n".as_slice(), output.as_slice()].concat();
            let remote = parse_remote(&output).expect("the script's fields");
            assert_eq!(remote, local, "{path:?} listing {browse:?}");
            local
        }
    }

    #[cfg(unix)]
    impl Drop for Folders {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;

            // So the temporary directory can be removed.
            let _ = std::fs::set_permissions(
                self.dir.join("denied"),
                std::fs::Permissions::from_mode(0o755),
            );
        }
    }

    /// [`REMOTE_SCRIPT`], run with this machine's `sh` and `git`, finds
    /// what reading the files here does for a typed path: a subfolder of a
    /// checkout, a linked worktree, a plain folder, a symlink to one and to
    /// a checkout, a prefix, a hidden folder, and a missing path.
    #[cfg(unix)]
    #[test]
    fn the_remote_script_finds_what_the_files_name() {
        let folders = Folders::new();
        let dir = &folders.dir;
        let (main, plain) = (dir.join("project"), dir.join("Plain"));
        let looked: Vec<Lookup> = [
            "project/docs",
            "wt",
            "Plain/",
            "link/",
            "plink",
            "p",
            ".h",
            "gone",
        ]
        .iter()
        .map(|rest| {
            let typed = Typed::parse(
                &format!("{}/{rest}", dir.display()),
                None,
                PathStyle::Native,
            )
            .unwrap();
            let read = Folders::read(Some(&typed.path), Some(&typed.browse));
            Lookup::new(&typed, read)
        })
        .collect();

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

    /// [`REMOTE_SCRIPT`] lists a folder as reading the files here does,
    /// asked as [`list`] asks, with nothing to resolve: every subfolder,
    /// with git roots by a `.git` folder or a `.git` file pointing to one,
    /// on a branch or detached; a symlink as the folder it leads to; and
    /// why a folder that is gone, a file, or unreadable can't be listed.
    #[cfg(unix)]
    #[test]
    fn the_remote_script_lists_what_the_files_hold() {
        let folders = Folders::new();
        let dir = &folders.dir;
        let listed = |folder: &Path| Folders::read(None, Some(folder)).listed;
        let branch = |name: &str| {
            Some(GitRoot {
                head: Some(repo::Head::Branch(name.to_owned())),
            })
        };
        let subfolder = |name: &str, git: Option<GitRoot>| Subfolder {
            name: name.into(),
            git,
        };
        let detached = Some(GitRoot {
            head: Some(repo::Head::Detached("ab12ab1".to_owned())),
        });
        let want = vec![
            subfolder(".hidden", None),
            // A folder, though nothing in it can be read.
            subfolder("denied", None),
            subfolder("det", detached),
            subfolder("fifo", Some(GitRoot { head: None })),
            subfolder("link", None),
            subfolder("Plain", None),
            subfolder("plink", branch("main")),
            subfolder("project", branch("main")),
            subfolder("rel", branch("main")),
            subfolder("wt", branch("feat")),
        ];
        assert_eq!(
            listed(dir),
            Some(Ok(Listing {
                real: Some(dir.clone()),
                subfolders: want,
            })),
            "every subfolder, sorted, files left out"
        );
        assert_eq!(
            listed(&dir.join("link")),
            Some(Ok(Listing {
                real: Some(dir.join("Plain")),
                subfolders: Vec::new(),
            })),
            "a symlink lists as the folder it leads to"
        );
        assert_eq!(listed(&dir.join("gone")), Some(Err(Unlisted::Gone)));
        assert_eq!(
            listed(&dir.join("project-notes")),
            Some(Err(Unlisted::NotFolder))
        );
        if folders.unreadable {
            assert_eq!(listed(&dir.join("denied")), Some(Err(Unlisted::Denied)));
        }
        assert_eq!(
            Folders::read(Some(&dir.join("wt")), None).listed,
            None,
            "nothing listed unasked"
        );
    }
}
