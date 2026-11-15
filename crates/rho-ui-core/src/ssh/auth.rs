//! Signing in to a host with the user's keys, as OpenSSH does in
//! `BatchMode`: rho-ui has no terminal to type a password or passphrase
//! in. It offers the key agent's keys first, those of the configured key
//! files ahead of the rest, then the key files that need no passphrase.
//! An agent such as 1Password's asks the user to approve each signature.

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::Arc,
};

use russh::{
    client::Handle,
    keys::{
        agent::client::{AgentClient, AgentStream},
        load_public_key, load_secret_key, Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg,
        PublicKey,
    },
};

use super::{
    config::{AgentKeys, IdentityAgent, Resolved},
    connection::Verifier,
};
use crate::daemon::DaemonError;

/// A key agent, whichever kind of stream reaches it.
type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// The named pipe Windows' own OpenSSH agent listens on.
#[cfg(windows)]
const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// Signs in to `host` on `handle` as `resolved` says.
///
/// # Errors
///
/// Fails, saying what was tried, when the host accepts none of the keys,
/// or the connection closes.
pub(super) async fn sign_in(
    handle: &mut Handle<Verifier>,
    host: &str,
    resolved: &Resolved,
) -> Result<(), DaemonError> {
    let user = resolved.user.as_str();
    let closed = |error: russh::Error| {
        DaemonError::Unreachable(format!(
            "the connection to {host} closed while signing in: {error}"
        ))
    };
    let configured: Vec<PublicKey> = resolved
        .identity_files
        .iter()
        .filter_map(|file| configured_key(file))
        .collect();
    let mut tried = Tried::default();
    let mut rsa_hash = RsaHash::Unasked;
    match agent(&resolved.identity_agent).await {
        Ok(None) => tried.agent = AgentState::Unset,
        Err(why) => tried.agent = AgentState::Unreachable(why),
        Ok(Some(mut agent)) => {
            // OpenSSH's agent needs this before it uses a key it may only
            // sign with for some hosts; other agents may not know it.
            if let Some(binding) = handle.agent_session_binding() {
                let _ = agent.bind_session(binding, false).await;
            }
            let keys: Vec<PublicKey> = match agent.request_identities().await {
                Ok(identities) => {
                    tried.agent = AgentState::Held(identities.len());
                    identities
                        .iter()
                        .map(|identity| identity.public_key().into_owned())
                        .collect()
                }
                Err(error) => {
                    tried.agent = AgentState::Unreachable(format!(
                        "the key agent didn't list its keys: {error}"
                    ));
                    Vec::new()
                }
            };
            for index in offers(&keys, &configured, resolved.agent_keys) {
                let key = keys[index].clone();
                let hash = rsa_hash_for(handle, &mut rsa_hash, &key)
                    .await
                    .map_err(closed)?;
                match handle
                    .authenticate_publickey_with(user, key.clone(), hash, &mut agent)
                    .await
                {
                    Ok(result) if result.success() => return Ok(()),
                    Ok(_) => {}
                    Err(error) => tried.unsigned.push(error.to_string()),
                }
                tried.offered.push(key);
            }
        }
    }
    for file in &resolved.identity_files {
        let key = match load_secret_key(file, None) {
            Ok(key) => key,
            Err(russh::keys::Error::KeyIsEncrypted) => {
                tried.encrypted.push(file.clone());
                continue;
            }
            // Not there, or not a private key, as ssh skips it.
            Err(_) => continue,
        };
        let public = key.public_key();
        if tried
            .offered
            .iter()
            .any(|offered| offered.key_data() == public.key_data())
        {
            continue;
        }
        let hash = rsa_hash_for(handle, &mut rsa_hash, public)
            .await
            .map_err(closed)?;
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
        if handle
            .authenticate_publickey(user, key)
            .await
            .map_err(closed)?
            .success()
        {
            return Ok(());
        }
        tried.files.push(file.clone());
    }
    Err(DaemonError::Unreachable(tried.failure(host, user)))
}

/// The public key of the key file `file`, to tell the agent's keys for it
/// from the rest: from `file.pub` beside it, else `file` itself when it
/// is a public key, else the public half an OpenSSH private key keeps in
/// the clear, passphrase or not.
fn configured_key(file: &Path) -> Option<PublicKey> {
    let mut beside = OsString::from(file.as_os_str());
    beside.push(".pub");
    load_public_key(PathBuf::from(beside))
        .or_else(|_| load_public_key(file))
        .ok()
        .or_else(|| {
            PrivateKey::read_openssh_file(file)
                .ok()
                .map(|key| key.public_key().clone())
        })
}

/// The order to offer the agent's keys `agent` in, as indices into it:
/// those of the `configured` key files first, in the files' order, then,
/// unless only those may go, the rest in the agent's order.
fn offers(agent: &[PublicKey], configured: &[PublicKey], which: AgentKeys) -> Vec<usize> {
    let position = |key: &PublicKey| {
        configured
            .iter()
            .position(|wanted| wanted.key_data() == key.key_data())
    };
    let mut first: Vec<(usize, usize)> = agent
        .iter()
        .enumerate()
        .filter_map(|(index, key)| Some((position(key)?, index)))
        .collect();
    first.sort_unstable();
    let rest = agent
        .iter()
        .enumerate()
        .filter(|(_, key)| which == AgentKeys::All && position(key).is_none())
        .map(|(index, _)| index);
    first
        .into_iter()
        .map(|(_, index)| index)
        .chain(rest)
        .collect()
}

/// The hash RSA signatures for the host use.
#[derive(Clone, Copy, Debug)]
enum RsaHash {
    /// Not asked yet.
    Unasked,
    /// This one; none for SHA-1 (`ssh-rsa`).
    Asked(Option<HashAlg>),
}

/// The hash to sign with `key` by: for an RSA key, the best the host
/// takes, asked once and kept in `known`; none for other keys.
async fn rsa_hash_for(
    handle: &Handle<Verifier>,
    known: &mut RsaHash,
    key: &PublicKey,
) -> Result<Option<HashAlg>, russh::Error> {
    if !matches!(key.algorithm(), Algorithm::Rsa { .. }) {
        return Ok(None);
    }
    if let RsaHash::Asked(hash) = *known {
        return Ok(hash);
    }
    let hash = handle.best_supported_rsa_hash().await?.flatten();
    *known = RsaHash::Asked(hash);
    Ok(hash)
}

/// The key agent `setting` names, if any is set.
///
/// # Errors
///
/// Says why when one is set but doesn't answer.
async fn agent(setting: &IdentityAgent) -> Result<Option<Agent>, String> {
    let path = match setting {
        IdentityAgent::Off => return Ok(None),
        IdentityAgent::Path(path) => Some(path.clone().into_os_string()),
        IdentityAgent::Variable(name) => std::env::var_os(name),
        IdentityAgent::Default => std::env::var_os("SSH_AUTH_SOCK"),
    };
    let path = path.filter(|path| !path.is_empty());
    match path {
        Some(path) => connect_agent(&path).await.map(Some).map_err(|error| {
            format!(
                "couldn't reach the key agent at {}: {error}",
                Path::new(&path).display()
            )
        }),
        None if *setting == IdentityAgent::Default => Ok(default_agent().await),
        None => Ok(None),
    }
}

/// Connects to the agent at the socket `path`.
#[cfg(unix)]
async fn connect_agent(path: &OsStr) -> Result<Agent, russh::keys::Error> {
    AgentClient::connect_uds(path)
        .await
        .map(AgentClient::dynamic)
}

/// Connects to the agent at the named pipe `path`.
#[cfg(windows)]
async fn connect_agent(path: &OsStr) -> Result<Agent, russh::keys::Error> {
    AgentClient::connect_named_pipe(path)
        .await
        .map(AgentClient::dynamic)
}

/// With no agent set, none: OpenSSH on Unix takes `SSH_AUTH_SOCK` alone.
#[cfg(unix)]
#[expect(
    clippy::unused_async,
    reason = "the same signature as on Windows, where connecting awaits"
)]
async fn default_agent() -> Option<Agent> {
    None
}

/// With no agent set, Windows' own OpenSSH agent, else Pageant, if either
/// runs.
#[cfg(windows)]
async fn default_agent() -> Option<Agent> {
    if let Ok(agent) = AgentClient::connect_named_pipe(OPENSSH_AGENT_PIPE).await {
        return Some(agent.dynamic());
    }
    AgentClient::connect_pageant()
        .await
        .ok()
        .map(AgentClient::dynamic)
}

/// What signing in tried, for the message when nothing worked.
#[derive(Debug, Default)]
struct Tried {
    agent: AgentState,
    /// The agent's keys offered.
    offered: Vec<PublicKey>,
    /// Why the agent didn't sign with a key the host would take.
    unsigned: Vec<String>,
    /// Key files offered.
    files: Vec<PathBuf>,
    /// Key files skipped for their passphrase.
    encrypted: Vec<PathBuf>,
}

#[derive(Debug, Default)]
enum AgentState {
    /// None is set.
    #[default]
    Unset,
    /// One is set but didn't answer, or list its keys; why.
    Unreachable(String),
    /// It held this many keys.
    Held(usize),
}

impl Tried {
    fn failure(&self, host: &str, user: &str) -> String {
        let mut notes = Vec::new();
        match &self.agent {
            AgentState::Unset => notes.push("no key agent is set (SSH_AUTH_SOCK)".to_owned()),
            AgentState::Unreachable(why) => notes.push(why.clone()),
            AgentState::Held(held) => notes.push(format!(
                "the key agent offered {} of its {held} keys",
                self.offered.len()
            )),
        }
        notes.extend(
            self.unsigned
                .iter()
                .map(|why| format!("the key agent didn't sign: {why}")),
        );
        if !self.files.is_empty() {
            let files: Vec<String> = self
                .files
                .iter()
                .map(|file| file.display().to_string())
                .collect();
            notes.push(format!("tried {}", files.join(", ")));
        }
        notes.extend(self.encrypted.iter().map(|file| {
            format!(
                "{0} needs its passphrase, which rho-ui can't ask for: add it to your key agent \
                 with `ssh-add {0}`",
                file.display()
            )
        }));
        format!(
            "{host} accepted none of your keys for {user}; {}",
            notes.join("; ")
        )
    }
}

#[cfg(test)]
mod tests {
    use russh::keys::key::safe_rng;

    use super::*;

    #[test]
    fn the_configured_keys_go_first_and_alone_with_identities_only() {
        let keys: Vec<PublicKey> = (0..3)
            .map(|_| {
                PrivateKey::random(&mut safe_rng(), Algorithm::Ed25519)
                    .expect("a key")
                    .public_key()
                    .clone()
            })
            .collect();
        let (b, c) = (&keys[1], &keys[2]);
        for (configured, which, order) in [
            (vec![], AgentKeys::All, vec![0, 1, 2]),
            (vec![c.clone(), b.clone()], AgentKeys::All, vec![2, 1, 0]),
            (vec![c.clone()], AgentKeys::Configured, vec![2]),
            (vec![], AgentKeys::Configured, vec![]),
        ] {
            assert_eq!(
                offers(&keys, &configured, which),
                order,
                "{} configured, {which:?}",
                configured.len()
            );
        }
    }
}
