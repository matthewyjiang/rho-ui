//! `Daemon` over ssh. The hermetic tests reach `fake_sshd`, an ssh server
//! in this process; where it runs a command, it runs this machine's `sh`
//! in a temporary home that plays the remote machine, and it forwards the
//! daemon's channel to a fake daemon's socket. The live test reaches a real
//! host with the user's ssh setup.

use rho_ui_core::{
    fake_sshd::{ClientKey, FakeSshd, HostKey},
    Daemon, DaemonError, DaemonTarget, SshTarget,
};
use smol::block_on;

/// Connects through `fake_sshd`, which runs the setup script and the
/// scripts after it with `sh`, to a fake daemon whose socket the machine's
/// `rho daemon status --json` reports. Sessions get the machine's login
/// environment, and scripts get their arguments as they are, quotes and
/// bytes that aren't UTF-8 too.
#[cfg(unix)]
#[test]
fn a_daemon_over_ssh_runs_sessions_in_the_machines_login_environment() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _, path::Path};

    use rho_ui_core::{
        fake_daemon::{write_program, FakeDaemon, Script},
        Checkout,
    };

    let dir = tempfile::tempdir().expect("a temporary directory");
    let home = dir.path().join("home");
    // Where the setup script looks for a plain `rho` not on PATH.
    std::fs::create_dir_all(home.join(".local/bin")).expect("home");
    // The login profile talks, and sets a variable over two lines.
    std::fs::write(
        home.join(".profile"),
        "echo 'Welcome to the remote machine'\nexport RHO_UI_REMOTE='from the login\nprofile'\n",
    )
    .expect("a profile");
    let fake = FakeDaemon::start(Script::default());
    write_program(
        &home.join(".local/bin/rho"),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  'daemon status --json') printf '{{\\n  \"pid\": 7,\\n  \"socket\": \"%s\"\\n}}\\n' '{}' ;;\nesac\n",
            fake.socket().display()
        ),
    );
    let sshd = FakeSshd::start(&home);

    block_on(async {
        let (daemon, _events) = Daemon::connect(DaemonTarget::Ssh(sshd.target()))
            .await
            .expect("the fake daemon answers over the fake sshd");
        assert_eq!(daemon.info().home(), Some(home.as_path()));
        let ssh = daemon.ssh().expect("a remote daemon's ssh link");
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
    });
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
    assert_eq!(sshd.sign_ins(), 0, "no key went to an unverified host");
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
    assert_eq!(sshd.sign_ins(), 1, "the refused key, not the encrypted one");
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
