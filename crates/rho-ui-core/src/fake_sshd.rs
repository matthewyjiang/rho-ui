//! An ssh server for tests, in the test's own process: russh's server
//! side on 127.0.0.1, with a host key and a client key of its own, that
//! signs in only that client key. It runs each `exec` request with this
//! machine's `sh`, in a home of the test's choosing and an environment of
//! only `HOME`, `PATH` (`/usr/bin:/bin`) and `SHELL` (`/bin/sh`), and
//! the variables the client sets (as an sshd whose `AcceptEnv` names
//! them all). After a `pty-req` the command runs under a real
//! pseudo-terminal of the size asked, with `TERM` as asked, and each
//! `window-change` resizes it, as sshd does; a client that closes the
//! channel first hangs up on it. [`FakeSshd::terminal_ended`] says how
//! each such session ended, and [`FakeSshd::refuse_terminals`] makes it
//! refuse pseudo-terminals, as `PermitTTY no` does. It forwards each
//! `direct-streamlocal` channel to the local socket it names, such as a
//! [`FakeDaemon`](crate::fake_daemon::FakeDaemon)'s.
//!
//! [`FakeSshd::target`] reaches it with a client set up as the test asks:
//! which `known_hosts` file vouches for the host, and which key files it
//! signs in with. The client uses no key agent.

use std::{
    ffi::OsString,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use russh::{
    keys::{key::safe_rng, ssh_key::LineEnding, Algorithm, PrivateKey, PublicKey},
    server::{self, Auth, ChannelOpenHandle, Msg, PublicKeyOffer, Session, VerifiedPublicKey},
    Channel, ChannelId, ChannelMsg, ChannelOpenFailure, ChannelReadHalf, ChannelWriteHalf,
    MethodKind, MethodSet, Pty,
};
use tokio::{
    net::TcpListener,
    runtime::Runtime,
    sync::{mpsc, Mutex},
};

use crate::{
    platform,
    ssh::{AgentKeys, IdentityAgent, Resolved, SshTarget},
};

/// Which `known_hosts` file the client checks the server's key against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKey {
    /// One that lists the server's key.
    Known,
    /// One that lists no key for the server.
    Unknown,
    /// One that lists another key of the same kind for the server, on its
    /// second line.
    Changed,
}

/// A key file the client may sign in with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientKey {
    /// The key the server accepts.
    Accepted,
    /// A key the server refuses.
    Refused,
    /// The accepted key, encrypted with a passphrase.
    Encrypted,
}

/// How a session under a pseudo-terminal ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalEnd {
    /// Its command exited with this status, after all it printed went.
    Exited(u32),
    /// The client closed the channel first, and the command, hung up on,
    /// exited.
    HungUp,
}

/// A running fake ssh server. Dropping it stops it.
#[derive(Debug)]
pub struct FakeSshd {
    port: u16,
    /// Holds the client's key files and `known_hosts` files.
    dir: tempfile::TempDir,
    /// Keys clients offered to sign in with.
    key_offers: Arc<AtomicUsize>,
    /// Clients that signed in.
    sign_ins: Arc<AtomicUsize>,
    /// Whether to refuse pseudo-terminals.
    refuse_terminals: Arc<AtomicBool>,
    /// How each session under a pseudo-terminal ended, in order.
    terminal_ends: Mutex<mpsc::UnboundedReceiver<TerminalEnd>>,
    /// Numbers each target's host, so each gets a connection of its own.
    targets: AtomicUsize,
    _runtime: Runtime,
}

impl FakeSshd {
    /// Starts a server whose commands run in `home`.
    ///
    /// # Panics
    ///
    /// If the OS refuses a thread, a port, or a temporary directory, or a
    /// key can't be made or written: a test can't go on without them.
    pub fn start(home: &Path) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory for the fake sshd");
        let host_key = random_key();
        let accepted = random_key();
        let write = |name: &str, key: &PrivateKey| {
            key.write_openssh_file(dir.path().join(name), LineEnding::LF)
                .expect("the fake sshd writes a key file");
        };
        write(file_name(ClientKey::Accepted), &accepted);
        write(file_name(ClientKey::Refused), &random_key());
        write(
            file_name(ClientKey::Encrypted),
            &accepted
                .encrypt(&mut safe_rng(), "passphrase")
                .expect("an encrypted key"),
        );

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_io()
            .enable_time()
            .build()
            .expect("a runtime for the fake sshd");
        let listener = runtime
            .block_on(TcpListener::bind("127.0.0.1:0"))
            .expect("the fake sshd binds a port");
        let port = listener
            .local_addr()
            .expect("the fake sshd's address")
            .port();
        let known = |key: &PublicKey| {
            format!(
                "[127.0.0.1]:{port} {}\n",
                key.to_openssh().expect("an OpenSSH public key")
            )
        };
        let known_hosts = [
            (HostKey::Known, known(host_key.public_key())),
            (HostKey::Unknown, String::new()),
            (
                HostKey::Changed,
                format!("# rotated\n{}", known(random_key().public_key())),
            ),
        ];
        for (which, text) in known_hosts {
            std::fs::write(dir.path().join(known_hosts_name(which)), text)
                .expect("the fake sshd writes a known_hosts file");
        }

        let config = Arc::new(server::Config {
            keys: vec![host_key],
            methods: MethodSet::from(&[MethodKind::PublicKey][..]),
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..server::Config::default()
        });
        let key_offers: Arc<AtomicUsize> = Arc::default();
        let sign_ins: Arc<AtomicUsize> = Arc::default();
        let refuse_terminals: Arc<AtomicBool> = Arc::default();
        let (terminal_ended, ends) = mpsc::unbounded_channel();
        let sshd = Sshd {
            home: home.to_path_buf(),
            accepted: accepted.public_key().clone(),
            key_offers: Arc::clone(&key_offers),
            sign_ins: Arc::clone(&sign_ins),
            refuse_terminals: Arc::clone(&refuse_terminals),
            terminal_ended,
        };
        runtime.spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (config, sshd) = (Arc::clone(&config), sshd.clone());
                tokio::spawn(async move {
                    if let Ok(session) = server::run_stream(config, stream, sshd).await {
                        let _ = session.await;
                    }
                });
            }
        });
        Self {
            port,
            dir,
            key_offers,
            sign_ins,
            refuse_terminals,
            terminal_ends: Mutex::new(ends),
            targets: AtomicUsize::new(0),
            _runtime: runtime,
        }
    }

    /// Reaches this server as a user whose `known_hosts` lists its key,
    /// signing in with the key it accepts.
    pub fn target(&self) -> SshTarget {
        self.target_with(HostKey::Known, &[ClientKey::Accepted])
    }

    /// Reaches this server with the `known_hosts` file `host_key` and the
    /// key files `keys`, in that order. Each target gets a host name, and
    /// so a connection, of its own.
    pub fn target_with(&self, host_key: HostKey, keys: &[ClientKey]) -> SshTarget {
        let number = self.targets.fetch_add(1, Ordering::Relaxed);
        let route = Resolved {
            hostname: "127.0.0.1".to_owned(),
            port: self.port,
            user: "tester".to_owned(),
            identity_files: keys.iter().map(|&key| self.key_file(key)).collect(),
            certificate_files: Vec::new(),
            skipped: Vec::new(),
            agent_keys: AgentKeys::Configured,
            identity_agent: IdentityAgent::Off,
            known_hosts: vec![self.known_hosts(host_key)],
            host_key_alias: None,
            revoked_host_keys: Vec::new(),
        };
        SshTarget::routed(format!("fake-sshd-{}-{number}", self.port), route)
    }

    /// The port it listens on, at 127.0.0.1: for an ssh config that
    /// reaches it, as a program that runs `ssh -G` reads.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The `known_hosts` file [`Self::target_with`] uses for `host_key`.
    pub fn known_hosts(&self, host_key: HostKey) -> PathBuf {
        self.dir.path().join(known_hosts_name(host_key))
    }

    /// The key file [`Self::target_with`] uses for `key`.
    pub fn key_file(&self, key: ClientKey) -> PathBuf {
        self.dir.path().join(file_name(key))
    }

    /// How many keys clients offered to sign in with.
    pub fn key_offers(&self) -> usize {
        self.key_offers.load(Ordering::SeqCst)
    }

    /// How many times a client signed in: one for each connection.
    pub fn sign_ins(&self) -> usize {
        self.sign_ins.load(Ordering::SeqCst)
    }

    /// Refuses each pseudo-terminal asked for from now on, as an sshd
    /// whose `PermitTTY` is `no`; the command then runs without one.
    pub fn refuse_terminals(&self) {
        self.refuse_terminals.store(true, Ordering::SeqCst);
    }

    /// How the next session under a pseudo-terminal ended, in the order
    /// they did. Waits for one to end.
    pub async fn terminal_ended(&self) -> Option<TerminalEnd> {
        self.terminal_ends.lock().await.recv().await
    }
}

fn file_name(key: ClientKey) -> &'static str {
    match key {
        ClientKey::Accepted => "id_accepted",
        ClientKey::Refused => "id_refused",
        ClientKey::Encrypted => "id_encrypted",
    }
}

fn known_hosts_name(host_key: HostKey) -> &'static str {
    match host_key {
        HostKey::Known => "known_hosts_known",
        HostKey::Unknown => "known_hosts_unknown",
        HostKey::Changed => "known_hosts_changed",
    }
}

fn random_key() -> PrivateKey {
    PrivateKey::random(&mut safe_rng(), Algorithm::Ed25519).expect("an Ed25519 key")
}

/// The server's side of one client's connection.
#[derive(Clone)]
struct Sshd {
    home: PathBuf,
    accepted: PublicKey,
    key_offers: Arc<AtomicUsize>,
    sign_ins: Arc<AtomicUsize>,
    refuse_terminals: Arc<AtomicBool>,
    terminal_ended: mpsc::UnboundedSender<TerminalEnd>,
}

impl server::Handler for Sshd {
    type Error = russh::Error;

    fn auth_publickey_offered(
        &mut self,
        _user: &str,
        _offer: PublicKeyOffer<'_>,
    ) -> impl std::future::Future<Output = Result<Auth, Self::Error>> + Send {
        self.key_offers.fetch_add(1, Ordering::SeqCst);
        std::future::ready(Ok(Auth::Accept))
    }

    fn auth_publickey(
        &mut self,
        _user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> impl std::future::Future<Output = Result<Auth, Self::Error>> + Send {
        let accepted = key.public_key.key_data() == self.accepted.key_data();
        if accepted {
            self.sign_ins.fetch_add(1, Ordering::SeqCst);
        }
        std::future::ready(Ok(if accepted {
            Auth::Accept
        } else {
            Auth::reject()
        }))
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        tokio::spawn(run_session(channel, self.clone()));
        Ok(())
    }

    // `run_session` takes the request from the channel; this answers it,
    // as sshd does.
    fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
        std::future::ready(if self.refuse_terminals.load(Ordering::SeqCst) {
            session.channel_failure(channel)
        } else {
            session.channel_success(channel)
        })
    }

    fn exec_request(
        &mut self,
        channel: ChannelId,
        _command: &[u8],
        session: &mut Session,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send {
        std::future::ready(session.channel_success(channel))
    }

    async fn channel_open_direct_streamlocal(
        &mut self,
        channel: Channel<Msg>,
        socket_path: &str,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        match platform::endpoint::connect(Path::new(socket_path)).await {
            Ok(mut daemon) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut channel = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut channel, &mut daemon).await;
                });
            }
            Err(_) => reply.reject(ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }
}

/// Runs the command of the session channel's `exec` request with `sh -c`
/// in `sshd`'s home: under a pseudo-terminal if the client asked for one
/// first and `sshd` didn't refuse it, else with its stdin from the channel
/// and its stdout and stderr to it. Then sends its exit status and closes
/// the channel; a session under a pseudo-terminal tells `sshd` how it
/// ended.
async fn run_session(channel: Channel<Msg>, sshd: Sshd) {
    let home = sshd.home;
    let (mut read, write) = channel.split();
    let mut env: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), home.clone().into_os_string()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("SHELL".into(), "/bin/sh".into()),
    ];
    let mut terminal = None;
    let command = loop {
        match read.wait().await {
            Some(ChannelMsg::RequestPty {
                term,
                col_width,
                row_height,
                ..
            }) if !sshd.refuse_terminals.load(Ordering::SeqCst) => {
                env.push(("TERM".into(), term.into()));
                terminal = Some(pty_size(col_width, row_height));
            }
            Some(ChannelMsg::SetEnv {
                variable_name,
                variable_value,
                ..
            }) => env.push((variable_name.into(), variable_value.into())),
            Some(ChannelMsg::Exec { command, .. }) => {
                break String::from_utf8_lossy(&command).into_owned();
            }
            Some(_) => {}
            None => return,
        }
    };
    let code = match terminal {
        Some(size) => {
            let end = in_terminal(&mut read, &write, &command, &home, &env, size).await;
            let _ = sshd.terminal_ended.send(end);
            match end {
                TerminalEnd::Exited(code) => code,
                TerminalEnd::HungUp => return,
            }
        }
        None => piped(&mut read, &write, &command, &home, &env).await,
    };
    let _ = write.exit_status(code).await;
    let _ = write.eof().await;
    let _ = write.close().await;
}

/// Runs `command` with `sh -c` in `home` with `env`, its stdin from the
/// channel and its stdout and stderr to it, and returns its exit status.
async fn piped(
    read: &mut ChannelReadHalf,
    write: &ChannelWriteHalf<Msg>,
    command: &str,
    home: &Path,
    env: &[(OsString, OsString)],
) -> u32 {
    let spawned = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(home)
        .env_clear()
        .envs(env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = spawned else {
        return 127;
    };
    let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return 127;
    };
    let (mut out, mut err) = (write.make_writer(), write.make_writer_ext(Some(1)));
    let feed = async {
        let _ = tokio::io::copy(&mut read.make_reader(), &mut stdin).await;
        drop(stdin);
    };
    let _ = tokio::join!(
        feed,
        tokio::io::copy(&mut stdout, &mut out),
        tokio::io::copy(&mut stderr, &mut err)
    );
    child
        .wait()
        .await
        .ok()
        .and_then(|status| status.code())
        .and_then(|code| u32::try_from(code).ok())
        .unwrap_or(255)
}

/// Runs `command` with `sh -c` in `home` with `env`, under a
/// pseudo-terminal of `size`: what it prints goes to the channel, the
/// channel's data to it, and each `window-change` resizes it. Returns its
/// exit status once it exits, as sshd sends it, after what it printed
/// that has reached the server: a background job that keeps the
/// pseudo-terminal open holds up nothing. A client that closes the
/// channel first hangs up on it.
async fn in_terminal(
    read: &mut ChannelReadHalf,
    write: &ChannelWriteHalf<Msg>,
    command: &str,
    home: &Path,
    env: &[(OsString, OsString)],
    size: PtySize,
) -> TerminalEnd {
    let Ok(pair) = native_pty_system().openpty(size) else {
        return TerminalEnd::Exited(127);
    };
    let mut shell = CommandBuilder::new("sh");
    shell.args(["-c", command]);
    shell.cwd(home);
    shell.env_clear();
    for (name, value) in env {
        shell.env(name, value);
    }
    let Ok(mut child) = pair.slave.spawn_command(shell) else {
        return TerminalEnd::Exited(127);
    };
    drop(pair.slave);
    let mut killer = child.clone_killer();
    let (Ok(mut reader), Ok(mut writer)) =
        (pair.master.try_clone_reader(), pair.master.take_writer())
    else {
        let _ = killer.kill();
        return TerminalEnd::Exited(127);
    };
    // Threads of their own, as a read and the wait block: not the
    // runtime's blocking pool, which dropping the runtime waits on.
    let (printed, mut output) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    std::thread::spawn(move || {
        let mut buffer = [0; 4096];
        while let Ok(read @ 1..) = reader.read(&mut buffer) {
            if printed.blocking_send(buffer[..read].to_vec()).is_err() {
                return;
            }
        }
    });
    let (exited, mut exit) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = exited.send(child.wait());
    });
    loop {
        tokio::select! {
            Some(bytes) = output.recv() => {
                let _ = write.data_bytes(bytes).await;
            }
            message = read.wait() => match message {
                // A blocking write, but a test types far less than the
                // pseudo-terminal holds.
                Some(ChannelMsg::Data { data }) => {
                    let _ = writer.write_all(&data);
                }
                Some(ChannelMsg::WindowChange {
                    col_width,
                    row_height,
                    ..
                }) => {
                    let _ = pair.master.resize(pty_size(col_width, row_height));
                }
                Some(_) => {}
                None => {
                    // SIGHUP, as sshd's closing the pseudo-terminal sends.
                    let _ = killer.kill();
                    let _ = (&mut exit).await;
                    return TerminalEnd::HungUp;
                }
            },
            status = &mut exit => {
                while let Ok(bytes) = output.try_recv() {
                    let _ = write.data_bytes(bytes).await;
                }
                return TerminalEnd::Exited(
                    status
                        .ok()
                        .and_then(Result::ok)
                        .map_or(255, |status| status.exit_code()),
                );
            }
        }
    }
}

/// A pseudo-terminal of `columns` by `rows`, as a `pty-req` or
/// `window-change` gives them.
fn pty_size(columns: u32, rows: u32) -> PtySize {
    PtySize {
        rows: u16::try_from(rows).unwrap_or(u16::MAX),
        cols: u16::try_from(columns).unwrap_or(u16::MAX),
        pixel_width: 0,
        pixel_height: 0,
    }
}
