use std::sync::Arc;

use aes_gcm::aead::{Aead, Generate, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{Context, Result, anyhow};
use base64::Engine;
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use zeroize::Zeroizing;

use crate::{Scope, SecretRecord, Store};

pub const ANTHROPIC_API_KEY: &str = "anthropic-api-key";
/// The GitHub App, as JSON: `mergepatrol_github::AppCredentials`.
pub const GITHUB_APP: &str = "github-app";

/// Secrets set up in the dashboard, encrypted at rest with `MERGEPATROL_SECRET_KEY`.
pub struct Vault {
    store: Arc<dyn Store>,
    /// The current key first, which encrypts. The others only decrypt, while a rotation runs.
    ciphers: Vec<Aes256Gcm>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretStatus {
    /// The last four characters, so a user can tell which key is set.
    pub hint: String,
    pub updated_at: String,
}

impl Vault {
    /// `keys` are 32 bytes each in base64, for example from `openssl rand -base64 32`, separated
    /// by commas: the current key, then the keys it replaces.
    pub fn new(store: Arc<dyn Store>, keys: &str) -> Result<Self> {
        let ciphers = keys
            .split(',')
            .map(|key| {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(key.trim())
                    .context("MERGEPATROL_SECRET_KEY is not base64")?;
                let key = Key::<Aes256Gcm>::try_from(bytes.as_slice())
                    .map_err(|_| anyhow!("MERGEPATROL_SECRET_KEY must be 32 bytes"))?;
                Ok(Aes256Gcm::new(&key))
            })
            .collect::<Result<_>>()?;
        Ok(Self { store, ciphers })
    }

    pub async fn set(&self, scope: Scope, name: &str, value: &str) -> Result<()> {
        let (nonce, ciphertext) = self.encrypt(scope, name, value)?;
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
                    nonce,
                    ciphertext,
                    hint,
                    updated_at: String::new(),
                },
            )
            .await
    }

    pub async fn get(&self, scope: Scope, name: &str) -> Result<Option<SecretString>> {
        let Some(rec) = self.store.secret(scope, name).await? else {
            return Ok(None);
        };
        Ok(Some(self.decrypt(scope, name, &rec)?.0))
    }

    /// Encrypts every secret with the current key, so the keys after it can be dropped.
    /// Returns how many it re-encrypted.
    pub async fn rotate(&self) -> Result<usize> {
        let mut rotated = 0;
        if self.ciphers.len() == 1 {
            return Ok(rotated);
        }
        for (scope, name) in self.store.secret_names().await? {
            let Some(rec) = self.store.secret(scope, &name).await? else {
                continue;
            };
            let (value, current) = self.decrypt(scope, &name, &rec)?;
            if current {
                continue;
            }
            let (nonce, ciphertext) = self.encrypt(scope, &name, value.expose_secret())?;
            self.store
                .replace_ciphertext(scope, &name, &rec.ciphertext, nonce, ciphertext)
                .await?;
            rotated += 1;
        }
        Ok(rotated)
    }

    fn encrypt(&self, scope: Scope, name: &str, value: &str) -> Result<(Vec<u8>, Vec<u8>)> {
        let nonce = Nonce::generate();
        // Scope and name are the associated data: a ciphertext cannot move to another
        // workspace or another name.
        let aad = aad(scope, name);
        let ciphertext = self.ciphers[0]
            .encrypt(
                &nonce,
                Payload {
                    msg: value.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("encrypt {name}"))?;
        Ok((nonce.to_vec(), ciphertext))
    }

    /// The value, and whether the current key encrypted it.
    fn decrypt(
        &self,
        scope: Scope,
        name: &str,
        rec: &SecretRecord,
    ) -> Result<(SecretString, bool)> {
        let nonce =
            Nonce::try_from(rec.nonce.as_slice()).map_err(|_| anyhow!("bad nonce for {name}"))?;
        let aad = aad(scope, name);
        let (i, plain) = self
            .ciphers
            .iter()
            .enumerate()
            .find_map(|(i, cipher)| {
                let payload = Payload {
                    msg: &rec.ciphertext,
                    aad: aad.as_bytes(),
                };
                cipher
                    .decrypt(&nonce, payload)
                    .ok()
                    .map(|p| (i, Zeroizing::new(p)))
            })
            .ok_or_else(|| anyhow!("cannot decrypt {name}: was MERGEPATROL_SECRET_KEY changed?"))?;
        Ok((std::str::from_utf8(&plain)?.into(), i == 0))
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
            vault
                .get(W, ANTHROPIC_API_KEY)
                .await
                .unwrap()
                .as_ref()
                .map(ExposeSecret::expose_secret),
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
    async fn rotate_moves_every_secret_to_the_new_key(pool: PgPool) {
        const NEW: &str = "HwECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
        let store = Arc::new(PgStore::migrate(pool).await.unwrap());
        let old = Vault::new(store.clone(), KEY).unwrap();
        old.set(W, ANTHROPIC_API_KEY, "sk-ant-wxyz").await.unwrap();
        old.set(Scope::Instance, GITHUB_APP, "{}").await.unwrap();
        let before = old.status(W, ANTHROPIC_API_KEY).await.unwrap().unwrap();

        // While both are set, the old key still decrypts.
        let both = Vault::new(store.clone(), &format!("{NEW}, {KEY}")).unwrap();
        assert_eq!(
            both.get(W, ANTHROPIC_API_KEY)
                .await
                .unwrap()
                .as_ref()
                .map(ExposeSecret::expose_secret),
            Some("sk-ant-wxyz")
        );
        assert_eq!(both.rotate().await.unwrap(), 2);
        assert_eq!(both.rotate().await.unwrap(), 0);

        let new = Vault::new(store, NEW).unwrap();
        assert_eq!(
            new.get(W, ANTHROPIC_API_KEY)
                .await
                .unwrap()
                .as_ref()
                .map(ExposeSecret::expose_secret),
            Some("sk-ant-wxyz")
        );
        assert_eq!(
            new.get(Scope::Instance, GITHUB_APP)
                .await
                .unwrap()
                .as_ref()
                .map(ExposeSecret::expose_secret),
            Some("{}")
        );
        assert!(old.get(W, ANTHROPIC_API_KEY).await.is_err());
        // A rotation is not a change of the secret.
        let after = new.status(W, ANTHROPIC_API_KEY).await.unwrap().unwrap();
        assert_eq!(
            (after.hint, after.updated_at),
            (before.hint, before.updated_at)
        );
    }

    #[sqlx::test(migrations = false)]
    async fn a_secret_cannot_move_to_another_workspace(pool: PgPool) {
        let store = Arc::new(PgStore::migrate(pool.clone()).await.unwrap());
        let vault = Vault::new(store.clone(), KEY).unwrap();
        vault
            .set(W, ANTHROPIC_API_KEY, "sk-ant-mine")
            .await
            .unwrap();
        assert!(
            vault
                .get(Scope::Installation(8), ANTHROPIC_API_KEY)
                .await
                .unwrap()
                .is_none()
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
