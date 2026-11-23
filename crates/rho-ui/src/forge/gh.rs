//! GitHub's CLI, `gh`: the script that finds it on a machine and reports
//! its version and accounts, and the parser of what that script prints.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
};

use gpui_kit::SharedString;
use rho_ui_core::platform;
use serde::Deserialize;

use super::{Account, AuthState, CliProbe, CliStatus, FoundBy, Job, TokenSource};
use crate::{folders, local};

/// The POSIX `sh` script that checks `gh` on a machine. Its argument is the
/// program set in Settings: empty finds `gh` on the `PATH`, then through
/// the user's login shell, as a non-interactive ssh session may lack the
/// `PATH` a terminal has; a `~/` path is from the home folder, and a bare
/// name is looked up on the `PATH`. After [`MARKER`] it prints NUL-separated
/// fields: `missing`; or `found`, how, the path, then `broken` and why, or
/// `version` and `auth` with what `gh` printed for each. Read
/// with [`parse`]. `gh auth status` reports a failing token in its JSON, so
/// its exit status is ignored. The login shell's last line is its answer,
/// after whatever its login files print; the script keeps to `sh`'s
/// builtins until it runs `gh`, so a bare `PATH` still finds it.
pub(super) const SCRIPT: &str = r#"unset CDPATH
export GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1 GH_SPINNER_DISABLED=1 NO_COLOR=1
nl='
'
want=$1
case $want in
  '')
    found=path
    gh=$(command -v gh 2>/dev/null)
    if ! [ -f "$gh" ] && [ -n "$SHELL" ]; then
      found=login
      gh=$("$SHELL" -l -c 'command -v gh' </dev/null 2>/dev/null)
      gh=${gh##*"$nl"}
    fi
    if ! [ -f "$gh" ]; then
      printf 'rho-ui-forge\0missing\0'
      exit 0
    fi ;;
  '~/'*) found=setting; gh=$HOME/${want#'~/'} ;;
  */*) found=setting; gh=$want ;;
  *) found=setting; gh=$(command -v "$want" 2>/dev/null) || gh=$want ;;
esac
printf 'rho-ui-forge\0found\0%s\0%s\0' "$found" "$gh"
if ! [ -f "$gh" ] || ! [ -x "$gh" ]; then
  printf 'broken\0\0'
  exit 0
fi
if ! version=$("$gh" --version 2>&1); then
  printf 'broken\0%s\0' "$version"
  exit 0
fi
printf 'version\0%s\0auth\0' "$version"
"$gh" auth status --json hosts 2>/dev/null
printf '\0'
"#;

/// Printed before the fields, so whatever a login file prints comes first
/// and is skipped.
const MARKER: &[u8] = b"rho-ui-forge\0";

/// Checks `gh`: [`SCRIPT`] on a remote machine, [`check_here`] here.
pub(super) const CHECK: Job = Job {
    script: SCRIPT,
    here: check_here,
};

/// `gh` at `program`, to run here as the scripts run it: never asking,
/// without colors, spinners or update notices.
pub(super) fn command(program: &OsStr) -> Command {
    let mut command = platform::background_command(program);
    command.envs([
        ("GH_PROMPT_DISABLED", "1"),
        ("GH_NO_UPDATE_NOTIFIER", "1"),
        ("GH_SPINNER_DISABLED", "1"),
        ("NO_COLOR", "1"),
    ]);
    command
}

/// The environment [`check_here_in`] finds `gh` in.
struct Env<'a> {
    path: Option<&'a OsStr>,
    shell: Option<&'a OsStr>,
    home: Option<&'a Path>,
}

/// [`SCRIPT`] in Rust, with the same argument, printing what it prints.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a Job's here may fail; this one says why in what it prints"
)]
fn check_here(args: &[OsString]) -> Result<Vec<u8>, String> {
    let path = std::env::var_os("PATH");
    let shell = std::env::var_os("SHELL");
    let home = platform::home_dir();
    let env = Env {
        path: path.as_deref(),
        shell: shell.as_deref(),
        home: home.as_deref(),
    };
    let want = args.first().map(OsString::as_os_str).unwrap_or_default();
    Ok(check_here_in(want, &env))
}

fn check_here_in(want: &OsStr, env: &Env<'_>) -> Vec<u8> {
    let mut out = MARKER.to_vec();
    let mut field = |bytes: &[u8]| {
        out.extend(bytes);
        out.push(0);
    };
    let want = Path::new(want);
    let found = if want.as_os_str().is_empty() {
        on_path("gh", env.path)
            .map(|gh| ("path", gh))
            .or_else(|| login_shell_gh(env.shell).map(|gh| ("login", gh)))
    } else if want.starts_with("~") {
        let gh = env.home.map_or_else(
            || want.to_path_buf(),
            |home| platform::expand_tilde(want, home),
        );
        Some(("setting", gh))
    } else if want.components().count() > 1 {
        Some(("setting", want.to_path_buf()))
    } else {
        Some((
            "setting",
            on_path(want, env.path).unwrap_or_else(|| want.to_path_buf()),
        ))
    };
    let Some((how, gh)) = found else {
        field(b"missing");
        return out;
    };
    field(b"found");
    field(how.as_bytes());
    field(&platform::path_to_bytes(gh.as_os_str()));
    if !is_program(&gh) {
        field(b"broken");
        field(b"");
        return out;
    }
    let said = |output: &std::process::Output| [&output.stdout[..], &output.stderr[..]].concat();
    match local::run(command(gh.as_os_str()).arg("--version")) {
        Err(why) => {
            field(b"broken");
            field(why.as_bytes());
            return out;
        }
        Ok(output) if !output.status.success() => {
            field(b"broken");
            field(&said(&output));
            return out;
        }
        Ok(output) => {
            field(b"version");
            field(&said(&output));
        }
    }
    field(b"auth");
    // A failing token is in the JSON, so how it ends doesn't matter.
    let auth = local::run(command(gh.as_os_str()).args(["auth", "status", "--json", "hosts"]));
    field(&auth.map(|output| output.stdout).unwrap_or_default());
    out
}

/// `name` in a folder of `path`, as the shell finds a program.
fn on_path(name: impl AsRef<Path>, path: Option<&OsStr>) -> Option<PathBuf> {
    let mut program = name.as_ref().as_os_str().to_owned();
    program.push(std::env::consts::EXE_SUFFIX);
    std::env::split_paths(path?)
        .map(|dir| dir.join(&program))
        .find(|candidate| is_program(candidate))
}

/// Where the user's login shell `shell` finds `gh`: its last line, after
/// whatever its login files print. Unix only, as `$SHELL` is.
fn login_shell_gh(shell: Option<&OsStr>) -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let mut command = platform::background_command(shell?);
    command.args(["-l", "-c", "command -v gh"]);
    let output = local::run(&mut command).ok()?;
    let line = output
        .stdout
        .split(|&byte| byte == b'\n')
        .rfind(|line| !line.is_empty())?;
    Some(platform::path_from_bytes(line)).filter(|gh| gh.is_file())
}

/// Whether `path` is a file it may run.
fn is_program(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// `gh auth status --json hosts`: each host's accounts.
#[derive(Deserialize)]
struct AuthStatus {
    hosts: BTreeMap<String, Vec<GhAccount>>,
}

/// One account as `gh auth status` reports it. `state` is `success` once
/// GitHub took its token; otherwise `error` says why.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhAccount {
    state: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    active: bool,
    host: String,
    #[serde(default)]
    login: String,
    #[serde(default)]
    token_source: Option<String>,
    #[serde(default)]
    scopes: Option<String>,
    #[serde(default)]
    git_protocol: Option<String>,
}

/// What [`SCRIPT`] found.
///
/// # Errors
///
/// If it printed something else.
pub(super) fn parse(output: &[u8]) -> Result<CliProbe, String> {
    let at = folders::find(output, MARKER).ok_or("The gh check printed nothing.")?;
    let fields: Vec<&[u8]> = output[at + MARKER.len()..].split(|&b| b == 0).collect();
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim().to_owned();
    let (found_by, path, rest) = match fields.as_slice() {
        [b"missing", ..] => return Ok(CliProbe::Missing),
        [b"found", how, path, rest @ ..] => {
            let found_by = match *how {
                b"setting" => FoundBy::Setting,
                b"path" => FoundBy::Path,
                b"login" => FoundBy::LoginShell,
                _ => return Err("The gh check printed something else.".to_owned()),
            };
            (found_by, platform::path_from_bytes(path), rest)
        }
        _ => return Err("The gh check printed something else.".to_owned()),
    };
    let broken = |why: String| CliProbe::Broken {
        path: path.clone(),
        found_by,
        why: why.into(),
    };
    match rest {
        [b"broken", why, ..] => {
            let why = match text(why) {
                why if why.is_empty() => "There's no program there.".to_owned(),
                why => why,
            };
            Ok(broken(why))
        }
        [b"version", version, b"auth", auth, ..] => {
            let Ok(auth) = serde_json::from_slice::<AuthStatus>(auth) else {
                return Ok(broken(
                    "This gh can't list its accounts as JSON (gh auth status --json). \
                     Update it to a newer release."
                        .to_owned(),
                ));
            };
            Ok(CliProbe::Found(CliStatus {
                path,
                found_by,
                version: version_number(&text(version)).into(),
                accounts: auth
                    .hosts
                    .into_values()
                    .flatten()
                    .map(Account::from)
                    .collect(),
            }))
        }
        _ => Err("The gh check printed something else.".to_owned()),
    }
}

impl From<GhAccount> for Account {
    fn from(account: GhAccount) -> Self {
        let why = account.error.unwrap_or_else(|| account.state.clone());
        let auth = if account.state == "success" {
            AuthState::SignedIn
        } else if refused(&why) {
            AuthState::Refused("GitHub refused its token.".into())
        } else {
            let line = why.lines().next().unwrap_or(&why).trim();
            AuthState::Unreachable(line.to_owned().into())
        };
        Self {
            host: account.host.into(),
            login: account.login.into(),
            active: account.active,
            auth,
            git_protocol: account.git_protocol.map(SharedString::from),
            token_source: account.token_source.as_deref().and_then(token_source),
            scopes: account
                .scopes
                .filter(|scopes| !scopes.is_empty())
                .map(SharedString::from),
        }
    }
}

/// Where `gh` says a token comes from: `keyring`, the name of a variable
/// such as `GH_TOKEN`, or the path of its `hosts.yml`.
fn token_source(source: &str) -> Option<TokenSource> {
    let variable = source
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    match source {
        "" => None,
        "keyring" => Some(TokenSource::Keyring),
        _ if variable => Some(TokenSource::Variable(source.to_owned().into())),
        _ => Some(TokenSource::File(source.to_owned().into())),
    }
}

/// Whether `gh` failed an account because GitHub refused its token, as
/// opposed to not reaching GitHub at all. The refusal arrives as the raw
/// 401 response, which says nothing a person needs.
fn refused(error: &str) -> bool {
    error.contains("401") || error.contains("Bad credentials")
}

/// `2.99.0` from `gh version 2.99.0 (2026-09-01)`, or the first line as it
/// is when it reads otherwise.
fn version_number(version: &str) -> String {
    let first = version.lines().next().unwrap_or_default();
    first
        .strip_prefix("gh version ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or(first)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    /// A `gh` that prints `auth` for `gh auth status`, and what a real one
    /// prints for its version.
    fn fake_gh(dir: &Path, name: &str, auth: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = dir.join(name);
        let script = format!(
            "#!/bin/sh\ncase $1 in\n  --version) echo 'gh version 2.99.0 (2026-09-01)' ;;\n  \
             auth) echo '{auth}' ;;\nesac\n"
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    /// Runs [`SCRIPT`] with `want`, a `PATH` of `path` and a login shell
    /// `shell`, and reads what it found; [`check_here`], run here in the
    /// same environment, must find the same.
    fn check(want: &str, path: &Path, shell: &Path, home: &Path) -> CliProbe {
        let env = Env {
            path: Some(path.as_os_str()),
            shell: Some(shell.as_os_str()),
            home: Some(home),
        };
        let here = parse(&check_here_in(OsStr::new(want), &env)).unwrap();
        let there = check_there(want, path, shell, home);
        assert_eq!(here, there, "Program {want:?}: here and there differ");
        there
    }

    #[cfg(unix)]
    fn check_there(want: &str, path: &Path, shell: &Path, home: &Path) -> CliProbe {
        use std::process::Command;

        let output = Command::new("/bin/sh")
            .args(["-c", SCRIPT, "sh", want])
            .env_clear()
            .env("PATH", path)
            .env("SHELL", shell)
            .env("HOME", home)
            .output()
            .unwrap();
        assert!(output.status.success(), "the script failed: {output:?}");
        parse(&output.stdout).unwrap()
    }

    #[cfg(unix)]
    /// A lookup: the Program setting, the `PATH`, the login shell, and the
    /// program found and how.
    type Lookup<'a> = (&'a str, &'a Path, &'a Path, Option<(PathBuf, FoundBy)>);

    #[cfg(unix)]
    /// The check finds `gh` the way the Program setting says, here and on
    /// a remote machine: a set path,
    /// a `~/` path, a name on the `PATH`; and with none, on the `PATH`,
    /// else where the login shell finds it, else nowhere.
    #[test]
    fn the_check_finds_gh_as_the_setting_says() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let on_path = dir.join("bin");
        let login_bin = dir.join("login-bin");
        let empty = dir.join("empty");
        for folder in [&on_path, &login_bin, &empty] {
            std::fs::create_dir(folder).unwrap();
        }
        let signed_in = r#"{"hosts":{}}"#;
        let gh = fake_gh(&on_path, "gh", signed_in);
        let renamed = fake_gh(&on_path, "gh-2", signed_in);
        let via_login = fake_gh(&login_bin, "gh", signed_in);
        // A login shell whose PATH holds `login-bin`, printing a greeting
        // first, as login files do.
        let shell = dir.join("login-sh");
        std::fs::write(
            &shell,
            format!(
                "#!/bin/sh\necho 'welcome'\nPATH={}:/usr/bin:/bin\nexec /bin/sh -c \"$3\"\n",
                login_bin.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let no_shell = dir.join("no-shell");

        let found = |probe: CliProbe| match probe {
            CliProbe::Found(status) => Some((status.path, status.found_by)),
            CliProbe::Broken { path, found_by, .. } => {
                panic!("{} ({found_by:?}) is broken", path.display())
            }
            CliProbe::Missing => None,
        };
        let cases: [Lookup<'_>; 5] = [
            ("", &on_path, &shell, Some((gh.clone(), FoundBy::Path))),
            ("", &empty, &shell, Some((via_login, FoundBy::LoginShell))),
            ("", &empty, &no_shell, None),
            (
                "gh-2",
                &on_path,
                &no_shell,
                Some((renamed, FoundBy::Setting)),
            ),
            ("~/bin/gh", &empty, &no_shell, Some((gh, FoundBy::Setting))),
        ];
        for (want, path, shell, expected) in cases {
            assert_eq!(
                found(check(want, path, shell, dir)),
                expected,
                "Program {want:?} with PATH {}",
                path.display()
            );
        }

        let probe = check("~/bin/nothing", &empty, &no_shell, dir);
        assert!(
            matches!(&probe, CliProbe::Broken { path, .. } if *path == on_path.join("nothing")),
            "a set path with no program there: {probe:?}"
        );
    }

    /// What `gh` says about its accounts, as real ones print it, becomes
    /// each account and whether its token works; a `gh` too old to print
    /// JSON reads as broken.
    #[test]
    fn accounts_parse_from_gh_auth_status() {
        let signed_in = r#"{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"octocat","tokenSource":"/home/me/.config/gh/hosts.yml","scopes":"repo, workflow","gitProtocol":"ssh"}]}}"#;
        let refused = r#"{"hosts":{"github.com":[{"state":"error","error":"non-200 OK status code: 401 Unauthorized body: \"{}\"","active":true,"host":"github.com","login":"","tokenSource":"GH_TOKEN","gitProtocol":"https"}]}}"#;
        let offline = r#"{"hosts":{"github.com":[{"state":"error","error":"Get \"https://api.github.com/\": proxyconnect tcp: dial tcp 127.0.0.1:9: connect: connection refused","active":true,"host":"github.com","login":"octocat","tokenSource":"/home/me/.config/gh/hosts.yml","gitProtocol":"ssh"}]}}"#;
        let output = |auth: &str| {
            let mut output = b"motd\n".to_vec();
            output.extend(MARKER);
            output.extend(b"found\0path\0/usr/bin/gh\0version\0gh version 2.99.0 (2026-09-01)\nhttps://github.com/cli/cli/releases/tag/v2.99.0\n\0auth\0");
            output.extend(auth.as_bytes());
            output.extend(b"\n\0");
            output
        };
        let accounts = |auth: &str| match parse(&output(auth)) {
            Ok(CliProbe::Found(status)) => {
                assert_eq!(status.version, "2.99.0");
                status
                    .accounts
                    .into_iter()
                    .map(|account| (account.host, account.login, account.auth))
                    .collect::<Vec<_>>()
            }
            other => panic!("not found: {other:?}"),
        };
        assert_eq!(
            accounts(signed_in),
            [("github.com".into(), "octocat".into(), AuthState::SignedIn)]
        );
        assert_eq!(
            accounts(refused),
            [(
                "github.com".into(),
                SharedString::default(),
                AuthState::Refused("GitHub refused its token.".into())
            )]
        );
        assert!(
            matches!(
                accounts(offline).as_slice(),
                [(_, login, AuthState::Unreachable(_))] if login == "octocat"
            ),
            "not reaching GitHub isn't a refused token"
        );
        assert_eq!(accounts(r#"{"hosts":{}}"#), []);
        assert!(
            matches!(parse(&output("")), Ok(CliProbe::Broken { .. })),
            "a gh without `auth status --json`"
        );
        assert!(parse(b"nothing").is_err());
    }
}
