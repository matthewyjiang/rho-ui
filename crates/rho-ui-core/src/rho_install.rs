//! A machine's `rho` program, apart from its daemon: whether rho is there,
//! whether a newer release is out, installing or updating it, and
//! restarting a daemon too old to talk to. On this machine rho-ui runs
//! `rho` itself: `rho --version` and `rho update --check --json` to check,
//! `rho update` to update, `rho daemon restart` to restart. Installing runs
//! rho's own installer, its `install.sh` with `sh` on Unix
//! ([`INSTALL_SCRIPT`]) and its `install.ps1` with `powershell` on Windows.
//! On a remote machine, which is Unix, each job is one POSIX `sh` script
//! over ssh: [`CHECK_SCRIPT`], [`INSTALL_SCRIPT`], [`UPDATE_SCRIPT`] and
//! [`RESTART_SCRIPT`].

#[cfg(not(target_family = "wasm"))]
use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde::Deserialize;
#[cfg(not(target_family = "wasm"))]
use tokio::sync::oneshot;

#[cfg(not(target_family = "wasm"))]
use crate::platform;
#[cfg(not(target_family = "wasm"))]
use crate::ssh::SshTarget;
use crate::{link::Link, server::ServerTarget, DaemonError};

/// Where a machine's `rho` program runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RhoInstall {
    /// This machine, running `rho`: a name looked up on `PATH`, or a path.
    /// Not in a browser, which reaches every machine through `rho-ui serve`.
    #[cfg(not(target_family = "wasm"))]
    Here {
        /// The program.
        rho: PathBuf,
    },
    /// A remote machine, running its target's `rho` over ssh.
    #[cfg(not(target_family = "wasm"))]
    Ssh(SshTarget),
    /// The machine of a `rho-ui serve`, running its target's `rho` there.
    Server(ServerTarget),
}

/// What [`RhoInstall::check`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RhoCheck {
    /// No `rho` program runs there.
    Missing,
    /// rho is installed.
    Installed {
        /// Its version, such as `3.1.0`: the program's on disk, which the
        /// daemon may not run yet.
        current: String,
        /// Whether a newer release is out.
        update: Update,
    },
}

/// Whether a newer rho is out, as `rho update --check` says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    /// The newest release is installed.
    UpToDate,
    /// A newer release is out, and [`RhoInstall::update`] installs it.
    Available {
        /// Its version.
        latest: String,
    },
    /// A newer release is out, but installing it needs a terminal, as when
    /// the package manager asks for a password.
    InTerminal {
        /// Its version.
        latest: String,
        /// The command that installs it.
        command: String,
    },
    /// rho couldn't tell, as when GitHub is out of reach, or it is from
    /// before `rho update --check`. Why, in rho's words.
    Unknown(String),
}

impl RhoInstall {
    /// Finds out whether rho is there and a newer release is out. Asks
    /// GitHub, through rho, once.
    ///
    /// # Errors
    ///
    /// Fails if rho or the script can't run there, as when ssh can't reach
    /// the machine.
    pub async fn check(&self) -> Result<RhoCheck, DaemonError> {
        let checked = match self.place().await? {
            #[cfg(not(target_family = "wasm"))]
            Place::Here(rho) => {
                let rho = rho.to_owned();
                on_own_thread(move || check_here(&find_rho(&rho))).await?
            }
            Place::There(link, rho) => {
                let output = link.run_script(CHECK_SCRIPT, &[rho]).await?;
                parse_check(&String::from_utf8_lossy(&output))
            }
        };
        checked.map_err(|why| DaemonError::Unreachable(format!("rho's update check {why}")))
    }

    /// Installs the newest rho with its installer, where the program is if
    /// it is a path, else in the installer's own folder
    /// ([`platform::rho_install_dir`] here, `~/.local/bin` on a remote
    /// machine).
    ///
    /// # Errors
    ///
    /// Fails, with what the installer printed, if it fails or can't run.
    pub async fn install(&self) -> Result<(), DaemonError> {
        match self.place().await? {
            #[cfg(not(target_family = "wasm"))]
            Place::Here(rho) => {
                let rho = rho.to_owned();
                on_own_thread(move || install_here(&rho)).await?
            }
            Place::There(link, rho) => link.run_long_script(INSTALL_SCRIPT, &[rho]).await.map(drop),
        }
    }

    /// Runs `rho update`, which installs the newest release the way rho was
    /// installed. A daemon running the old program upgrades in place once
    /// nothing would be cut short.
    ///
    /// # Errors
    ///
    /// Fails, with what rho printed, if it fails or can't run.
    pub async fn update(&self) -> Result<(), DaemonError> {
        match self.place().await? {
            #[cfg(not(target_family = "wasm"))]
            Place::Here(rho) => {
                let rho = rho.to_owned();
                on_own_thread(move || update_here(&find_rho(&rho))).await?
            }
            Place::There(link, rho) => link.run_long_script(UPDATE_SCRIPT, &[rho]).await.map(drop),
        }
    }

    /// Restarts the machine's daemon with `rho daemon restart`, which
    /// stops a daemon that refused rho-ui's handshake for its older
    /// protocol and starts the rho installed. That is what rho says to do
    /// then: `rho/daemon/restart` needs a handshake first. Here it runs
    /// with this process's environment, as the daemon rho-ui connects to
    /// is the one of its rho home.
    ///
    /// # Errors
    ///
    /// Fails, with what rho printed, if it fails or can't run.
    pub async fn restart_daemon(&self) -> Result<(), DaemonError> {
        match self.place().await? {
            #[cfg(not(target_family = "wasm"))]
            Place::Here(rho) => {
                let rho = find_rho(rho);
                on_own_thread(move || {
                    run(platform::background_command(&rho).args(["daemon", "restart"]))
                })
                .await?
            }
            Place::There(link, rho) => link.run_script(RESTART_SCRIPT, &[rho]).await.map(drop),
        }
    }

    /// Where `rho` runs: here, or through the link to its machine, opened
    /// if none is.
    #[cfg_attr(
        target_family = "wasm",
        expect(
            clippy::unused_async,
            clippy::unused_async_trait_impl,
            reason = "only an ssh link waits to open"
        )
    )]
    async fn place(&self) -> Result<Place<'_>, DaemonError> {
        Ok(match self {
            #[cfg(not(target_family = "wasm"))]
            Self::Here { rho } => Place::Here(rho),
            #[cfg(not(target_family = "wasm"))]
            Self::Ssh(target) => Place::There(Link::Ssh(target.link().await?), target.rho()),
            Self::Server(target) => Place::There(Link::Server(target.link()), target.rho()),
        })
    }
}

/// Where a [`RhoInstall`]'s `rho` runs.
enum Place<'a> {
    /// This machine, running this program.
    #[cfg(not(target_family = "wasm"))]
    Here(&'a Path),
    /// Another machine, running this program there.
    There(Link, &'a str),
}

#[cfg(not(target_family = "wasm"))]
/// Runs `job` on a thread of its own, and waits for it without blocking:
/// it may wait minutes on the network, and isn't the daemon socket's work
/// (see `exec::io`).
///
/// # Errors
///
/// [`DaemonError::Closed`] if the job panicked.
async fn on_own_thread<T: Send + 'static>(
    job: impl FnOnce() -> T + Send + 'static,
) -> Result<T, DaemonError> {
    let (reply, ran) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = reply.send(job());
    });
    ran.await.map_err(|_| DaemonError::Closed)
}

#[cfg(not(target_family = "wasm"))]
/// Runs `command` here and returns how it ended and what it printed.
/// Blocks until it ends.
///
/// # Errors
///
/// Says why if it can't start.
fn output(command: &mut Command) -> Result<Output, DaemonError> {
    command.output().map_err(|error| {
        let program = command.get_program().to_string_lossy();
        DaemonError::Unreachable(format!("couldn't run {program}: {error}"))
    })
}

#[cfg(not(target_family = "wasm"))]
/// Whether a program that ended as `output` says succeeded, else an error
/// with what it printed in `said`, or how it ended if that is empty.
fn succeeded(output: &Output, said: &[u8]) -> Result<(), DaemonError> {
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(said);
    Err(DaemonError::Unreachable(match said.trim() {
        "" => format!("it ended with {}", output.status),
        said => said.to_owned(),
    }))
}

/// Runs `command` here. Blocks until it ends.
///
/// # Errors
///
/// Fails, with what it wrote to stderr, if it fails or can't start.
#[cfg(not(target_family = "wasm"))]
fn run(command: &mut Command) -> Result<(), DaemonError> {
    let output = output(command)?;
    succeeded(&output, &output.stderr)
}

#[cfg(not(target_family = "wasm"))]
/// Runs `rho update` with the program `rho` here. Blocks until it ends.
///
/// # Errors
///
/// Fails, with all rho printed, stdout and then stderr, if it fails or
/// can't start, as [`UPDATE_SCRIPT`] reports both.
fn update_here(rho: &Path) -> Result<(), DaemonError> {
    let output = output(platform::background_command(rho).arg("update"))?;
    succeeded(
        &output,
        &[output.stdout.as_slice(), output.stderr.as_slice()].concat(),
    )
}

#[cfg(not(target_family = "wasm"))]
/// What the program `rho` here says of itself and of the newest release,
/// read as [`parse_check`] reads [`CHECK_SCRIPT`]'s output. A program that
/// isn't there is [`RhoCheck::Missing`]. Blocks while rho asks GitHub.
fn check_here(rho: &Path) -> Result<RhoCheck, String> {
    let couldnt = |error: io::Error| format!("couldn't run {}: {error}", rho.display());
    let version = match platform::background_command(rho).arg("--version").output() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(RhoCheck::Missing),
        Err(error) => return Err(couldnt(error)),
        Ok(output) => output.stdout,
    };
    let check = platform::background_command(rho)
        .args(["update", "--check", "--json"])
        .output()
        .map_err(couldnt)?;
    let said = if check.status.success() {
        check.stdout
    } else {
        check.stderr
    };
    let version = String::from_utf8_lossy(&version);
    read_check(
        check.status.code(),
        version.lines().next().unwrap_or_default(),
        String::from_utf8_lossy(&said).trim(),
    )
}

/// Runs rho's `install.sh` here with [`INSTALL_SCRIPT`], as on a remote
/// machine: the installer is a POSIX `sh` script.
#[cfg(unix)]
fn install_here(rho: &Path) -> Result<(), DaemonError> {
    let mut command = platform::background_command("sh");
    command.arg("-c").arg(INSTALL_SCRIPT).arg("sh").arg(rho);
    run(&mut command)
}

/// Runs rho's [`INSTALL_PS1`] with `powershell`, into the folder of the
/// program `rho` when it is a path, as [`INSTALL_SCRIPT`] does with
/// `install.sh`.
#[cfg(windows)]
fn install_here(rho: &Path) -> Result<(), DaemonError> {
    let mut command = platform::background_command("powershell");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
        ])
        .arg(format!("irm {INSTALL_PS1} | iex"));
    let rho = expand_home(rho, platform::home_dir().as_deref());
    if let Some(dir) = rho.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        command.env("RHO_INSTALL_DIR", dir);
    }
    run(&mut command)
}

/// The `sh` that finds the program `$rho` names on a remote machine: a
/// leading `~/` is the home directory, and plain `rho` not on `PATH` falls
/// back to `~/.local/bin`, where rho's install script puts it. [`find_rho`]
/// is the same rule here. A macro, so `concat!` and `format!` can take it.
macro_rules! find_rho_sh {
    () => {
        r#"case $rho in "~/"*) rho="$HOME/${rho#??}" ;; esac
if [ "$rho" = rho ] && ! command -v rho >/dev/null 2>&1 && [ -x "$HOME/.local/bin/rho" ]; then
  rho=$HOME/.local/bin/rho
fi
"#
    };
}
pub(crate) use find_rho_sh;

/// Checks rho on a remote machine, as [`check_here`] does here. Prints
/// [`MISSING`] if there is no rho, else [`CHECKED`], `rho update --check
/// --json`'s exit status on a line, `rho --version`, and what the check
/// printed: its JSON on stdout when it succeeded, else its stderr. Read
/// with [`parse_check`].
const CHECK_SCRIPT: &str = concat!(
    "rho=$1\n",
    find_rho_sh!(),
    r#"if ! command -v "$rho" >/dev/null 2>&1; then
  printf '\nrho-ui-rho-missing\n'
  exit
fi
version=$("$rho" --version 2>/dev/null)
errors=$(mktemp) || exit
said=$("$rho" update --check --json 2>"$errors")
status=$?
[ "$status" -eq 0 ] || said=$(cat "$errors")
rm -f "$errors"
printf '\nrho-ui-rho-checked\n%s\n%s\n%s' "$status" "$version" "$said"
"#
);

/// Runs rho's install script, published with its docs, into the folder
/// of the program when it is a path: on a remote machine, and here on
/// Unix. Output goes to stderr, which a failure reports.
const INSTALL_SCRIPT: &str = r#"exec 1>&2
rho=$1
case $rho in
  "~/"*) RHO_INSTALL_DIR="$HOME/$(dirname "${rho#??}")"; export RHO_INSTALL_DIR ;;
  */*) RHO_INSTALL_DIR=$(dirname "$rho"); export RHO_INSTALL_DIR ;;
esac
script=$(curl -fsSL https://matthewyjiang.github.io/rho/install.sh) || exit
printf '%s\n' "$script" | sh
"#;

/// rho's installer for Windows, a PowerShell script published beside the
/// `install.sh` that [`INSTALL_SCRIPT`] fetches.
#[cfg(windows)]
const INSTALL_PS1: &str = "https://matthewyjiang.github.io/rho/install.ps1";

/// Runs `rho update` on a remote machine, with no terminal. Output goes
/// to stderr, which a failure reports.
const UPDATE_SCRIPT: &str = concat!(
    "exec 1>&2\nrho=$1\n",
    find_rho_sh!(),
    "exec \"$rho\" update </dev/null\n"
);

/// Runs `rho daemon restart` on a remote machine. Output goes to stderr,
/// which a failure reports.
const RESTART_SCRIPT: &str = concat!(
    "exec 1>&2\nrho=$1\n",
    find_rho_sh!(),
    "exec \"$rho\" daemon restart </dev/null\n"
);

/// Printed when there is no rho, after whatever a login file printed.
const MISSING: &str = "\nrho-ui-rho-missing\n";
/// Printed before the check's results.
const CHECKED: &str = "\nrho-ui-rho-checked\n";

/// What `rho update --check --json` prints.
#[derive(Deserialize)]
struct CheckJson {
    current: String,
    latest: String,
    update_available: bool,
    /// `null` without an update.
    command: Option<String>,
    needs_terminal: bool,
}

/// Reads [`CHECK_SCRIPT`]'s output.
fn parse_check(output: &str) -> Result<RhoCheck, String> {
    if output.ends_with(MISSING) {
        return Ok(RhoCheck::Missing);
    }
    let (_, checked) = output
        .rsplit_once(CHECKED)
        .ok_or("printed nothing it should")?;
    let mut lines = checked.splitn(3, '\n');
    let status = lines.next().unwrap_or_default();
    let version = lines.next().unwrap_or_default();
    let said = lines.next().unwrap_or_default().trim();
    read_check(status.parse().ok(), version, said)
}

/// What a check of an installed rho found, from `rho update --check
/// --json`'s exit code (`None` if a signal ended it), the first line `rho
/// --version` printed, and what the check said: its JSON when it
/// succeeded, else its stderr.
fn read_check(code: Option<i32>, version: &str, said: &str) -> Result<RhoCheck, String> {
    if code == Some(0) {
        let json: CheckJson = serde_json::from_str(said)
            .map_err(|error| format!("printed what isn't its JSON ({error}): {said}"))?;
        return Ok(RhoCheck::Installed {
            current: json.current,
            update: match (json.update_available, json.needs_terminal) {
                (false, _) => Update::UpToDate,
                (true, false) => Update::Available {
                    latest: json.latest,
                },
                (true, true) => Update::InTerminal {
                    latest: json.latest,
                    command: json.command.unwrap_or_else(|| "rho update".to_owned()),
                },
            },
        });
    }
    // `rho 3.1.0`, as clap prints it.
    let current = version
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("found a rho whose --version printed {version:?}"))?
        .to_owned();
    let update = match code {
        // clap's exit status for an option it doesn't know: a rho from
        // before `update --check`. It may still be the newest release.
        Some(2) => Update::Unknown(format!("rho {current} can't check for updates")),
        _ => Update::Unknown(said.strip_prefix("Error: ").unwrap_or(said).to_owned()),
    };
    Ok(RhoCheck::Installed { current, update })
}

#[cfg(not(target_family = "wasm"))]
/// The program `rho` names on this machine: itself, its leading `~/` the
/// home directory, unless it is plain `rho`, which `PATH` lacks, and rho's
/// installer put one in its folder ([`platform::rho_install_dir`]): a
/// desktop session's `PATH` often lacks that. [`find_rho_sh`] is the same
/// rule in `sh`.
#[must_use]
pub(crate) fn find_rho(rho: &Path) -> PathBuf {
    find_rho_in(
        rho,
        std::env::var_os("PATH").as_deref(),
        platform::home_dir().as_deref(),
        platform::rho_install_dir(),
    )
}

#[cfg(not(target_family = "wasm"))]
fn find_rho_in(
    rho: &Path,
    path: Option<&OsStr>,
    home: Option<&Path>,
    install_dir: Option<PathBuf>,
) -> PathBuf {
    let rho = expand_home(rho, home);
    if rho != Path::new("rho") {
        return rho;
    }
    // `rho.exe` on Windows, which `Command` finds as `rho`.
    let program = format!("rho{}", std::env::consts::EXE_SUFFIX);
    let on_path = path
        .is_some_and(|path| std::env::split_paths(path).any(|dir| dir.join(&program).is_file()));
    if on_path {
        return rho;
    }
    install_dir
        .map(|dir| dir.join(&program))
        .filter(|installed| installed.is_file())
        .unwrap_or(rho)
}

#[cfg(not(target_family = "wasm"))]
/// `rho` with a leading `~` as `home` (see [`platform::expand_tilde`]),
/// as `sh` reads the program's setting.
fn expand_home(rho: &Path, home: Option<&Path>) -> PathBuf {
    home.map_or_else(
        || rho.to_path_buf(),
        |home| platform::expand_tilde(rho, home),
    )
}

#[cfg(test)]
#[path = "rho_install_tests.rs"]
mod tests;
