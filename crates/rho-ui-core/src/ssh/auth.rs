//! Signing in to a host with the user's keys, as OpenSSH does in
//! `BatchMode`: rho-ui has no terminal to type a password or passphrase
//! in. It offers the key agent's keys and certificates first, those of
//! the configured files ahead of the rest, then each key file that needs
//! no passphrase, with the certificates for its key, then the key alone.
//! An agent such as 1Password's asks the user to approve each signature.
//!
//! The files are read before connecting ([`Identities::read`]), off the
//! I/O runtime, so signing in waits only on the network and the agent.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

use russh::{
    client::Handle,
    keys::{
        agent::{
            client::{AgentClient, AgentStream},
            AgentIdentity,
        },
        load_public_key, load_secret_key,
        ssh_key::{public::KeyData, Signature},
        Algorithm, Certificate, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey,
    },
};

use super::{
    config::{AgentKeys, IdentityAgent, Resolved, Skipped},
    host_key::Verifier,
};
use crate::daemon::DaemonError;

/// A key agent, whichever kind of stream reaches it.
type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// The named pipe Windows' own OpenSSH agent listens on.
#[cfg(windows)]
const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// The key and certificate files the ssh config names, as read from disk.
#[derive(Debug, Default)]
pub(super) struct Identities {
    /// The public keys of the configured key and certificate files, in
    /// order: the agent's keys for them go first, or alone with
    /// `IdentitiesOnly`.
    configured: Vec<PublicKey>,
    /// Key files that need no passphrase and hold a kind of key russh
    /// signs with, in the order to offer them.
    files: Vec<KeyFile>,
    /// Key files skipped for their passphrase.
    encrypted: Vec<PathBuf>,
    /// Files not offered, and why.
    skipped: Vec<Skipped>,
}

/// A key file's key, and the certificates for it.
#[derive(Debug)]
struct KeyFile {
    path: PathBuf,
    key: Arc<PrivateKey>,
    /// From `<path>-cert.pub` beside it, then `CertificateFile`.
    certificates: Vec<CertificateFile>,
}

/// A certificate read from a file.
#[derive(Debug)]
struct CertificateFile {
    path: PathBuf,
    certificate: Certificate,
}

impl Identities {
    /// Reads the key and certificate files `resolved` names, as OpenSSH
    /// does: a key file's certificate may sit beside it, as
    /// `<file>-cert.pub`, or be a `CertificateFile`. A file that isn't
    /// there, or isn't a key, is left out, as ssh leaves it. Reads the
    /// files, so runs off the I/O runtime.
    pub(super) fn read(resolved: &Resolved) -> Self {
        let named: Vec<CertificateFile> = resolved
            .certificate_files
            .iter()
            .filter_map(|path| read_certificate(path))
            .collect();
        let mut identities = Self {
            skipped: resolved.skipped.clone(),
            ..Self::default()
        };
        identities.configured = resolved
            .identity_files
            .iter()
            .filter_map(|file| configured_key(file))
            .chain(named.iter().map(|named| key_of(&named.certificate)))
            .collect();
        let mut paired = vec![false; named.len()];
        for path in &resolved.identity_files {
            let key = match load_secret_key(path, None) {
                Ok(key) => key,
                Err(russh::keys::Error::KeyIsEncrypted) => {
                    identities.encrypted.push(path.clone());
                    continue;
                }
                // Not there, or not a private key, as ssh skips it.
                Err(_) => continue,
            };
            let algorithm = key.algorithm();
            if !signs_with(&algorithm) {
                // russh can't sign with it, and a failed signature ends
                // the connection, so it would stop every key after it.
                identities.skipped.push(Skipped {
                    file: path.display().to_string(),
                    why: format!("rho-ui can't sign with its kind of key, {algorithm}"),
                });
                continue;
            }
            let data = key.public_key().key_data();
            let rsa = matches!(algorithm, Algorithm::Rsa { .. });
            let mut certificates: Vec<CertificateFile> =
                read_certificate(&suffixed(path, "-cert.pub"))
                    .into_iter()
                    .filter(|beside| beside.certificate.public_key() == data)
                    .collect();
            for (index, named) in named.iter().enumerate() {
                if named.certificate.public_key() == data {
                    paired[index] = true;
                    certificates.push(CertificateFile {
                        path: named.path.clone(),
                        certificate: named.certificate.clone(),
                    });
                }
            }
            if rsa {
                // russh signs an RSA certificate file's sign-in with SHA-512
                // under the name `ssh-rsa-cert-v01`, which sshd refuses, and
                // each refusal spends one of its MaxAuthTries.
                identities
                    .skipped
                    .extend(certificates.drain(..).map(|certificate| {
                        Skipped {
                            file: certificate.path.display().to_string(),
                            why:
                                "rho-ui can't sign in with an RSA certificate from a file; add the \
                              key and its certificate to your key agent"
                                    .to_owned(),
                        }
                    }));
            }
            identities.files.push(KeyFile {
                path: path.clone(),
                key: Arc::new(key),
                certificates,
            });
        }
        let unpaired = named.iter().zip(paired).filter(|(_, paired)| !paired);
        identities
            .skipped
            .extend(unpaired.map(|(named, _)| Skipped {
                file: named.path.display().to_string(),
                why: UNPAIRED.to_owned(),
            }));
        identities
    }
}

/// Why a `CertificateFile` whose key no key file holds isn't offered.
const UNPAIRED: &str = "rho-ui offers a certificate file with its key file, and found no key \
                        file it can use that holds its key";

/// Whether russh signs with keys of `algorithm`: not with FIDO (`sk-`) or
/// DSA keys.
fn signs_with(algorithm: &Algorithm) -> bool {
    matches!(
        algorithm,
        Algorithm::Ed25519 | Algorithm::Ecdsa { .. } | Algorithm::Rsa { .. }
    )
}

/// `path` with `suffix` added to its name.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// The certificate in the file `path`, if it holds one.
fn read_certificate(path: &Path) -> Option<CertificateFile> {
    let text = std::fs::read_to_string(path).ok()?;
    let certificate = Certificate::from_openssh(text.trim()).ok()?;
    Some(CertificateFile {
        path: path.to_path_buf(),
        certificate,
    })
}

/// The key `certificate` certifies.
fn key_of(certificate: &Certificate) -> PublicKey {
    PublicKey::from(certificate.public_key().clone())
}

/// The public key of the key file `file`, to tell the agent's keys for it
/// from the rest: from `file.pub` beside it, else `file` itself when it
/// is a public key, else the public half an OpenSSH private key keeps in
/// the clear, passphrase or not.
fn configured_key(file: &Path) -> Option<PublicKey> {
    load_public_key(suffixed(file, ".pub"))
        .or_else(|_| load_public_key(file))
        .ok()
        .or_else(|| {
            PrivateKey::read_openssh_file(file)
                .ok()
                .map(|key| key.public_key().clone())
        })
}

/// One way to prove who the user is.
#[derive(Clone, Copy, Debug)]
enum Offer<'a> {
    /// A key the agent holds, which signs.
    AgentKey(&'a PublicKey),
    /// A certificate the agent holds, which signs with its key.
    AgentCertificate(&'a Certificate),
    /// A key file's key, on its own.
    FileKey(&'a KeyFile),
    /// A certificate file, signed with its key file's key.
    FileCertificate(&'a KeyFile, &'a CertificateFile),
}

/// What an [`Offer`] shows the host: a key, or a certificate.
#[derive(PartialEq)]
enum Credential<'a> {
    Key(&'a KeyData),
    /// A certificate, told apart by its authority's signature, which
    /// covers all of it.
    Certificate(&'a Signature),
}

impl<'a> Offer<'a> {
    fn credential(&self) -> Credential<'a> {
        match *self {
            Self::AgentKey(key) => Credential::Key(key.key_data()),
            Self::FileKey(file) => Credential::Key(file.key.public_key().key_data()),
            Self::AgentCertificate(certificate) => Credential::Certificate(certificate.signature()),
            Self::FileCertificate(_, file) => Credential::Certificate(file.certificate.signature()),
        }
    }
}

/// What to offer the host, in order: the agent's identities, `agent`, as
/// [`agent_order`] puts them, then each key file's certificates and its
/// key. Each key and each certificate goes once, so a key the agent and
/// a file both hold doesn't spend two of the host's tries (sshd's
/// `MaxAuthTries`, 6 by default); a certificate and its key are two
/// offers, as with OpenSSH, since a host may take either alone.
fn offers<'a>(
    agent: &'a [AgentIdentity],
    identities: &'a Identities,
    which: AgentKeys,
) -> Vec<Offer<'a>> {
    let from_agent = agent_order(agent, &identities.configured, which)
        .into_iter()
        .map(|index| match &agent[index] {
            AgentIdentity::PublicKey { key, .. } => Offer::AgentKey(key),
            AgentIdentity::Certificate { certificate, .. } => Offer::AgentCertificate(certificate),
        });
    let from_files = identities.files.iter().flat_map(|file| {
        file.certificates
            .iter()
            .map(move |certificate| Offer::FileCertificate(file, certificate))
            .chain([Offer::FileKey(file)])
    });
    let mut shown: Vec<Credential<'a>> = Vec::new();
    from_agent
        .chain(from_files)
        .filter(|offer| {
            let credential = offer.credential();
            let new = !shown.contains(&credential);
            if new {
                shown.push(credential);
            }
            new
        })
        .collect()
}

/// The order to offer the agent's identities `agent` in, as indices into
/// it: those of the `configured` files' keys first, in the files' order,
/// then, unless only those may go, the rest in the agent's order. A
/// certificate counts by the key it certifies.
fn agent_order(agent: &[AgentIdentity], configured: &[PublicKey], which: AgentKeys) -> Vec<usize> {
    let position = |identity: &AgentIdentity| {
        let key = identity.public_key();
        configured
            .iter()
            .position(|wanted| wanted.key_data() == key.key_data())
    };
    let mut first: Vec<(usize, usize)> = agent
        .iter()
        .enumerate()
        .filter_map(|(index, identity)| Some((position(identity)?, index)))
        .collect();
    first.sort_unstable();
    let rest = agent
        .iter()
        .enumerate()
        .filter(|(_, identity)| which == AgentKeys::All && position(identity).is_none())
        .map(|(index, _)| index);
    first
        .into_iter()
        .map(|(_, index)| index)
        .chain(rest)
        .collect()
}

/// Signs in to `host` on `handle` as `resolved` says, with the key agent
/// and `identities`, `resolved`'s files.
///
/// # Errors
///
/// Fails, saying what was tried, when the host accepts none of the keys,
/// or the connection closes.
pub(super) async fn sign_in(
    handle: &mut Handle<Verifier>,
    host: &str,
    resolved: &Resolved,
    identities: &Identities,
) -> Result<(), DaemonError> {
    let user = resolved.user.as_str();
    let (mut agent, held, agent_state) = open_agent(handle, &resolved.identity_agent).await;
    let mut tried = Tried {
        agent: agent_state,
        agent_offered: 0,
        unsigned: Vec::new(),
        files: Vec::new(),
    };
    let mut rsa_hash = RsaHash::Unasked;
    for offer in offers(&held, identities, resolved.agent_keys) {
        let accepted = try_offer(
            handle,
            user,
            &offer,
            agent.as_mut(),
            &mut rsa_hash,
            &mut tried,
        )
        .await
        .map_err(|error| {
            DaemonError::Unreachable(format!(
                "the connection to {host} closed while signing in: {error}"
            ))
        })?;
        if accepted {
            return Ok(());
        }
    }
    Err(DaemonError::Unreachable(
        tried.failure(host, user, identities),
    ))
}

/// The key agent `setting` names, bound to the connection on `handle`,
/// with the keys and certificates it holds; or none, and why.
async fn open_agent(
    handle: &Handle<Verifier>,
    setting: &IdentityAgent,
) -> (Option<Agent>, Vec<AgentIdentity>, AgentState) {
    let mut agent = match agent(setting).await {
        Ok(agent) => agent,
        Err(why) => return (None, Vec::new(), AgentState::Missing(why)),
    };
    // OpenSSH's agent needs this before it uses a key it may only sign
    // with for some hosts; other agents may not know it.
    if let Some(binding) = handle.agent_session_binding() {
        let _ = agent.bind_session(binding, false).await;
    }
    match agent.request_identities().await {
        Ok(held) => {
            let state = AgentState::Held(held.len());
            (Some(agent), held, state)
        }
        Err(error) => (
            Some(agent),
            Vec::new(),
            AgentState::Missing(format!("the key agent didn't list its keys: {error}")),
        ),
    }
}

/// Offers `offer` to the host as `user`, signing with `agent` for the
/// agent's own, and notes it in `tried`. Whether the host accepted it.
///
/// # Errors
///
/// Fails if the connection closes.
async fn try_offer(
    handle: &mut Handle<Verifier>,
    user: &str,
    offer: &Offer<'_>,
    agent: Option<&mut Agent>,
    rsa_hash: &mut RsaHash,
    tried: &mut Tried,
) -> Result<bool, russh::Error> {
    let accepted = match (*offer, agent) {
        (Offer::AgentKey(key), Some(agent)) => {
            tried.agent_offered += 1;
            let hash = rsa_hash_for(handle, rsa_hash, &key.algorithm()).await?;
            handle
                .authenticate_publickey_with(user, key.clone(), hash, agent)
                .await
                .inspect_err(|error| tried.unsigned.push(error.to_string()))
                .is_ok_and(|result| result.success())
        }
        (Offer::AgentCertificate(certificate), Some(agent)) => {
            tried.agent_offered += 1;
            let hash = rsa_hash_for(handle, rsa_hash, &certificate.algorithm()).await?;
            handle
                .authenticate_certificate_with(user, certificate.clone(), hash, agent)
                .await
                .inspect_err(|error| tried.unsigned.push(error.to_string()))
                .is_ok_and(|result| result.success())
        }
        // The agent's identities come only with the agent.
        (Offer::AgentKey(_) | Offer::AgentCertificate(_), None) => false,
        (Offer::FileKey(file), _) => {
            tried.files.push(file.path.clone());
            let hash = rsa_hash_for(handle, rsa_hash, &file.key.algorithm()).await?;
            let key = PrivateKeyWithHashAlg::new(Arc::clone(&file.key), hash);
            handle.authenticate_publickey(user, key).await?.success()
        }
        (Offer::FileCertificate(file, certificate), _) => {
            tried.files.push(certificate.path.clone());
            handle
                .authenticate_openssh_cert(
                    user,
                    Arc::clone(&file.key),
                    certificate.certificate.clone(),
                )
                .await?
                .success()
        }
    };
    Ok(accepted)
}

/// The hash RSA signatures for the host use.
#[derive(Clone, Copy, Debug)]
enum RsaHash {
    /// Not asked yet.
    Unasked,
    /// This one; none for SHA-1 (`ssh-rsa`).
    Asked(Option<HashAlg>),
}

/// The hash to sign with a key of `algorithm` by: for RSA, the best the
/// host takes, asked once and kept in `known`; none for other keys.
async fn rsa_hash_for(
    handle: &Handle<Verifier>,
    known: &mut RsaHash,
    algorithm: &Algorithm,
) -> Result<Option<HashAlg>, russh::Error> {
    if !matches!(algorithm, Algorithm::Rsa { .. }) {
        return Ok(None);
    }
    if let RsaHash::Asked(hash) = *known {
        return Ok(hash);
    }
    let hash = handle.best_supported_rsa_hash().await?.flatten();
    *known = RsaHash::Asked(hash);
    Ok(hash)
}

/// The key agent `setting` names.
///
/// # Errors
///
/// Says why there is none: the config turns it off, none is set, or the
/// one set doesn't answer.
async fn agent(setting: &IdentityAgent) -> Result<Agent, String> {
    let (path, variable) = match setting {
        IdentityAgent::Off => {
            return Err("your ssh config turns the key agent off (IdentityAgent none)".to_owned())
        }
        IdentityAgent::Path(path) => (Some(path.clone().into_os_string()), None),
        IdentityAgent::Variable(name) => (std::env::var_os(name), Some(name.as_str())),
        IdentityAgent::Default => (std::env::var_os("SSH_AUTH_SOCK"), None),
    };
    match (path.filter(|path| !path.is_empty()), variable) {
        (Some(path), _) => connect_agent(&path).await.map_err(|error| {
            format!(
                "couldn't reach the key agent at {}: {error}",
                Path::new(&path).display()
            )
        }),
        (None, Some(name)) => Err(format!(
            "no key agent is set (${name}, which your ssh config's IdentityAgent names)"
        )),
        (None, None) => default_agent().await,
    }
}

/// Connects to the agent at the socket `path`.
#[cfg(unix)]
async fn connect_agent(path: &std::ffi::OsStr) -> Result<Agent, russh::keys::Error> {
    AgentClient::connect_uds(path)
        .await
        .map(AgentClient::dynamic)
}

/// Connects to the agent at the named pipe `path`.
#[cfg(windows)]
async fn connect_agent(path: &std::ffi::OsStr) -> Result<Agent, russh::keys::Error> {
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
async fn default_agent() -> Result<Agent, String> {
    Err("no key agent is set (SSH_AUTH_SOCK)".to_owned())
}

/// With no agent set, Windows' own OpenSSH agent, if it runs. Pageant
/// isn't asked: its client waits on a window message, which would hold
/// the I/O runtime's one worker while Pageant asks for a PIN.
#[cfg(windows)]
async fn default_agent() -> Result<Agent, String> {
    AgentClient::connect_named_pipe(OPENSSH_AGENT_PIPE)
        .await
        .map(AgentClient::dynamic)
        .map_err(|_| {
            "no key agent is set (SSH_AUTH_SOCK), and Windows' OpenSSH agent isn't running"
                .to_owned()
        })
}

/// What signing in tried, for the message when nothing worked.
#[derive(Debug)]
struct Tried {
    agent: AgentState,
    /// How many of the agent's keys and certificates were offered.
    agent_offered: usize,
    /// Why the agent didn't sign with a key the host would take.
    unsigned: Vec<String>,
    /// Key and certificate files offered.
    files: Vec<PathBuf>,
}

#[derive(Debug)]
enum AgentState {
    /// There is none, or it didn't list its keys; why.
    Missing(String),
    /// It held this many keys and certificates.
    Held(usize),
}

impl Tried {
    fn failure(&self, host: &str, user: &str, identities: &Identities) -> String {
        let mut notes = Vec::new();
        match &self.agent {
            AgentState::Missing(why) => notes.push(why.clone()),
            AgentState::Held(held) => notes.push(format!(
                "the key agent offered {} of its {held} keys",
                self.agent_offered
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
        notes.extend(identities.encrypted.iter().map(|file| {
            format!(
                "{0} needs its passphrase, which rho-ui can't ask for: add it to your key agent \
                 with `ssh-add {0}`",
                file.display()
            )
        }));
        notes.extend(
            identities
                .skipped
                .iter()
                .map(|skipped| format!("skipped {}: {}", skipped.file, skipped.why)),
        );
        format!(
            "{host} accepted none of your keys for {user}; {}",
            notes.join("; ")
        )
    }
}

#[cfg(test)]
mod tests {
    use russh::keys::{
        key::safe_rng,
        ssh_key::certificate::{Builder, CertType},
    };

    use super::*;

    fn private_key() -> PrivateKey {
        PrivateKey::random(&mut safe_rng(), Algorithm::Ed25519).expect("a key")
    }

    /// A user certificate for `key`, signed by a fresh authority.
    fn certificate(key: &PublicKey) -> Certificate {
        let mut builder = Builder::new([7_u8; 16], key.key_data().clone(), 0, u64::MAX)
            .expect("a certificate builder");
        builder
            .cert_type(CertType::User)
            .expect("a user certificate")
            .all_principals_valid()
            .expect("any principal");
        builder.sign(&private_key()).expect("a signed certificate")
    }

    /// What each offer is, by name, for comparing orders.
    fn names(
        offers: &[Offer<'_>],
        name: impl Fn(&Credential<'_>) -> &'static str,
    ) -> Vec<&'static str> {
        offers
            .iter()
            .map(|offer| name(&offer.credential()))
            .collect()
    }

    /// The agent's identities go first, those of the configured files
    /// ahead of the rest and alone with `IdentitiesOnly`; then each key
    /// file's certificate and key. A key or certificate that the agent and
    /// a file both hold goes once, and an agent certificate goes as a
    /// certificate, not as a bare key that repeats its key's offer.
    #[test]
    fn offers_go_configured_first_and_each_once() {
        let (a, b, c) = (private_key(), private_key(), private_key());
        let (a_cert, c_cert) = (certificate(a.public_key()), certificate(c.public_key()));
        let agent = [
            AgentIdentity::from(a.public_key().clone()),
            AgentIdentity::from(a_cert.clone()),
            AgentIdentity::from(b.public_key().clone()),
        ];
        let file = |key: &PrivateKey, certificates| KeyFile {
            path: PathBuf::from("/k"),
            key: Arc::new(key.clone()),
            certificates,
        };
        let identities = Identities {
            configured: vec![b.public_key().clone(), c.public_key().clone()],
            files: vec![
                file(&b, Vec::new()),
                file(
                    &c,
                    vec![CertificateFile {
                        path: PathBuf::from("/k-cert.pub"),
                        certificate: c_cert.clone(),
                    }],
                ),
            ],
            ..Identities::default()
        };
        let name = |credential: &Credential<'_>| match *credential {
            Credential::Key(data) if data == a.public_key().key_data() => "a",
            Credential::Key(data) if data == b.public_key().key_data() => "b",
            Credential::Key(data) if data == c.public_key().key_data() => "c",
            Credential::Certificate(signature) if signature == a_cert.signature() => "a cert",
            Credential::Certificate(signature) if signature == c_cert.signature() => "c cert",
            Credential::Key(_) | Credential::Certificate(_) => "unknown",
        };
        for (which, order) in [
            (AgentKeys::All, vec!["b", "a", "a cert", "c cert", "c"]),
            (AgentKeys::Configured, vec!["b", "c cert", "c"]),
        ] {
            assert_eq!(
                names(&offers(&agent, &identities, which), name),
                order,
                "{which:?}"
            );
        }
    }
}
