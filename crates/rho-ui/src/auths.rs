//! Each connected machine's sign-ins: the auth profiles its daemon can run
//! sessions on, whether each is signed in, and the credential store the
//! daemon keeps credentials in. Settings → Providers lists them, and a
//! session whose model is signed out offers its sign-in above the composer
//! ([`crate::sign_in`]); both act through here, so a sign-in made in one
//! shows in the other.
//!
//! Nothing pushes changes to the list. It is read when the machine
//! connects, after every change made here, when Providers opens, and when a
//! session's turn fails, as one does after a sign-out elsewhere.
//!
//! A machine runs one login at a time from here. The daemon opens no
//! browser: rho-ui opens the login's page when its prompt arrives, and
//! shows the page and any code until the login ends.

use std::collections::BTreeMap;

use gpui_kit::{Context, SharedString};
use rho_ui_core::{
    AuthSummary, CredentialStoreKind, CredentialStoreStatus, Daemon, DaemonError, LoginId,
    LoginOutcome, LoginPrompt, UncheckedKey,
};

use crate::machines::MachineId;

/// What is known of one machine's auth profiles.
pub(crate) enum Profiles {
    /// Being read.
    Reading,
    Ready(Vec<AuthSummary>),
    /// Couldn't be read, in the daemon's words, as from a rho too old to
    /// sign in through.
    Failed(SharedString),
}

/// A login under way on a machine.
pub(crate) struct Login {
    /// The auth profile it signs in to.
    pub(crate) auth: String,
    /// The daemon's ID for it, once it answered. Its prompt may arrive
    /// first: the login has only one.
    id: Option<LoginId>,
    /// What the user must do, once the daemon says.
    pub(crate) prompt: Option<LoginPrompt>,
}

/// What came of the user's last request for a profile, shown beside it
/// until their next one.
#[derive(Clone)]
pub(crate) enum Note {
    /// It didn't land, and why.
    Failed(SharedString),
    /// A key saved that the provider couldn't check, and why.
    Unchecked(SharedString),
}

/// One connected machine's sign-ins.
struct MachineAuths {
    id: MachineId,
    daemon: Daemon,
    profiles: Profiles,
    store: Option<CredentialStoreStatus>,
    login: Option<Login>,
    /// Profiles with a key save or a sign-out on its way to the daemon.
    busy: Vec<String>,
    notes: BTreeMap<String, Note>,
    /// Why the last change of credential store didn't land.
    store_problem: Option<SharedString>,
}

/// Every connected machine's sign-ins. A machine that isn't connected has
/// none: [`crate::machines::Machines`] says why.
#[derive(Default)]
pub(crate) struct Auths {
    machines: Vec<MachineAuths>,
}

impl Auths {
    fn entry(&self, machine: &MachineId) -> Option<&MachineAuths> {
        self.machines.iter().find(|entry| entry.id == *machine)
    }

    fn entry_mut(&mut self, machine: &MachineId) -> Option<&mut MachineAuths> {
        self.machines.iter_mut().find(|entry| entry.id == *machine)
    }

    /// `machine`'s auth profiles, while it is connected.
    pub(crate) fn profiles(&self, machine: &MachineId) -> Option<&Profiles> {
        self.entry(machine).map(|entry| &entry.profiles)
    }

    /// Auth profile `auth` on `machine`, once listed.
    pub(crate) fn profile(&self, machine: &MachineId, auth: &str) -> Option<&AuthSummary> {
        match self.profiles(machine)? {
            Profiles::Ready(profiles) => profiles.iter().find(|profile| profile.auth == auth),
            Profiles::Reading | Profiles::Failed(_) => None,
        }
    }

    /// The credential store `machine`'s daemon writes, once read.
    pub(crate) fn store(&self, machine: &MachineId) -> Option<&CredentialStoreStatus> {
        self.entry(machine)?.store.as_ref()
    }

    /// Why the last change of `machine`'s credential store didn't land.
    pub(crate) fn store_problem(&self, machine: &MachineId) -> Option<&SharedString> {
        self.entry(machine)?.store_problem.as_ref()
    }

    /// The login under way on `machine`, if any.
    pub(crate) fn login(&self, machine: &MachineId) -> Option<&Login> {
        self.entry(machine)?.login.as_ref()
    }

    /// Whether a request for `auth` on `machine` is on its way: a key, a
    /// sign-out, or its login.
    pub(crate) fn is_busy(&self, machine: &MachineId, auth: &str) -> bool {
        self.entry(machine).is_some_and(|entry| {
            entry.busy.iter().any(|busy| busy == auth)
                || entry.login.as_ref().is_some_and(|login| login.auth == auth)
        })
    }

    /// What came of the user's last request for `auth` on `machine`.
    pub(crate) fn note(&self, machine: &MachineId, auth: &str) -> Option<&Note> {
        self.entry(machine)?.notes.get(auth)
    }

    /// `machine` connected to `daemon`: reads its sign-ins, which replace
    /// any from an earlier connection.
    pub(crate) fn connected(
        &mut self,
        machine: &MachineId,
        daemon: Daemon,
        cx: &mut Context<'_, Self>,
    ) {
        self.forget(machine, cx);
        self.machines.push(MachineAuths {
            id: machine.clone(),
            daemon,
            profiles: Profiles::Reading,
            store: None,
            login: None,
            busy: Vec::new(),
            notes: BTreeMap::new(),
            store_problem: None,
        });
        self.refresh(machine, cx);
    }

    /// `machine`'s connection is gone, or the machine is. Its daemon
    /// cancels a login the connection started.
    pub(crate) fn forget(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let before = self.machines.len();
        self.machines.retain(|entry| entry.id != *machine);
        if self.machines.len() != before {
            cx.notify();
        }
    }

    /// Reads `machine`'s profiles and credential store afresh. What is
    /// shown stays until they arrive.
    pub(crate) fn refresh(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.entry(machine) else {
            return;
        };
        let daemon = entry.daemon.clone();
        let id = machine.clone();
        cx.spawn(async move |this, cx| {
            let profiles = daemon.auths().await;
            let store = daemon.credential_store().await;
            let _ = this.update(cx, |this, cx| {
                let Some(entry) = this.entry_mut(&id) else {
                    return;
                };
                entry.profiles = match profiles {
                    Ok(profiles) => Profiles::Ready(profiles),
                    Err(error) => Profiles::Failed(error.to_string().into()),
                };
                if let Ok(store) = store {
                    entry.store = Some(store);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Saves `key` for API-key profile `auth` on `machine`.
    pub(crate) fn save_key(
        &mut self,
        machine: &MachineId,
        auth: &str,
        key: &str,
        cx: &mut Context<'_, Self>,
    ) {
        let key = key.trim().to_owned();
        if key.is_empty() {
            return;
        }
        self.request(machine, auth, cx, async move |daemon, auth| {
            daemon.save_key(&auth, &key).await
        });
    }

    /// Signs `machine` out of `auth`, deleting its saved credentials.
    pub(crate) fn logout(&mut self, machine: &MachineId, auth: &str, cx: &mut Context<'_, Self>) {
        self.request(machine, auth, cx, async move |daemon, auth| {
            daemon.logout(&auth).await.map(|()| None)
        });
    }

    /// Sends `send` for `auth` on `machine`, unless one for it is on its
    /// way, then reads the profiles again. It answers with why a saved key
    /// went unchecked, if it did.
    fn request(
        &mut self,
        machine: &MachineId,
        auth: &str,
        cx: &mut Context<'_, Self>,
        send: impl AsyncFnOnce(Daemon, String) -> Result<Option<UncheckedKey>, DaemonError> + 'static,
    ) {
        if self.is_busy(machine, auth) {
            return;
        }
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        entry.busy.push(auth.to_owned());
        entry.notes.remove(auth);
        let daemon = entry.daemon.clone();
        let (id, auth) = (machine.clone(), auth.to_owned());
        cx.spawn(async move |this, cx| {
            let answer = send(daemon, auth.clone()).await;
            let _ = this.update(cx, |this, cx| {
                let Some(entry) = this.entry_mut(&id) else {
                    return;
                };
                entry.busy.retain(|busy| *busy != auth);
                let note = match answer {
                    Ok(None) => None,
                    Ok(Some(unchecked)) => Some(Note::Unchecked(
                        format!(
                            "Saved, though the provider couldn't check it: {}",
                            unchecked.message
                        )
                        .into(),
                    )),
                    Err(error) => Some(Note::Failed(error.to_string().into())),
                };
                if let Some(note) = note {
                    entry.notes.insert(auth, note);
                }
                this.refresh(&id, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Starts `auth`'s login on `machine`, unless one is under way there.
    pub(crate) fn login_to(&mut self, machine: &MachineId, auth: &str, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        if entry.login.is_some() {
            return;
        }
        entry.notes.remove(auth);
        entry.login = Some(Login {
            auth: auth.to_owned(),
            id: None,
            prompt: None,
        });
        let daemon = entry.daemon.clone();
        let (id, auth) = (machine.clone(), auth.to_owned());
        cx.spawn(async move |this, cx| {
            let started = daemon.login(&auth).await;
            let _ = this.update(cx, |this, cx| {
                let Some(entry) = this.entry_mut(&id) else {
                    return;
                };
                match started {
                    Ok(login) => {
                        // Its end may have arrived first, and closed it.
                        if let Some(under_way) = entry.login.as_mut() {
                            under_way.id = Some(login);
                        }
                    }
                    Err(error) => {
                        entry.login = None;
                        entry
                            .notes
                            .insert(auth, Note::Failed(error.to_string().into()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Stops the login under way on `machine`.
    pub(crate) fn cancel_login(&mut self, machine: &MachineId, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        let Some(login) = entry.login.as_ref() else {
            return;
        };
        let Some(id) = login.id.clone() else {
            // It hasn't started; its end follows its start, and closes it.
            return;
        };
        let daemon = entry.daemon.clone();
        cx.spawn(async move |_, _| {
            // A login that already ended needs no stopping; its end closes it.
            let _ = daemon.cancel_login(&id).await;
        })
        .detach();
    }

    /// Whether `login` is the one under way on `entry`, as far as is known.
    fn is_current(entry: &MachineAuths, login: &LoginId) -> bool {
        entry
            .login
            .as_ref()
            .is_some_and(|under_way| under_way.id.as_ref().is_none_or(|id| id == login))
    }

    /// The prompt of `machine`'s login `login` arrived: shows it, and
    /// opens its page.
    pub(crate) fn login_prompt(
        &mut self,
        machine: &MachineId,
        login: LoginId,
        prompt: LoginPrompt,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        if !Self::is_current(entry, &login) {
            return;
        }
        let page = prompt.url_with_code.as_ref().unwrap_or(&prompt.url).clone();
        if let Some(under_way) = entry.login.as_mut() {
            under_way.id = Some(login);
            under_way.prompt = Some(prompt);
        }
        cx.open_url(&page);
        cx.notify();
    }

    /// `machine`'s login `login` ended.
    pub(crate) fn login_finished(
        &mut self,
        machine: &MachineId,
        login: &LoginId,
        outcome: LoginOutcome,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        if !Self::is_current(entry, login) {
            return;
        }
        let Some(ended) = entry.login.take() else {
            return;
        };
        if let LoginOutcome::Failed { message } = outcome {
            entry.notes.insert(ended.auth, Note::Failed(message.into()));
        }
        self.refresh(machine, cx);
        cx.notify();
    }

    /// Keeps `machine`'s credentials in `store` from now on.
    pub(crate) fn choose_store(
        &mut self,
        machine: &MachineId,
        store: CredentialStoreKind,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(entry) = self.entry_mut(machine) else {
            return;
        };
        entry.store_problem = None;
        let daemon = entry.daemon.clone();
        let id = machine.clone();
        cx.spawn(async move |this, cx| {
            let chosen = daemon.choose_store(store).await;
            let _ = this.update(cx, |this, cx| {
                let Some(entry) = this.entry_mut(&id) else {
                    return;
                };
                match chosen {
                    Ok(status) => entry.store = Some(status),
                    Err(error) => entry.store_problem = Some(error.to_string().into()),
                }
                // A store that couldn't be read may hold credentials.
                this.refresh(&id, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
