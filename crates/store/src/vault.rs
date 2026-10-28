use std::sync::Arc;

use aes_gcm::aead::{Aead, Generate, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde::Serialize;

use crate::{SecretRecord, Store};

pub const ANTHROPIC_API_KEY: &str = "anthropic-api-key";

/// Secrets that users enter in the dashboard, encrypted at rest with `MERGEPATROL_SECRET_KEY`.
/// Without that key the vault is off: it reads and stores nothing.
pub struct Vault {
    store: Arc<dyn Store>,
    cipher: Option<Aes256Gcm>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretStatus {
    /// The last four characters, so a user can tell which key is set.
    pub hint: String,
    pub updated_at: String,
}

impl Vault {
    /// `key` is 32 bytes in base64, for example from `openssl rand -base64 32`.
    pub fn new(store: Arc<dyn Store>, key: Option<&str>) -> Result<Self> {
        let cipher = match key {
            None => None,
            Some(b64) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64.trim())
                    .context("MERGEPATROL_SECRET_KEY is not base64")?;
                let key = Key::<Aes256Gcm>::try_from(bytes.as_slice())
                    .map_err(|_| anyhow!("MERGEPATROL_SECRET_KEY must be 32 bytes"))?;
                Some(Aes256Gcm::new(&key))
            }
        };
        Ok(Self { store, cipher })
    }

    pub fn enabled(&self) -> bool {
        self.cipher.is_some()
    }

    pub async fn set(&self, name: &str, value: &str) -> Result<()> {
        let cipher = self.cipher()?;
        let nonce = Nonce::generate();
        // The name is the associated data, so a ciphertext cannot be moved to another name.
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: value.as_bytes(),
                    aad: name.as_bytes(),
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

    pub async fn get(&self, name: &str) -> Result<Option<String>> {
        let Some(cipher) = &self.cipher else {
            return Ok(None);
        };
        let Some(rec) = self.store.secret(name).await? else {
            return Ok(None);
        };
        let nonce =
            Nonce::try_from(rec.nonce.as_slice()).map_err(|_| anyhow!("bad nonce for {name}"))?;
        let plain = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &rec.ciphertext,
                    aad: name.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("cannot decrypt {name}: was MERGEPATROL_SECRET_KEY changed?"))?;
        Ok(Some(String::from_utf8(plain)?))
    }

    pub async fn status(&self, name: &str) -> Result<Option<SecretStatus>> {
        Ok(self.store.secret(name).await?.map(|r| SecretStatus {
            hint: r.hint,
            updated_at: r.updated_at,
        }))
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        self.store.delete_secret(name).await
    }

    fn cipher(&self) -> Result<&Aes256Gcm> {
        match &self.cipher {
            Some(c) => Ok(c),
            None => bail!("MERGEPATROL_SECRET_KEY is not set, so secrets cannot be stored"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteStore;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    #[tokio::test]
    async fn round_trip_and_hint() {
        let store = Arc::new(SqliteStore::in_memory().await.unwrap());
        let vault = Vault::new(store.clone(), Some(KEY)).unwrap();
        vault
            .set(ANTHROPIC_API_KEY, "sk-ant-secret-wxyz")
            .await
            .unwrap();
        assert_eq!(
            vault.get(ANTHROPIC_API_KEY).await.unwrap().as_deref(),
            Some("sk-ant-secret-wxyz")
        );
        assert_eq!(
            vault.status(ANTHROPIC_API_KEY).await.unwrap().unwrap().hint,
            "wxyz"
        );

        let raw = store.secret(ANTHROPIC_API_KEY).await.unwrap().unwrap();
        assert!(!String::from_utf8_lossy(&raw.ciphertext).contains("sk-ant"));

        vault.delete(ANTHROPIC_API_KEY).await.unwrap();
        assert!(vault.get(ANTHROPIC_API_KEY).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn wrong_key_fails_loudly() {
        let store = Arc::new(SqliteStore::in_memory().await.unwrap());
        Vault::new(store.clone(), Some(KEY))
            .unwrap()
            .set("x", "v")
            .await
            .unwrap();
        let other =
            Vault::new(store, Some("HwECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=")).unwrap();
        assert!(other.get("x").await.is_err());
    }

    #[tokio::test]
    async fn without_a_key_the_vault_is_off() {
        let store = Arc::new(SqliteStore::in_memory().await.unwrap());
        let vault = Vault::new(store, None).unwrap();
        assert!(!vault.enabled());
        assert!(vault.set("x", "v").await.is_err());
    }
}
