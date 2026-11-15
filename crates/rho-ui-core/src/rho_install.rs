//! A machine's `rho` program, apart from its daemon: whether rho is there,
//! whether a newer release is out, and installing or updating it. Each
//! runs a POSIX `sh` script, here or over ssh, so both machines take one
//! path: [`CHECK_SCRIPT`] asks `rho update --check --json`, [`INSTALL_SCRIPT`]
//! runs rho's install script, and [`UPDATE_SCRIPT`] runs `rho update`.

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use serde::Deserialize;
use tokio::sync::oneshot;

use crate::{platform, ssh::SshTarget, DaemonError};

/// Where a machine's `rho` program runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RhoInstall {
    /// This machine, running `rho`: a name looked up on `PATH`, or a path.
    Here {
        /// The program.
        rho: PathBuf,
    },
    /// A remote machine, running its target's `rho` over ssh.
    Ssh(SshTarget),
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
    /// Fails if the script can't run there, as when ssh can't reach the
    /// machine.
    pub async fn check(&self) -> Result<RhoCheck, DaemonError> {
        let output = self.run(CHECK_SCRIPT, Deadline::Short).await?;
        parse_check(&String::from_utf8_lossy(&output))
            .map_err(|why| DaemonError::Unreachable(format!("rho's update check {why}")))
    }

    /// Installs the newest rho with its install script, where the program
    /// is if it is a path, else in `~/.local/bin`, the script's default.
    ///
    /// # Errors
    ///
    /// Fails, with what the script printed, if it fails or can't run.
    pub async fn install(&self) -> Result<(), DaemonError> {
        self.run(INSTALL_SCRIPT, Deadline::None).await.map(drop)
    }

    /// Runs `rho update`, which installs the newest release the way rho was
    /// installed. A daemon running the old program upgrades in place once
    /// nothing would be cut short.
    ///
    /// # Errors
    ///
    /// Fails, with what rho printed, if it fails or can't run.
    pub async fn update(&self) -> Result<(), DaemonError> {
        self.run(UPDATE_SCRIPT, Deadline::None).await.map(drop)
    }

    async fn run(&self, script: &'static str, deadline: Deadline) -> Result<Vec<u8>, DaemonError> {
        match self {
            Self::Here { rho } => {
                let rho = rho.clone().into_os_string();
                let (reply, ran) = oneshot::channel();
                // A thread of its own: it may wait minutes on the network,
                // and isn't the daemon socket's work (see `daemon::io`).
                std::thread::spawn(move || {
                    let _ = reply.send(run_here(script, &rho));
                });
                ran.await.map_err(|_| DaemonError::Closed)?
            }
            Self::Ssh(target) => {
                let link = target.link().await?;
                let args = [target.rho()];
                match deadline {
                    Deadline::Short => link.run_script(script, &args).await,
                    Deadline::None => link.run_long_script(script, &args).await,
                }
            }
        }
    }
}

/// How long a script may take over ssh.
#[derive(Clone, Copy)]
enum Deadline {
    /// [`crate::SshLink::run_script`]'s.
    Short,
    /// None: an install's time is the network's.
    None,
}

/// Runs `script` with `sh` here, with `rho` as `$1`.
fn run_here(script: &str, rho: &OsString) -> Result<Vec<u8>, DaemonError> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(rho)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| DaemonError::Unreachable(format!("couldn't run sh: {error}")))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let said = String::from_utf8_lossy(&output.stderr);
        Err(DaemonError::Unreachable(match said.trim() {
            "" => format!("it ended with {}", output.status),
            said => said.to_owned(),
        }))
    }
}

/// Where rho's install script puts `rho` by default, from the home
/// directory. A desktop session's or a non-interactive ssh command's
/// `PATH` often lacks it, so a program set to plain `rho` falls back to it.
pub const INSTALL_DIR: &str = ".local/bin";

/// The `sh` that finds the program `$rho` names: a leading `~/` is the
/// home directory, and plain `rho` not on `PATH` falls back to the install
/// script's [`INSTALL_DIR`]. [`find_rho`] is the same rule here. A macro,
/// so `concat!` and `format!` can take it.
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

/// Prints [`MISSING`] if there is no rho, else [`CHECKED`], `rho update
/// --check --json`'s exit status on a line, `rho --version`, and what the
/// check printed: its JSON on stdout when it succeeded, else its stderr.
/// Read with [`parse_check`].
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
/// of the program when it is a path. Output goes to stderr, which a
/// failure reports.
const INSTALL_SCRIPT: &str = r#"exec 1>&2
rho=$1
case $rho in
  "~/"*) RHO_INSTALL_DIR="$HOME/$(dirname "${rho#??}")"; export RHO_INSTALL_DIR ;;
  */*) RHO_INSTALL_DIR=$(dirname "$rho"); export RHO_INSTALL_DIR ;;
esac
script=$(curl -fsSL https://matthewyjiang.github.io/rho/install.sh) || exit
printf '%s\n' "$script" | sh
"#;

/// Runs `rho update` with no terminal. Output goes to stderr, which a
/// failure reports.
const UPDATE_SCRIPT: &str = concat!(
    "exec 1>&2\nrho=$1\n",
    find_rho_sh!(),
    "exec \"$rho\" update </dev/null\n"
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
    if status == "0" {
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
    let update = match status {
        // clap's exit status for an option it doesn't know: a rho from
        // before `update --check`. It may still be the newest release.
        "2" => Update::Unknown(format!("rho {current} can't check for updates")),
        _ => Update::Unknown(said.strip_prefix("Error: ").unwrap_or(said).to_owned()),
    };
    Ok(RhoCheck::Installed { current, update })
}

/// The program `rho` names on this machine: itself, unless it is plain
/// `rho`, which `PATH` lacks, and rho's install script put one in
/// [`INSTALL_DIR`]; [`find_rho_sh`] is the same rule in `sh`.
#[must_use]
pub(crate) fn find_rho(rho: &Path) -> PathBuf {
    find_rho_in(
        rho,
        std::env::var_os("PATH").as_deref(),
        platform::home_dir(),
    )
}

fn find_rho_in(rho: &Path, path: Option<&OsStr>, home: Option<PathBuf>) -> PathBuf {
    let on_path =
        || path.is_some_and(|path| std::env::split_paths(path).any(|dir| dir.join(rho).is_file()));
    if rho != Path::new("rho") || on_path() {
        return rho.to_path_buf();
    }
    home.map(|home| home.join(INSTALL_DIR).join("rho"))
        .filter(|installed| installed.is_file())
        .unwrap_or_else(|| rho.to_path_buf())
}

#[cfg(test)]
#[path = "rho_install_tests.rs"]
mod tests;
