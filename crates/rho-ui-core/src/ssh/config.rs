//! How to reach a host and sign in there, as the user's ssh config says:
//! read from `ssh -G`, which applies every `Host`, `Match` and `Include`
//! the way ssh itself does, so rho-ui reaches the same machine as `ssh
//! HOST` in a terminal. Without a local `ssh` program, the destination
//! alone and OpenSSH's defaults.

use std::{
    borrow::Cow,
    fmt::Write as _,
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
    /// Certificates for the keys of those files, or of the key agent's
    /// (`CertificateFile`).
    pub(crate) certificate_files: Vec<PathBuf>,
    /// Key and certificate files left out because their names hold a
    /// token rho-ui can't expand.
    pub(crate) skipped: Vec<Skipped>,
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
    /// Files of host keys never to accept (`RevokedHostKeys`).
    pub(crate) revoked_host_keys: Vec<PathBuf>,
}

/// A key or certificate file rho-ui doesn't offer, and why, for the
/// message when signing in fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Skipped {
    /// The file, as the config or the disk names it.
    pub(crate) file: String,
    /// Why, for people.
    pub(crate) why: String,
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
    /// pipe.
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
/// Looks at the disk on Windows, so off the I/O runtime.
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

/// This machine's side of the ssh config's tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Local {
    /// The user's home directory (`%d`, and a leading `~`).
    home: Option<PathBuf>,
    /// The user this process runs as (`%u`), the default for a
    /// destination that names none.
    user: String,
    /// This machine's hostname (`%l`), as `gethostname` gives it.
    hostname: String,
}

impl Local {
    fn this_process() -> Self {
        let variable = if cfg!(windows) { "USERNAME" } else { "USER" };
        Self {
            home: platform::home_dir(),
            user: std::env::var(variable).unwrap_or_default(),
            hostname: gethostname::gethostname().to_string_lossy().into_owned(),
        }
    }
}

/// Resolves `host`, a destination ssh accepts, with `ssh -G`. Runs on the
/// I/O runtime.
///
/// # Errors
///
/// Fails, in ssh's words, when ssh can't read its config, takes too long,
/// or the host is reached through a proxy, which rho-ui doesn't do yet.
pub(super) async fn resolve(host: &str) -> Result<Resolved, DaemonError> {
    let (program, local) = super::read_files(|| (ssh_program(), Local::this_process())).await?;
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
            return Ok(defaults(host, &local));
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
        &local,
        &|name: &str| std::env::var(name).ok(),
    )
    .map_err(DaemonError::Unreachable)
}

/// Reads `ssh -G host`'s output: one `keyword value` line per setting,
/// keywords in lowercase. ssh expands `~` and tokens in every path but
/// `IdentityFile`'s and `CertificateFile`'s, which this does, looking up
/// their `${VAR}`s with `var`; a file with a token it can't expand is
/// [`Skipped`].
///
/// # Errors
///
/// Fails when the host is reached through `ProxyCommand` or `ProxyJump`,
/// naming it, or the output lacks the host or user.
fn parse(
    host: &str,
    output: &str,
    local: &Local,
    var: &dyn Fn(&str) -> Option<String>,
) -> Result<Resolved, String> {
    let home = local.home.as_deref();
    let mut original = None;
    let mut hostname = None;
    let mut user = None;
    let mut port = 22;
    let mut identity_files = Vec::new();
    let mut certificate_files = Vec::new();
    let mut agent_keys = AgentKeys::All;
    let mut identity_agent = IdentityAgent::Default;
    let mut user_known = Vec::new();
    let mut global_known = Vec::new();
    let mut host_key_alias = None;
    let mut revoked_host_keys = Vec::new();
    for line in output.lines() {
        let Some((keyword, value)) = line.split_once(' ') else {
            continue;
        };
        match keyword {
            "host" => original = Some(value.to_owned()),
            "hostname" => hostname = Some(value.to_owned()),
            "user" => user = Some(value.to_owned()),
            "port" => {
                port = value
                    .parse()
                    .map_err(|_| format!("ssh's config gives {host} the port {value:?}"))?;
            }
            "identityfile" => identity_files.push(value.to_owned()),
            "certificatefile" => certificate_files.push(value.to_owned()),
            "identitiesonly" if value == "yes" => agent_keys = AgentKeys::Configured,
            "identityagent" => identity_agent = agent_setting(value, home),
            "userknownhostsfile" => user_known = paths(value, home),
            "globalknownhostsfile" => global_known = paths(value, home),
            "hostkeyalias" => host_key_alias = Some(value.to_owned()),
            "revokedhostkeys" => revoked_host_keys = paths(value, home),
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
    let tokens = Tokens {
        local,
        hostname: &hostname,
        port,
        user: &user,
        original: original
            .as_deref()
            .unwrap_or_else(|| host.rsplit_once('@').map_or(host, |(_, name)| name)),
        alias: host_key_alias.as_deref(),
    };
    let mut skipped = Vec::new();
    let mut expand = |raw: Vec<String>| -> Vec<PathBuf> {
        raw.into_iter()
            .filter_map(|file| match tokens.expand(&file, var) {
                Ok(path) => Some(path),
                Err(why) => {
                    skipped.push(Skipped { file, why });
                    None
                }
            })
            .collect()
    };
    let identity_files = expand(identity_files);
    let certificate_files = expand(certificate_files);
    Ok(Resolved {
        identity_files,
        certificate_files,
        skipped,
        agent_keys,
        identity_agent,
        known_hosts: user_known.into_iter().chain(global_known).collect(),
        host_key_alias,
        revoked_host_keys,
        hostname,
        port,
        user,
    })
}

/// A space-separated list of paths, each with a leading `~` as `home`.
/// The files a list setting names, such as `UserKnownHostsFile`. `ssh -G`
/// prints `none` for one that names none (OpenSSH's
/// `dump_cfg_strarray_oneline`), as `RevokedHostKeys` is when unset.
fn paths(value: &str, home: Option<&Path>) -> Vec<PathBuf> {
    if value.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
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

/// What the tokens in an `IdentityFile` or `CertificateFile` stand for,
/// as `ssh_config(5)`'s TOKENS lists them.
struct Tokens<'a> {
    local: &'a Local,
    /// The hostname connected to (`%h`).
    hostname: &'a str,
    /// `%p`.
    port: u16,
    /// The user signed in as (`%r`).
    user: &'a str,
    /// The destination's host as given, before `HostName` (`%n`).
    original: &'a str,
    /// `HostKeyAlias`, which `%k` is when set.
    alias: Option<&'a str>,
}

impl Tokens<'_> {
    /// The file `raw` names as ssh would open it: its `%` tokens and
    /// `${VAR}`s, which `var` looks up, expanded, then a leading `~` as
    /// home.
    ///
    /// # Errors
    ///
    /// Says why, to follow the file's name, when it holds a token rho-ui
    /// can't expand: `%i`, one ssh doesn't take, or a variable that isn't
    /// set.
    fn expand(&self, raw: &str, var: &dyn Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
        let home = self.local.home.as_deref();
        let mut expanded = String::new();
        let mut rest = raw;
        while let Some(at) = rest.find(['%', '$']) {
            expanded.push_str(&rest[..at]);
            let tail = &rest[at..];
            if let Some(name) = tail.strip_prefix("${") {
                let (name, after) = name
                    .split_once('}')
                    .ok_or("holds a `${` with no `}` after it")?;
                let value =
                    var(name).ok_or_else(|| format!("uses ${{{name}}}, which isn't set"))?;
                expanded.push_str(&value);
                rest = after;
                continue;
            }
            let mut chars = tail.chars();
            let sign = chars.next();
            if sign == Some('$') {
                expanded.push('$');
                rest = chars.as_str();
                continue;
            }
            let token = chars.next().ok_or("ends in a lone %")?;
            let value: Cow<'_, str> = match token {
                '%' => "%".into(),
                'C' => self.connection_hash().into(),
                'd' => home
                    .ok_or("uses %d, and rho-ui found no home directory")?
                    .to_string_lossy(),
                'h' => self.hostname.into(),
                'i' => {
                    return Err("uses %i, your user ID, which rho-ui doesn't expand yet".to_owned())
                }
                // ProxyJump's value: none, as rho-ui refuses a host it names.
                'j' => "".into(),
                'k' => self.alias.unwrap_or(self.original).into(),
                'L' => self.short_hostname().into(),
                'l' => self.local.hostname.as_str().into(),
                'n' => self.original.into(),
                'p' => self.port.to_string().into(),
                'r' => self.user.into(),
                'u' => self.local.user.as_str().into(),
                other => return Err(format!("uses %{other}, which ssh doesn't take there")),
            };
            expanded.push_str(&value);
            rest = chars.as_str();
        }
        expanded.push_str(rest);
        Ok(tilde(Path::new(&expanded), home))
    }

    /// This machine's hostname up to its first dot (`%L`).
    fn short_hostname(&self) -> &str {
        let hostname = self.local.hostname.as_str();
        hostname
            .split_once('.')
            .map_or(hostname, |(short, _)| short)
    }

    /// `%C`: the SHA-1 of `%l%h%p%r%j`, in lowercase hex, as OpenSSH's
    /// `ssh_connection_hash` makes it.
    fn connection_hash(&self) -> String {
        let joined = format!(
            "{}{}{}{}",
            self.local.hostname, self.hostname, self.port, self.user
        );
        let digest =
            ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, joined.as_bytes());
        let mut hex = String::new();
        for byte in digest.as_ref() {
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    }
}

/// What OpenSSH would use for `host` with no config: its `user@`, else
/// the local user, port 22, the default key files and `known_hosts` in
/// `~/.ssh`, and the agent `SSH_AUTH_SOCK` names.
fn defaults(host: &str, local: &Local) -> Resolved {
    let (user, hostname) = host.rsplit_once('@').unwrap_or((local.user.as_str(), host));
    let ssh_dir = local.home.as_ref().map(|home| home.join(".ssh"));
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
        certificate_files: Vec::new(),
        skipped: Vec::new(),
        agent_keys: AgentKeys::All,
        identity_agent: IdentityAgent::Default,
        known_hosts: in_ssh_dir(&["known_hosts", "known_hosts2"]),
        host_key_alias: None,
        revoked_host_keys: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ssh -G` output, cut from OpenSSH 10.0's, with every setting rho-ui
    /// reads.
    const SSH_G: &str = "host box\nuser me\nhostname box.example.com\nport 2222\n\
        identitiesonly yes\nidentityfile ~/.ssh/id_ed25519\nidentityfile %d/keys/%r@%h_%%\n\
        identityfile ~/.ssh/id_%u\nidentityfile ~/.ssh/id_%i\n\
        certificatefile ${KEYS}/box-cert.pub\nhostkeyalias boxalias\n\
        identityagent /run/agent.sock\n\
        userknownhostsfile /home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2\n\
        globalknownhostsfile /etc/ssh/ssh_known_hosts\n\
        revokedhostkeys /home/me/.ssh/revoked\nproxyusefdpass no\n";

    fn local() -> Local {
        Local {
            home: Some(PathBuf::from("/home/me")),
            user: "local".to_owned(),
            hostname: "laptop.lan".to_owned(),
        }
    }

    fn vars(name: &str) -> Option<String> {
        (name == "KEYS").then(|| "/keys".to_owned())
    }

    #[test]
    fn ssh_g_output_resolves_the_route() {
        let home = Path::new("/home/me");
        let resolved = parse("box", SSH_G, &local(), &vars).expect("a route");
        assert_eq!(
            resolved,
            Resolved {
                hostname: "box.example.com".to_owned(),
                port: 2222,
                user: "me".to_owned(),
                identity_files: vec![
                    home.join(".ssh/id_ed25519"),
                    PathBuf::from("/home/me/keys/me@box.example.com_%"),
                    home.join(".ssh/id_local"),
                ],
                certificate_files: vec![PathBuf::from("/keys/box-cert.pub")],
                skipped: vec![Skipped {
                    file: "~/.ssh/id_%i".to_owned(),
                    why: "uses %i, your user ID, which rho-ui doesn't expand yet".to_owned(),
                }],
                agent_keys: AgentKeys::Configured,
                identity_agent: IdentityAgent::Path(PathBuf::from("/run/agent.sock")),
                known_hosts: vec![
                    PathBuf::from("/home/me/.ssh/known_hosts"),
                    PathBuf::from("/home/me/.ssh/known_hosts2"),
                    PathBuf::from("/etc/ssh/ssh_known_hosts"),
                ],
                host_key_alias: Some("boxalias".to_owned()),
                revoked_host_keys: vec![PathBuf::from("/home/me/.ssh/revoked")],
            }
        );
    }

    /// Each token a key file's name may hold expands as OpenSSH's does,
    /// and one rho-ui can't expand says which.
    #[test]
    fn key_file_tokens_expand_as_openssh_does() {
        let local = local();
        let tokens = Tokens {
            local: &local,
            hostname: "box.example.com",
            port: 2222,
            user: "me",
            original: "box",
            alias: None,
        };
        // `printf '%s' laptop.lanbox.example.com2222me | sha1sum`
        let hash = "71b1d853681377c78ba474f90c1376144b9bd198";
        for (raw, expanded) in [
            (
                "/k/%h-%p-%r-%n-%k",
                Ok("/k/box.example.com-2222-me-box-box".to_owned()),
            ),
            (
                "/k/%u@%l-%L-%j%%",
                Ok("/k/local@laptop.lan-laptop-%".to_owned()),
            ),
            ("/k/%C", Ok(format!("/k/{hash}"))),
            ("${KEYS}/$x", Ok("/keys/$x".to_owned())),
            ("~/k-%d", Ok("/home/me/k-/home/me".to_owned())),
            (
                "/k/${NOPE}",
                Err("uses ${NOPE}, which isn't set".to_owned()),
            ),
            (
                "/k/%z",
                Err("uses %z, which ssh doesn't take there".to_owned()),
            ),
            ("/k/%", Err("ends in a lone %".to_owned())),
        ] {
            assert_eq!(
                tokens.expand(raw, &vars),
                expanded.map(PathBuf::from),
                "{raw}"
            );
        }
    }

    /// `ssh -G` prints a list setting that names no file as `none`, as
    /// `RevokedHostKeys` is when unset: that's no file, not one named
    /// `none`, which would refuse every host as unreadable.
    #[test]
    fn a_list_of_files_reads_none_as_no_file() {
        for (line, revoked) in [
            ("revokedhostkeys none", vec![]),
            (
                "revokedhostkeys ~/.ssh/revoked /etc/ssh/revoked",
                vec![
                    PathBuf::from("/home/me/.ssh/revoked"),
                    PathBuf::from("/etc/ssh/revoked"),
                ],
            ),
        ] {
            let output = format!("user me\nhostname box\nuserknownhostsfile none\n{line}\n");
            let resolved = parse("box", &output, &local(), &vars).expect("a route");
            assert_eq!(resolved.revoked_host_keys, revoked, "{line}");
        }
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
            let resolved = parse("box", &output, &local(), &vars).expect("a route");
            assert_eq!(resolved.identity_agent, agent, "{line}");
        }
        for (line, directive) in [
            ("proxyjump bastion", Some("ProxyJump")),
            ("proxycommand nc %h %p", Some("ProxyCommand")),
            ("proxycommand none", None),
        ] {
            let output = format!("user me\nhostname box\n{line}\n");
            match (parse("box", &output, &local(), &vars), directive) {
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
        let local = local();
        let named = defaults("admin@box", &local);
        assert_eq!(
            (named.user.as_str(), named.hostname.as_str()),
            ("admin", "box")
        );
        assert_eq!(
            named.identity_files[0],
            Path::new("/home/me/.ssh/id_ed25519")
        );
        let plain = defaults("box", &local);
        assert_eq!(
            (plain.user.as_str(), plain.hostname.as_str()),
            ("local", "box")
        );
    }
}
