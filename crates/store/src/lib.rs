//! Persistence. `SqliteStore` is the only backend for now. Postgres comes with the hosted version.

mod vault;

pub use vault::{ANTHROPIC_API_KEY, GITHUB_APP, SecretStatus, Vault};

use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use async_trait::async_trait;
use mergepatrol_core::{Lane, PrNumber, RepoName};
use serde::Serialize;
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

/// One lane: the queue of one trunk in one repo of one installation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LaneKey {
    pub installation_id: u64,
    pub repo: RepoName,
    pub trunk: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EventRecord {
    pub repo: String,
    pub pr: PrNumber,
    pub at: String,
    pub kind: String,
    pub message: String,
}

/// An encrypted secret, as stored. `Vault` encrypts and decrypts.
#[derive(Clone, Debug)]
pub struct SecretRecord {
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub hint: String,
    /// Set by the store on write.
    pub updated_at: String,
}

#[async_trait]
pub trait Store: Send + Sync {
    async fn load_lane(&self, key: &LaneKey) -> Result<Lane>;
    async fn save_lane(&self, key: &LaneKey, lane: &Lane) -> Result<()>;
    async fn lanes(&self) -> Result<Vec<(LaneKey, Lane)>>;
    async fn record(&self, key: &LaneKey, pr: PrNumber, kind: &str, message: &str) -> Result<()>;
    /// The newest events first.
    async fn events(
        &self,
        repo: &RepoName,
        pr: Option<PrNumber>,
        limit: u32,
    ) -> Result<Vec<EventRecord>>;
    async fn put_secret(&self, name: &str, secret: SecretRecord) -> Result<()>;
    async fn secret(&self, name: &str) -> Result<Option<SecretRecord>>;
    async fn delete_secret(&self, name: &str) -> Result<()>;

    /// The repos that have a setting. Repos without one are off.
    async fn repo_settings(&self) -> Result<Vec<RepoSetting>>;
    async fn set_repo_enabled(
        &self,
        repo: &RepoName,
        installation_id: u64,
        enabled: bool,
    ) -> Result<()>;

    /// `token_hash` is the SHA-256 of the cookie value, so a leaked database holds no live sessions.
    async fn create_session(&self, token_hash: &str, session: &Session) -> Result<()>;
    /// Only sessions that have not expired.
    async fn session(&self, token_hash: &str) -> Result<Option<Session>>;
    async fn delete_session(&self, token_hash: &str) -> Result<()>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoSetting {
    pub repo: RepoName,
    pub installation_id: u64,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionKind {
    /// Signed in with GitHub.
    User { login: String, avatar_url: String },
    /// Unlocked with the setup token, before the GitHub App exists.
    Setup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub kind: SessionKind,
    /// Unix seconds.
    pub expires_at: i64,
}

pub struct SqliteStore {
    pool: SqlitePool,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS lanes (
    installation_id INTEGER NOT NULL,
    repo TEXT NOT NULL,
    trunk TEXT NOT NULL,
    lane TEXT NOT NULL,
    PRIMARY KEY (installation_id, repo, trunk)
);
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    installation_id INTEGER NOT NULL,
    repo TEXT NOT NULL,
    pr INTEGER NOT NULL,
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    kind TEXT NOT NULL,
    message TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_by_pr ON events (repo, pr, id);
CREATE TABLE IF NOT EXISTS repos (
    repo TEXT PRIMARY KEY,
    installation_id INTEGER NOT NULL,
    enabled INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    login TEXT,
    avatar_url TEXT,
    expires_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS secrets (
    name TEXT PRIMARY KEY,
    nonce BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    hint TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
";

impl SqliteStore {
    pub async fn open(path: &Path) -> Result<Self> {
        let url = format!("sqlite://{}", path.to_str().context("non-UTF-8 path")?);
        let opts = SqliteConnectOptions::from_str(&url)?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePool::connect_with(opts).await?;
        sqlx::raw_sql(SCHEMA).execute(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn in_memory() -> Result<Self> {
        // One connection: each in-memory connection is its own database.
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::raw_sql(SCHEMA).execute(&pool).await?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl Store for SqliteStore {
    async fn load_lane(&self, key: &LaneKey) -> Result<Lane> {
        let row = sqlx::query(
            "SELECT lane FROM lanes WHERE installation_id = ? AND repo = ? AND trunk = ?",
        )
        .bind(key.installation_id as i64)
        .bind(key.repo.to_string())
        .bind(&key.trunk)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => Ok(serde_json::from_str(row.get::<&str, _>("lane"))?),
            None => Ok(Lane::default()),
        }
    }

    async fn save_lane(&self, key: &LaneKey, lane: &Lane) -> Result<()> {
        sqlx::query(
            "INSERT INTO lanes (installation_id, repo, trunk, lane) VALUES (?, ?, ?, ?)
             ON CONFLICT (installation_id, repo, trunk) DO UPDATE SET lane = excluded.lane",
        )
        .bind(key.installation_id as i64)
        .bind(key.repo.to_string())
        .bind(&key.trunk)
        .bind(serde_json::to_string(lane)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn lanes(&self) -> Result<Vec<(LaneKey, Lane)>> {
        let rows = sqlx::query(
            "SELECT installation_id, repo, trunk, lane FROM lanes ORDER BY repo, trunk",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let repo: &str = row.get("repo");
                let key = LaneKey {
                    installation_id: row.get::<i64, _>("installation_id") as u64,
                    repo: RepoName::parse(repo).context("bad repo in store")?,
                    trunk: row.get("trunk"),
                };
                Ok((key, serde_json::from_str(row.get::<&str, _>("lane"))?))
            })
            .collect()
    }

    async fn record(&self, key: &LaneKey, pr: PrNumber, kind: &str, message: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO events (installation_id, repo, pr, kind, message) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(key.installation_id as i64)
        .bind(key.repo.to_string())
        .bind(pr as i64)
        .bind(kind)
        .bind(message)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn events(
        &self,
        repo: &RepoName,
        pr: Option<PrNumber>,
        limit: u32,
    ) -> Result<Vec<EventRecord>> {
        let rows = sqlx::query(
            "SELECT repo, pr, at, kind, message FROM events
             WHERE repo = ? AND (? IS NULL OR pr = ?) ORDER BY id DESC LIMIT ?",
        )
        .bind(repo.to_string())
        .bind(pr.map(|p| p as i64))
        .bind(pr.map(|p| p as i64))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| EventRecord {
                repo: row.get("repo"),
                pr: row.get::<i64, _>("pr") as u64,
                at: row.get("at"),
                kind: row.get("kind"),
                message: row.get("message"),
            })
            .collect())
    }

    async fn put_secret(&self, name: &str, secret: SecretRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO secrets (name, nonce, ciphertext, hint) VALUES (?, ?, ?, ?)
             ON CONFLICT (name) DO UPDATE SET nonce = excluded.nonce, ciphertext = excluded.ciphertext,
                hint = excluded.hint, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(name)
        .bind(secret.nonce)
        .bind(secret.ciphertext)
        .bind(secret.hint)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn secret(&self, name: &str) -> Result<Option<SecretRecord>> {
        let row =
            sqlx::query("SELECT nonce, ciphertext, hint, updated_at FROM secrets WHERE name = ?")
                .bind(name)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|row| SecretRecord {
            nonce: row.get("nonce"),
            ciphertext: row.get("ciphertext"),
            hint: row.get("hint"),
            updated_at: row.get("updated_at"),
        }))
    }

    async fn delete_secret(&self, name: &str) -> Result<()> {
        sqlx::query("DELETE FROM secrets WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn repo_settings(&self) -> Result<Vec<RepoSetting>> {
        let rows = sqlx::query("SELECT repo, installation_id, enabled FROM repos ORDER BY repo")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(RepoSetting {
                    repo: RepoName::parse(row.get("repo")).context("bad repo in store")?,
                    installation_id: row.get::<i64, _>("installation_id") as u64,
                    enabled: row.get::<i64, _>("enabled") != 0,
                })
            })
            .collect()
    }

    async fn set_repo_enabled(
        &self,
        repo: &RepoName,
        installation_id: u64,
        enabled: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO repos (repo, installation_id, enabled) VALUES (?, ?, ?)
             ON CONFLICT (repo) DO UPDATE SET installation_id = excluded.installation_id, enabled = excluded.enabled",
        )
        .bind(repo.to_string())
        .bind(installation_id as i64)
        .bind(enabled as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn create_session(&self, token_hash: &str, session: &Session) -> Result<()> {
        let (kind, login, avatar) = match &session.kind {
            SessionKind::User { login, avatar_url } => {
                ("user", Some(login.as_str()), Some(avatar_url.as_str()))
            }
            SessionKind::Setup => ("setup", None, None),
        };
        sqlx::query("INSERT INTO sessions (token_hash, kind, login, avatar_url, expires_at) VALUES (?, ?, ?, ?, ?)")
            .bind(token_hash)
            .bind(kind)
            .bind(login)
            .bind(avatar)
            .bind(session.expires_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn session(&self, token_hash: &str) -> Result<Option<Session>> {
        let row = sqlx::query(
            "SELECT kind, login, avatar_url, expires_at FROM sessions
             WHERE token_hash = ? AND expires_at > unixepoch()",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let kind = match row.get::<&str, _>("kind") {
            "user" => SessionKind::User {
                login: row.get("login"),
                avatar_url: row.get("avatar_url"),
            },
            _ => SessionKind::Setup,
        };
        Ok(Some(Session {
            kind,
            expires_at: row.get("expires_at"),
        }))
    }

    async fn delete_session(&self, token_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mergepatrol_core::Candidate;

    #[tokio::test]
    async fn sessions_expire() {
        let store = SqliteStore::in_memory().await.unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let live = Session {
            kind: SessionKind::Setup,
            expires_at: now + 60,
        };
        let dead = Session {
            kind: SessionKind::Setup,
            expires_at: now - 1,
        };
        store.create_session("a", &live).await.unwrap();
        store.create_session("b", &dead).await.unwrap();
        assert_eq!(store.session("a").await.unwrap(), Some(live));
        assert_eq!(store.session("b").await.unwrap(), None);
    }

    #[tokio::test]
    async fn repo_settings_upsert() {
        let store = SqliteStore::in_memory().await.unwrap();
        let repo = RepoName::parse("o/r").unwrap();
        store.set_repo_enabled(&repo, 9, true).await.unwrap();
        store.set_repo_enabled(&repo, 9, false).await.unwrap();
        assert_eq!(
            store.repo_settings().await.unwrap(),
            [RepoSetting {
                repo,
                installation_id: 9,
                enabled: false
            }]
        );
    }

    #[tokio::test]
    async fn lanes_and_events_round_trip() {
        let store = SqliteStore::in_memory().await.unwrap();
        let key = LaneKey {
            installation_id: 1,
            repo: RepoName::parse("o/r").unwrap(),
            trunk: "main".into(),
        };
        let mut lane = Lane::default();
        lane.sync(&[Candidate {
            number: 4,
            priority: false,
            hold: None,
        }]);
        store.save_lane(&key, &lane).await.unwrap();
        assert_eq!(store.load_lane(&key).await.unwrap(), lane);

        store.record(&key, 4, "enqueued", "hi").await.unwrap();
        store.record(&key, 5, "enqueued", "other").await.unwrap();
        let events = store.events(&key.repo, Some(4), 10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message, "hi");
    }
}
