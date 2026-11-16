//! `Daemon` over ssh. The hermetic tests reach `fake_sshd`, an ssh server
//! in this process; where it runs a command, it runs this machine's `sh`
//! in a temporary home that plays the remote machine, under a real
//! pseudo-terminal when the client asks for one, and it forwards the
//! daemon's channel to a fake daemon's socket. The live test reaches a real
//! host with the user's ssh setup.

use rho_ui_core::{
    fake_sshd::{ClientKey, FakeSshd, HostKey},
    Daemon, DaemonError, DaemonTarget, Link, SshTarget,
};
use smol::block_on;

/// Connects through `fake_sshd`, which runs the setup script and the
/// scripts after it with `sh`, to a fake daemon whose socket the machine's
/// `rho daemon status --json` reports. Sessions get the machine's login
/// environment, and scripts get their arguments as they are, quotes and
/// bytes that aren't UTF-8 too. Two connects at once, and one after both
/// daemons are gone, share one ssh connection: one sign-in, which a key
/// agent such as 1Password's would ask the user to approve.
#[cfg(unix)]
#[test]
fn a_daemon_over_ssh_runs_sessions_in_the_machines_login_environment() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _, path::Path};

    use rho_ui_core::{
        fake_daemon::{FakeDaemon, Script},
        Checkout,
    };

    let dir = tempfile::tempdir().expect("a temporary directory");
    let home = dir.path().join("home");
    let fake = FakeDaemon::start(Script::default());
    install_rho(&home, &fake);
    // The login profile talks, and sets a variable over two lines.
    std::fs::write(
        home.join(".profile"),
        "echo 'Welcome to the remote machine'\nexport RHO_UI_REMOTE='from the login\nprofile'\n",
    )
    .expect("a profile");
    let sshd = FakeSshd::start(&home);

    let target = sshd.target();
    let connect = || Daemon::connect(DaemonTarget::Ssh(target.clone()));
    block_on(async {
        let (first, second) = futures::join!(connect(), connect());
        let (daemon, _events) = first.expect("the fake daemon answers over the fake sshd");
        let (other, _other_events) = second.expect("a second connect at once");
        assert_eq!(daemon.link(), other.link(), "one connection for both");
        drop(other);
        assert_eq!(daemon.info().home(), Some(home.as_path()));
        let ssh = daemon.link().expect("a remote daemon's ssh link");
        let args = [
            OsStr::new("it's"),
            OsStr::from_bytes(b"a b\xff"),
            OsStr::new(""),
        ];
        let printed = ssh
            .run_script(r#"printf '%s\0' "$HOME" "$@""#, &args)
            .await
            .expect("a script runs on the machine");
        let got: Vec<&[u8]> = printed.split(|&byte| byte == 0).collect();
        assert_eq!(
            got,
            [home.as_os_str().as_bytes(), b"it's", b"a b\xff", b"", b""]
        );

        let sessions = daemon.list_sessions().await.expect("a session list");
        let titles: Vec<_> = sessions.iter().map(|session| session.title()).collect();
        assert_eq!(titles, [Some("first"), None]);

        daemon
            .new_session(Path::new("/w/a"), Checkout::Local)
            .await
            .expect("a new session");
        drop(daemon);
        // Nothing holds the connection now; it lingers for the next.
        connect().await.expect("a connect after both daemons went");
    });
    assert_eq!(sshd.sign_ins(), 1, "one connection, made once");
    assert_eq!(
        fake.new_session_var("RHO_UI_REMOTE").as_deref(),
        Some("from the login\nprofile")
    );
    assert_eq!(
        fake.new_session_var("HOME"),
        Some(home.display().to_string())
    );
    assert_eq!(
        fake.new_session_var("CARGO_MANIFEST_DIR"),
        None,
        "this process's environment stays here"
    );
}

/// Gives the machine whose home is `home` a `rho` where the setup script
/// looks for a plain `rho` not on PATH, `~/.local/bin`, whose
/// `daemon status --json` reports `fake`'s socket.
#[cfg(unix)]
fn install_rho(home: &std::path::Path, fake: &rho_ui_core::fake_daemon::FakeDaemon) {
    std::fs::create_dir_all(home.join(".local/bin")).expect("home");
    rho_ui_core::fake_daemon::write_program(
        &home.join(".local/bin/rho"),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  'daemon status --json') printf '{{\\n  \"pid\": 7,\\n  \"socket\": \"%s\"\\n}}\\n' '{}' ;;\nesac\n",
            fake.socket().display()
        ),
    );
}

/// A terminal on the machine runs the login shell in the folder asked,
/// under a pseudo-terminal there of the type and size asked, on the
/// terminals' own ssh connection, which every shell there shares, so they
/// sign in once more in all and leave the daemon's sessions to scripts:
/// what is typed reaches
/// the shell, Enter as a terminal sends it, a CR; what it prints comes
/// back; a resize reaches the programs in it; and its exit ends it. A
/// host that refuses the pseudo-terminal leaves a shell that didn't
/// start, and says so.
#[cfg(unix)]
#[test]
fn a_remote_shell_runs_under_a_pseudo_terminal_on_the_terminals_connection() {
    use rho_ui_core::{
        fake_daemon::{FakeDaemon, Script},
        fake_sshd::TerminalEnd,
        RemoteShell, ShellError,
    };

    let dir = tempfile::tempdir().expect("a temporary directory");
    let home = dir.path().join("home");
    // A quote and a space, which the command line quotes.
    let project = home.join("it's a project");
    std::fs::create_dir_all(&project).expect("a project folder");
    // As the login shell's `$PWD` has it, through any symlink in the
    // temporary directory's path, as macOS's has.
    let project = rho_ui_core::platform::canonicalize(&project).expect("the folder resolves");
    let fake = FakeDaemon::start(Script::default());
    install_rho(&home, &fake);
    let sshd = FakeSshd::start(&home);
    block_on(async {
        let (daemon, _events) = Daemon::connect(DaemonTarget::Ssh(sshd.target()))
            .await
            .expect("the fake daemon answers over the fake sshd");
        let Some(Link::Ssh(ssh)) = daemon.link() else {
            panic!("a remote daemon's ssh link");
        };
        let RemoteShell {
            mut output,
            input,
            control,
            ended,
        } = ssh.open_shell(&project, "xterm-256color", 80, 24);
        // `$((6*7))` and the like print what the echo of the line typed
        // doesn't hold.
        input.write(br#"printf '%s|' "$TERM" "$COLORTERM" "$PWD" "$((6*7))"; stty size"#.to_vec());
        input.write(b"\r".to_vec());
        let want = format!("xterm-256color|truecolor|{}|42|24 80", project.display());
        read_until(&mut output, &want).await;

        control.resize(100, 30);
        input.write(b"stty size\r".to_vec());
        read_until(&mut output, "30 100").await;

        input.write(b"exit\r".to_vec());
        within("the shell's end", ended.wait())
            .await
            .expect("the shell exits");
        assert_eq!(
            within("sshd's end of it", sshd.terminal_ended()).await,
            Some(TerminalEnd::Exited(0))
        );

        sshd.refuse_terminals();
        let refused = ssh.open_shell(&project, "xterm-256color", 80, 24);
        match within("the refusal", refused.ended.wait()).await {
            Err(ShellError::NotStarted(why)) if why.contains("pseudo-terminal") => {}
            other => panic!("not a refused pseudo-terminal: {other:?}"),
        }
    });
    assert_eq!(
        sshd.sign_ins(),
        2,
        "the daemon's connection, and the terminals' own, which both shells shared"
    );
}

/// A machine runs at most `MAX_SHELLS` terminals' shells at once, the
/// sessions sshd allows on their connection: one more doesn't start, and
/// names the limit. Dropping a shell's control hangs up on it at once,
/// though its output and a clone of its input live on, and frees its
/// slot for another.
#[cfg(unix)]
#[test]
fn a_machine_runs_at_most_max_shells_and_a_dropped_control_hangs_up() {
    use rho_ui_core::{
        fake_daemon::{FakeDaemon, Script},
        fake_sshd::TerminalEnd,
        RemoteShell, ShellError, MAX_SHELLS,
    };

    let dir = tempfile::tempdir().expect("a temporary directory");
    let home = dir.path().join("home");
    let fake = FakeDaemon::start(Script::default());
    install_rho(&home, &fake);
    let sshd = FakeSshd::start(&home);
    block_on(async {
        let (daemon, _events) = Daemon::connect(DaemonTarget::Ssh(sshd.target()))
            .await
            .expect("the fake daemon answers over the fake sshd");
        let Some(Link::Ssh(ssh)) = daemon.link() else {
            panic!("a remote daemon's ssh link");
        };
        let open = || ssh.open_shell(&home, "xterm-256color", 80, 24);
        let mut shells = Vec::new();
        for _ in 0..MAX_SHELLS {
            let mut shell = open();
            started(&mut shell).await;
            shells.push(shell);
        }
        // Its control and output stay, so nothing but the limit ends it.
        let over = open();
        match within("the shell past the limit", over.ended.wait()).await {
            Err(ShellError::NotStarted(why)) if why.contains(&format!("at most {MAX_SHELLS}")) => {}
            other => panic!("not the limit: {other:?}"),
        }

        let RemoteShell {
            output,
            input,
            control,
            ended,
        } = shells.pop().expect("a shell");
        drop(control);
        within("the hung-up shell's end", ended.wait())
            .await
            .expect("a hang-up ends the shell");
        assert_eq!(
            within("sshd's end of it", sshd.terminal_ended()).await,
            Some(TerminalEnd::HungUp)
        );
        drop((output, input));
        started(&mut open()).await;
    });
    assert_eq!(
        sshd.sign_ins(),
        2,
        "every shell on the terminals' one connection"
    );
}

/// Waits for `shell` to answer what is typed.
#[cfg(unix)]
async fn started(shell: &mut rho_ui_core::RemoteShell) {
    // The echo of the line typed doesn't hold what `$((1+1))` prints.
    shell.input.write(b"echo started-$((1+1))\r".to_vec());
    read_until(&mut shell.output, "started-2").await;
}

/// How long a test waits on the fake sshd's shell before it fails: far
/// longer than a shell on this machine takes to answer.
#[cfg(unix)]
const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// `future`'s output, failing the test if it takes over [`WAIT`].
#[cfg(unix)]
async fn within<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    let timeout = async {
        smol::Timer::after(WAIT).await;
        None
    };
    smol::future::or(async { Some(future.await) }, timeout)
        .await
        .unwrap_or_else(|| panic!("{what} took over {WAIT:?}"))
}

/// Reads `output` until what the shell printed holds `want`.
#[cfg(unix)]
async fn read_until(output: &mut rho_ui_core::ShellOutput, want: &str) {
    let mut printed = String::new();
    let found = within(&format!("the shell printing {want:?}"), async {
        while let Some(bytes) = output.recv().await {
            printed.push_str(&String::from_utf8_lossy(&bytes));
            if printed.contains(want) {
                return true;
            }
        }
        false
    })
    .await;
    assert!(
        found,
        "the shell ended before printing {want:?}: {printed:?}"
    );
}

/// Why connecting to `target` failed.
fn refusal(target: SshTarget) -> String {
    match block_on(Daemon::connect(DaemonTarget::Ssh(target))) {
        Err(DaemonError::Unreachable(why)) => why,
        Err(other) => panic!("not unreachable: {other:?}"),
        Ok(_) => panic!("connected"),
    }
}

/// A host key `known_hosts` doesn't vouch for stops the connection before
/// any key is offered, and a changed one names the line that differs.
#[test]
fn a_host_key_not_in_known_hosts_stops_the_connection_before_signing_in() {
    let home = tempfile::tempdir().expect("a temporary directory");
    let sshd = FakeSshd::start(home.path());
    let unknown = refusal(sshd.target_with(HostKey::Unknown, &[ClientKey::Accepted]));
    assert!(unknown.contains("known_hosts"), "{unknown}");
    let changed = refusal(sshd.target_with(HostKey::Changed, &[ClientKey::Accepted]));
    let line = format!("{}:2", sshd.known_hosts(HostKey::Changed).display());
    assert!(changed.contains(&line), "{changed}");
    assert_eq!(sshd.key_offers(), 0, "no key went to an unverified host");
}

/// With no key the host accepts, signing in fails, and names a key file
/// it skipped for its passphrase.
#[test]
fn signing_in_with_no_accepted_key_names_the_key_that_needs_a_passphrase() {
    let home = tempfile::tempdir().expect("a temporary directory");
    let sshd = FakeSshd::start(home.path());
    let why =
        refusal(sshd.target_with(HostKey::Known, &[ClientKey::Refused, ClientKey::Encrypted]));
    let encrypted = sshd.key_file(ClientKey::Encrypted).display().to_string();
    assert!(why.contains(&format!("ssh-add {encrypted}")), "{why}");
    assert_eq!(
        sshd.key_offers(),
        1,
        "the refused key, not the encrypted one"
    );
}

/// Connects twice to the daemon on `$RHO_UI_LIVE_SSH`, an ssh destination
/// with rho installed (`$RHO_UI_LIVE_SSH_RHO` names its program there): the
/// second rides the first's connection. Each lists the saved sessions and
/// reads the host's home directory.
#[test]
#[ignore = "needs RHO_UI_LIVE_SSH, a host with rho, reached with the user's ssh setup"]
fn a_live_daemon_over_ssh() {
    let host = std::env::var("RHO_UI_LIVE_SSH").expect("RHO_UI_LIVE_SSH names an ssh host");
    let mut target = SshTarget::new(host);
    if let Ok(rho) = std::env::var("RHO_UI_LIVE_SSH_RHO") {
        target = target.with_rho(rho);
    }
    block_on(async {
        for attempt in ["first", "second"] {
            let started = std::time::Instant::now();
            let (daemon, _events) = Daemon::connect(DaemonTarget::Ssh(target.clone()))
                .await
                .unwrap_or_else(|error| panic!("{attempt} connect: {error}"));
            println!("{attempt} connect took {:?}", started.elapsed());
            daemon
                .list_sessions()
                .await
                .unwrap_or_else(|error| panic!("{attempt} list: {error}"));
            assert!(daemon.info().home().is_some(), "the host's home");
        }
    });
}
