//! Running programs on this machine, such as `git` in a folder, in the
//! background of the app (see [`platform::background_command`]), and
//! reading what they print. Each function blocks until the program ends;
//! call them off the UI thread.

use std::{
    path::Path,
    process::{Command, Output, Stdio},
};

use rho_ui_core::platform;

/// `git` with `args`, to run in `dir`.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Command {
    let mut command = platform::background_command("git");
    command.current_dir(dir).args(args);
    command
}

/// Runs `command`, and returns how it ended and what it printed.
///
/// # Errors
///
/// Says why if it can't start, as when the program isn't there.
pub(crate) fn run(command: &mut Command) -> Result<Output, String> {
    command.output().map_err(|error| {
        let program = command.get_program().to_string_lossy();
        format!("Couldn't run {program}: {error}")
    })
}

/// Why `command` failed, from its `output`: what it wrote to stderr, else
/// how it ended.
pub(crate) fn failure(command: &Command, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    match stderr.trim() {
        "" => {
            let program = command.get_program().to_string_lossy();
            format!("{program} ended with {}.", output.status)
        }
        why => why.to_owned(),
    }
}

/// What `command` printed to stdout.
///
/// # Errors
///
/// Says why, as [`failure`] does, if it can't start or fails.
pub(crate) fn stdout(command: &mut Command) -> Result<Vec<u8>, String> {
    let output = run(command)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(failure(command, &output))
    }
}

/// What `command` printed to stdout, less the newlines that end it, as
/// `$(…)` drops them in `sh`.
///
/// # Errors
///
/// As [`stdout`].
pub(crate) fn line(command: &mut Command) -> Result<Vec<u8>, String> {
    let mut line = stdout(command)?;
    let end = line
        .iter()
        .rposition(|&byte| byte != b'\n')
        .map_or(0, |at| at + 1);
    line.truncate(end);
    Ok(line)
}

/// Whether `command` starts and succeeds.
pub(crate) fn succeeds(command: &mut Command) -> bool {
    run(command).is_ok_and(|output| output.status.success())
}

/// The first `count` bytes `command` prints to stdout, after which it is
/// stopped, as `| head -c` stops it. How it ends doesn't matter.
///
/// # Errors
///
/// Says why if it can't start, or its output can't be read.
pub(crate) fn head(command: &mut Command, count: usize) -> Result<Vec<u8>, String> {
    use std::io::Read as _;

    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Couldn't run {program}: {error}"))?;
    let mut bytes = Vec::new();
    let most = u64::try_from(count).unwrap_or(u64::MAX);
    let read = child
        .stdout
        .take()
        .expect("stdout is piped")
        .take(most)
        .read_to_end(&mut bytes);
    if bytes.len() >= count {
        // The rest is past what is read, as when `head` closes the pipe.
        let _ = child.kill();
    }
    let _ = child.wait();
    read.map_err(|error| format!("Couldn't read what {program} printed: {error}"))?;
    Ok(bytes)
}

/// For tests: programs run as a fixture needs them, failing the test if
/// they fail.
#[cfg(test)]
pub(crate) mod test {
    use std::{path::Path, process::Command};

    /// Runs `git` in `dir` with `args`, as a user with a name who doesn't
    /// sign commits.
    pub(crate) fn git(dir: &Path, args: &[&str]) {
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
            .current_dir(dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// Runs the POSIX `sh` script `script` with `args`, as a remote machine
    /// runs it, and returns its stdout, or why it failed.
    #[cfg(unix)]
    pub(crate) fn sh(
        script: &str,
        args: &[impl AsRef<std::ffi::OsStr>],
    ) -> Result<Vec<u8>, String> {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script).arg("sh").args(args);
        super::stdout(&mut command)
    }
}
