//! `Daemon` over ssh. The hermetic test runs a fake `ssh` that plays the
//! remote machine in a temporary directory, with the fake daemon listening
//! where that machine's rho home puts it. The live test runs the real `ssh`.

use std::{
    path::Path,
    process::{Command, Stdio},
};

use rho_ui_core::{
    fake_daemon::{FakeDaemon, Script},
    Checkout, Daemon, DaemonTarget, SshTarget,
};
use smol::block_on;

/// Forwards stdio to the Unix socket in `argv[1]`, as `ssh -W` does.
const BRIDGE: &str = r"
import os, socket, sys, threading
daemon = socket.socket(socket.AF_UNIX)
daemon.connect(sys.argv[1])
def up():
    while data := os.read(0, 65536):
        daemon.sendall(data)
    daemon.shutdown(socket.SHUT_WR)
threading.Thread(target=up, daemon=True).start()
while data := daemon.recv(65536):
    os.write(1, data)
";

/// A fake `ssh`: no master runs, starting one succeeds, `sh -s` runs
/// locally in a clean environment whose home is `home`, and `-W` bridges
/// to the socket with Python.
fn fake_ssh(bin: &Path, home: &Path, bridge: &Path) -> String {
    format!(
        r#"#!/bin/sh
mode=run
while [ $# -gt 0 ]; do
  case $1 in
    -O) mode=check; shift 2 ;;
    -N|-f) mode=master; shift ;;
    -W) mode=bridge; socket=$2; shift 2 ;;
    -o) shift 2 ;;
    -*) shift ;;
    *) break ;;
  esac
done
shift
case $mode in
  check) exit 255 ;;
  master) exit 0 ;;
  bridge) exec python3 '{bridge}' "$socket" ;;
esac
[ "$*" = "sh -s" ] || {{ echo "fake ssh runs only sh -s, not $*" >&2; exit 2; }}
cd '{home}' && exec env -i HOME='{home}' PATH='{bin}:/usr/bin:/bin' SHELL=/bin/sh sh -s
"#,
        bridge = bridge.display(),
        home = home.display(),
        bin = bin.display(),
    )
}

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

fn have_python() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn a_daemon_over_ssh_runs_sessions_in_the_machines_login_environment() {
    if !have_python() {
        eprintln!("skipped: the fake ssh bridges its socket with python3, which is missing");
        return;
    }
    let dir = tempfile::tempdir().expect("a temporary directory");
    let (bin, home) = (dir.path().join("bin"), dir.path().join("home"));
    std::fs::create_dir_all(&bin).expect("bin");
    std::fs::create_dir_all(&home).expect("home");
    // The login profile talks, and sets a variable over two lines.
    std::fs::write(
        home.join(".profile"),
        "echo 'Welcome to the remote machine'\nexport RHO_UI_REMOTE='from the login\nprofile'\n",
    )
    .expect("a profile");
    let bridge = dir.path().join("bridge.py");
    std::fs::write(&bridge, BRIDGE).expect("the bridge");
    write_program(&bin.join("rho"), "#!/bin/sh\nexit 0\n");
    let ssh = dir.path().join("ssh");
    write_program(&ssh, &fake_ssh(&bin, &home, &bridge));
    let fake = FakeDaemon::start_in_home(Script::default(), &home.join(".rho"));

    block_on(async {
        let target = DaemonTarget::Ssh(SshTarget::new("remote").with_ssh(&ssh));
        let (daemon, _events) = Daemon::connect(target)
            .await
            .expect("the fake daemon answers over the fake ssh");
        assert_eq!(daemon.info().home(), Some(home.as_path()));

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

#[test]
fn a_failing_ssh_says_why_and_names_the_host() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let ssh = dir.path().join("ssh");
    write_program(
        &ssh,
        "#!/bin/sh\ncase \"$*\" in *-O*) exit 255 ;; esac\necho 'Host key verification failed.' >&2\nexit 255\n",
    );
    let target = DaemonTarget::Ssh(SshTarget::new("workstation").with_ssh(&ssh));
    let error = block_on(Daemon::connect(target)).expect_err("ssh fails");
    assert_eq!(
        error.to_string(),
        "couldn't start an ssh connection to workstation: Host key verification failed."
    );
}

/// Connects twice to the daemon on `$RHO_UI_LIVE_SSH`, an ssh destination
/// with rho installed (`$RHO_UI_LIVE_SSH_RHO` names its program there): the
/// second rides the first's master. Each lists the saved sessions and reads
/// the host's home directory.
/// `$RHO_UI_LIVE_SSH_PROGRAM` names the local ssh to run, such as a wrapper
/// that passes its own `UserKnownHostsFile`.
#[test]
#[ignore = "needs RHO_UI_LIVE_SSH, a host with rho, reached with the user's ssh setup"]
fn a_live_daemon_over_ssh() {
    let host = std::env::var("RHO_UI_LIVE_SSH").expect("RHO_UI_LIVE_SSH names an ssh host");
    let mut target = SshTarget::new(host);
    if let Ok(rho) = std::env::var("RHO_UI_LIVE_SSH_RHO") {
        target = target.with_rho(rho);
    }
    if let Ok(ssh) = std::env::var("RHO_UI_LIVE_SSH_PROGRAM") {
        target = target.with_ssh(ssh);
    }
    block_on(async {
        for attempt in ["first", "second"] {
            let (daemon, _events) = Daemon::connect(DaemonTarget::Ssh(target.clone()))
                .await
                .unwrap_or_else(|error| panic!("{attempt} connect: {error}"));
            daemon
                .list_sessions()
                .await
                .unwrap_or_else(|error| panic!("{attempt} list: {error}"));
            assert!(daemon.info().home().is_some(), "the host's home");
        }
    });
}
