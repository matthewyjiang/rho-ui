//! Reaching a rho daemon on another machine over ssh.
//!
//! One ssh master connection per host carries everything: [`connect`]
//! starts it (or reuses a live one), runs a setup script on the machine
//! that starts its daemon and reports where it listens and the login
//! environment, then forwards stdio to the daemon's socket with `ssh -W`.
//! A new ssh connection costs a key signature, which an agent such as
//! 1Password's asks the user to approve, so all but the first ride the
//! master. Measured with OpenSSH 10.6 to localhost: the master took about
//! 2 s, each call through it about 5 ms.

use std::{
    collections::HashMap,
    ffi::OsStr,
    io,
    os::unix::ffi::OsStrExt as _,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use rho_protocol::{
    Client, ClientEnvironment, ClientError, ClientHello, DaemonEndpoint, HelloResponse,
    Notifications,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStderr, Command},
    sync::oneshot,
};

use crate::daemon::{io, DaemonError, Machine};

/// A rho daemon on another machine, reached over ssh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshTarget {
    host: String,
    rho: String,
    ssh: PathBuf,
}

impl SshTarget {
    /// The daemon on `host`, any destination ssh accepts (`user@host`, or a
    /// `Host` alias from `~/.ssh/config`). It runs the program `rho` there
    /// and the local program `ssh` here; change them with [`Self::with_rho`]
    /// and [`Self::with_ssh`].
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            rho: "rho".to_owned(),
            ssh: PathBuf::from("ssh"),
        }
    }

    /// Runs `rho` on the machine to start its daemon: a name looked up on
    /// the PATH of a non-interactive ssh command, or a path, where a
    /// leading `~/` means that machine's home.
    #[must_use]
    pub fn with_rho(mut self, rho: impl Into<String>) -> Self {
        self.rho = rho.into();
        self
    }

    /// Runs `ssh` here instead of the `ssh` on PATH.
    #[must_use]
    pub fn with_ssh(mut self, ssh: impl Into<PathBuf>) -> Self {
        self.ssh = ssh.into();
        self
    }
}

/// Connects to the daemon `target` names, starting it there first if none
/// runs, and completes the handshake as `hello`. Runs on the I/O runtime.
pub(crate) async fn connect(
    target: &SshTarget,
    hello: ClientHello,
) -> Result<(Client, Notifications, HelloResponse, Machine), DaemonError> {
    let host = &target.host;
    if host.is_empty() || host.starts_with('-') {
        return Err(DaemonError::Unreachable(format!(
            "{host:?} is not an ssh destination"
        )));
    }
    let control = control_dir()
        .map_err(|error| {
            DaemonError::Unreachable(format!(
                "couldn't make a private directory for ssh's control socket: {error}"
            ))
        })
        .and_then(|dir| {
            // Named by the ssh program too, so a test's fake ssh never
            // touches the app's master for the same host.
            let key = format!("{}\0{host}", target.ssh.display());
            control_path(&dir, &key).map_err(DaemonError::Unreachable)
        })?;
    let ssh = SshLink {
        target: target.clone(),
        control,
    };
    ssh.ensure_master().await?;
    let setup = ssh.setup().await?;
    let endpoint = DaemonEndpoint::locate(&setup.rho_home, setup.xdg_runtime_dir.as_deref())
        .map_err(|error| DaemonError::Unreachable(format!("on {host}: {error}")))?;
    let (client, notifications, hello) = ssh.bridge(&endpoint.socket, hello).await?;
    let machine = Machine {
        environment: setup.environment,
        home: Some(setup.home),
        ssh: Some(ssh),
    };
    Ok((client, notifications, hello, machine))
}

/// The ssh commands to one remote machine, all through one control
/// socket: the daemon connection's own, then [`Self::run_script`]'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshLink {
    target: SshTarget,
    control: PathBuf,
}

impl SshLink {
    /// Runs the POSIX `sh` script `script` on the machine, with `args` as
    /// its positional parameters, and returns what it wrote to stdout. It
    /// runs only through the master the daemon connection opened: once that
    /// is gone this fails rather than connect again, which could ask the
    /// user's key agent for another approval.
    ///
    /// # Errors
    ///
    /// Fails if no master runs, ssh cannot run, the script exits with an
    /// error, or it runs past 60 s.
    pub async fn run_script(
        &self,
        script: &str,
        args: &[impl AsRef<OsStr>],
    ) -> Result<Vec<u8>, DaemonError> {
        let mut input = b"set --".to_vec();
        for arg in args {
            input.push(b' ');
            input.extend(quote_bytes(arg.as_ref().as_bytes()));
        }
        input.push(b'\n');
        input.extend(script.as_bytes());
        let (reply, ran) = oneshot::channel();
        let link = self.clone();
        io().spawn(async move {
            let _ = reply.send(link.run(input).await);
        });
        ran.await.map_err(|_| DaemonError::Closed)?
    }

    async fn run(&self, input: Vec<u8>) -> Result<Vec<u8>, DaemonError> {
        let host = &self.target.host;
        let alive = self
            .command()
            .args(["-O", "check", host])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|error| self.cannot_run(&error))?
            .success();
        if !alive {
            return Err(DaemonError::Unreachable(format!(
                "no ssh connection to {host}"
            )));
        }
        let mut child = self
            .multiplexed()
            .args(["-T", "-o", "ForwardAgent=no", "-o", "ForwardX11=no"])
            .args(["-o", "ClearAllForwardings=yes", host, "sh", "-s"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| self.cannot_run(&error))?;
        let mut stdin = child.stdin.take().expect("ssh's stdin is piped");
        // A failed write means ssh exited, and its stderr says why.
        let _ = stdin.write_all(&input).await;
        drop(stdin);
        let output = tokio::time::timeout(SCRIPT_DEADLINE, child.wait_with_output())
            .await
            .map_err(|_| {
                DaemonError::Unreachable(format!(
                    "a script on {host} ran past {} s",
                    SCRIPT_DEADLINE.as_secs()
                ))
            })?
            .map_err(|error| self.cannot_run(&error))?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(DaemonError::Unreachable(format!(
                "a script on {host} failed: {}",
                or_status(stderr.trim(), output.status)
            )))
        }
    }

    /// `ssh` with the options every call takes. `BatchMode`: the app has
    /// no terminal to type a password or accept a host key in. An agent
    /// still signs, and an unknown host key fails at once with ssh's
    /// message, which the user sees.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.target.ssh);
        command
            .arg("-o")
            .arg(format!("ControlPath={}", self.control.display()))
            .args(["-o", "BatchMode=yes"]);
        command
    }

    /// `ssh` that rides the master and never becomes one, nor connects on
    /// its own. When the master refuses a session, as at sshd's
    /// `MaxSessions` (10 by default), ssh would otherwise make a connection
    /// of its own, which may ask the user's key agent for an approval and
    /// counts against sshd's `MaxStartups`. `ProxyCommand=false` makes that
    /// connection fail at once, before it reaches the network; the master
    /// ignores it.
    fn multiplexed(&self) -> Command {
        let mut command = self.command();
        command.args(["-o", "ControlMaster=no", "-o", "ProxyCommand=false"]);
        command
    }

    fn cannot_run(&self, error: &io::Error) -> DaemonError {
        DaemonError::Unreachable(format!(
            "couldn't run {}: {error}",
            self.target.ssh.display()
        ))
    }

    /// Makes sure a master connection to the host runs, starting one if
    /// none does.
    ///
    /// The master runs as its own process (`-f`) with no pipes to this
    /// one: a master forked from a command whose output is piped keeps the
    /// pipes open, so waiting for that output would never end. Its stderr
    /// goes to a file, read when it fails, and removed either way.
    async fn ensure_master(&self) -> Result<(), DaemonError> {
        // One at a time per host, so two connections to a host never both
        // start a master: the second would find the socket taken and stay
        // a stray connection. Other hosts don't wait on this one's
        // approval or timeout.
        let lock = master_lock(&self.control);
        let _one_at_a_time = lock.lock().await;
        let host = &self.target.host;
        let alive = self
            .command()
            .args(["-O", "check", host])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|error| self.cannot_run(&error))?
            .success();
        if alive {
            return Ok(());
        }
        // A master that died without cleanup leaves its socket, which
        // would make the new one give up multiplexing.
        match std::fs::remove_file(&self.control) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(DaemonError::Unreachable(format!(
                    "couldn't remove the stale ssh control socket {}: {error}",
                    self.control.display()
                )))
            }
        }
        let log_path = self.control.with_extension("log");
        let log = std::fs::File::create(&log_path).map_err(|error| {
            DaemonError::Unreachable(format!("couldn't create {}: {error}", log_path.display()))
        })?;
        // ControlPersist=600: the master outlives this connection by ten
        // minutes, so a retry or reconnect within them needs no new
        // connection, and no new approval from the user's key agent.
        // ConnectTimeout bounds reaching the host, not signing in: a host
        // that doesn't answer would otherwise wait out the kernel's SYN
        // retries (tcp_syn_retries = 6 here, about 127 s). A whole first
        // connect to localhost, sign-in included, took 2.6 s, so 15 s is
        // far past any host that answers.
        let status = self
            .command()
            .args(["-o", "ControlMaster=yes", "-o", "ControlPersist=600"])
            .args(["-o", "ConnectTimeout=15"])
            // A link that died, as when the laptop sleeps or changes
            // networks, ends the master within 15 s x 3, and with it every
            // `ssh -W`, so the app sees the connection close and offers
            // Retry, instead of TCP retrying for many minutes.
            .args([
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
            ])
            // Errors only: a server's sign-in banner isn't one, and would
            // fill the app's error message.
            .args(["-o", "LogLevel=ERROR"])
            .args(["-N", "-f", host])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .status()
            .await
            .map_err(|error| self.cannot_run(&error));
        // Read only if it failed, so not kept: a running master writes on
        // to the unlinked file, freed when it exits.
        let said = match status {
            Ok(status) if !status.success() => std::fs::read_to_string(&log_path).ok(),
            _ => None,
        };
        let _ = std::fs::remove_file(&log_path);
        let status = status?;
        if status.success() {
            return Ok(());
        }
        let said = said.unwrap_or_default();
        Err(DaemonError::Unreachable(format!(
            "couldn't start an ssh connection to {host}: {}",
            or_status(said.trim(), status)
        )))
    }

    /// Runs [`setup_script`] on the host and reads what it reports.
    async fn setup(&self) -> Result<Setup, DaemonError> {
        let host = &self.target.host;
        let mut child = self
            .multiplexed()
            .args(["-T", "-o", "ForwardAgent=no", "-o", "ForwardX11=no"])
            .args(["-o", "ClearAllForwardings=yes", host, "sh", "-s"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| self.cannot_run(&error))?;
        let mut stdin = child.stdin.take().expect("ssh's stdin is piped");
        // A failed write means ssh exited, and its stderr says why.
        let _ = stdin
            .write_all(setup_script(&self.target.rho).as_bytes())
            .await;
        drop(stdin);
        let output = tokio::time::timeout(SETUP_DEADLINE, child.wait_with_output())
            .await
            .map_err(|_| {
                DaemonError::Unreachable(format!(
                    "{host} didn't finish starting rho within {} s: check that a login shell \
                     there starts without waiting for input",
                    SETUP_DEADLINE.as_secs()
                ))
            })?
            .map_err(|error| self.cannot_run(&error))?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            let said = or_status(stderr.trim(), output.status);
            // 255 is ssh's own failure; anything else is the script's.
            return Err(DaemonError::Unreachable(
                if output.status.code() == Some(255) {
                    format!("couldn't run a command on {host} over ssh: {said}")
                } else {
                    format!("couldn't start the rho daemon on {host}: {said}")
                },
            ));
        }
        parse_setup(&String::from_utf8_lossy(&output.stdout)).map_err(|problem| {
            DaemonError::Unreachable(format!("couldn't read {host}'s rho setup: {problem}"))
        })
    }

    /// Forwards stdio to `socket` on the host with `ssh -W`, and completes
    /// the handshake over it. The ssh process lives as long as the
    /// connection: dropping the client closes its stdin, and it exits.
    async fn bridge(
        &self,
        socket: &Path,
        hello: ClientHello,
    ) -> Result<(Client, Notifications, HelloResponse), DaemonError> {
        let host = &self.target.host;
        let mut child = self
            .multiplexed()
            .arg("-W")
            .arg(socket)
            .arg(host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| self.cannot_run(&error))?;
        let stdin = child.stdin.take().expect("ssh's stdin is piped");
        let stdout = child.stdout.take().expect("ssh's stdout is piped");
        let stderr = child.stderr.take().expect("ssh's stderr is piped");
        let said = tokio::spawn(read_stderr(stderr));
        match Client::handshake(tokio::io::join(stdout, stdin), hello).await {
            Ok(connected) => {
                tokio::spawn(async move {
                    // Reaps ssh once the connection ends.
                    let _ = child.wait().await;
                });
                Ok(connected)
            }
            // The daemon answered; its message names the side to update.
            Err(error @ (ClientError::VersionMismatch(_) | ClientError::Rpc { .. })) => {
                let _ = child.kill().await;
                Err(error.into())
            }
            Err(error) => {
                // ssh wrote why before it closed its end, so its stderr is
                // complete once it is gone.
                let _ = child.kill().await;
                let said = said.await.unwrap_or_default();
                let said = if said.is_empty() {
                    error.to_string()
                } else {
                    said
                };
                Err(DaemonError::Unreachable(format!(
                    "couldn't reach the rho daemon on {host} at {}: {said}",
                    socket.display()
                )))
            }
        }
    }
}

/// `said`, or when it is empty, how the command exited.
fn or_status(said: &str, status: std::process::ExitStatus) -> String {
    if said.is_empty() {
        format!("ssh exited with {status}")
    } else {
        said.to_owned()
    }
}

/// How much of ssh's stderr an error message keeps. ssh's error for a host
/// it cannot resolve measured 79 bytes, so this holds dozens of lines.
const STDERR_KEPT: usize = 4096;

/// Reads ssh's stderr to its end, keeping the first [`STDERR_KEPT`] bytes
/// and dropping the rest, so ssh never blocks on a full pipe.
async fn read_stderr(mut stderr: ChildStderr) -> String {
    let mut kept = Vec::new();
    let mut buffer = [0; 1024];
    while let Ok(read @ 1..) = stderr.read(&mut buffer).await {
        let room = STDERR_KEPT.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(room)]);
    }
    String::from_utf8_lossy(&kept).trim().to_owned()
}

/// The private directory that holds the control sockets, made if missing:
/// `$XDG_RUNTIME_DIR/rho-ui/ssh`, else one named after the user in the
/// temporary directory.
///
/// # Errors
///
/// Fails if the directory cannot be made, or other users can open it.
fn control_dir() -> io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let dir = match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(run) if run.is_absolute() => run.join("rho-ui").join("ssh"),
        _ => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".to_owned());
            std::env::temp_dir().join(format!("rho-ui-ssh-{user}"))
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

/// Bytes `bind(2)` takes in a Unix socket path, as rho-protocol counts
/// them: `sockaddr_un.sun_path` less its NUL.
#[cfg(any(target_os = "linux", target_os = "android"))]
const MAX_SOCKET_PATH_BYTES: usize = 107;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const MAX_SOCKET_PATH_BYTES: usize = 103;

/// What ssh adds to the control path while it binds the master's socket:
/// a dot and 16 random letters (`muxserver_listen` in OpenSSH's mux.c).
const MASTER_SUFFIX_BYTES: usize = 17;

/// The lock that keeps one master starting at a time for the control
/// socket `control`, one per socket for the life of the process.
fn master_lock(control: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .expect("the master locks' mutex is never held across a panic");
    Arc::clone(locks.entry(control.to_path_buf()).or_default())
}

/// The control socket for `key`, the ssh program and host, in `dir`, named
/// by a hash of `key`.
///
/// # Errors
///
/// Fails, saying the limit, when the path is too long for a socket.
fn control_path(dir: &Path, key: &str) -> Result<PathBuf, String> {
    let path = dir.join(format!("{:016x}", fnv1a(key.as_bytes())));
    let len = path.as_os_str().len() + MASTER_SUFFIX_BYTES;
    if len > MAX_SOCKET_PATH_BYTES {
        return Err(format!(
            "ssh's control socket {} would take {len} bytes while ssh binds it, over the \
             {MAX_SOCKET_PATH_BYTES}-byte limit for Unix socket paths; point XDG_RUNTIME_DIR \
             at a shorter directory",
            path.display()
        ));
    }
    Ok(path)
}

/// 64-bit FNV-1a: stable across Rust releases, unlike `DefaultHasher`, so
/// every rho-ui build finds the master another one started.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// How long the setup script may take on the host. Through the master,
/// a whole connect to localhost, setup included, took 29 ms with the
/// daemon running; `rho daemon start` with none running took 20 ms (5
/// runs), and `zsh -l -c env` over a fresh ssh connection 2.1 s. 30 s is
/// far past those: it catches a login profile that waits for input or a
/// home directory that hangs.
const SETUP_DEADLINE: Duration = Duration::from_secs(30);

/// How long [`SshLink::run_script`] waits: a tripwire for a script stuck on
/// a hung disk. rho-ui's longest, reading the git repositories of every
/// workspace, took 1.3 s for 133 worktrees (about 10 ms each), so this
/// leaves room for thousands.
const SCRIPT_DEADLINE: Duration = Duration::from_secs(60);

/// The line the setup script prints before the login environment, so
/// whatever a login profile prints comes before it and is skipped.
const ENV_MARKER: &str = "rho-ui-login-environment-4f1c9a";

/// The line the setup script prints before what it reports, so whatever
/// the shell ssh runs it from prints first (bash reads `.bashrc` for an
/// ssh command, zsh reads `.zshenv`) is skipped.
const SETUP_MARKER: &str = "rho-ui-setup-7d2e40";

/// `text` as one single-quoted POSIX shell word.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// [`quote`] for bytes, such as a path that needn't be UTF-8.
fn quote_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut quoted = vec![b'\''];
    for &byte in bytes {
        if byte == b'\'' {
            quoted.extend(br"'\''");
        } else {
            quoted.push(byte);
        }
    }
    quoted.push(b'\'');
    quoted
}

/// The POSIX sh script [`SshLink::setup`] feeds `sh -s` on the host, so the
/// user's login shell, which may be zsh or fish, never parses it. It
/// starts the daemon through `rho` as `rho daemon start` does locally,
/// then prints, after [`SETUP_MARKER`], the canonical rho home,
/// `XDG_RUNTIME_DIR` (maybe empty), `HOME`, and after [`ENV_MARKER`], the
/// login shell's `env`. `env -0`
/// would be easier to read, but macOS and the BSDs lack it.
fn setup_script(rho: &str) -> String {
    let rho = quote(rho);
    format!(
        r#"rho={rho}
case $rho in "~/"*) rho="$HOME/${{rho#??}}" ;; esac
if ! command -v "$rho" >/dev/null 2>&1; then
  echo "there is no program $rho; a non-interactive ssh command's PATH may lack it, so set this machine's rho path in Settings to an absolute path" >&2
  exit 127
fi
"$rho" daemon start </dev/null >/dev/null || exit
home=${{RHO_HOME:-$HOME/.rho}}
home=$(CDPATH= cd -P -- "$home" && pwd -P) || exit
printf '\n%s\n%s\n%s\n%s\n' {SETUP_MARKER} "$home" "${{XDG_RUNTIME_DIR-}}" "$HOME"
exec "${{SHELL:-/bin/sh}}" -l -c 'printf "\n%s\n" {ENV_MARKER}; env'
"#
    )
}

/// What the setup script reported.
#[derive(Debug)]
struct Setup {
    /// The rho home's canonical path, which names its socket.
    rho_home: PathBuf,
    xdg_runtime_dir: Option<PathBuf>,
    /// The machine's `HOME`.
    home: PathBuf,
    /// The login environment, for sessions to run in.
    environment: ClientEnvironment,
}

/// Reads [`setup_script`]'s output.
///
/// # Errors
///
/// Fails, saying what is missing, when the output is cut short.
fn parse_setup(output: &str) -> Result<Setup, &'static str> {
    let (_, output) = output
        .rsplit_once(&format!("\n{SETUP_MARKER}\n"))
        .ok_or("it printed nothing it should")?;
    let mut lines = output.splitn(4, '\n');
    let mut line = || lines.next().ok_or("it printed less than it should");
    let rho_home = line()?;
    let xdg_runtime_dir = line()?;
    let home = line()?;
    let login = line()?;
    if rho_home.is_empty() || home.is_empty() {
        return Err("it printed no rho home or no HOME");
    }
    let (_, env) = login.split_once(&format!("\n{ENV_MARKER}\n")).ok_or(
        "its login shell printed no environment; check that `$SHELL -l -c env` works there",
    )?;
    Ok(Setup {
        rho_home: PathBuf::from(rho_home),
        xdg_runtime_dir: Some(xdg_runtime_dir)
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from),
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
    use std::process::Command;

    use super::*;

    /// Writes the program `text` to `path`, runnable, from a child process. A
    /// handle written through here would leak into the processes other tests
    /// spawn meanwhile, until they exec, and running the file then fails with
    /// "Text file busy" (ETXTBSY).
    fn write_program(path: &Path, text: &str) {
        let status = Command::new("sh")
            .args([
                "-c",
                r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#,
                "sh",
                text,
            ])
            .arg(path)
            .status()
            .expect("sh runs");
        assert!(status.success(), "writing {} failed", path.display());
    }

    fn vars(environment: ClientEnvironment) -> Vec<(String, String)> {
        environment.into_iter().collect()
    }

    /// Starting a master keeps no log file, whether it starts or fails:
    /// each ssh program and host names its own, so kept ones would pile
    /// up. A failure still says what ssh wrote.
    #[test]
    fn starting_a_master_leaves_no_log() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let io = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("a runtime");
        for (name, master, said) in [
            ("ok", "exit 0", None),
            (
                "fails",
                "echo 'Permission denied.' >&2; exit 255",
                Some("Permission denied."),
            ),
        ] {
            let program = dir.path().join(format!("ssh-{name}"));
            write_program(
                &program,
                &format!("#!/bin/sh\ncase \"$*\" in *-O*) exit 255 ;; esac\n{master}\n"),
            );
            let target = SshTarget::new("box").with_ssh(&program);
            let ssh = SshLink {
                target,
                control: dir.path().join(format!("{name}.sock")),
            };
            let result = io.block_on(ssh.ensure_master());
            match said {
                None => result.expect("the master starts"),
                Some(said) => {
                    let error = result.expect_err("the master fails").to_string();
                    assert!(error.ends_with(said), "{error}");
                }
            }
        }
        let logs: Vec<PathBuf> = std::fs::read_dir(dir.path())
            .expect("read the directory")
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "log"))
            .collect();
        assert_eq!(logs, Vec::<PathBuf>::new());
    }

    /// A script gets its arguments as they are, quotes and bytes that
    /// aren't UTF-8 too, and runs only through a live master: without one
    /// it fails rather than make a connection of its own.
    #[test]
    fn scripts_get_their_arguments_verbatim_and_need_the_master() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let args = [
            OsStr::new("it's"),
            OsStr::from_bytes(b"a b\xff"),
            OsStr::new(""),
        ];
        let script = r#"for arg do printf '%s\0' "$arg"; done"#;
        for (name, master) in [("up", "exit 0"), ("down", "exit 255")] {
            let program = dir.path().join(format!("ssh-{name}"));
            // Stands in for the host: runs what it is sent with this sh.
            write_program(
                &program,
                &format!("#!/bin/sh\ncase \"$*\" in *-O*) {master} ;; esac\nexec sh -s\n"),
            );
            let link = SshLink {
                target: SshTarget::new("box").with_ssh(&program),
                control: dir.path().join(format!("{name}.sock")),
            };
            let ran = futures::executor::block_on(link.run_script(script, &args));
            if name == "up" {
                let printed = ran.expect("the script runs");
                let got: Vec<&[u8]> = printed.split(|&byte| byte == 0).collect();
                assert_eq!(got, [&b"it's"[..], b"a b\xff", b"", b""]);
            } else {
                let error = ran.expect_err("no master, no script").to_string();
                assert_eq!(error, "no ssh connection to box");
            }
        }
    }

    /// Runs the setup script's first lines, up to starting rho, in `sh` with
    /// a stub `rho` that prints the path it ran as.
    #[test]
    fn the_script_quotes_the_rho_program_and_expands_a_leading_tilde() {
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

    #[test]
    fn setup_output_skips_what_the_login_profile_prints() {
        let output = format!(
            "from .bashrc\n\n{SETUP_MARKER}\n/srv/rho\n\n/home/me\nWelcome!\nNOT=a var\n{ENV_MARKER}\nPATH=/bin\nSSH_TTY=/dev/pts/1\n"
        );
        let setup = parse_setup(&output).expect("a full setup");
        assert_eq!(setup.rho_home, Path::new("/srv/rho"));
        assert_eq!(setup.xdg_runtime_dir, None);
        assert_eq!(setup.home, Path::new("/home/me"));
        assert_eq!(
            vars(setup.environment),
            [("PATH".to_owned(), "/bin".to_owned())]
        );

        let quiet = format!("\n{SETUP_MARKER}\n/r\n/run/user/7\n/h\n\n{ENV_MARKER}\nA=1\n");
        let setup = parse_setup(&quiet).expect("a full setup");
        assert_eq!(
            setup.xdg_runtime_dir.as_deref(),
            Some(Path::new("/run/user/7"))
        );

        assert!(parse_setup(&format!("\n{SETUP_MARKER}\n/r\n\n/h\nno marker\n")).is_err());
        assert!(parse_setup(&format!("\n{SETUP_MARKER}\n/r\n")).is_err());
        assert!(parse_setup("/r\n\n/h\n").is_err(), "no setup marker");
    }

    #[test]
    fn a_control_path_leaves_room_for_the_suffix_ssh_binds_with() {
        let host = "workstation";
        let short = control_path(Path::new("/run/user/1000/rho-ui/ssh"), host)
            .expect("a short directory fits");
        assert_eq!(short.file_name().map(std::ffi::OsStr::len), Some(16));
        assert_eq!(
            short,
            control_path(Path::new("/run/user/1000/rho-ui/ssh"), host).expect("fits"),
            "one host, one path"
        );

        // "/", the directory's name, "/", 16 hex digits, and ssh's 17.
        let long = PathBuf::from(format!("/{}", "d".repeat(MAX_SOCKET_PATH_BYTES - 34)));
        let error = control_path(&long, host).expect_err("too long");
        assert!(
            error.contains(&format!("{MAX_SOCKET_PATH_BYTES}-byte")),
            "{error}"
        );
        let fits = PathBuf::from(format!("/{}", "d".repeat(MAX_SOCKET_PATH_BYTES - 35)));
        assert!(control_path(&fits, host).is_ok());
    }
}
