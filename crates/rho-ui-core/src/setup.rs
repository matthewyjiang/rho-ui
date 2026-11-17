//! Starting the Rho daemon on a machine rho-ui reaches through a [`Link`],
//! and reading what the client needs of it: where its socket is, the
//! machine's home, and its login environment.

use std::{path::PathBuf, time::Duration};

use rho_protocol::ClientEnvironment;
use serde::Deserialize;

use crate::{
    daemon::DaemonError,
    link::{quote, Link, ScriptError},
    rho_install::find_rho_sh,
};

/// How long the setup script may take on the host. Over an ssh connection
/// already open (OpenSSH's `ControlMaster`, measured before rho-ui had a
/// client of its own), a whole connect to localhost, setup included, took
/// 29 ms with the daemon running; `rho daemon start` with none running
/// took 20 ms (5 runs), and `zsh -l -c env` over a fresh ssh connection
/// 2.1 s. 30 s is far past those: it catches a login profile that waits
/// for input or a home directory that hangs.
const SETUP_DEADLINE: Duration = Duration::from_secs(30);

/// The line the setup script prints before the login environment, so
/// whatever a login profile prints comes before it and is skipped.
const ENV_MARKER: &str = "rho-ui-login-environment-4f1c9a";

/// The line the setup script prints before what it reports, so whatever
/// the shell sshd runs it from prints first (bash reads `.bashrc` for an
/// ssh command, zsh reads `.zshenv`) is skipped.
const SETUP_MARKER: &str = "rho-ui-setup-7d2e40";

/// What the setup script reported.
#[derive(Debug)]
pub(crate) struct Setup {
    /// The daemon's Unix socket on the host, as its own `rho daemon
    /// status` says: a path there, whatever this machine's paths are like.
    pub(crate) socket: String,
    /// The host's `HOME`.
    pub(crate) home: PathBuf,
    /// The login environment, for sessions to run in.
    pub(crate) environment: ClientEnvironment,
}

/// Runs [`setup_script`] on `link`'s machine, starting the daemon through
/// `rho` there, and reads what it reports.
///
/// # Errors
///
/// Fails, in the script's words, if rho isn't there or its daemon doesn't
/// start, the login shell hangs, or the output is cut short.
pub(crate) async fn run(link: &Link, rho: &str) -> Result<Setup, DaemonError> {
    let host = link.name();
    let script = setup_script(rho);
    let stdout = link
        .script(script.into_bytes(), Some(SETUP_DEADLINE))
        .await
        .map_err(|error| {
            let why = match error {
                ScriptError::Session(error) => return error,
                ScriptError::TimedOut(deadline) => format!(
                    "{host} didn't finish starting rho within {} s: check that a login shell \
                     there starts without waiting for input",
                    deadline.as_secs()
                ),
                ScriptError::Cut => {
                    format!("the connection to {host} closed while starting rho there")
                }
                ScriptError::Refused => {
                    format!("couldn't start the Rho daemon on {host}: sshd refused to run sh")
                }
                ScriptError::Failed(why) => {
                    format!("couldn't start the Rho daemon on {host}: {why}")
                }
            };
            DaemonError::Unreachable(why)
        })?;
    parse_setup(&String::from_utf8_lossy(&stdout)).map_err(|problem| {
        DaemonError::Unreachable(format!("couldn't read {host}'s Rho setup: {problem}"))
    })
}

/// The POSIX sh script [`run`] feeds `sh -s` on the host, so the user's
/// login shell, which may be zsh or fish, never parses it. It finds `rho`
/// as [`find_rho_sh`] says and starts the daemon through it, as
/// `rho daemon start` does locally, then prints, after [`SETUP_MARKER`],
/// `HOME`, `rho daemon status --json` on one line (JSON keeps newlines
/// only between tokens), and after [`ENV_MARKER`], the login shell's
/// `env`. `env -0` would be easier to read, but macOS and the BSDs lack it.
fn setup_script(rho: &str) -> String {
    let rho = quote(rho);
    format!(
        r#"rho={rho}
{find_rho}if ! command -v "$rho" >/dev/null 2>&1; then
  echo "there is no program $rho; a non-interactive ssh command's PATH may lack it, so set this machine's Rho path in Settings to an absolute path" >&2
  exit 127
fi
"$rho" daemon start </dev/null >/dev/null || exit
status=$("$rho" daemon status --json </dev/null) || {{
  echo "\`$rho daemon status --json\` failed; rho-ui reads the daemon's socket from it, so update rho there" >&2
  exit 1
}}
printf '\n%s\n%s\n' {SETUP_MARKER} "$HOME"
printf '%s\n' "$status" | tr '\n' ' '
echo
exec "${{SHELL:-/bin/sh}}" -l -c 'printf "\n%s\n" {ENV_MARKER}; env'
"#,
        find_rho = find_rho_sh!(),
    )
}

/// What of `rho daemon status --json` the client reads.
#[derive(Deserialize)]
struct DaemonStatus {
    socket: String,
}

/// Reads [`setup_script`]'s output.
///
/// # Errors
///
/// Fails, saying what is missing, when the output is cut short.
fn parse_setup(output: &str) -> Result<Setup, String> {
    let (_, output) = output
        .rsplit_once(&format!("\n{SETUP_MARKER}\n"))
        .ok_or("it printed nothing it should")?;
    let mut lines = output.splitn(3, '\n');
    let mut line = || lines.next().ok_or("it printed less than it should");
    let home = line()?;
    let status = line()?;
    let login = line()?;
    if home.is_empty() {
        return Err("it printed no HOME".to_owned());
    }
    let status: DaemonStatus = serde_json::from_str(status).map_err(|error| {
        format!("`rho daemon status --json` printed no socket ({error}); update rho there")
    })?;
    let (_, env) = login.split_once(&format!("\n{ENV_MARKER}\n")).ok_or(
        "its login shell printed no environment; check that `$SHELL -l -c env` works there",
    )?;
    Ok(Setup {
        socket: status.socket,
        home: PathBuf::from(home),
        environment: parse_env(env),
    })
}

/// Variables of the setup's own ssh session, wrong for any other.
const SETUP_SESSION_VARS: [&str; 3] = ["SSH_CLIENT", "SSH_CONNECTION", "SSH_TTY"];

/// Reads `env`'s output: `NAME=value` lines, where a line that does not
/// start with a word and `=` continues the value before it, which held a
/// newline. Entries whose name isn't a variable's, such as the
/// `BASH_FUNC_name%%=() {` lines of functions bash exports, are dropped
/// with the lines that continue them. A value whose own lines look like
/// `NAME=value` reads wrong; `env` gives no way to tell.
fn parse_env(output: &str) -> ClientEnvironment {
    let mut vars: Vec<(String, String)> = Vec::new();
    let output = output.strip_suffix('\n').unwrap_or(output);
    for line in output.split('\n') {
        match line.split_once('=') {
            Some((name, value)) if !name.is_empty() && !name.contains(char::is_whitespace) => {
                vars.push((name.to_owned(), value.to_owned()));
            }
            _ => {
                if let Some((_, value)) = vars.last_mut() {
                    value.push('\n');
                    value.push_str(line);
                }
            }
        }
    }
    vars.into_iter()
        .filter(|(name, _)| is_name(name) && !SETUP_SESSION_VARS.contains(&name.as_str()))
        .collect()
}

/// Whether `name` is a shell variable name: `[A-Za-z_][A-Za-z0-9_]*`.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn vars(environment: ClientEnvironment) -> Vec<(String, String)> {
        environment.into_iter().collect()
    }

    /// Runs the setup script's first lines, up to starting rho, in `sh` with
    /// a stub `rho` that prints the path it ran as.
    #[cfg(unix)]
    #[test]
    fn the_script_quotes_the_rho_program_and_expands_a_leading_tilde() {
        use std::process::Command;

        use crate::fake_daemon::write_program;

        let dir = tempfile::tempdir().expect("a temporary directory");
        let home = dir.path();
        let odd = "it's rho";
        for path in [home.join(odd), home.join("bin/rho")] {
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("the parent");
            write_program(&path, "#!/bin/sh\necho \"ran $0\" >&2\nexit 3\n");
        }
        for (rho, ran) in [
            (home.join(odd).display().to_string(), home.join(odd)),
            ("~/bin/rho".to_owned(), home.join("bin/rho")),
        ] {
            let output = Command::new("sh")
                .arg("-c")
                .arg(setup_script(&rho))
                .env("HOME", home)
                .output()
                .expect("sh runs");
            assert_eq!(output.status.code(), Some(3), "rho {rho:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                stderr.trim(),
                format!("ran {}", ran.display()),
                "rho {rho:?}"
            );
        }

        let missing = Command::new("sh")
            .arg("-c")
            .arg(setup_script("no-such-rho-program"))
            .output()
            .expect("sh runs");
        assert_eq!(missing.status.code(), Some(127));
    }

    #[test]
    fn env_output_reads_multiline_values_and_drops_the_setup_sessions_vars() {
        let env = "HOME=/home/me\nNOTE=one\n two\n\nSSH_CONNECTION=1 2 3 4\n_X=a=b\n\
                   BASH_FUNC_which%%=() {  ( alias;\n eval ${which_declare} ) | /usr/bin/which\n}\n\
                   PATH=/bin\n";
        assert_eq!(
            vars(parse_env(env)),
            [
                ("HOME".to_owned(), "/home/me".to_owned()),
                ("NOTE".to_owned(), "one\n two\n".to_owned()),
                ("PATH".to_owned(), "/bin".to_owned()),
                ("_X".to_owned(), "a=b".to_owned()),
            ],
            "an exported bash function goes, body and all"
        );
        assert_eq!(vars(parse_env("")), []);
    }

    /// The socket is the one the host's rho reports, and what login
    /// profiles print around the report is skipped.
    #[test]
    fn setup_output_reads_the_daemons_socket_and_skips_what_profiles_print() {
        let status =
            r#"{ "pid": 7, "version": "3.1.0", "socket": "/run/user/7/rho/ab/daemon.sock" } "#;
        let output = format!(
            "from .bashrc\n\n{SETUP_MARKER}\n/home/me\n{status}\nWelcome!\nNOT=a var\n{ENV_MARKER}\nPATH=/bin\nSSH_TTY=/dev/pts/1\n"
        );
        let setup = parse_setup(&output).expect("a full setup");
        assert_eq!(setup.socket, "/run/user/7/rho/ab/daemon.sock");
        assert_eq!(setup.home, Path::new("/home/me"));
        assert_eq!(
            vars(setup.environment),
            [("PATH".to_owned(), "/bin".to_owned())]
        );

        for (cut, output) in [
            ("no setup marker", "/home/me\n{}\n".to_owned()),
            ("no status", format!("\n{SETUP_MARKER}\n/home/me\n")),
            (
                "a status without a socket",
                format!("\n{SETUP_MARKER}\n/h\n{{\"pid\": 7}}\n\n{ENV_MARKER}\nA=1\n"),
            ),
            (
                "no environment",
                format!("\n{SETUP_MARKER}\n/h\n{status}\nno marker\n"),
            ),
        ] {
            assert!(parse_setup(&output).is_err(), "{cut}");
        }
    }
}
