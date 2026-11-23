//! rho-ui's own releases on GitHub: whether one newer than this rho-ui is
//! out ([`newer_release`]), and installing it over this rho-ui
//! ([`install_release`]) with that release's own install script, the one
//! its docs tell people to run.

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

/// Installs release `version` over the rho-ui program `exe` with the
/// release's install script: `install.sh` with `sh` on Linux and macOS,
/// `install.ps1` with `powershell` on Windows. The script puts it where
/// `exe` is, checks it against the release's `SHA256SUMS`, and checks that
/// it runs before it replaces anything. The running rho-ui keeps running
/// the old program until it restarts.
///
/// # Errors
///
/// Fails, with what the script printed, if it fails or can't run.
pub async fn install_release(version: &str, exe: &Path) -> Result<(), DaemonError> {
    let script = format!(
        "https://raw.githubusercontent.com/matthewyjiang/rho-ui/v{version}/scripts/install.{}",
        if cfg!(windows) { "ps1" } else { "sh" }
    );
    let mut command = install_command(&script);
    command.env("RHO_UI_VERSION", format!("v{version}"));
    if let Some((name, dir)) = install_dir(exe) {
        command.env(name, dir);
    }
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
        .arg(r#"script=$(curl -fsSL "$1") || exit; printf '%s\n' "$script" | sh"#)
        .arg("sh")
        .arg(url);
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
        .arg(format!("irm {url} | iex"));
    command
}

/// The install script's setting that puts the release where `exe` is:
/// on macOS the folder of the `Rho.app` that holds it, elsewhere its own
/// folder.
fn install_dir(exe: &Path) -> Option<(&'static str, PathBuf)> {
    if cfg!(target_os = "macos") {
        let app = exe
            .ancestors()
            .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))?;
        Some(("RHO_UI_APP_DIR", app.parent()?.to_path_buf()))
    } else {
        Some(("RHO_UI_INSTALL_DIR", exe.parent()?.to_path_buf()))
    }
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
}
