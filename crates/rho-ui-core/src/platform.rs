//! What differs between the operating systems rho-ui runs on, in one
//! place: where the user's home and rho-ui's own folders are, which shell
//! a terminal runs, how a path becomes the bytes a database row or a
//! remote shell takes, and how to reach a daemon's local endpoint.
//!
//! The rest of rho-ui, this crate and the app, asks here instead of
//! reading variables or naming an OS itself. Each function says what it
//! does on Unix and on Windows; Unix covers Linux, macOS and the BSDs.

use std::{
    ffi::{OsStr, OsString},
    io,
    path::PathBuf,
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
/// U+FFFD for an unpaired surrogate, which no file Explorer makes has.
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

#[cfg(windows)]
mod imp {
    use std::{ffi::OsStr, path::PathBuf};

    pub(super) fn path_to_bytes(path: &OsStr) -> Vec<u8> {
        path.to_string_lossy().into_owned().into_bytes()
    }

    pub(super) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
        String::from_utf8_lossy(bytes).into_owned().into()
    }
}

/// Bytes `bind(2)` takes in a Unix socket path, as rho-protocol counts
/// them: `sockaddr_un.sun_path` less its NUL.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) const MAX_SOCKET_PATH_BYTES: usize = 107;
/// Bytes `bind(2)` takes in a Unix socket path, as rho-protocol counts
/// them: `sockaddr_un.sun_path` less its NUL.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub(crate) const MAX_SOCKET_PATH_BYTES: usize = 103;

/// A folder only this user can open, for sockets of rho-ui's named `leaf`,
/// made if missing: `$XDG_RUNTIME_DIR/rho-ui/<leaf>`, else `rho-ui-<leaf>`
/// in the temporary directory, named after the user too where users share
/// it. macOS gives each user a temporary directory of their own (`$TMPDIR`,
/// under `/var/folders`), and has no room for a name: with a 49-byte
/// `$TMPDIR`, an ssh control socket there takes 93 of the
/// [`MAX_SOCKET_PATH_BYTES`], and with a 12-letter user name in the
/// folder's name it took 106.
///
/// # Errors
///
/// Fails if the folder can't be made, or other users can open it.
#[cfg(unix)]
pub(crate) fn private_socket_dir(leaf: &str) -> io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

    let dir = match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(run) if run.is_absolute() => run.join("rho-ui").join(leaf),
        _ if cfg!(target_os = "macos") => std::env::temp_dir().join(format!("rho-ui-{leaf}")),
        _ => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".to_owned());
            std::env::temp_dir().join(format!("rho-ui-{leaf}-{user}"))
        }
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let mode = std::fs::metadata(&dir)?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(io::Error::other(format!(
            "{} is open to other users (mode {:o})",
            dir.display(),
            mode & 0o777
        )));
    }
    Ok(dir)
}

/// Windows has no folder for ssh's control sockets: Win32-OpenSSH can't
/// share a connection (`ControlMaster`), which reaching a remote machine
/// is built on.
///
/// # Errors
///
/// Always.
#[cfg(windows)]
pub(crate) fn private_socket_dir(_leaf: &str) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "OpenSSH on Windows can't share a connection (ControlMaster), which rho-ui reaches \
         remote machines through",
    ))
}

/// The daemon's local endpoint: a Unix socket, or on Windows a named pipe.
#[cfg(unix)]
pub(crate) mod endpoint {
    use std::{
        io,
        path::{Path, PathBuf},
        process::Command,
    };

    use rho_protocol::{
        Client, ClientHello, HelloResponse, LocalDaemon, LocalDaemonError, Notifications,
    };
    use tokio::net::UnixStream;

    use crate::DaemonError;

    /// Connects to the daemon of the rho home `home` as `hello`, first
    /// starting it with `rho daemon start` through `rho` when none runs.
    /// Runs on the I/O runtime.
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
    pub(crate) async fn connect(path: &Path) -> io::Result<UnixStream> {
        UnixStream::connect(path).await
    }
}

/// The daemon's local endpoint: a Unix socket, or on Windows a named pipe.
#[cfg(windows)]
pub(crate) mod endpoint {
    use std::{
        io,
        path::{Path, PathBuf},
        process::Command,
    };

    use rho_protocol::{Client, ClientHello, CloseOnShutdown, HelloResponse, Notifications};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

    use crate::DaemonError;

    /// Can't yet reach the daemon of the rho home `home`: rho-protocol's
    /// `LocalDaemon`, which finds the daemon or starts it, is Unix-only.
    ///
    /// # Errors
    ///
    /// Always, saying so.
    #[expect(
        clippy::unused_async,
        reason = "the same signature as on Unix, where it awaits"
    )]
    pub(crate) async fn connect_or_start(
        home: PathBuf,
        _rho: Command,
        _hello: ClientHello,
    ) -> Result<(Client, Notifications, HelloResponse), DaemonError> {
        Err(DaemonError::Unreachable(format!(
            "rho-ui can't yet find or start the Rho daemon of {} on Windows: \
             rho-protocol's LocalDaemon is Unix-only",
            home.display()
        )))
    }

    /// Connects to the named pipe at `path`. Runs on the I/O runtime. A
    /// pipe can't close one direction alone, so the client's shutdown
    /// closes it whole, as rho's own client does; else a dropped client
    /// and the daemon would each wait for the other.
    ///
    /// # Errors
    ///
    /// Fails if nothing listens there.
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
    fn windows_finds_home_and_app_data() {
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
