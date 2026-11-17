//! What differs between the operating systems rho-ui runs on, in one
//! place: where the user's home, rho-ui's own folders and rho's install
//! folder are, how a setting's `~` reads, which shell a terminal runs, how
//! to run a program in the background, how a path becomes the bytes a
//! database row or a remote shell takes, and how to reach a daemon's local
//! endpoint.
//!
//! The rest of rho-ui, this crate and the app, asks here instead of
//! reading variables or naming an OS itself. Each function says what it
//! does on Unix and on Windows; Unix covers Linux, macOS and the BSDs.

use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The user's home directory: `$HOME`, else on Windows `%USERPROFILE%`,
/// then `%HOMEDRIVE%%HOMEPATH%`, as rho finds it. An empty variable counts
/// as unset.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    home_dir_from(|name| std::env::var_os(name))
}

fn home_dir_from(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |name| var(name).filter(|value| !value.is_empty());
    if let Some(home) = set("HOME") {
        return Some(PathBuf::from(home));
    }
    if cfg!(windows) {
        if let Some(profile) = set("USERPROFILE") {
            return Some(PathBuf::from(profile));
        }
        if let (Some(drive), Some(path)) = (set("HOMEDRIVE"), set("HOMEPATH")) {
            return Some(PathBuf::from(drive).join(path));
        }
    }
    None
}

/// The rho home as `rho` finds it: `$RHO_HOME`, else `.rho` in
/// [`home_dir`]. rho-protocol doesn't export this rule, so it follows
/// rho's `paths::rho_dir`.
#[must_use]
pub fn rho_home() -> Option<PathBuf> {
    rho_home_from(|name| std::env::var_os(name))
}

fn rho_home_from(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    var("RHO_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir_from(var).map(|home| home.join(".rho")))
}

/// `path`, a setting, with a leading `~` as `home`, as `sh` reads one: `~`
/// alone is `home`, and `~/x` is `x` in it, as is `~\x` on Windows, where
/// `\` divides a path too. Anything else, `~user` among it, is kept as it
/// is.
#[must_use]
pub fn expand_tilde(path: &Path, home: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) if rest.as_os_str().is_empty() => home.to_path_buf(),
        Ok(rest) => home.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// Which of rho-ui's own folders [`app_dir`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppDir {
    /// The settings the user chooses.
    Config,
    /// What rho-ui remembers between runs.
    State,
}

/// rho-ui's folder for `which`. On Unix, `rho-ui` in the XDG base
/// directory: `$XDG_CONFIG_HOME` (else `~/.config`) or `$XDG_STATE_HOME`
/// (else `~/.local/state`), where a relative variable counts as unset, as
/// the spec says. macOS follows XDG too, as rho and most command-line
/// tools there do, so the files are where a Linux user looks. On Windows,
/// `rho-ui\config` or `rho-ui\state` in `%APPDATA%`, the roaming profile.
/// `None` when the variables that place it are all unset.
#[must_use]
pub fn app_dir(which: AppDir) -> Option<PathBuf> {
    app_dir_from(which, |name| std::env::var_os(name))
}

fn app_dir_from(which: AppDir, var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if cfg!(windows) {
        let leaf = match which {
            AppDir::Config => "config",
            AppDir::State => "state",
        };
        let app_data = var("APPDATA")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())?;
        return Some(app_data.join("rho-ui").join(leaf));
    }
    let (variable, fallback) = match which {
        AppDir::Config => ("XDG_CONFIG_HOME", ".config"),
        AppDir::State => ("XDG_STATE_HOME", ".local/state"),
    };
    let base = var(variable)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home_dir_from(&var).map(|home| home.join(fallback)))?;
    Some(base.join("rho-ui"))
}

/// The `file://` URI of `path`, an absolute path on this machine:
/// `file:///home/me/a.md` on Unix, and on Windows `file:///C:/Users/me/a.md`,
/// with `/` for `\\`, as a URI names a Windows path. Its name is what follows
/// the last `/`.
#[must_use]
pub fn file_uri(path: &Path) -> String {
    if cfg!(windows) {
        format!("file:///{}", path.display().to_string().replace('\\', "/"))
    } else {
        format!("file://{}", path.display())
    }
}

/// The path of a `file://` URI that [`file_uri`] made, or `None` if `uri`
/// isn't one. On Windows, with `\\` again for `/`.
#[must_use]
pub fn file_uri_path(uri: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        let path = uri.strip_prefix("file:///")?;
        Some(PathBuf::from(path.replace('/', "\\")))
    } else {
        uri.strip_prefix("file://").map(PathBuf::from)
    }
}

/// The folder rho's installer puts `rho` in when not told where. On Unix,
/// `~/.local/bin`, as its `install.sh` does. On Windows,
/// `%LOCALAPPDATA%\Programs\rho\bin`, as its `install.ps1` does; that adds
/// the folder to the user's `PATH`, but not to that of programs already
/// running. `None` when the variable that places it is unset.
#[must_use]
pub fn rho_install_dir() -> Option<PathBuf> {
    rho_install_dir_from(|name| std::env::var_os(name))
}

fn rho_install_dir_from(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if cfg!(windows) {
        let local = var("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())?;
        return Some(local.join("Programs").join("rho").join("bin"));
    }
    home_dir_from(var).map(|home| home.join(".local").join("bin"))
}

/// A command for `program` that runs in the background of the app, as
/// `git` and `rho` do for it: its stdin is empty, and on Windows it opens no
/// console window, which would otherwise flash up for each one a GUI app
/// starts.
#[must_use]
pub fn background_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// The process creation flag that starts a console program without a
/// console window (`CREATE_NO_WINDOW` in the Win32 API).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `path` resolved, as [`std::fs::canonicalize`] does. On Windows, without
/// the `\\?\` prefix that gives it, where the path reads the same without:
/// git, the shells and the user all take `C:\code`, and few take
/// `\\?\C:\code`. A rho home is the exception: rho names its daemon after
/// the home's path as [`std::fs::canonicalize`] gives it. `clippy.toml`
/// disallows std's, so each call to it says why.
///
/// # Errors
///
/// As [`std::fs::canonicalize`]: if `path` isn't there, or can't be read.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    dunce::canonicalize(path)
}

/// The shell a terminal on this machine runs. On Unix, `$SHELL`, else
/// `/bin/sh`. On Windows, `%ComSpec%`, else `cmd.exe`.
#[must_use]
pub fn default_shell() -> PathBuf {
    let (variable, fallback) = if cfg!(windows) {
        ("ComSpec", "cmd.exe")
    } else {
        ("SHELL", "/bin/sh")
    };
    std::env::var_os(variable)
        .filter(|shell| !shell.is_empty())
        .map_or_else(|| PathBuf::from(fallback), PathBuf::from)
}

/// `path` as bytes, for a database row or a remote shell. On Unix, the
/// path's own bytes, which needn't be UTF-8. On Windows, its UTF-8, with
/// U+FFFD for an unpaired surrogate, which no file Explorer makes has; in
/// a browser, its UTF-8 too, as the paths there come from the daemon's
/// JSON, which is UTF-8 already.
/// [`path_from_bytes`] reads it back.
#[must_use]
pub fn path_to_bytes(path: &OsStr) -> Vec<u8> {
    imp::path_to_bytes(path)
}

/// The path [`path_to_bytes`] gave `bytes`, or one a remote shell
/// printed. On Windows, bytes that aren't UTF-8 read as U+FFFD.
#[must_use]
pub fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    imp::path_from_bytes(bytes)
}

#[cfg(unix)]
mod imp {
    use std::{
        ffi::{OsStr, OsString},
        os::unix::ffi::{OsStrExt as _, OsStringExt as _},
        path::PathBuf,
    };

    pub(super) fn path_to_bytes(path: &OsStr) -> Vec<u8> {
        path.as_bytes().to_vec()
    }

    pub(super) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
        OsString::from_vec(bytes.to_vec()).into()
    }
}

#[cfg(not(unix))]
mod imp {
    use std::{ffi::OsStr, path::PathBuf};

    pub(super) fn path_to_bytes(path: &OsStr) -> Vec<u8> {
        path.to_string_lossy().into_owned().into_bytes()
    }

    pub(super) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
        String::from_utf8_lossy(bytes).into_owned().into()
    }
}

/// The daemon's local endpoint: a Unix socket, or on Windows a named pipe.
/// A browser has neither, and reaches the daemon through `rho-ui serve`.
#[cfg(not(target_family = "wasm"))]
pub(crate) mod endpoint {
    use std::{
        io,
        path::{Path, PathBuf},
        process::Command,
    };

    #[cfg(windows)]
    use rho_protocol::CloseOnShutdown;
    use rho_protocol::{
        Client, ClientHello, HelloResponse, LocalDaemon, LocalDaemonError, Notifications,
    };
    #[cfg(windows)]
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    #[cfg(unix)]
    use tokio::net::UnixStream;

    use crate::DaemonError;

    /// Connects to the daemon of the rho home `home` as `hello`, first
    /// starting it with `rho daemon start` through `rho` when none runs.
    /// Runs on the I/O runtime, whose timer `LocalDaemon` needs on Windows
    /// to wait out a daemon pipe whose instances are all taken.
    ///
    /// # Errors
    ///
    /// Says why, naming the command or socket that failed.
    pub(crate) async fn connect_or_start(
        home: PathBuf,
        rho: Command,
        hello: ClientHello,
    ) -> Result<(Client, Notifications, HelloResponse), DaemonError> {
        LocalDaemon::new(home)
            .connect_or_start(rho, hello)
            .await
            .map_err(|error| match error {
                LocalDaemonError::Handshake(error) => error.into(),
                // Each names the command or socket that failed, and why.
                other => DaemonError::Unreachable(other.to_string()),
            })
    }

    /// Connects to the socket at `path`. Runs on the I/O runtime.
    ///
    /// # Errors
    ///
    /// Fails if nothing listens there.
    #[cfg(unix)]
    pub(crate) async fn connect(path: &Path) -> io::Result<UnixStream> {
        UnixStream::connect(path).await
    }

    /// Connects to the named pipe at `path`. Runs on the I/O runtime. A
    /// pipe can't close one direction alone, so the client's shutdown
    /// closes it whole, as rho's own client does; else a dropped client
    /// and the daemon would each wait for the other.
    ///
    /// # Errors
    ///
    /// Fails if nothing listens there.
    #[cfg(windows)]
    #[expect(
        clippy::unused_async,
        reason = "the same signature as on Unix, where connecting awaits"
    )]
    pub(crate) async fn connect(path: &Path) -> io::Result<CloseOnShutdown<NamedPipeClient>> {
        ClientOptions::new().open(path).map(CloseOnShutdown::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        |name| {
            vars.iter()
                .find(|(set, _)| *set == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn the_rho_home_is_rho_home_else_dot_rho_in_home() {
        let both = [("RHO_HOME", "/srv/rho"), ("HOME", "/home/me")];
        assert_eq!(rho_home_from(env(&both)), Some(PathBuf::from("/srv/rho")));
        let empty = [("RHO_HOME", ""), ("HOME", "/home/me")];
        assert_eq!(
            rho_home_from(env(&empty)),
            Some(PathBuf::from("/home/me/.rho"))
        );
        assert_eq!(rho_home_from(env(&[("HOME", "")])), None);
    }

    /// A leading `~` is the home directory, alone or as a path's first
    /// part; `~user` and a `~` further in are kept.
    #[test]
    fn a_leading_tilde_is_home() {
        let home = Path::new("/home/me");
        let expand = |path: &str| expand_tilde(Path::new(path), home);
        assert_eq!(expand("~"), home);
        assert_eq!(expand("~/"), home);
        assert_eq!(expand("~/code"), home.join("code"));
        assert_eq!(expand("~me/code"), Path::new("~me/code"));
        assert_eq!(expand("code/~"), Path::new("code/~"));
        assert_eq!(expand("/srv/code"), Path::new("/srv/code"));
        #[cfg(windows)]
        assert_eq!(expand(r"~\code"), home.join("code"));
    }

    #[cfg(unix)]
    #[test]
    fn unix_app_dirs_follow_xdg_and_take_no_relative_base() {
        let set = [("XDG_STATE_HOME", "/x/state"), ("HOME", "/home/me")];
        assert_eq!(
            app_dir_from(AppDir::State, env(&set)),
            Some(PathBuf::from("/x/state/rho-ui"))
        );
        let relative = [("XDG_CONFIG_HOME", "rel"), ("HOME", "/home/me")];
        assert_eq!(
            app_dir_from(AppDir::Config, env(&relative)),
            Some(PathBuf::from("/home/me/.config/rho-ui"))
        );
        assert_eq!(app_dir_from(AppDir::Config, env(&[])), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_finds_home_app_data_and_the_rho_install_dir() {
        let profile = [
            ("USERPROFILE", r"C:\Users\me"),
            ("APPDATA", r"C:\Users\me\AppData\Roaming"),
        ];
        assert_eq!(
            home_dir_from(env(&profile)),
            Some(PathBuf::from(r"C:\Users\me"))
        );
        assert_eq!(
            app_dir_from(AppDir::State, env(&profile)),
            Some(PathBuf::from(r"C:\Users\me\AppData\Roaming\rho-ui\state"))
        );
        let local = [("LOCALAPPDATA", r"C:\Users\me\AppData\Local")];
        assert_eq!(
            rho_install_dir_from(env(&local)),
            Some(PathBuf::from(r"C:\Users\me\AppData\Local\Programs\rho\bin"))
        );
    }

    /// A path that isn't UTF-8 keeps its bytes there and back, so a project
    /// saved with one reads back as the same folder.
    #[cfg(unix)]
    #[test]
    fn path_bytes_round_trip_what_isnt_utf8() {
        let bytes = b"/w/\xff";
        assert_eq!(path_to_bytes(path_from_bytes(bytes).as_os_str()), bytes);
    }
}
