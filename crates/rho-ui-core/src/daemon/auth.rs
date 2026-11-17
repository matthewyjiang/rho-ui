//! Signing the daemon in to providers, and out: the auth profiles its
//! sessions can run on, an API key saved or an interactive login run
//! through it, and the credential store it keeps them in.
//!
//! Every request carries the environment the daemon's sessions get from
//! this client, so a profile counts as signed in exactly when those
//! sessions can use it, and a credential an environment variable holds is
//! one the daemon won't overwrite or delete (it says which variable).

use rho_protocol::{
    methods, AuthChooseStoreParams, AuthKeyParams, AuthListParams, AuthLoginCancelParams,
    AuthLoginParams, AuthLogoutParams, AuthSummary, ClientEnvironment, CredentialStoreKind,
    CredentialStoreStatus, LoginFlow, LoginId, StatusParams, UncheckedKey,
};

use super::{Daemon, DaemonError};

impl Daemon {
    /// Every auth profile that takes a credential, signed in or not, with
    /// the custom providers of the daemon's config. Nothing pushes changes
    /// to it; ask again after a sign-in elsewhere.
    ///
    /// # Errors
    ///
    /// Fails if the daemon rejects the request, as a rho too old to sign
    /// in through, or the connection is gone.
    pub async fn auths(&self) -> Result<Vec<AuthSummary>, DaemonError> {
        let params = AuthListParams {
            environment: ClientEnvironment::clone(&self.environment),
        };
        let list = self.client.request::<methods::AuthList>(&params).await?;
        Ok(list.auths)
    }

    /// Checks `key` with the provider of auth profile `auth` and saves it.
    /// Running sessions on the profile use it from their next prompt.
    /// Returns why the provider didn't check it, when it couldn't: the key
    /// is saved anyway.
    ///
    /// # Errors
    ///
    /// Fails if the provider refuses the key, an environment variable
    /// holds the profile's credential, the credential store can't be
    /// written, or the connection is gone. Nothing was saved then.
    pub async fn save_key(
        &self,
        auth: &str,
        key: &str,
    ) -> Result<Option<UncheckedKey>, DaemonError> {
        let params = AuthKeyParams {
            auth: auth.to_owned(),
            key: key.to_owned(),
            environment: ClientEnvironment::clone(&self.environment),
        };
        let saved = self.client.request::<methods::AuthKey>(&params).await?;
        Ok(saved.unchecked)
    }

    /// Deletes auth profile `auth`'s saved credentials. Running sessions on
    /// it fail from their next prompt until it is signed in again.
    ///
    /// # Errors
    ///
    /// Fails if an environment variable holds the profile's credential, the
    /// credential store can't be written, or the connection is gone.
    pub async fn logout(&self, auth: &str) -> Result<(), DaemonError> {
        let params = AuthLogoutParams {
            auth: auth.to_owned(),
            environment: ClientEnvironment::clone(&self.environment),
        };
        self.client.request::<methods::AuthLogout>(&params).await?;
        Ok(())
    }

    /// Starts auth profile `auth`'s interactive login, which the daemon
    /// runs. What the user must do follows as [`super::DaemonEvent::LoginPrompt`],
    /// then how it ended as [`super::DaemonEvent::LoginFinished`]. The login
    /// belongs to this connection: the daemon cancels it when the
    /// connection closes.
    ///
    /// A daemon on this machine signs in in the browser, whose page
    /// redirects to a port the daemon listens on here. One on a remote
    /// machine, or behind `rho-ui serve`, signs in with a device code
    /// instead, which any browser can enter, as that redirect couldn't
    /// reach it; a provider with only the browser flow uses it anyway.
    ///
    /// # Errors
    ///
    /// Fails if the profile takes no interactive login, one to it is under
    /// way already, an environment variable holds its credential, or the
    /// connection is gone.
    pub async fn login(&self, auth: &str) -> Result<LoginId, DaemonError> {
        let flow = match self.link {
            None => LoginFlow::Browser,
            Some(_) => LoginFlow::DeviceCode,
        };
        let params = AuthLoginParams {
            auth: auth.to_owned(),
            flow: Some(flow),
            environment: ClientEnvironment::clone(&self.environment),
        };
        let started = self.client.request::<methods::AuthLogin>(&params).await?;
        Ok(started.login)
    }

    /// Stops login `login`. It then finishes as cancelled, or as succeeded
    /// when the provider had already approved it.
    ///
    /// # Errors
    ///
    /// Fails if the login already ended, or the connection is gone.
    pub async fn cancel_login(&self, login: &LoginId) -> Result<(), DaemonError> {
        let params = AuthLoginCancelParams {
            login: login.clone(),
        };
        self.client
            .request::<methods::AuthLoginCancel>(&params)
            .await?;
        Ok(())
    }

    /// The credential store every sign-in through the daemon writes, what
    /// chose it, and whether the daemon can reach it.
    ///
    /// # Errors
    ///
    /// Fails if the connection is gone.
    pub async fn credential_store(&self) -> Result<CredentialStoreStatus, DaemonError> {
        let status = self
            .client
            .request::<methods::Status>(&StatusParams {})
            .await?;
        Ok(status.credential_store)
    }

    /// Keeps credentials in `store` from now on, saved in the daemon's
    /// config. Credentials already saved stay where they are.
    ///
    /// # Errors
    ///
    /// Fails if the store refuses a test secret, `RHO_CREDENTIAL_STORE`
    /// chooses the store instead, or the connection is gone.
    pub async fn choose_store(
        &self,
        store: CredentialStoreKind,
    ) -> Result<CredentialStoreStatus, DaemonError> {
        let params = AuthChooseStoreParams {
            store,
            environment: ClientEnvironment::clone(&self.environment),
        };
        let chosen = self
            .client
            .request::<methods::AuthChooseStore>(&params)
            .await?;
        Ok(chosen.credential_store)
    }
}
