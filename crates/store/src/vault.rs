use std::sync::Arc;

use aes_gcm::aead::{Aead, Generate, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{Context, Result, anyhow};
use base64::Engine;
use serde::Serialize;

use crate::{Scope, SecretRecord, Store};

pub const ANTHROPIC_API_KEY: &str = "anthropic-api-key";
/// The GitHub App, as JSON: `mergepatrol_github::AppCredentials`.
pub const GITHUB_APP: &str = "github-app";

/// Secrets set up in the dashboard, encrypted at rest with `MERGEPATROL_SECRET_KEY`.
pub struct Vault {
    store: Arc<dyn Store>,
    cipher: Aes256Gcm,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretStatus {
    /// The last four characters, so a user can tell which key is set.
    pub hint: String,
    pub updated_at: String,
}

impl Vault {
    /// `key` is 32 bytes in base64, for example from `openssl rand -base64 32`.
    pub fn new(store: Arc<dyn Store>, key: &str) -> Result<Self> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(key.trim())
            .context("MERGEPATROL_SECRET_KEY is not base64")?;
        let key = Key::<Aes256Gcm>::try_from(bytes.as_slice())
            .map_err(|_| anyhow!("MERGEPATROL_SECRET_KEY must be 32 bytes"))?;
        Ok(Self {
            store,
            cipher: Aes256Gcm::new(&key),
        })
    }

    pub async fn set(&self, scope: Scope, name: &str, value: &str) -> Result<()> {
        let cipher = &self.cipher;
        let nonce = Nonce::generate();
        // Scope and name are the associated data: a ciphertext cannot move to another
        // workspace or another name.
        let aad = aad(scope, name);
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: value.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("encrypt {name}"))?;
        let hint: String = value
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        self.store
            .put_secret(
                scope,
                name,
                SecretRecord {
                    nonce: nonce.to_vec(),
                    ciphertext,
                    hint,
                    updated_at: String::new(),
                },
            )
            .await
    }

    pub async fn get(&self, scope: Scope, name: &str) -> Result<Option<String>> {
        let cipher = &self.cipher;
        let Some(rec) = self.store.secret(scope, name).await? else {
            return Ok(None);
        };
        let nonce =
            Nonce::try_from(rec.nonce.as_slice()).map_err(|_| anyhow!("bad nonce for {name}"))?;
        let plain = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &rec.ciphertext,
                    aad: aad(scope, name).as_bytes(),
                },
            )
            .map_err(|_| anyhow!("cannot decrypt {name}: was MERGEPATROL_SECRET_KEY changed?"))?;
        Ok(Some(String::from_utf8(plain)?))
    }

    pub async fn status(&self, scope: Scope, name: &str) -> Result<Option<SecretStatus>> {
        Ok(self.store.secret(scope, name).await?.map(|r| SecretStatus {
            hint: r.hint,
            updated_at: r.updated_at,
        }))
    }

    pub async fn delete(&self, scope: Scope, name: &str) -> Result<()> {
        self.store.delete_secret(scope, name).await
    }
}

fn aad(scope: Scope, name: &str) -> String {
    format!("{}/{name}", scope.key())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PgStore;
    use sqlx::PgPool;

    const W: Scope = Scope::Installation(7);
    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    #[sqlx::test(migrations = false)]
    async fn round_trip_and_hint(pool: PgPool) {
        let store = Arc::new(PgStore::migrate(pool).await.unwrap());
        let vault = Vault::new(store.clone(), KEY).unwrap();
        vault
            .set(W, ANTHROPIC_API_KEY, "sk-ant-secret-wxyz")
            .await
            .unwrap();
        assert_eq!(
            vault.get(W, ANTHROPIC_API_KEY).await.unwrap().as_deref(),
            Some("sk-ant-secret-wxyz")
        );
        assert_eq!(
            vault
                .status(W, ANTHROPIC_API_KEY)
                .await
                .unwrap()
                .unwrap()
                .hint,
            "wxyz"
        );

        let raw = store.secret(W, ANTHROPIC_API_KEY).await.unwrap().unwrap();
        assert!(!String::from_utf8_lossy(&raw.ciphertext).contains("sk-ant"));

        vault.delete(W, ANTHROPIC_API_KEY).await.unwrap();
        assert!(vault.get(W, ANTHROPIC_API_KEY).await.unwrap().is_none());
    }

    #[sqlx::test(migrations = false)]
    async fn wrong_key_fails_loudly(pool: PgPool) {
        let store = Arc::new(PgStore::migrate(pool).await.unwrap());
        Vault::new(store.clone(), KEY)
            .unwrap()
            .set(W, "x", "v")
            .await
            .unwrap();
        let other = Vault::new(store, "HwECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=").unwrap();
        assert!(other.get(W, "x").await.is_err());
    }

    #[sqlx::test(migrations = false)]
    async fn a_secret_cannot_move_to_another_workspace(pool: PgPool) {
        let store = Arc::new(PgStore::migrate(pool.clone()).await.unwrap());
        let vault = Vault::new(store.clone(), KEY).unwrap();
        vault
            .set(W, ANTHROPIC_API_KEY, "sk-ant-mine")
            .await
            .unwrap();
        assert_eq!(
            vault
                .get(Scope::Installation(8), ANTHROPIC_API_KEY)
                .await
                .unwrap(),
            None
        );

        // Copy the row to another workspace: it must not decrypt there.
        sqlx::query("UPDATE secrets SET scope = 'installation:8'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            vault
                .get(Scope::Installation(8), ANTHROPIC_API_KEY)
                .await
                .is_err()
        );
    }
}
