//! What a job's agent calls its model with: for OpenAI's models the workspace's ChatGPT login
//! when it has one, else the workspace's API key for the provider.

use std::time::Duration;

use anyhow::{Context, Result};
use mergepatrol_core::Provider;
use mergepatrol_executor::proxy::Credential;
use mergepatrol_resolver::{ChatGptAuth, ChatGptLogin};
use mergepatrol_store::{CHATGPT_LOGIN, Scope, Vault, api_key_secret};
use secrecy::ExposeSecret;

/// What an access token must still have left when a job gets it: the job cannot renew it.
const FRESH_FOR: Duration = Duration::from_secs(2 * 60 * 60);

/// Renews the workspaces' ChatGPT logins.
#[derive(Default)]
pub struct ChatGpt {
    auth: ChatGptAuth,
    /// A refresh token works once, so one renewal at a time. One orchestrator runs.
    renewing: tokio::sync::Mutex<()>,
}

impl ChatGpt {
    #[cfg(test)]
    pub fn new(auth: ChatGptAuth) -> Self {
        Self {
            auth,
            renewing: Default::default(),
        }
    }

    /// The credential for `provider` in `scope`. `None` when the workspace has none.
    pub async fn credential(
        &self,
        vault: &Vault,
        scope: Scope,
        provider: Provider,
    ) -> Result<Option<Credential>> {
        if provider == Provider::OpenAi {
            match self.login(vault, scope).await {
                Ok(Some(login)) => {
                    return Ok(Some(Credential::ChatGpt {
                        access_token: login.access_token.into(),
                        account_id: login.account_id,
                    }));
                }
                Ok(None) => {}
                // A rebase without conflicts needs no agent: the job runs, on the key if any.
                Err(e) => tracing::warn!(?scope, error = format!("{e:#}"), "no ChatGPT login"),
            }
        }
        Ok(vault
            .get(scope, api_key_secret(provider))
            .await?
            .map(Credential::ApiKey))
    }

    /// The workspace's login, renewed if it expires within `FRESH_FOR`.
    async fn login(&self, vault: &Vault, scope: Scope) -> Result<Option<ChatGptLogin>> {
        match load(vault, scope).await? {
            Some(login) if login.expires_within(FRESH_FOR) => {}
            login => return Ok(login),
        }
        let _renewing = self.renewing.lock().await;
        // Another job may have renewed it while this one waited.
        let Some(login) = load(vault, scope).await? else {
            return Ok(None);
        };
        if !login.expires_within(FRESH_FOR) {
            return Ok(Some(login));
        }
        let renewed = self.auth.refresh(&login).await?;
        store(vault, scope, &renewed).await?;
        tracing::info!(?scope, "a ChatGPT login was renewed");
        Ok(Some(renewed))
    }
}

async fn load(vault: &Vault, scope: Scope) -> Result<Option<ChatGptLogin>> {
    vault
        .get(scope, CHATGPT_LOGIN)
        .await?
        .map(|json| serde_json::from_str(json.expose_secret()).context("the ChatGPT login"))
        .transpose()
}

async fn store(vault: &Vault, scope: Scope, login: &ChatGptLogin) -> Result<()> {
    vault
        .set_with_hint(
            scope,
            CHATGPT_LOGIN,
            &serde_json::to_string(login)?,
            login.hint(),
        )
        .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mergepatrol_resolver::testing::issuer;
    use mergepatrol_store::PgStore;
    use sqlx::PgPool;

    use super::*;

    const W: Scope = Scope::Installation(7);

    fn key(c: Option<Credential>) -> Option<String> {
        match c? {
            Credential::ApiKey(k) => Some(k.expose_secret().to_owned()),
            Credential::ChatGpt { .. } => None,
        }
    }

    /// The access token, if the credential is the ChatGPT login.
    fn access(c: Option<Credential>) -> Option<String> {
        match c? {
            Credential::ChatGpt {
                access_token,
                account_id,
            } => {
                assert_eq!(account_id, "acct");
                Some(access_token.expose_secret().to_owned())
            }
            Credential::ApiKey(_) => None,
        }
    }

    #[sqlx::test(migrations = false)]
    async fn a_chatgpt_login_beats_the_openai_key_and_is_renewed(pool: PgPool) {
        let pg = Arc::new(PgStore::migrate(pool).await.unwrap());
        let vault = Vault::new(pg, &format!("{}=", "A".repeat(43))).unwrap();
        let issuer = issuer().await;
        let chatgpt = ChatGpt::new(ChatGptAuth::new(&issuer.url));
        let credential = |p| chatgpt.credential(&vault, W, p);

        assert!(credential(Provider::OpenAi).await.unwrap().is_none());
        vault
            .set(W, api_key_secret(Provider::OpenAi), "sk-openai")
            .await
            .unwrap();
        vault
            .set(W, api_key_secret(Provider::Anthropic), "sk-ant")
            .await
            .unwrap();
        assert_eq!(
            key(credential(Provider::OpenAi).await.unwrap()).as_deref(),
            Some("sk-openai")
        );

        // Signed in, with a token that expires in a minute: too soon for a job.
        let auth = ChatGptAuth::new(&issuer.url);
        let device = auth.start().await.unwrap();
        auth.poll(&device).await.unwrap();
        let login = auth.poll(&device).await.unwrap().unwrap();
        store(&vault, W, &login).await.unwrap();
        assert_eq!(
            vault.status(W, CHATGPT_LOGIN).await.unwrap().unwrap().hint,
            "me@example.com"
        );

        let renewed = access(credential(Provider::OpenAi).await.unwrap()).unwrap();
        assert_ne!(renewed, login.access_token);
        // Stored, so the next job neither renews it again nor spends the used refresh token.
        assert_eq!(
            access(credential(Provider::OpenAi).await.unwrap()).unwrap(),
            renewed
        );
        let refreshes = |grants: &Vec<mergepatrol_resolver::testing::Grant>| {
            grants
                .iter()
                .filter(|g| g.contains(&("grant_type".into(), "refresh_token".into())))
                .count()
        };
        assert_eq!(refreshes(&issuer.grants.lock().unwrap()), 1);

        // Only OpenAI's models run on the plan.
        assert_eq!(
            key(credential(Provider::Anthropic).await.unwrap()).as_deref(),
            Some("sk-ant")
        );
    }
}
