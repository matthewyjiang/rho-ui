//! rho-ui's own releases on GitHub: whether one newer than this rho-ui is
//! out ([`newer_release`]), and installing it over this rho-ui
//! ([`install_release`]) with that release's own install script, the one
//! its docs tell people to run. Only an [`Installation`] the scripts made
//! can be updated in place.

#[cfg(unix)]
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::{
    platform,
    rho_install::{on_own_thread, output, succeeded},
    DaemonError,
};

/// GitHub redirects this to the newest release's page,
/// `.../releases/tag/v{version}`. A page, not GitHub's API, so a check
/// uses none of the 60 API requests an hour that rho's own update checks
/// share.
const LATEST: &str = "https://github.com/matthewyjiang/rho-ui/releases/latest";

/// How long `curl` may take to ask for [`LATEST`] or fetch an install
/// script, in seconds: each took under half a second here, so this only
/// stops one that hangs. The release's own download, inside the script,
/// has no limit: on a slow link it takes as long as it takes.
const FETCH_LIMIT_SECS: &str = "60";

/// A rho-ui that the install scripts put in place, which an update
/// replaces: on macOS `Rho.app`, elsewhere `rho-ui` (`rho-ui.exe`), the
/// names the scripts write. Another, as a renamed copy, would get the
/// release beside it instead, and offer it again after every restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installation {
    /// The program, which a restart runs.
    exe: PathBuf,
    /// The install script's setting that puts a release where `exe` is,
    /// and the folder it names.
    dir: (&'static str, PathBuf),
}

/// How the install scripts lay rho-ui out.
#[derive(Clone, Copy, Debug)]
enum Layout {
    /// macOS: `{dir}/Rho.app/Contents/MacOS/rho-ui`, `dir` in `RHO_UI_APP_DIR`.
    App,
    /// Linux and Windows: `{dir}/rho-ui`, `dir` in `RHO_UI_INSTALL_DIR`.
    Bin,
}

impl Installation {
    /// The installation of the program `exe`, a full path without
    /// symlinks, if the install scripts laid it out.
    #[must_use]
    pub fn of(exe: &Path) -> Option<Self> {
        let layout = if cfg!(target_os = "macos") {
            Layout::App
        } else {
            Layout::Bin
        };
        Self::laid_out(exe, layout)
    }

    fn laid_out(exe: &Path, layout: Layout) -> Option<Self> {
        let program = format!("rho-ui{}", std::env::consts::EXE_SUFFIX);
        if exe.file_name()? != program.as_str() {
            return None;
        }
        let dir = match layout {
            Layout::App => {
                let app = exe.parent()?.parent()?.parent()?;
                let bundled =
                    exe.parent()?.ends_with("Contents/MacOS") && app.file_name()? == "Rho.app";
                bundled.then_some(("RHO_UI_APP_DIR", app.parent()?.to_path_buf()))?
            }
            Layout::Bin => ("RHO_UI_INSTALL_DIR", exe.parent()?.to_path_buf()),
        };
        Some(Self {
            exe: exe.to_path_buf(),
            dir,
        })
    }

    /// The program, which a restart runs.
    #[must_use]
    pub fn exe(&self) -> &Path {
        &self.exe
    }
}

/// Finds out whether a release newer than `current`, this rho-ui's
/// version, is out, and returns its version, such as `0.1.2`. Runs `curl`,
/// which Linux, macOS and Windows 10 and later have.
///
/// # Errors
///
/// Fails if `curl` can't run or reach GitHub, or GitHub names no release.
pub async fn newer_release(current: &str) -> Result<Option<String>, DaemonError> {
    let current = current.to_owned();
    on_own_thread(move || {
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let output = output(platform::background_command("curl").args([
            "-fsS",
            "--max-time",
            FETCH_LIMIT_SECS,
            "-o",
            null,
            "-w",
            "%{redirect_url}",
            LATEST,
        ]))?;
        succeeded(&output, &output.stderr)?;
        newer(&String::from_utf8_lossy(&output.stdout), &current)
            .map_err(|why| DaemonError::Unreachable(format!("rho-ui's update check failed: {why}")))
    })
    .await?
}

/// The version `redirect`, where GitHub sends [`LATEST`], names, if it is
/// newer than `current`.
fn newer(redirect: &str, current: &str) -> Result<Option<String>, String> {
    let (_, tag) = redirect
        .trim()
        .rsplit_once("/tag/v")
        .ok_or_else(|| format!("GitHub sent no release, but {redirect:?}"))?;
    let latest = semver::Version::parse(tag).map_err(|error| format!("release v{tag}: {error}"))?;
    let current =
        semver::Version::parse(current).map_err(|error| format!("version {current}: {error}"))?;
    Ok((latest > current).then(|| tag.to_owned()))
}

/// Installs release `version` over `installation` with the release's
/// install script: `install.sh` with `sh` on Linux and macOS,
/// `install.ps1` with `powershell` on Windows. The script checks it
/// against the release's `SHA256SUMS`, and that it runs, before it
/// replaces anything. The running rho-ui keeps running the old program
/// until it restarts.
///
/// # Errors
///
/// Fails, with what the script printed, if it fails or can't run.
pub async fn install_release(
    version: &str,
    installation: &Installation,
) -> Result<(), DaemonError> {
    let script = format!(
        "https://raw.githubusercontent.com/matthewyjiang/rho-ui/v{version}/scripts/install.{}",
        if cfg!(windows) { "ps1" } else { "sh" }
    );
    let mut command = install_command(&script);
    let (setting, dir) = &installation.dir;
    command
        .env("RHO_UI_VERSION", format!("v{version}"))
        .env(setting, dir);
    on_own_thread(move || {
        let output = output(&mut command)?;
        succeeded(
            &output,
            &[output.stdout.as_slice(), output.stderr.as_slice()].concat(),
        )
    })
    .await?
}

/// The command that runs the install script at `url`.
#[cfg(unix)]
fn install_command(url: &str) -> std::process::Command {
    let mut command = platform::background_command("sh");
    command
        .arg("-c")
        .arg(r#"script=$(curl -fsSL --max-time "$2" "$1") || exit; printf '%s\n' "$script" | sh"#)
        .arg("sh")
        .arg(url)
        .arg(FETCH_LIMIT_SECS);
    command
}

/// The command that runs the install script at `url`.
#[cfg(windows)]
fn install_command(url: &str) -> std::process::Command {
    let mut command = platform::background_command("powershell");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
        ])
        .arg(format!("irm -TimeoutSec {FETCH_LIMIT_SECS} {url} | iex"));
    command
}

/// Starts `installation`'s program with `args` once this process has
/// exited, in this one's folder and environment, so a restart from a
/// terminal reads its arguments as the first start did. The caller quits.
///
/// # Errors
///
/// Fails if `sh` can't start.
#[cfg(unix)]
pub fn relaunch(
    installation: &Installation,
    args: impl IntoIterator<Item = OsString>,
) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    platform::background_command("sh")
        .arg("-c")
        .arg(r#"while kill -0 "$0" 2>/dev/null; do sleep 0.1; done; exec "$@""#)
        .arg(std::process::id().to_string())
        .arg(&installation.exe)
        .args(args)
        // Its own process group, so nothing that ends this one's ends it.
        .process_group(0)
        .spawn()
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::newer;

    /// Versions compare as versions, not strings, and a build newer than
    /// the latest release, as a build from main is, is offered nothing.
    #[test]
    fn only_a_newer_release_is_offered() {
        let tag = |version: &str| {
            format!("https://github.com/matthewyjiang/rho-ui/releases/tag/v{version}\n")
        };
        let cases = [
            ("0.1.2", "0.1.1", Some("0.1.2")),
            ("0.1.10", "0.1.9", Some("0.1.10")),
            ("0.1.1", "0.1.1", None),
            ("0.1.1", "0.2.0", None),
        ];
        for (latest, current, offered) in cases {
            assert_eq!(
                newer(&tag(latest), current),
                Ok(offered.map(str::to_owned)),
                "latest {latest}, current {current}"
            );
        }
        assert!(newer("", "0.1.1").is_err(), "no redirect names no release");
    }

    /// An update installs where the scripts put the running program, and
    /// only over a program laid out as they lay it out: any other would get
    /// the release beside it, and the offer again after the restart.
    #[cfg(unix)]
    #[test]
    fn an_update_installs_over_the_scripts_layout_only() {
        use super::{Installation, Layout};
        use std::path::Path;

        let cases = [
            (
                "/home/u/.local/bin/rho-ui",
                Layout::Bin,
                Some(("RHO_UI_INSTALL_DIR", "/home/u/.local/bin")),
            ),
            ("/home/u/bin/rho-ui-0.1.1", Layout::Bin, None),
            (
                "/Applications/Rho.app/Contents/MacOS/rho-ui",
                Layout::App,
                Some(("RHO_UI_APP_DIR", "/Applications")),
            ),
            (
                "/Applications/Rho 2.app/Contents/MacOS/rho-ui",
                Layout::App,
                None,
            ),
            ("/Users/u/.local/bin/rho-ui", Layout::App, None),
        ];
        for (exe, layout, dir) in cases {
            let found = Installation::laid_out(Path::new(exe), layout);
            let dir = dir.map(|(setting, dir)| (setting, Path::new(dir).to_path_buf()));
            assert_eq!(found.map(|found| found.dir), dir, "{exe}");
        }
    }
}
