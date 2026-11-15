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

/// The script finds rho through `sh` as a machine runs it, and tells a
/// machine without rho from one whose rho answers.
#[test]
#[cfg_attr(windows, ignore = "runs a fake rho written as a POSIX sh script")]
fn the_check_script_finds_rho_or_says_it_is_missing() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let rho = dir.path().join("rho");
    write_program(
        &rho,
        r#"#!/bin/sh
case $1 in
  --version) echo "rho 3.1.0" ;;
  update) echo '{"current":"3.1.0","latest":"3.1.0","update_available":false,"method":"script","command":null,"needs_terminal":false}' ;;
esac
"#,
    );
    let check = |rho: PathBuf| smol::block_on(RhoInstall::Here { rho }.check());
    assert_eq!(
        check(rho),
        Ok(RhoCheck::Installed {
            current: "3.1.0".to_owned(),
            update: Update::UpToDate,
        })
    );
    assert_eq!(check(dir.path().join("gone")), Ok(RhoCheck::Missing));
}

/// Plain `rho` falls back to the install script's folder only when `PATH`
/// lacks it and the script put one there; a program set by path is kept.
#[test]
fn find_rho_falls_back_to_the_install_folder() {
    let home = tempfile::tempdir().expect("a temporary home");
    let on_path = tempfile::tempdir().expect("a temporary PATH folder");
    std::fs::write(on_path.path().join("rho"), "").expect("rho on PATH");
    let installed = home.path().join(INSTALL_DIR).join("rho");
    let empty = tempfile::tempdir().expect("an empty home");
    std::fs::create_dir_all(installed.parent().expect("a folder")).expect("the install folder");
    std::fs::write(&installed, "").expect("rho in the install folder");
    let path = |dir: &Path| Some(dir.as_os_str().to_owned());
    let rho = Path::new("rho");
    let cases = [
        (rho, path(on_path.path()), home.path(), rho.to_path_buf()),
        (rho, path(empty.path()), home.path(), installed),
        (rho, None, empty.path(), rho.to_path_buf()),
        (
            Path::new("/opt/rho"),
            None,
            home.path(),
            PathBuf::from("/opt/rho"),
        ),
    ];
    for (program, path, home, want) in cases {
        assert_eq!(
            find_rho_in(program, path.as_deref(), Some(home.to_path_buf())),
            want,
            "program {program:?}, PATH {path:?}, home {home:?}"
        );
    }
}
