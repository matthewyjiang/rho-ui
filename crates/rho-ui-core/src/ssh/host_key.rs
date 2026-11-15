//! Checking a host's key against the user's `known_hosts` files, as OpenSSH
//! does with `StrictHostKeyChecking yes`: rho-ui has no terminal to ask
//! whether to trust a new key in, so a key that isn't there stops the
//! connection, and rho-ui never writes the files. A key in the
//! `RevokedHostKeys` file stops it too. rho-ui checks plain host keys
//! only, not host certificates.

use std::path::PathBuf;

use base64::Engine as _;
use russh::{
    client,
    keys::{Algorithm, PublicKey, PublicKeyOrCertificate},
    Preferred,
};

use super::{config::Resolved, quote};

/// A key the `known_hosts` files list for the host, or the
/// `RevokedHostKeys` file lists, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KnownKey {
    key: PublicKey,
    listing: Listing,
    file: PathBuf,
    /// Counted from 1.
    line: usize,
}

/// What a line says of its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listing {
    /// The host's key: a plain line.
    Trusted,
    /// Never the host's key: a line marked `@revoked`, or one of the
    /// `RevokedHostKeys` file.
    Revoked,
    /// A certificate authority that vouches for the host's certificates:
    /// a line marked `@cert-authority`.
    Authority,
}

/// Whether the host's key is the one the files list.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    /// It is.
    Known,
    /// The files list no key of its kind for the host.
    Unknown,
    /// They list another key of its kind for the host, here.
    Changed { file: PathBuf, line: usize },
    /// They mark it revoked, here.
    Revoked { file: PathBuf, line: usize },
    /// They list no key of its kind, but a certificate authority for it,
    /// here, and rho-ui doesn't check host certificates.
    Certified { file: PathBuf, line: usize },
}

impl Verdict {
    /// Why rho-ui won't connect to `host`, filed under `name` in
    /// `known_hosts`, with a key of this verdict; `None` when it will.
    fn refusal(&self, host: &str, name: &str) -> Option<String> {
        match self {
            Self::Known => None,
            Self::Unknown => Some(format!(
                "{host}'s host key isn't in your known_hosts: run `ssh {host}` once in a \
                 terminal to check the key and accept it, then retry"
            )),
            Self::Changed { file, line } => Some(format!(
                "{host}'s host key differs from the one in {}:{line}. Someone may be \
                 intercepting the connection; if you know the key changed, remove the old one \
                 with `ssh-keygen -R {}` and accept the new one with `ssh {host}` in a \
                 terminal",
                file.display(),
                quote(name)
            )),
            Self::Revoked { file, line } => Some(format!(
                "{host}'s host key is marked revoked in {}:{line}",
                file.display()
            )),
            Self::Certified { file, line } => Some(format!(
                "{host}'s host key isn't in your known_hosts, which vouches for it only through \
                 the certificate authority in {}:{line}, and rho-ui doesn't check host \
                 certificates yet: it needs the host's own key in known_hosts",
                file.display()
            )),
        }
    }
}

/// Checks the host's key against what the files list while the
/// connection opens. russh's client handler.
pub(super) struct Verifier {
    host: String,
    /// The name the host is filed under in `known_hosts`.
    name: String,
    known: Vec<KnownKey>,
}

/// Why a connection didn't open.
#[derive(Debug)]
pub(super) enum Refusal {
    /// ssh failed, as russh says.
    Ssh(russh::Error),
    /// rho-ui won't take the host's key; why, for people.
    HostKey(String),
}

impl From<russh::Error> for Refusal {
    fn from(error: russh::Error) -> Self {
        Self::Ssh(error)
    }
}

impl Verifier {
    /// Checks `host`'s key, reached as `resolved` says, against `known`.
    pub(super) fn new(host: &str, resolved: &Resolved, known: Vec<KnownKey>) -> Self {
        Self {
            host: host.to_owned(),
            name: lookup_name(resolved),
            known,
        }
    }

    /// Whether the host's key is one the files name for it.
    fn check(&self, server_public_key: &PublicKeyOrCertificate) -> Result<bool, Refusal> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_public_key else {
            return Err(Refusal::HostKey(format!(
                "{} offered a host certificate, and rho-ui checks only plain host keys",
                self.host
            )));
        };
        match verdict(&self.known, key).refusal(&self.host, &self.name) {
            None => Ok(true),
            Some(why) => Err(Refusal::HostKey(why)),
        }
    }
}

impl client::Handler for Verifier {
    type Error = Refusal;

    fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        std::future::ready(self.check(server_public_key))
    }
}

/// The name `resolved`'s host is filed under in `known_hosts`: its
/// `HostKeyAlias`, else its hostname, as `[hostname]:port` off port 22.
fn lookup_name(resolved: &Resolved) -> String {
    match &resolved.host_key_alias {
        Some(alias) => alias.to_ascii_lowercase(),
        None if resolved.port == 22 => resolved.hostname.to_ascii_lowercase(),
        None => format!(
            "[{}]:{}",
            resolved.hostname.to_ascii_lowercase(),
            resolved.port
        ),
    }
}

/// The keys `resolved`'s `known_hosts` files list for its host, and those
/// its `RevokedHostKeys` file lists. A `known_hosts` file that can't be
/// read lists none, as for ssh, and so does a `RevokedHostKeys` file that
/// isn't there. Reads the files, so runs off the I/O runtime.
///
/// # Errors
///
/// Fails, saying why, when the `RevokedHostKeys` file can't be read or
/// isn't a text list of keys, such as a binary KRL: rho-ui won't connect
/// without checking it.
pub(super) fn read(resolved: &Resolved) -> Result<Vec<KnownKey>, String> {
    let name = lookup_name(resolved);
    let mut known: Vec<KnownKey> = resolved
        .known_hosts
        .iter()
        .flat_map(|file| {
            let text = std::fs::read(file).unwrap_or_default();
            entries(&String::from_utf8_lossy(&text), &name)
                .into_iter()
                .map(|(line, listing, key)| KnownKey {
                    key,
                    listing,
                    file: file.clone(),
                    line,
                })
        })
        .collect();
    for file in &resolved.revoked_host_keys {
        let refused = |why: String| {
            format!(
                "rho-ui can't check {}, your RevokedHostKeys: {why}. It reads a text list of \
                 public keys, not a KRL, and won't connect without checking",
                file.display()
            )
        };
        // As OpenSSH: a RevokedHostKeys that isn't there or can't be read
        // refuses every host.
        match std::fs::read(file) {
            Err(error) => return Err(refused(error.to_string())),
            Ok(bytes) => known.extend(revoked(&bytes).map_err(refused)?.into_iter().map(
                |(line, key)| KnownKey {
                    key,
                    listing: Listing::Revoked,
                    file: file.clone(),
                    line,
                },
            )),
        }
    }
    Ok(known)
}

/// The keys a `RevokedHostKeys` file in text lists, with their line
/// numbers: one public key a line, blank lines and `#` comments skipped.
///
/// # Errors
///
/// Says why when the file is a binary key revocation list (KRL), or a line
/// isn't a key: OpenSSH fails a check of either it can't read.
fn revoked(bytes: &[u8]) -> Result<Vec<(usize, PublicKey)>, String> {
    /// How a KRL starts (OpenSSH's `PROTOCOL.krl`).
    const KRL_MAGIC: &[u8] = b"SSHKRL\n\0";
    if bytes.starts_with(KRL_MAGIC) {
        return Err("it is a key revocation list (KRL)".to_owned());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "it isn't text".to_owned())?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#')
        })
        .map(|(index, line)| {
            PublicKey::from_openssh(line.trim())
                .map(|key| (index + 1, key))
                .map_err(|_| format!("line {} isn't a public key", index + 1))
        })
        .collect()
}

/// The keys `text`, a `known_hosts` file, lists for `name`, with their
/// line numbers: lines of `[marker] patterns keytype base64 [comment]`.
/// The patterns are a comma-separated list that may hold `*`, `?` and
/// negations (`!pattern`), or one hashed name (`|1|salt|hash`). Lines
/// whose key doesn't parse are left out.
fn entries(text: &str, name: &str) -> Vec<(usize, Listing, PublicKey)> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let mut fields = line.split_whitespace();
            let first = fields.next()?;
            let (listing, patterns) = match first {
                "@revoked" => (Listing::Revoked, fields.next()?),
                "@cert-authority" => (Listing::Authority, fields.next()?),
                marker if marker.starts_with('@') || marker.starts_with('#') => return None,
                patterns => (Listing::Trusted, patterns),
            };
            if !hosts_match(patterns, name) {
                return None;
            }
            let (algorithm, base64) = (fields.next()?, fields.next()?);
            let key = PublicKey::from_openssh(&format!("{algorithm} {base64}")).ok()?;
            Some((index + 1, listing, key))
        })
        .collect()
}

/// Whether `name`, in lowercase, matches a `known_hosts` line's host
/// patterns.
fn hosts_match(patterns: &str, name: &str) -> bool {
    if let Some(hashed) = patterns.strip_prefix("|1|") {
        return hashed_match(hashed, name);
    }
    let mut matched = false;
    for pattern in patterns.split(',') {
        match pattern.strip_prefix('!') {
            Some(negated) if glob(negated.as_bytes(), name.as_bytes()) => return false,
            Some(_) => {}
            None => matched |= glob(pattern.as_bytes(), name.as_bytes()),
        }
    }
    matched
}

/// Whether `salt|hash`, both base64, is HMAC-SHA1 of `name` keyed with
/// the salt: OpenSSH's `HashKnownHosts`.
fn hashed_match(hashed: &str, name: &str) -> bool {
    let engine = base64::engine::general_purpose::STANDARD;
    let Some((salt, hash)) = hashed.split_once('|') else {
        return false;
    };
    let (Ok(salt), Ok(hash)) = (engine.decode(salt), engine.decode(hash)) else {
        return false;
    };
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, &salt);
    ring::hmac::verify(&key, name.as_bytes(), &hash).is_ok()
}

/// Whether `text` matches `pattern`, where `*` matches any run of bytes
/// and `?` any one, ignoring ASCII case.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|skip| glob(rest, &text[skip..])),
        Some((b'?', rest)) => text.split_first().is_some_and(|(_, tail)| glob(rest, tail)),
        Some((wanted, rest)) => text
            .split_first()
            .is_some_and(|(got, tail)| wanted.eq_ignore_ascii_case(got) && glob(rest, tail)),
    }
}

/// Whether `offered`, the key the host proved it holds, is one `known`
/// lists for it. A host that holds keys of several kinds offers one, so a
/// key of another kind than those listed is unknown, not changed; and a
/// host may have two keys of one kind listed, as when its key is rotated.
/// A host the files vouch for only through a certificate authority is
/// [`Verdict::Certified`] rather than unknown or changed.
fn verdict(known: &[KnownKey], offered: &PublicKey) -> Verdict {
    let same = |known: &&KnownKey| known.key.key_data() == offered.key_data();
    let listed = |listing: Listing| move |known: &&KnownKey| known.listing == listing;
    if let Some(revoked) = known.iter().filter(listed(Listing::Revoked)).find(same) {
        return Verdict::Revoked {
            file: revoked.file.clone(),
            line: revoked.line,
        };
    }
    if known
        .iter()
        .filter(listed(Listing::Trusted))
        .any(|known| same(&known))
    {
        return Verdict::Known;
    }
    // Another key of its kind beats a certificate authority: the warning
    // that the connection may be intercepted mustn't turn into advice.
    if let Some(other) = known
        .iter()
        .filter(listed(Listing::Trusted))
        .find(|known| known.key.algorithm() == offered.algorithm())
    {
        return Verdict::Changed {
            file: other.file.clone(),
            line: other.line,
        };
    }
    match known.iter().find(listed(Listing::Authority)) {
        Some(authority) => Verdict::Certified {
            file: authority.file.clone(),
            line: authority.line,
        },
        None => Verdict::Unknown,
    }
}

/// The host key algorithms to ask for, in order: russh's defaults, those
/// of the kinds `known` lists first, as OpenSSH orders them, so a host
/// with keys of several kinds proves itself with one the user has.
pub(super) fn preferred(known: &[KnownKey]) -> Vec<Algorithm> {
    let listed = |algorithm: &Algorithm| {
        known
            .iter()
            .filter(|known| known.listing == Listing::Trusted)
            .any(|known| same_kind(&known.key.algorithm(), algorithm))
    };
    let (first, rest): (Vec<Algorithm>, Vec<Algorithm>) =
        Preferred::DEFAULT.key.iter().cloned().partition(listed);
    first.into_iter().chain(rest).collect()
}

/// Whether host keys of algorithms `a` and `b` are of one kind: equal,
/// or both RSA, whichever hash each signs with.
fn same_kind(a: &Algorithm, b: &Algorithm) -> bool {
    a == b || (matches!(a, Algorithm::Rsa { .. }) && matches!(b, Algorithm::Rsa { .. }))
}

#[cfg(test)]
mod tests {
    use russh::keys::{key::safe_rng, EcdsaCurve, PrivateKey};

    use super::*;

    fn key(algorithm: Algorithm) -> PublicKey {
        PrivateKey::random(&mut safe_rng(), algorithm)
            .expect("a key")
            .public_key()
            .clone()
    }

    fn line(name: &str, key: &PublicKey) -> String {
        format!("{name} {}", key.to_openssh().expect("an OpenSSH key"))
    }

    /// Lines match by plain name, pattern, negation, hashed name, or
    /// `[host]:port`, and number from 1 counting comments.
    #[test]
    fn known_hosts_lines_match_the_host_and_keep_their_numbers() {
        // A hashed entry for example.com, from russh's own tests.
        const HASHED: &str = "|1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|nuuC5vEqXlEZ/8BXQR7m619W6Ak= \
            ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILIG2T/B0l0gaqj3puu510tu9N1OkQ4znY3LYuEm5zCF";
        let ed = key(Algorithm::Ed25519);
        let text = [
            "# a comment".to_owned(),
            line("box,10.0.0.7", &ed),
            line("*.example.org,!bad.example.org", &ed),
            line("[box]:2222", &ed),
            HASHED.to_owned(),
            format!("@revoked {}", line("box", &ed)),
            format!("@cert-authority {}", line("*.example.org", &ed)),
            "box ssh-ed25519 not-base64".to_owned(),
        ]
        .join("\n");
        let lines = |name: &str| -> Vec<(usize, Listing)> {
            entries(&text, name)
                .into_iter()
                .map(|(line, listing, _)| (line, listing))
                .collect()
        };
        assert_eq!(lines("box"), [(2, Listing::Trusted), (6, Listing::Revoked)]);
        assert_eq!(lines("10.0.0.7"), [(2, Listing::Trusted)]);
        assert_eq!(
            lines("good.example.org"),
            [(3, Listing::Trusted), (7, Listing::Authority)]
        );
        assert_eq!(lines("bad.example.org"), [(7, Listing::Authority)]);
        assert_eq!(lines("[box]:2222"), [(4, Listing::Trusted)]);
        assert_eq!(lines("example.com"), [(5, Listing::Trusted)]);
        assert_eq!(lines("other"), []);
    }

    #[test]
    fn the_verdict_compares_keys_of_the_offered_kind() {
        let (ed, other_ed) = (key(Algorithm::Ed25519), key(Algorithm::Ed25519));
        let ecdsa = key(Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        });
        let at = |line, listing, key: &PublicKey| KnownKey {
            key: key.clone(),
            listing,
            file: PathBuf::from("/kh"),
            line,
        };
        let changed = |line| Verdict::Changed {
            file: PathBuf::from("/kh"),
            line,
        };
        let cases = [
            ("listed", vec![at(1, Listing::Trusted, &ed)], Verdict::Known),
            ("nothing listed", vec![], Verdict::Unknown),
            (
                "another kind listed",
                vec![at(1, Listing::Trusted, &ecdsa)],
                Verdict::Unknown,
            ),
            (
                "another key of its kind",
                vec![
                    at(1, Listing::Trusted, &ecdsa),
                    at(2, Listing::Trusted, &other_ed),
                ],
                changed(2),
            ),
            (
                "rotated: two of its kind, one its own",
                vec![
                    at(1, Listing::Trusted, &other_ed),
                    at(2, Listing::Trusted, &ed),
                ],
                Verdict::Known,
            ),
            (
                "revoked",
                vec![at(1, Listing::Trusted, &ed), at(2, Listing::Revoked, &ed)],
                Verdict::Revoked {
                    file: PathBuf::from("/kh"),
                    line: 2,
                },
            ),
            (
                "another key of its kind, and a certificate authority",
                vec![
                    at(1, Listing::Trusted, &other_ed),
                    at(2, Listing::Authority, &ecdsa),
                ],
                Verdict::Changed {
                    file: PathBuf::from("/kh"),
                    line: 1,
                },
            ),
            (
                "vouched for only by a certificate authority",
                vec![
                    at(1, Listing::Trusted, &ecdsa),
                    at(2, Listing::Authority, &ecdsa),
                ],
                Verdict::Certified {
                    file: PathBuf::from("/kh"),
                    line: 2,
                },
            ),
        ];
        for (case, known, expected) in cases {
            assert_eq!(verdict(&known, &ed), expected, "{case}");
        }
    }

    /// A `RevokedHostKeys` file is read only as a text list of keys; one
    /// that isn't, as a binary KRL, fails, so the check doesn't pass open.
    #[test]
    fn revoked_host_keys_read_a_text_list_and_refuse_anything_else() {
        let ed = key(Algorithm::Ed25519);
        let text = format!(
            "# revoked\n\n{}\n",
            ed.to_openssh().expect("an OpenSSH key")
        );
        let listed = revoked(text.as_bytes()).expect("a text list");
        assert_eq!(listed, [(3, ed)]);
        for (case, bytes) in [
            ("a KRL", b"SSHKRL\n\0\x01\x02".to_vec()),
            (
                "a line that isn't a key",
                b"ssh-ed25519 not-base64\n".to_vec(),
            ),
        ] {
            assert!(revoked(&bytes).is_err(), "{case}");
        }
    }

    /// A host with keys of several kinds is asked first for one the files
    /// list, so it isn't refused as unknown for offering another.
    #[test]
    fn listed_kinds_are_asked_for_first() {
        // Made by `ssh-keygen -t rsa -b 2048`; generating one here is slow.
        const RSA: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCyCwtyH9Ut7y8iL8qfwhw6xllMSAMXnfx81wUr\
            G5/YJdSPjTQwvhS9y+rimZHrzxzOCNdZjMz2gZUyXY2SLsZ0XvVExxsx1ftXss/eRnO4r01x8j5BXsR9oWRH1JKl\
            4fbcyaJawz4nxOn8oVrqHnhVcYUu/u+moBItkSvP/jOtqzQyQKj/633c4uCrhUbLL100zIxk7VLMFgMeiPK/PsVpg\
            HgC72IWUhQOuD5L8AOWRqba8FAi7H/trD0purbiCgIiF5A+96z3urOB9VvBHpuAC8x0Xykv5OvPvSym2rVml+Gkz2\
            Hlm6XIRZt7HFs0AOJkXI95ZdFh53qphwhxCCIR";
        let rsa = KnownKey {
            key: PublicKey::from_openssh(RSA).expect("an RSA key"),
            listing: Listing::Trusted,
            file: PathBuf::from("/kh"),
            line: 1,
        };
        let order = preferred(&[rsa]);
        assert!(
            order
                .iter()
                .take(3)
                .all(|algorithm| matches!(algorithm, Algorithm::Rsa { .. })),
            "{order:?}"
        );
        assert_eq!(order.len(), Preferred::DEFAULT.key.len());
    }
}
