//! Checking a host's key against the user's `known_hosts` files, as OpenSSH
//! does with `StrictHostKeyChecking yes`: rho-ui has no terminal to ask
//! whether to trust a new key in, so a key that isn't there stops the
//! connection, and rho-ui never writes the files.

use std::path::PathBuf;

use base64::Engine as _;
use russh::{
    keys::{Algorithm, PublicKey},
    Preferred,
};

use super::config::Resolved;

/// A key the `known_hosts` files list for the host, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KnownKey {
    key: PublicKey,
    listing: Listing,
    file: PathBuf,
    /// Counted from 1.
    line: usize,
}

/// What a `known_hosts` line says of its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listing {
    /// The host's key: a plain line.
    Trusted,
    /// Never the host's key: a line marked `@revoked`.
    Revoked,
}

/// Whether the host's key is the one the files list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// It is.
    Known,
    /// The files list no key of its kind for the host.
    Unknown,
    /// They list another key of its kind for the host, here.
    Changed { file: PathBuf, line: usize },
    /// They mark it `@revoked`, here.
    Revoked { file: PathBuf, line: usize },
}

impl Verdict {
    /// Why rho-ui won't connect to `host`, filed under `name` in
    /// `known_hosts`, with a key of this verdict; `None` when it will.
    pub(super) fn refusal(&self, host: &str, name: &str) -> Option<String> {
        match self {
            Self::Known => None,
            Self::Unknown => Some(format!(
                "{host}'s host key isn't in your known_hosts: run `ssh {host}` once in a \
                 terminal to check the key and accept it, then retry"
            )),
            Self::Changed { file, line } => Some(format!(
                "{host}'s host key differs from the one in {}:{line}. Someone may be \
                 intercepting the connection; if you know the key changed, remove the old one \
                 with `ssh-keygen -R {name}` and accept the new one with `ssh {host}` in a \
                 terminal",
                file.display()
            )),
            Self::Revoked { file, line } => Some(format!(
                "{host}'s host key is marked revoked in {}:{line}",
                file.display()
            )),
        }
    }
}

/// The name `resolved`'s host is filed under in `known_hosts`: its
/// `HostKeyAlias`, else its hostname, as `[hostname]:port` off port 22.
pub(super) fn lookup_name(resolved: &Resolved) -> String {
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

/// The keys `resolved`'s `known_hosts` files list for its host. A file
/// that can't be read lists none, as for ssh.
pub(super) fn read(resolved: &Resolved) -> Vec<KnownKey> {
    let name = lookup_name(resolved);
    resolved
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
        .collect()
}

/// The keys `text`, a `known_hosts` file, lists for `name`, with their
/// line numbers: lines of `[marker] patterns keytype base64 [comment]`.
/// The patterns are a comma-separated list that may hold `*`, `?` and
/// negations (`!pattern`), or one hashed name (`|1|salt|hash`). Lines
/// marked `@cert-authority` are left out, as rho-ui takes no host
/// certificates, and so are lines whose key doesn't parse.
fn entries(text: &str, name: &str) -> Vec<(usize, Listing, PublicKey)> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let mut fields = line.split_whitespace();
            let first = fields.next()?;
            let (listing, patterns) = match first {
                "@revoked" => (Listing::Revoked, fields.next()?),
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
pub(super) fn verdict(known: &[KnownKey], offered: &PublicKey) -> Verdict {
    let same = |known: &&KnownKey| known.key.key_data() == offered.key_data();
    let trusted = |known: &&KnownKey| known.listing == Listing::Trusted;
    if let Some(revoked) = known
        .iter()
        .find(|known| known.listing == Listing::Revoked && same(known))
    {
        return Verdict::Revoked {
            file: revoked.file.clone(),
            line: revoked.line,
        };
    }
    if known.iter().filter(trusted).any(|known| same(&known)) {
        return Verdict::Known;
    }
    match known
        .iter()
        .filter(trusted)
        .find(|known| known.key.algorithm() == offered.algorithm())
    {
        Some(other) => Verdict::Changed {
            file: other.file.clone(),
            line: other.line,
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
            format!("@cert-authority {}", line("*", &ed)),
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
        assert_eq!(lines("good.example.org"), [(3, Listing::Trusted)]);
        assert_eq!(lines("bad.example.org"), []);
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
        ];
        for (case, known, expected) in cases {
            assert_eq!(verdict(&known, &ed), expected, "{case}");
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
