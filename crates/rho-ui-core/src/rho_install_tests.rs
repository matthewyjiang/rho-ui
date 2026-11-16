use super::*;
use crate::fake_daemon::write_program;

/// Each way `rho update --check --json` can end reads as the update the
/// views offer: a newer rho to install unattended or in a terminal, none,
/// or why rho couldn't tell, as a rho from before `--check` can't. Login
/// noise before the markers is skipped.
#[test]
fn the_check_reads_every_way_rho_answers() {
    let json = |available: bool, terminal: bool| {
        // rho names no command without an update.
        let command = if available {
            r#""sudo pacman -Sy rho""#
        } else {
            "null"
        };
        format!(
            r#"{{"current":"3.1.0","latest":"3.2.0","update_available":{available},"method":"pacman","command":{command},"needs_terminal":{terminal}}}"#
        )
    };
    let checked = |status: &str, said: &str| {
        format!("Welcome to the box!{CHECKED}{status}\nrho 3.1.0\n{said}")
    };
    let installed = |update| RhoCheck::Installed {
        current: "3.1.0".to_owned(),
        update,
    };
    let cases = [
        ("motd\nrho-ui-rho-missing\n".to_owned(), RhoCheck::Missing),
        (
            checked("0", &json(false, false)),
            installed(Update::UpToDate),
        ),
        (
            checked("0", &json(true, false)),
            installed(Update::Available {
                latest: "3.2.0".to_owned(),
            }),
        ),
        (
            checked("0", &json(true, true)),
            installed(Update::InTerminal {
                latest: "3.2.0".to_owned(),
                command: "sudo pacman -Sy rho".to_owned(),
            }),
        ),
        (
            checked("2", "error: unexpected argument '--check' found"),
            installed(Update::Unknown(
                "rho 3.1.0 can't check for updates".to_owned(),
            )),
        ),
        (
            checked(
                "1",
                "Error: could not check the latest rho release: timed out\n",
            ),
            installed(Update::Unknown(
                "could not check the latest rho release: timed out".to_owned(),
            )),
        ),
    ];
    for (output, want) in cases {
        assert_eq!(parse_check(&output), Ok(want), "output {output:?}");
    }
}

/// What [`CHECK_SCRIPT`], run through `sh` as a remote machine runs it,
/// makes of the program `rho`.
fn check_there(rho: &Path) -> Result<RhoCheck, String> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(CHECK_SCRIPT)
        .arg("sh")
        .arg(rho)
        .output()
        .expect("sh runs");
    parse_check(&String::from_utf8_lossy(&output.stdout))
}

/// The check runs rho here, and tells one whose rho answers from one whose
/// rho can't reach GitHub by what it wrote to stderr; [`CHECK_SCRIPT`]
/// finds the same.
#[test]
#[cfg_attr(windows, ignore = "runs a fake rho written as a POSIX sh script")]
fn the_check_reads_what_rho_says() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let fake = |name: &str, update: &str| {
        let rho = dir.path().join(name);
        let script = format!(
            "#!/bin/sh\ncase $1 in\n  --version) echo \"rho 3.1.0\" ;;\n  update) {update} ;;\nesac\n"
        );
        write_program(&rho, &script);
        rho
    };
    let current = fake(
        "rho",
        r#"echo '{"current":"3.1.0","latest":"3.1.0","update_available":false,"method":"script","command":null,"needs_terminal":false}'"#,
    );
    let offline = fake(
        "offline",
        "echo 'Error: could not reach GitHub' >&2; exit 1",
    );
    let installed = |update| RhoCheck::Installed {
        current: "3.1.0".to_owned(),
        update,
    };
    let cases = [
        (current, installed(Update::UpToDate)),
        (
            offline,
            installed(Update::Unknown("could not reach GitHub".to_owned())),
        ),
    ];
    for (rho, want) in cases {
        let here = smol::block_on(RhoInstall::Here { rho: rho.clone() }.check());
        assert_eq!(here.as_ref(), Ok(&want), "here, {rho:?}");
        assert_eq!(check_there(&rho), Ok(want), "the remote script, {rho:?}");
    }
}

/// A machine with no program where `rho` points checks as missing, rather
/// than failing: here on every OS, as it needs no fake rho, and through
/// [`CHECK_SCRIPT`] on Unix.
#[test]
fn the_check_says_a_missing_rho_is_missing() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let rho = dir.path().join("gone");
    #[cfg(unix)]
    assert_eq!(
        check_there(&rho),
        Ok(RhoCheck::Missing),
        "the remote script"
    );
    let here = smol::block_on(RhoInstall::Here { rho }.check());
    assert_eq!(here, Ok(RhoCheck::Missing), "here");
}

/// Plain `rho` falls back to the installer's folder only when `PATH`
/// lacks it and the installer put one there; a program set by path is
/// kept, from the home directory after `~/`.
#[test]
fn find_rho_falls_back_to_the_install_folder() {
    let name = format!("rho{}", std::env::consts::EXE_SUFFIX);
    let home = tempfile::tempdir().expect("a temporary home");
    let on_path = tempfile::tempdir().expect("a temporary PATH folder");
    std::fs::write(on_path.path().join(&name), "").expect("rho on PATH");
    let install = tempfile::tempdir().expect("a temporary install folder");
    let installed = install.path().join(&name);
    std::fs::write(&installed, "").expect("rho in the install folder");
    let empty = tempfile::tempdir().expect("an empty folder");
    let path = |dir: &Path| Some(dir.as_os_str().to_owned());
    let rho = Path::new("rho");
    let cases = [
        (rho, path(on_path.path()), install.path(), rho.to_path_buf()),
        (rho, path(empty.path()), install.path(), installed),
        (rho, None, empty.path(), rho.to_path_buf()),
        (
            Path::new("/opt/rho"),
            None,
            install.path(),
            PathBuf::from("/opt/rho"),
        ),
        (
            Path::new("~/bin/rho"),
            None,
            install.path(),
            home.path().join("bin/rho"),
        ),
    ];
    for (program, path, install_dir, want) in cases {
        assert_eq!(
            find_rho_in(
                program,
                path.as_deref(),
                Some(home.path()),
                Some(install_dir.to_path_buf())
            ),
            want,
            "program {program:?}, PATH {path:?}, install folder {install_dir:?}"
        );
    }
}
