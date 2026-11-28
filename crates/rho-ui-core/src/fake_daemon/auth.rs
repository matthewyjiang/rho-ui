//! The fake daemon's sign-ins: two auth profiles, [`KEY_AUTH`], which
//! every session runs on, and [`LOGIN_AUTH`], and the credential store.

use std::collections::BTreeMap;

use rho_protocol::{
    login_relay, AuthKeyParams, AuthKeyResponse, AuthKind, AuthLoginCancelParams,
    AuthLoginCancelResponse, AuthLoginParams, AuthLoginResponse, AuthLogoutParams,
    AuthLogoutResponse, AuthStatus, AuthSummary, CredentialStoreKind, CredentialStoreStatus,
    ErrorCode, LoginFlow, LoginId, LoginOutcome, LoginPrompt, LoginRelay, LoginStream, RpcError,
    StoreSelectedBy, UncheckedKey, UncheckedReason,
};

use super::Script;

/// The auth profile every session runs on, which takes an API key. It
/// starts signed in, unless [`Script::signed_out`].
pub const KEY_AUTH: &str = "fake-api-key";

/// An auth profile that takes an interactive login, signed out at first.
pub const LOGIN_AUTH: &str = "fake-login";

/// The key [`KEY_AUTH`]'s provider refuses.
pub const REFUSED_KEY: &str = "refuse";

/// A key [`KEY_AUTH`]'s provider can't be asked about, saved unchecked.
pub const UNCHECKED_KEY: &str = "unchecked";

/// The page a login's prompt opens.
pub const LOGIN_URL: &str = "https://fake.example/login";

/// The code a device-code login's prompt asks for.
pub const LOGIN_CODE: &str = "FAKE-CODE";

/// The key [`KEY_AUTH`] has saved when it starts signed in.
pub const SAVED_KEY: &str = "fake-key-0";

/// The fake's credentials, and the login under way, if any.
#[derive(Debug)]
pub(super) struct FakeAuth {
    /// The profiles signed in, by ID, each with what signs it in.
    credentials: BTreeMap<String, Credential>,
    store: CredentialStoreStatus,
    login: Option<Login>,
    logins: u64,
}

/// What signs a profile in.
#[derive(Debug)]
enum Credential {
    /// An API key saved in the credential store.
    Key(String),
    /// An API key in the daemon's environment, which isn't saved.
    FromEnv,
    /// An interactive login's tokens.
    Login,
}

#[derive(Debug)]
struct Login {
    id: LoginId,
    auth: &'static str,
    relay: LoginRelay,
}

/// The profiles, in the daemon's order: ID, label, kind.
const PROFILES: [(&str, &str, AuthKind); 2] = [
    (KEY_AUTH, "Fake (API key)", AuthKind::ApiKey),
    (LOGIN_AUTH, "Fake Pro (login)", AuthKind::Login),
];

impl FakeAuth {
    pub(super) fn new(script: &Script) -> Self {
        let store = CredentialStoreStatus {
            store: CredentialStoreKind::File,
            selected_by: StoreSelectedBy::Config,
            unreachable: None,
        };
        Self {
            credentials: (!script.signed_out)
                .then(|| (KEY_AUTH.to_owned(), Credential::Key(SAVED_KEY.to_owned())))
                .into_iter()
                .collect(),
            store,
            login: None,
            logins: 0,
        }
    }

    pub(super) fn is_signed_in(&self, auth: &str) -> bool {
        self.credentials.contains_key(auth)
    }

    /// The kind of profile `auth`, if the daemon has it.
    pub(super) fn kind(auth: &str) -> Option<AuthKind> {
        PROFILES
            .iter()
            .find(|&&(id, _, _)| id == auth)
            .map(|&(_, _, kind)| kind)
    }

    /// The API key saved for `auth`: none for a key from the environment
    /// or a login.
    pub(super) fn saved_key(&self, auth: &str) -> Option<&str> {
        match self.credentials.get(auth)? {
            Credential::Key(key) => Some(key.as_str()),
            Credential::FromEnv | Credential::Login => None,
        }
    }

    /// Saves `key` for `auth`, signing it in, or with `None` deletes its
    /// credential, signing it out. Nothing checks the key.
    pub(super) fn set_key(&mut self, auth: &str, key: Option<String>) {
        match key {
            Some(key) => {
                self.credentials
                    .insert(auth.to_owned(), Credential::Key(key));
            }
            None => {
                self.credentials.remove(auth);
            }
        }
    }

    /// `auth`'s key comes from the daemon's environment from now on: it
    /// is signed in, with no key saved.
    pub(super) fn key_from_env(&mut self, auth: &str) {
        self.credentials
            .insert(auth.to_owned(), Credential::FromEnv);
    }

    /// Signs [`LOGIN_AUTH`] in, as a finished login does.
    pub(super) fn sign_in_login(&mut self) {
        self.credentials
            .insert(LOGIN_AUTH.to_owned(), Credential::Login);
    }

    pub(super) fn store(&self) -> CredentialStoreStatus {
        self.store.clone()
    }

    pub(super) fn list(&self) -> Vec<AuthSummary> {
        PROFILES
            .iter()
            .map(|&(auth, label, kind)| AuthSummary {
                auth: auth.to_owned(),
                provider: "fake".to_owned(),
                label: label.to_owned(),
                kind,
                status: if self.is_signed_in(auth) {
                    AuthStatus::SignedIn
                } else {
                    AuthStatus::SignedOut
                },
            })
            .collect()
    }

    /// The profile `auth` names, if it takes `kind`.
    fn profile(auth: &str, kind: AuthKind) -> Result<&'static str, RpcError> {
        PROFILES
            .iter()
            .find(|&&(id, _, of)| id == auth && of == kind)
            .map(|&(id, _, _)| id)
            .ok_or_else(|| {
                RpcError::new(ErrorCode::InvalidParams, format!("no such profile: {auth}"))
            })
    }

    pub(super) fn save_key(&mut self, params: &AuthKeyParams) -> Result<AuthKeyResponse, RpcError> {
        let auth = Self::profile(&params.auth, AuthKind::ApiKey)?;
        if params.key.trim() == REFUSED_KEY {
            return Err(RpcError::new(
                ErrorCode::KeyRefused,
                "the provider refused the key",
            ));
        }
        let unchecked = (params.key.trim() == UNCHECKED_KEY).then(|| UncheckedKey {
            reason: UncheckedReason::Unreachable,
            message: "HTTP 404".to_owned(),
        });
        self.set_key(auth, Some(params.key.trim().to_owned()));
        Ok(AuthKeyResponse {
            unchecked,
            ..AuthKeyResponse::default()
        })
    }

    pub(super) fn logout(&mut self, params: &AuthLogoutParams) -> AuthLogoutResponse {
        self.credentials.remove(params.auth.as_str());
        AuthLogoutResponse {}
    }

    /// Starts a login whose prompt follows at once: for a device code, the
    /// page and [`LOGIN_CODE`]. It waits for [`Self::approve`].
    pub(super) fn login(
        &mut self,
        params: &AuthLoginParams,
    ) -> Result<(AuthLoginResponse, LoginStream), RpcError> {
        let auth = Self::profile(&params.auth, AuthKind::Login)?;
        if self.login.is_some() {
            return Err(RpcError::new(ErrorCode::LoginBusy, "a login is under way"));
        }
        self.logins += 1;
        let id = LoginId::new(format!("login-{}", self.logins));
        let (relay, stream) = login_relay();
        let device = params.flow == Some(LoginFlow::DeviceCode);
        relay.prompt(LoginPrompt {
            url: LOGIN_URL.to_owned(),
            user_code: device.then(|| LOGIN_CODE.to_owned()),
            url_with_code: None,
            instruction: if device {
                format!("Enter the code {LOGIN_CODE} at {LOGIN_URL}")
            } else {
                format!("Sign in at {LOGIN_URL}")
            },
        });
        self.login = Some(Login {
            id: id.clone(),
            auth,
            relay,
        });
        Ok((AuthLoginResponse { login: id }, stream))
    }

    pub(super) fn cancel_login(
        &mut self,
        params: &AuthLoginCancelParams,
    ) -> Result<AuthLoginCancelResponse, RpcError> {
        match self.login.take_if(|login| login.id == params.login) {
            Some(login) => {
                login.relay.finish(LoginOutcome::Cancelled);
                Ok(AuthLoginCancelResponse {})
            }
            None => Err(RpcError::new(ErrorCode::LoginNotFound, "no such login")),
        }
    }

    /// The provider approves the login under way: its profile is signed
    /// in, and the login succeeds.
    pub(super) fn approve(&mut self) {
        let login = self.login.take().expect("a login under way");
        self.credentials
            .insert(login.auth.to_owned(), Credential::Login);
        login.relay.finish(LoginOutcome::Succeeded);
    }

    pub(super) fn choose_store(&mut self, store: CredentialStoreKind) -> CredentialStoreStatus {
        self.store = CredentialStoreStatus {
            store,
            selected_by: StoreSelectedBy::Config,
            unreachable: None,
        };
        self.store()
    }
}
