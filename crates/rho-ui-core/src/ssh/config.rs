//! How to reach a host and sign in there, as the user's ssh config says:
//! read from `ssh -G`, which applies every `Host`, `Match` and `Include`
//! the way ssh itself does, so rho-ui reaches the same machine as `ssh
//! HOST` in a terminal. Without a local `ssh` program, the destination
//! alone and OpenSSH's defaults.

use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use crate::{daemon::DaemonError, platform};

/// What rho-ui needs of the ssh config to reach one host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// The address to connect to (`HostName`).
    pub(crate) hostname: String,
    /// The TCP port (`Port`).
    pub(crate) port: u16,
    /// Who to sign in as (`User`).
    pub(crate) user: String,
    /// Key files, in the order to try them (`IdentityFile`).
    pub(crate) identity_files: Vec<PathBuf>,
    /// Which of the key agent's keys to offer (`IdentitiesOnly`).
    pub(crate) agent_keys: AgentKeys,
    /// Where the key agent listens (`IdentityAgent`).
    pub(crate) identity_agent: IdentityAgent,
    /// The `known_hosts` files that vouch for the host's key: the user's
    /// (`UserKnownHostsFile`), then the system's (`GlobalKnownHostsFile`).
    pub(crate) known_hosts: Vec<PathBuf>,
    /// The name the host's key is filed under there (`HostKeyAlias`), in
    /// place of its hostname and port.
    pub(crate) host_key_alias: Option<String>,
}

/// Which of the key agent's keys to offer the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentKeys {
    /// All of them, those of the configured key files first.
    All,
    /// Only those of the configured key files (`IdentitiesOnly yes`).
    Configured,
}

/// Where the key agent listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IdentityAgent {
    /// Where `SSH_AUTH_SOCK` says; on Windows, without it, OpenSSH's agent
    /// pipe, then Pageant.
    Default,
    /// Nowhere: sign in with key files only (`IdentityAgent none`).
    Off,
    /// At this socket, or on Windows this named pipe.
    Path(PathBuf),
    /// Where this environment variable says (`IdentityAgent $NAME`).
    Variable(String),
}

/// How long `ssh -G` may take. It took 4 ms here (OpenSSH 10.0, 5 runs);
/// this is a tripwire for a config whose `Match exec` hangs.
const RESOLVE_DEADLINE: Duration = Duration::from_secs(5);

/// The local `ssh` program: on Windows the one Windows ships, in
/// `%SystemRoot%\System32\OpenSSH`, when it is there, else `ssh` on PATH.
pub(crate) fn ssh_program() -> PathBuf {
    if cfg!(windows) {
        if let Some(root) = std::env::var_os("SystemRoot") {
            let shipped = Path::new(&root)
                .join("System32")
                .join("OpenSSH")
                .join("ssh.exe");
            if shipped.is_file() {
                return shipped;
            }
        }
    }
    PathBuf::from("ssh")
}

/// Resolves `host`, a destination ssh accepts, with `ssh -G`. Runs on the
/// I/O runtime.
///
/// # Errors
///
/// Fails, in ssh's words, when ssh can't read its config, takes too long,
/// or the host is reached through a proxy, which rho-ui doesn't do yet.
pub(super) async fn resolve(host: &str) -> Result<Resolved, DaemonError> {
    let home = platform::home_dir();
    let program = ssh_program();
    let mut command = tokio::process::Command::from(platform::background_command(&program));
    command
        .args(["-G", "--", host])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(RESOLVE_DEADLINE, command.output()).await {
        Err(_) => {
            return Err(DaemonError::Unreachable(format!(
                "`ssh -G {host}`, which reads your ssh config, didn't finish within {} s",
                RESOLVE_DEADLINE.as_secs()
            )))
        }
        Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(defaults(host, home.as_deref(), &local_user()));
        }
        Ok(Err(error)) => {
            return Err(DaemonError::Unreachable(format!(
                "couldn't run {}: {error}",
                program.display()
            )))
        }
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        let said = match said.trim() {
            "" => format!("it exited with {}", output.status),
            said => said.to_owned(),
        };
        return Err(DaemonError::Unreachable(format!(
            "ssh couldn't read its config for {host}: {said}"
        )));
    }
    parse(
        host,
        &String::from_utf8_lossy(&output.stdout),
        home.as_deref(),
    )
    .map_err(DaemonError::Unreachable)
}

/// Reads `ssh -G host`'s output: one `keyword value` line per setting,
/// keywords in lowercase. ssh expands `~` in every path but
/// `IdentityFile`'s, which this does, with its `%d`, `%h` and `%r`.
///
/// # Errors
///
/// Fails when the host is reached through `ProxyCommand` or `ProxyJump`,
/// naming it, or the output lacks the host or user.
fn parse(host: &str, output: &str, home: Option<&Path>) -> Result<Resolved, String> {
    let mut hostname = None;
    let mut user = None;
    let mut port = 22;
    let mut identity_files = Vec::new();
    let mut agent_keys = AgentKeys::All;
    let mut identity_agent = IdentityAgent::Default;
    let mut user_known = Vec::new();
    let mut global_known = Vec::new();
    let mut host_key_alias = None;
    for line in output.lines() {
        let Some((keyword, value)) = line.split_once(' ') else {
            continue;
        };
        match keyword {
            "hostname" => hostname = Some(value.to_owned()),
            "user" => user = Some(value.to_owned()),
            "port" => {
                port = value
                    .parse()
                    .map_err(|_| format!("ssh's config gives {host} the port {value:?}"))?;
            }
            "identityfile" => identity_files.push(value.to_owned()),
            "identitiesonly" if value == "yes" => agent_keys = AgentKeys::Configured,
            "identityagent" => identity_agent = agent_setting(value, home),
            "userknownhostsfile" => user_known = paths(value, home),
            "globalknownhostsfile" => global_known = paths(value, home),
            "hostkeyalias" => host_key_alias = Some(value.to_owned()),
            "proxycommand" | "proxyjump" if value != "none" => {
                let directive = if keyword == "proxycommand" {
                    "ProxyCommand"
                } else {
                    "ProxyJump"
                };
                return Err(format!(
                    "your ssh config reaches {host} through {directive}, which rho-ui doesn't \
                     support yet"
                ));
            }
            _ => {}
        }
    }
    let hostname = hostname.ok_or_else(|| format!("`ssh -G {host}` printed no hostname"))?;
    let user = user.ok_or_else(|| format!("`ssh -G {host}` printed no user"))?;
    let identity_files = identity_files
        .iter()
        .map(|raw| identity_file(raw, home, &hostname, &user))
        .collect();
    Ok(Resolved {
        identity_files,
        agent_keys,
        identity_agent,
        known_hosts: user_known.into_iter().chain(global_known).collect(),
        host_key_alias,
        hostname,
        port,
        user,
    })
}

/// A space-separated list of paths, each with a leading `~` as `home`.
fn paths(value: &str, home: Option<&Path>) -> Vec<PathBuf> {
    value
        .split_whitespace()
        .map(|path| tilde(Path::new(path), home))
        .collect()
}

fn tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    match home {
        Some(home) => platform::expand_tilde(path, home),
        None => path.to_path_buf(),
    }
}

/// `IdentityAgent`'s value: `none`, `SSH_AUTH_SOCK` or `$NAME` for a
/// variable, else a path.
fn agent_setting(value: &str, home: Option<&Path>) -> IdentityAgent {
    match value {
        "none" => IdentityAgent::Off,
        "SSH_AUTH_SOCK" => IdentityAgent::Default,
        _ => match value.strip_prefix('$') {
            Some(name) => {
                let name = name
                    .strip_prefix('{')
                    .and_then(|name| name.strip_suffix('}'))
                    .unwrap_or(name);
                IdentityAgent::Variable(name.to_owned())
            }
            None => IdentityAgent::Path(tilde(Path::new(value), home)),
        },
    }
}

/// An `IdentityFile` as ssh would open it: `%d` is `home`, `%h` the
/// hostname, `%r` the user and `%%` a `%`, then a leading `~` is `home`.
/// Other tokens stay as they are.
fn identity_file(raw: &str, home: Option<&Path>, hostname: &str, user: &str) -> PathBuf {
    let mut expanded = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            expanded.push(c);
            continue;
        }
        match (chars.next(), home) {
            (Some('%') | None, _) => expanded.push('%'),
            (Some('d'), Some(home)) => expanded.push_str(&home.to_string_lossy()),
            (Some('h'), _) => expanded.push_str(hostname),
            (Some('r'), _) => expanded.push_str(user),
            (Some(other), _) => {
                expanded.push('%');
                expanded.push(other);
            }
        }
    }
    tilde(Path::new(&expanded), home)
}

/// The user this process runs as, the default for a destination that
/// names none.
fn local_user() -> String {
    let variable = if cfg!(windows) { "USERNAME" } else { "USER" };
    std::env::var(variable).unwrap_or_default()
}

/// What OpenSSH would use for `host` with no config: its `user@`, else
/// `local_user`, port 22, the default key files and `known_hosts` in
/// `~/.ssh`, and the agent `SSH_AUTH_SOCK` names.
fn defaults(host: &str, home: Option<&Path>, local_user: &str) -> Resolved {
    let (user, hostname) = host.rsplit_once('@').unwrap_or((local_user, host));
    let ssh_dir = home.map(|home| home.join(".ssh"));
    let in_ssh_dir = |names: &[&str]| -> Vec<PathBuf> {
        ssh_dir
            .iter()
            .flat_map(|dir| names.iter().map(|name| dir.join(name)))
            .collect()
    };
    Resolved {
        hostname: hostname.to_owned(),
        port: 22,
        user: user.to_owned(),
        identity_files: in_ssh_dir(&["id_ed25519", "id_ecdsa", "id_rsa"]),
        agent_keys: AgentKeys::All,
        identity_agent: IdentityAgent::Default,
        known_hosts: in_ssh_dir(&["known_hosts", "known_hosts2"]),
        host_key_alias: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ssh -G` output, cut from OpenSSH 10.0's, with every setting rho-ui
    /// reads.
    const SSH_G: &str = "host box\nuser me\nhostname box.example.com\nport 2222\n\
        identitiesonly yes\nidentityfile ~/.ssh/id_ed25519\nidentityfile %d/keys/%r@%h_%%\n\
        identityfile ~/.ssh/id_%u\nhostkeyalias boxalias\nidentityagent /run/agent.sock\n\
        userknownhostsfile /home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2\n\
        globalknownhostsfile /etc/ssh/ssh_known_hosts\nproxyusefdpass no\n";

    #[test]
    fn ssh_g_output_resolves_the_route() {
        let home = Path::new("/home/me");
        let resolved = parse("box", SSH_G, Some(home)).expect("a route");
        assert_eq!(
            resolved,
            Resolved {
                hostname: "box.example.com".to_owned(),
                port: 2222,
                user: "me".to_owned(),
                identity_files: vec![
                    home.join(".ssh/id_ed25519"),
                    PathBuf::from("/home/me/keys/me@box.example.com_%"),
                    home.join(".ssh/id_%u"),
                ],
                agent_keys: AgentKeys::Configured,
                identity_agent: IdentityAgent::Path(PathBuf::from("/run/agent.sock")),
                known_hosts: vec![
                    PathBuf::from("/home/me/.ssh/known_hosts"),
                    PathBuf::from("/home/me/.ssh/known_hosts2"),
                    PathBuf::from("/etc/ssh/ssh_known_hosts"),
                ],
                host_key_alias: Some("boxalias".to_owned()),
            }
        );
    }

    /// The settings that change where the agent is or whether the host is
    /// reached at all.
    #[test]
    fn agent_and_proxy_settings() {
        for (line, agent) in [
            ("identityagent none", IdentityAgent::Off),
            ("identityagent SSH_AUTH_SOCK", IdentityAgent::Default),
            (
                "identityagent ${OTHER_SOCK}",
                IdentityAgent::Variable("OTHER_SOCK".to_owned()),
            ),
            (
                "identityagent $OTHER_SOCK",
                IdentityAgent::Variable("OTHER_SOCK".to_owned()),
            ),
        ] {
            let output = format!("user me\nhostname box\n{line}\n");
            let resolved = parse("box", &output, None).expect("a route");
            assert_eq!(resolved.identity_agent, agent, "{line}");
        }
        for (line, directive) in [
            ("proxyjump bastion", Some("ProxyJump")),
            ("proxycommand nc %h %p", Some("ProxyCommand")),
            ("proxycommand none", None),
        ] {
            let output = format!("user me\nhostname box\n{line}\n");
            match (parse("box", &output, None), directive) {
                (Ok(_), None) => {}
                (Err(error), Some(directive)) => {
                    assert!(error.contains(directive), "{line}: {error}");
                }
                (result, _) => panic!("{line}: {result:?}"),
            }
        }
    }

    /// Without an ssh program, a destination's `user@` is the user, and
    /// the files are OpenSSH's defaults.
    #[test]
    fn without_ssh_the_destination_names_the_user() {
        let home = Path::new("/home/me");
        let named = defaults("admin@box", Some(home), "me");
        assert_eq!(
            (named.user.as_str(), named.hostname.as_str()),
            ("admin", "box")
        );
        assert_eq!(named.identity_files[0], home.join(".ssh/id_ed25519"));
        let plain = defaults("box", Some(home), "me");
        assert_eq!(
            (plain.user.as_str(), plain.hostname.as_str()),
            ("me", "box")
        );
    }
}
