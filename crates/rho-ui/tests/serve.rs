//! `rho-ui serve`, run as the command, reached as rho-ui's web build reaches
//! it. Its home is a temporary one that plays the server's machine, with a
//! `rho` whose `daemon status --json` names a fake daemon's socket.
#![cfg(unix)]

use std::{
    ffi::OsStr,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    net::TcpStream,
    os::unix::ffi::OsStrExt as _,
    path::Path,
    pin::pin,
    process::{Child, ChildStderr, Command, Stdio},
    time::Duration,
};

use futures::{
    channel::oneshot,
    executor::block_on,
    future::{select, Either},
};
use rho_ui_core::{
    fake_daemon::{write_program, FakeDaemon, Script},
    fake_sshd::{ClientKey, FakeSshd, HostKey},
    server::wire::{self, SettingsFile, StateWrite},
    Checkout, Daemon, DaemonError, DaemonTarget, RemoteShell, ServerTarget, ShellError,
};

/// `rho-ui serve`, running until dropped.
struct Server {
    child: Child,
    /// Its stderr, kept open so its later messages have somewhere to go.
    _stderr: BufReader<ChildStderr>,
    /// As `127.0.0.1:PORT`.
    address: String,
    token: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// Starts it on a free port, with `home` as its home, `sh` as its login
    /// shell, and a web build of one page, and reads the address and token
    /// it prints.
    fn start(home: &Path) -> Self {
        let web = home.join("web");
        std::fs::create_dir_all(&web).expect("a web build folder");
        std::fs::write(web.join("index.html"), "<p>rho-ui</p>").expect("a page");
        let mut child = Command::new(env!("CARGO_BIN_EXE_rho-ui"))
            .args(["serve", "--port", "0", "--web"])
            .arg(&web)
            .env_clear()
            // Not this machine's PATH: a `rho` on it would start a real
            // daemon in place of the fake one. A test's own programs go
            // in `~/bin`.
            .env("PATH", format!("{}/bin:/usr/bin:/bin", home.display()))
            .env("HOME", home)
            .env("SHELL", "/bin/sh")
            .stderr(Stdio::piped())
            .spawn()
            .expect("rho-ui serve starts");
        let mut line = String::new();
        let mut stderr = BufReader::new(child.stderr.take().expect("piped"));
        stderr
            .read_line(&mut line)
            .expect("rho-ui serve says where it listens");
        let url = line
            .trim()
            .strip_prefix("rho-ui serve: open http://")
            .unwrap_or_else(|| panic!("an address, not {line:?}"));
        let (address, token) = url.split_once("/#token=").expect("a token");
        Self {
            address: address.to_owned(),
            token: token.to_owned(),
            child,
            _stderr: stderr,
        }
    }

    fn target(&self) -> ServerTarget {
        ServerTarget::new(format!("http://{}", self.address), &self.token)
    }

    /// Sends `request` (headers, without the blank line that ends them) and
    /// returns the response's status line and headers. It reads only to
    /// the headers' end: the connection stays open, kept alive or upgraded.
    fn http(&self, request: &str) -> String {
        let mut stream = TcpStream::connect(&self.address).expect("the server listens");
        write!(stream, "{request}\r\nHost: {}\r\n\r\n", self.address).expect("a request");
        let mut response = Vec::new();
        let mut byte = [0];
        while !response.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => response.push(byte[0]),
                _ => break,
            }
        }
        String::from_utf8_lossy(&response).trim_end().to_owned()
    }
}

/// How long the shell's 3 MiB may take: all of it takes 0.1 s here, so
/// this only stops a test whose output stalled from hanging the run.
const SHELL_BOUND: Duration = Duration::from_secs(60);

/// Resolves once `limit` passes, timed on a thread of its own.
fn after(limit: Duration) -> oneshot::Receiver<()> {
    let (done, passed) = oneshot::channel();
    std::thread::spawn(move || {
        std::thread::sleep(limit);
        let _ = done.send(());
    });
    passed
}

/// Gives the machine whose home is `home` a `rho` where the setup script
/// looks for one not on PATH, `~/.local/bin`, whose `daemon status --json`
/// names `fake`'s socket.
fn install_rho(home: &Path, fake: &FakeDaemon) {
    std::fs::create_dir_all(home.join(".local/bin")).expect("home");
    write_program(
        &home.join(".local/bin/rho"),
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  'daemon status --json') printf '{{\"pid\": 7, \"socket\": \"%s\"}}\\n' '{}' ;;\nesac\n",
            fake.socket().display()
        ),
    );
}

/// Through `rho-ui serve`, a client reaches the daemon on the server's
/// machine, whose sessions run in that machine's login environment; runs
/// scripts there with their arguments as they are, quotes and bytes that
/// aren't UTF-8 too; and runs the terminal's shell there, in its folder,
/// at the size it was last given, with all it prints however much: what
/// rho-ui's web build does in a browser.
#[test]
fn a_daemon_through_the_server_runs_sessions_scripts_and_shells_on_its_machine() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let home = dir.path().join("home");
    let fake = FakeDaemon::start(Script::default());
    install_rho(&home, &fake);
    std::fs::write(
        home.join(".profile"),
        "echo 'Welcome'\nexport RHO_UI_SERVED='from the login\nprofile'\n",
    )
    .expect("a profile");
    let server = Server::start(&home);

    block_on(async {
        let (daemon, _events) = Daemon::connect(DaemonTarget::Server(server.target()))
            .await
            .expect("the fake daemon answers through the server");
        assert_eq!(daemon.info().home(), Some(home.as_path()));
        let link = daemon.link().expect("the server's link");
        let args = [
            OsStr::new("it's"),
            OsStr::from_bytes(b"a b\xff"),
            OsStr::new(""),
        ];
        let printed = link
            .run_script(r#"printf '%s\0' "$HOME" "$@""#, &args)
            .await
            .expect("a script runs on the machine");
        let got: Vec<&[u8]> = printed.split(|&byte| byte == 0).collect();
        assert_eq!(
            got,
            [home.as_os_str().as_bytes(), b"it's", b"a b\xff", b"", b""]
        );
        let failed = link
            .run_script("echo 'no such thing' >&2; exit 3", &[""; 0])
            .await
            .expect_err("a failing script");
        assert!(
            failed.to_string().ends_with("failed: no such thing"),
            "{failed}"
        );

        let work = home.join("work");
        std::fs::create_dir_all(&work).expect("a folder for the shell");
        let RemoteShell {
            mut output,
            input,
            control,
            ended,
        } = link.open_shell(&work, "xterm-256color", 80, 24);
        control.resize(100, 30);
        // Three windows of output (`wire::SHELL_WINDOW`) of `#` (`\043`,
        // which the echoed command and a temporary folder's name lack): it
        // all comes only if the client says what it took, else the read
        // hangs. The shell exits only once all of it came, as output still
        // on its way when it hangs up can be lost.
        input.write(
            b"stty size; pwd; head -c 3145728 /dev/zero | tr '\\0' '\\043'; echo DONE-$((6*7))\n"
                .to_vec(),
        );
        let read_all = async {
            let mut printed = Vec::new();
            let mut exited = false;
            while let Some(bytes) = output.recv().await {
                printed.extend(bytes);
                if !exited && printed.windows(7).any(|window| window == b"DONE-42") {
                    input.write(b"exit\n".to_vec());
                    exited = true;
                }
            }
            printed
        };
        let printed = match select(pin!(read_all), after(SHELL_BOUND)).await {
            Either::Left((printed, _)) => printed,
            Either::Right(_) => panic!("the shell's output stopped coming"),
        };
        ended.wait().await.expect("the shell exits");
        drop(control);
        // The terminal echoes the typed line too, so look for each answer
        // on a line of its own.
        let printed = String::from_utf8_lossy(&printed);
        assert!(printed.contains("\n30 100\r\n"), "{printed:?}");
        assert!(
            printed.contains(&format!("\n{}\r\n", work.display())),
            "{printed:?}"
        );
        assert_eq!(printed.matches('#').count(), 3 * wire::SHELL_WINDOW);

        daemon
            .new_session(&home, Checkout::Local)
            .await
            .expect("a new session");
    });
    assert_eq!(
        fake.new_session_var("RHO_UI_SERVED").as_deref(),
        Some("from the login\nprofile")
    );
    assert_eq!(
        fake.new_session_var("HOME"),
        Some(home.display().to_string())
    );
}

/// Only a page that holds the token gets through: the page's files are
/// served to anyone, and set no cookie that a browser would send to every
/// other server on the host; a WebSocket must offer the token as its
/// subprotocol, which the server names back, and come from a page this
/// server served, not one of another server on the machine.
#[test]
fn only_a_page_holding_the_token_reaches_the_server() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let home = dir.path().join("home");
    let fake = FakeDaemon::start(Script::default());
    install_rho(&home, &fake);
    let server = Server::start(&home);
    let ours = format!("Origin: http://{}", server.address);
    let protocol = |token: &str| format!("Sec-WebSocket-Protocol: {}", wire::token_protocol(token));
    let upgrade = |headers: &[&str]| {
        server.http(&format!(
            "GET /script HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n{}",
            headers.join("\r\n")
        ))
    };

    let page = server.http("GET / HTTP/1.1");
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    assert!(!page.to_ascii_lowercase().contains("set-cookie"), "{page}");
    for refused in [
        upgrade(&[&ours]),
        upgrade(&[&ours, &protocol("wrong")]),
        upgrade(&[&ours, &format!("Cookie: rho-ui-token={}", server.token)]),
    ] {
        assert!(refused.starts_with("HTTP/1.1 401"), "{refused}");
    }
    let elsewhere = upgrade(&["Origin: http://127.0.0.1:1", &protocol(&server.token)]);
    assert!(elsewhere.starts_with("HTTP/1.1 403"), "{elsewhere}");
    let accepted = upgrade(&[&ours, &protocol(&server.token)]);
    assert!(accepted.starts_with("HTTP/1.1 101"), "{accepted}");
    assert!(
        accepted
            .to_ascii_lowercase()
            .contains(&protocol(&server.token).to_ascii_lowercase()),
        "{accepted}"
    );

    let wrong_token = ServerTarget::new(format!("http://{}", server.address), "wrong");
    match block_on(Daemon::connect(DaemonTarget::Server(wrong_token))) {
        Err(DaemonError::Unreachable(why)) => assert!(why.contains("401"), "{why}"),
        other => panic!("expected a refusal, got {:?}", other.map(|_| ())),
    }
}

/// The page shares the desktop app's settings file and state database on
/// the server's machine: it loads them as they are, and what it writes
/// back is in both files once the server has applied it.
#[test]
fn the_page_shares_the_desktop_apps_settings_and_state() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let home = dir.path().join("home");
    let fake = FakeDaemon::start(Script::default());
    install_rho(&home, &fake);
    let config = home.join(".config/rho-ui");
    std::fs::create_dir_all(&config).expect("a config folder");
    std::fs::write(config.join("settings"), "appearance = light\n").expect("settings");
    let server = Server::start(&home);

    let shared = block_on(server.target().open_state()).expect("the state loads");
    assert_eq!(
        shared.settings,
        SettingsFile::Text {
            text: "appearance = light\n".to_owned()
        }
    );
    let mut copy = rusqlite::Connection::open_in_memory().expect("SQLite in memory");
    copy.deserialize_read_exact(
        rusqlite::MAIN_DB,
        shared.database.as_slice(),
        shared.database.len(),
        false,
    )
    .expect("the image loads");
    let tables: i64 = copy
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name = 'kv'",
            [],
            |row| row.get(0),
        )
        .expect("the schema reads");
    assert_eq!(tables, 1);

    shared.writer.write(StateWrite::Sql {
        sql: "INSERT INTO kv (key, value) VALUES ('from-page', 'yes')".to_owned(),
    });
    shared.writer.write(StateWrite::Settings {
        text: "appearance = dark\n".to_owned(),
    });
    drop(shared.writer);
    block_on(shared.done.wait());

    let settings = std::fs::read_to_string(config.join("settings")).expect("settings");
    assert_eq!(settings, "appearance = dark\n");
    let db = rusqlite::Connection::open(home.join(".local/state/rho-ui/state.sqlite3"))
        .expect("the state database");
    let value: String = db
        .query_row("SELECT value FROM kv WHERE key = 'from-page'", [], |row| {
            row.get(0)
        })
        .expect("the page's row");
    assert_eq!(value, "yes");
}

/// Through `rho-ui serve`, a client reaches a remote machine that the
/// server reaches over ssh, as the desktop app on its machine would: the
/// daemon there, scripts there, which fail in their own words, and a
/// shell there, in its folder at its size. Every script and the daemon's
/// socket share one ssh connection, and the terminals one more, as on the
/// desktop: each sign-in is one a key agent such as 1Password's would ask
/// the user to approve. `fake_sshd` plays the remote machine, and the
/// server's ssh config, which its `ssh -G` reads, names it.
#[test]
fn a_remote_machine_through_the_server_runs_sessions_scripts_and_shells_there() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let home = dir.path().join("home");
    let remote = dir.path().join("remote");
    let fake = FakeDaemon::start(Script::default());
    install_rho(&remote, &fake);
    let sshd = FakeSshd::start(&remote);
    std::fs::create_dir_all(home.join(".ssh")).expect("an ssh folder");
    std::fs::create_dir_all(home.join("bin")).expect("a bin folder");
    std::fs::write(
        home.join(".ssh/config"),
        format!(
            "Host workstation\n  HostName 127.0.0.1\n  Port {}\n  User tester\n  \
             IdentityFile {}\n  IdentitiesOnly yes\n  IdentityAgent none\n  \
             UserKnownHostsFile {}\n  GlobalKnownHostsFile /dev/null\n",
            sshd.port(),
            sshd.key_file(ClientKey::Accepted).display(),
            sshd.known_hosts(HostKey::Known).display(),
        ),
    )
    .expect("an ssh config");
    // ssh reads the config in the home the system says, not `$HOME`.
    write_program(
        &home.join("bin/ssh"),
        "#!/bin/sh\nexec /usr/bin/ssh -F \"$HOME/.ssh/config\" \"$@\"\n",
    );
    let server = Server::start(&home);

    block_on(async {
        let target = server.target().with_machine("workstation");
        let (daemon, _events) = Daemon::connect(DaemonTarget::Server(target))
            .await
            .expect("the fake daemon answers through the server and ssh");
        assert_eq!(daemon.info().home(), Some(remote.as_path()));
        let link = daemon.link().expect("the server's link");
        let printed = link
            .run_script(r#"printf '%s' "$HOME""#, &[""; 0])
            .await
            .expect("a script runs on the remote machine");
        assert_eq!(printed, remote.as_os_str().as_bytes());
        let failed = link
            .run_script("echo 'no such thing' >&2; exit 3", &[""; 0])
            .await
            .expect_err("a failing script");
        assert!(
            failed.to_string().ends_with("failed: no such thing"),
            "{failed}"
        );

        let RemoteShell {
            mut output,
            input,
            control,
            ended,
        } = link.open_shell(&remote, "xterm-256color", 80, 24);
        control.resize(100, 30);
        // `HOME` tells the remote shell from one here in the same folder.
        input.write(b"stty size; pwd; echo \"home=$HOME\"; echo DONE-$((6*7))\n".to_vec());
        let read_all = async {
            let mut printed = Vec::new();
            let mut exited = false;
            while let Some(bytes) = output.recv().await {
                printed.extend(bytes);
                if !exited && printed.windows(7).any(|window| window == b"DONE-42") {
                    input.write(b"exit\n".to_vec());
                    exited = true;
                }
            }
            printed
        };
        let printed = match select(pin!(read_all), after(SHELL_BOUND)).await {
            Either::Left((printed, _)) => printed,
            Either::Right(_) => panic!("the remote shell's output stopped coming"),
        };
        ended.wait().await.expect("the remote shell exits");
        drop(control);
        let printed = String::from_utf8_lossy(&printed);
        assert!(printed.contains("\n30 100\r\n"), "{printed:?}");
        assert!(
            printed.contains(&format!("\n{}\r\n", remote.display())),
            "{printed:?}"
        );
        assert!(
            printed.contains(&format!("\nhome={}\r\n", remote.display())),
            "{printed:?}"
        );
        // A shell the machine won't start says why, as on the desktop,
        // rather than closing as if it exited.
        sshd.refuse_terminals();
        let refused = link.open_shell(&remote, "xterm-256color", 80, 24);
        match select(pin!(refused.ended.wait()), after(SHELL_BOUND)).await {
            Either::Left((Err(ShellError::NotStarted(why)), _))
                if why.contains("pseudo-terminal") => {}
            Either::Left((other, _)) => panic!("not a refused pseudo-terminal: {other:?}"),
            Either::Right(_) => panic!("the refused shell never ended"),
        }

        daemon
            .new_session(&remote, Checkout::Local)
            .await
            .expect("a new session on the remote machine");
    });
    assert_eq!(
        sshd.sign_ins(),
        2,
        "one connection for the daemon and scripts, one for terminals"
    );
}

/// A settings file that exists but can't be read, as one with bytes that
/// aren't UTF-8, reaches the page as unreadable, and what the page saves
/// never writes over it, as the desktop app never does.
#[test]
fn a_settings_file_the_server_cant_read_is_never_written_over() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let home = dir.path().join("home");
    let config = home.join(".config/rho-ui");
    std::fs::create_dir_all(&config).expect("a config folder");
    let unreadable = b"appearance = light\nui_font = \xff\n";
    std::fs::write(config.join("settings"), unreadable).expect("settings");
    let server = Server::start(&home);

    let shared = block_on(server.target().open_state()).expect("the state loads");
    assert!(
        matches!(&shared.settings, SettingsFile::Unreadable { why } if why.contains("settings")),
        "{:?}",
        shared.settings
    );
    shared.writer.write(StateWrite::Settings {
        text: "appearance = dark\n".to_owned(),
    });
    drop(shared.writer);
    block_on(shared.done.wait());
    let settings = std::fs::read(config.join("settings")).expect("settings");
    assert_eq!(settings, unreadable);
}
