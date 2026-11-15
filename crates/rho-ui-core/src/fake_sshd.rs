//! An ssh server for tests, in the test's own process: russh's server
//! side on 127.0.0.1, with a host key and a client key of its own, that
//! signs in only that client key. It runs each `exec` request with this
//! machine's `sh`, in a home of the test's choosing and an environment of
//! only `HOME`, `PATH` (`/usr/bin:/bin`) and `SHELL` (`/bin/sh`), and
//! forwards each `direct-streamlocal` channel to the local socket it
//! names, such as a [`FakeDaemon`](crate::fake_daemon::FakeDaemon)'s.
//!
//! [`FakeSshd::target`] reaches it with a client set up as the test asks:
//! which `known_hosts` file vouches for the host, and which key files it
//! signs in with. The client uses no key agent.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use russh::{
    keys::{key::safe_rng, ssh_key::LineEnding, Algorithm, PrivateKey, PublicKey},
    server::{self, Auth, ChannelOpenHandle, Msg, PublicKeyOffer, Session, VerifiedPublicKey},
    Channel, ChannelMsg, ChannelOpenFailure, MethodKind, MethodSet,
};
use tokio::{net::TcpListener, runtime::Runtime};

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

/// A running fake ssh server. Dropping it stops it.
#[derive(Debug)]
pub struct FakeSshd {
    port: u16,
    /// Holds the client's key files and `known_hosts` files.
    dir: tempfile::TempDir,
    /// Clients that offered a key to sign in with.
    sign_ins: Arc<AtomicUsize>,
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
        let sign_ins = Arc::new(AtomicUsize::new(0));
        let sshd = Sshd {
            home: home.to_path_buf(),
            accepted: accepted.public_key().clone(),
            sign_ins: Arc::clone(&sign_ins),
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
            sign_ins,
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
            agent_keys: AgentKeys::Configured,
            identity_agent: IdentityAgent::Off,
            known_hosts: vec![self.known_hosts(host_key)],
            host_key_alias: None,
        };
        SshTarget::routed(format!("fake-sshd-{}-{number}", self.port), route)
    }

    /// The `known_hosts` file [`Self::target_with`] uses for `host_key`.
    pub fn known_hosts(&self, host_key: HostKey) -> PathBuf {
        self.dir.path().join(known_hosts_name(host_key))
    }

    /// The key file [`Self::target_with`] uses for `key`.
    pub fn key_file(&self, key: ClientKey) -> PathBuf {
        self.dir.path().join(file_name(key))
    }

    /// How many times a client offered a key to sign in with.
    pub fn sign_ins(&self) -> usize {
        self.sign_ins.load(Ordering::SeqCst)
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
    sign_ins: Arc<AtomicUsize>,
}

impl server::Handler for Sshd {
    type Error = russh::Error;

    fn auth_publickey_offered(
        &mut self,
        _user: &str,
        _offer: PublicKeyOffer<'_>,
    ) -> impl std::future::Future<Output = Result<Auth, Self::Error>> + Send {
        self.sign_ins.fetch_add(1, Ordering::SeqCst);
        std::future::ready(Ok(Auth::Accept))
    }

    fn auth_publickey(
        &mut self,
        _user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> impl std::future::Future<Output = Result<Auth, Self::Error>> + Send {
        let accepted = key.public_key.key_data() == self.accepted.key_data();
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
        tokio::spawn(run_command(channel, self.home.clone()));
        Ok(())
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
/// in `home`, its stdin from the channel, its stdout and stderr to it,
/// then sends its exit status and closes the channel.
async fn run_command(channel: Channel<Msg>, home: PathBuf) {
    let (mut read, write) = channel.split();
    let command = loop {
        match read.wait().await {
            Some(ChannelMsg::Exec { command, .. }) => break command,
            Some(_) => {}
            None => return,
        }
    };
    let spawned = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&*String::from_utf8_lossy(&command))
        .current_dir(&home)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let code = match spawned {
        Ok(mut child) => {
            let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
                (child.stdin.take(), child.stdout.take(), child.stderr.take())
            else {
                return;
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
        Err(_) => 127,
    };
    let _ = write.exit_status(code).await;
    let _ = write.eof().await;
    let _ = write.close().await;
}
