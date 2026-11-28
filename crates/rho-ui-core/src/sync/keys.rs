//! Sign-ins: the source's saved API keys, sealed to the target and saved
//! there (`rho/sync/import_keys`), and its logins, which are never copied:
//! the target signs in itself.

use rho_protocol::{AuthKind, AuthStatus, AuthSummary, ImportOutcome, SecretItem, SyncId};

use super::secrets::{self, Held, Secrets};
use super::{Action, Change, Copy, GroupKind, Offer, Part, Planned, Row, RowKey, Side};
use crate::Daemon;

/// The source's signed-in profiles the target doesn't hold the same of:
/// an API key it lacks or holds otherwise, or a login it isn't signed in
/// to. A key only the target has stays: a sync never removes one.
pub(super) fn rows(source: &Side, target: &Side, secrets: &Secrets) -> Vec<Row> {
    let from = match &source.auths {
        Part::Read(from) => from,
        Part::Failed(why) => {
            return vec![Row::unread(
                GroupKind::SignIns,
                "sign-ins",
                "Sign-ins",
                source,
                why,
            )];
        }
        Part::Lacks => return Vec::new(),
    };
    let to = target.take(&target.auths);
    let to = to.as_ref().map(|to| to.as_slice()).map_err(String::as_str);
    from.iter()
        .filter(|profile| profile.status == AuthStatus::SignedIn)
        .filter_map(|profile| match profile.kind {
            AuthKind::ApiKey => key_row(profile, source, to, secrets),
            AuthKind::Login => login_row(profile, &target.label, to),
            _ => None,
        })
        .collect()
}

/// API key `profile`'s row, unless the target holds the same key.
fn key_row(
    profile: &AuthSummary,
    source: &Side,
    to: Result<&[AuthSummary], &str>,
    secrets: &Secrets,
) -> Option<Row> {
    let auth = &profile.auth;
    let write = |replaced| {
        Copy::Write(Change::Key {
            auth: auth.clone(),
            replaced,
        })
    };
    let held = to
        .and_then(|_| secrets.held(&SecretItem::api_key(auth)))
        .map_err(str::to_owned);
    let (theirs, copy) = match held {
        Ok(Held::Same) => return None,
        Ok(Held::Differs) => (Some("A different key"), write(true)),
        Ok(Held::Absent) => (None, write(false)),
        Ok(Held::Missing) => {
            let why = format!("It comes from an environment variable on {}", source.label);
            (None, Copy::Cannot(why))
        }
        Ok(Held::Unknown) => (None, Copy::Cannot("Couldn't compare it".to_owned())),
        Err(why) => (None, Copy::Cannot(why)),
    };
    let ours = format!("the key from {}", source.label);
    Some(row(profile, &ours, theirs, copy, None))
}

/// Login `profile`'s row, while the target isn't signed in to it.
fn login_row(profile: &AuthSummary, target: &str, to: Result<&[AuthSummary], &str>) -> Option<Row> {
    let to = match to {
        Ok(to) => to,
        Err(why) => {
            let copy = Copy::Cannot(why.to_owned());
            return Some(row(profile, "Signed in", None, copy, None));
        }
    };
    let there = to.iter().find(|there| there.auth == profile.auth);
    match there {
        Some(there) if there.status == AuthStatus::SignedIn => None,
        Some(_) => {
            let why = format!("Logins aren't copied: sign in on {target}");
            let offer = Offer::SignIn {
                auth: profile.auth.clone(),
            };
            let copy = Copy::Cannot(why);
            Some(row(
                profile,
                "Signed in",
                Some("Signed out"),
                copy,
                Some(offer),
            ))
        }
        None => {
            let why = format!("Rho on {target} doesn't have this provider");
            let copy = Copy::Cannot(why);
            Some(row(
                profile,
                "Signed in",
                Some("No such provider"),
                copy,
                None,
            ))
        }
    }
}

fn row(
    profile: &AuthSummary,
    ours: &str,
    theirs: Option<&str>,
    copy: Copy,
    offer: Option<Offer>,
) -> Row {
    Row {
        key: RowKey {
            kind: GroupKind::SignIns,
            id: profile.auth.clone(),
        },
        label: profile.label.clone(),
        source: Some(ours.to_owned()),
        target: theirs.map(str::to_owned),
        action: Action::of(Some(ours), theirs),
        one_machine: false,
        restart: false,
        note: None,
        offer,
        copy,
    }
}

/// Saves the keys `items` copy on `target`, the machine `label`, sealed
/// from `source`, journaled under `sync`: each item's failure, if it
/// failed, in order; or why none was saved.
pub(super) async fn write(
    source: &Daemon,
    target: &Daemon,
    label: &str,
    items: &[Planned],
    sync: &SyncId,
) -> Result<Vec<Option<String>>, String> {
    // One per item, so each outcome is its item's.
    let wanted: Vec<Option<SecretItem>> = items
        .iter()
        .map(|item| match &item.change {
            Change::Key { auth, .. } => Some(SecretItem::api_key(auth)),
            _ => None,
        })
        .collect();
    let sealed = wanted.iter().flatten().cloned().collect();
    let export = secrets::seal(source, target, sealed, label).await?;
    let saved = if export.sealed.items.is_empty() {
        Vec::new()
    } else {
        target
            .sync_import_keys(export.sealed, sync.clone())
            .await
            .map_err(|error| error.to_string())?
    };
    let outcome = |item: &SecretItem| {
        if export.missing.contains(item) {
            return Some("The source no longer has it saved".to_owned());
        }
        match saved
            .iter()
            .find(|saved| saved.item == *item)
            .map(|saved| &saved.outcome)
        {
            Some(ImportOutcome::Saved) => None,
            Some(ImportOutcome::Failed { message, .. }) => Some(message.clone()),
            _ => Some(format!("Rho on {label} didn't say whether it saved it")),
        }
    };
    let not_key = || Some("It isn't a key".to_owned());
    Ok(wanted
        .iter()
        .map(|item| item.as_ref().map_or_else(not_key, outcome))
        .collect())
}
