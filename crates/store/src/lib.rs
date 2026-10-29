//! Persistence, in Postgres. Migrations are in `migrations/`, embedded, and run at startup.

mod vault;

pub use vault::{ANTHROPIC_API_KEY, GITHUB_APP, SecretStatus, Vault};

use anyhow::{Context, Result};
use async_trait::async_trait;
use mergepatrol_core::{Lane, PrNumber, RepoName};
use serde::Serialize;
use sqlx::PgPool;
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;

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
    /// The lanes of one workspace.
    async fn lanes(&self, installation_id: u64) -> Result<Vec<(LaneKey, Lane)>>;
    async fn record(&self, key: &LaneKey, pr: PrNumber, kind: &str, message: &str) -> Result<()>;
    /// The newest events first.
    async fn events(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: Option<PrNumber>,
        limit: u32,
    ) -> Result<Vec<EventRecord>>;
    async fn put_secret(&self, scope: Scope, name: &str, secret: SecretRecord) -> Result<()>;
    async fn secret(&self, scope: Scope, name: &str) -> Result<Option<SecretRecord>>;
    async fn delete_secret(&self, scope: Scope, name: &str) -> Result<()>;

    /// The repos that have a setting, in every workspace. Repos without one are off.
    async fn repo_settings(&self) -> Result<Vec<RepoSetting>>;
    async fn set_repo_enabled(
        &self,
        installation_id: u64,
        repo: &RepoName,
        enabled: bool,
    ) -> Result<()>;

    /// Remembers the account of each installation, as GitHub showed it at a sign-in.
    async fn save_workspaces(&self, workspaces: &[Workspace]) -> Result<()>;
    async fn workspaces(&self, installation_ids: &[u64]) -> Result<Vec<Workspace>>;

    /// `token_hash` is the SHA-256 of the cookie value, so a leaked database holds no live sessions.
    async fn create_session(&self, token_hash: &str, session: &Session) -> Result<()>;
    /// Only sessions that have not expired.
    async fn session(&self, token_hash: &str) -> Result<Option<Session>>;
    async fn delete_session(&self, token_hash: &str) -> Result<()>;
}

/// Who a secret belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// This Mergepatrol: the self-hosted GitHub App.
    Instance,
    /// One workspace: its Anthropic API key.
    Installation(u64),
}

impl Scope {
    pub fn key(&self) -> String {
        match self {
            Self::Instance => "instance".into(),
            Self::Installation(id) => format!("installation:{id}"),
        }
    }
}

/// One installation of the GitHub App, on an organization or a user account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub installation_id: u64,
    pub account: String,
    /// `Organization` or `User`.
    pub account_type: String,
    pub avatar_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoSetting {
    pub repo: RepoName,
    pub installation_id: u64,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionKind {
    /// Signed in with GitHub. `installations` are the ones the user could access then.
    User {
        login: String,
        avatar_url: String,
        installations: Vec<u64>,
    },
    /// Unlocked with the setup token, before the GitHub App exists.
    Setup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub kind: SessionKind,
    /// Unix seconds.
    pub expires_at: i64,
}

pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(url)
            .await
            .context("connect to Postgres")?;
        Self::migrate(pool).await
    }

    pub async fn migrate(pool: PgPool) -> Result<Self> {
        sqlx::migrate!()
            .run(&pool)
            .await
            .context("migrate the database")?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl Store for PgStore {
    async fn load_lane(&self, key: &LaneKey) -> Result<Lane> {
        let row = sqlx::query(
            "SELECT lane FROM lanes WHERE installation_id = $1 AND repo = $2 AND trunk = $3",
        )
        .bind(key.installation_id as i64)
        .bind(key.repo.to_string())
        .bind(&key.trunk)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => Ok(serde_json::from_value(row.get("lane"))?),
            None => Ok(Lane::default()),
        }
    }

    async fn save_lane(&self, key: &LaneKey, lane: &Lane) -> Result<()> {
        sqlx::query(
            "INSERT INTO lanes (installation_id, repo, trunk, lane) VALUES ($1, $2, $3, $4)
             ON CONFLICT (installation_id, repo, trunk) DO UPDATE SET lane = excluded.lane",
        )
        .bind(key.installation_id as i64)
        .bind(key.repo.to_string())
        .bind(&key.trunk)
        .bind(serde_json::to_value(lane)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn lanes(&self, installation_id: u64) -> Result<Vec<(LaneKey, Lane)>> {
        let rows = sqlx::query(
            "SELECT repo, trunk, lane FROM lanes WHERE installation_id = $1 ORDER BY repo, trunk",
        )
        .bind(installation_id as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let key = LaneKey {
                    installation_id,
                    repo: RepoName::parse(row.get("repo")).context("bad repo in store")?,
                    trunk: row.get("trunk"),
                };
                Ok((key, serde_json::from_value(row.get("lane"))?))
            })
            .collect()
    }

    async fn record(&self, key: &LaneKey, pr: PrNumber, kind: &str, message: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO events (installation_id, repo, pr, kind, message) VALUES ($1, $2, $3, $4, $5)",
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
        installation_id: u64,
        repo: &RepoName,
        pr: Option<PrNumber>,
        limit: u32,
    ) -> Result<Vec<EventRecord>> {
        // Timestamps leave the database as RFC 3339 strings in UTC.
        let rows = sqlx::query(
            r#"SELECT repo, pr, to_char(at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS at,
                kind, message
             FROM events
             WHERE installation_id = $1 AND repo = $2 AND ($3::BIGINT IS NULL OR pr = $3)
             ORDER BY id DESC LIMIT $4"#,
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(pr.map(|p| p as i64))
        .bind(limit as i64)
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

    async fn put_secret(&self, scope: Scope, name: &str, secret: SecretRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO secrets (scope, name, nonce, ciphertext, hint) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (scope, name) DO UPDATE SET nonce = excluded.nonce,
                ciphertext = excluded.ciphertext, hint = excluded.hint, updated_at = now()",
        )
        .bind(scope.key())
        .bind(name)
        .bind(secret.nonce)
        .bind(secret.ciphertext)
        .bind(secret.hint)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn secret(&self, scope: Scope, name: &str) -> Result<Option<SecretRecord>> {
        let row = sqlx::query(
            r#"SELECT nonce, ciphertext, hint,
                to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at
             FROM secrets WHERE scope = $1 AND name = $2"#,
        )
        .bind(scope.key())
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

    async fn delete_secret(&self, scope: Scope, name: &str) -> Result<()> {
        sqlx::query("DELETE FROM secrets WHERE scope = $1 AND name = $2")
            .bind(scope.key())
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn repo_settings(&self) -> Result<Vec<RepoSetting>> {
        let rows = sqlx::query(
            "SELECT repo, installation_id, enabled FROM repos ORDER BY installation_id, repo",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(RepoSetting {
                    repo: RepoName::parse(row.get("repo")).context("bad repo in store")?,
                    installation_id: row.get::<i64, _>("installation_id") as u64,
                    enabled: row.get("enabled"),
                })
            })
            .collect()
    }

    async fn set_repo_enabled(
        &self,
        installation_id: u64,
        repo: &RepoName,
        enabled: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO repos (installation_id, repo, enabled) VALUES ($1, $2, $3)
             ON CONFLICT (installation_id, repo) DO UPDATE SET enabled = excluded.enabled",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(enabled)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn save_workspaces(&self, workspaces: &[Workspace]) -> Result<()> {
        for w in workspaces {
            sqlx::query(
                "INSERT INTO workspaces (installation_id, account, account_type, avatar_url)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (installation_id) DO UPDATE SET account = excluded.account,
                    account_type = excluded.account_type, avatar_url = excluded.avatar_url,
                    updated_at = now()",
            )
            .bind(w.installation_id as i64)
            .bind(&w.account)
            .bind(&w.account_type)
            .bind(&w.avatar_url)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    async fn workspaces(&self, installation_ids: &[u64]) -> Result<Vec<Workspace>> {
        let ids: Vec<i64> = installation_ids.iter().map(|&i| i as i64).collect();
        let rows = sqlx::query(
            "SELECT installation_id, account, account_type, avatar_url FROM workspaces
             WHERE installation_id = ANY($1) ORDER BY lower(account)",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| Workspace {
                installation_id: row.get::<i64, _>("installation_id") as u64,
                account: row.get("account"),
                account_type: row.get("account_type"),
                avatar_url: row.get("avatar_url"),
            })
            .collect())
    }

    async fn create_session(&self, token_hash: &str, session: &Session) -> Result<()> {
        let (kind, login, avatar, installations) = match &session.kind {
            SessionKind::User {
                login,
                avatar_url,
                installations,
            } => (
                "user",
                Some(login.as_str()),
                Some(avatar_url.as_str()),
                installations.iter().map(|&i| i as i64).collect(),
            ),
            SessionKind::Setup => ("setup", None, None, Vec::new()),
        };
        sqlx::query(
            "INSERT INTO sessions (token_hash, kind, login, avatar_url, installations, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(token_hash)
        .bind(kind)
        .bind(login)
        .bind(avatar)
        .bind(installations)
        .bind(session.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn session(&self, token_hash: &str) -> Result<Option<Session>> {
        let row = sqlx::query(
            "SELECT kind, login, avatar_url, installations, expires_at FROM sessions
             WHERE token_hash = $1 AND expires_at > extract(epoch FROM now())",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let kind = match row.get::<&str, _>("kind") {
            "user" => SessionKind::User {
                login: row.get("login"),
                avatar_url: row.get("avatar_url"),
                installations: row
                    .get::<Vec<i64>, _>("installations")
                    .into_iter()
                    .map(|i| i as u64)
                    .collect(),
            },
            _ => SessionKind::Setup,
        };
        Ok(Some(Session {
            kind,
            expires_at: row.get("expires_at"),
        }))
    }

    async fn delete_session(&self, token_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
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

    #[sqlx::test(migrations = false)]
    async fn sessions_expire(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let live = Session {
            kind: SessionKind::User {
                login: "ada".into(),
                avatar_url: "a".into(),
                installations: vec![1, 2],
            },
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

    #[sqlx::test(migrations = false)]
    async fn repo_settings_upsert(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let repo = RepoName::parse("o/r").unwrap();
        store.set_repo_enabled(9, &repo, true).await.unwrap();
        store.set_repo_enabled(9, &repo, false).await.unwrap();
        // The same repo name under another installation is its own setting.
        store.set_repo_enabled(10, &repo, true).await.unwrap();
        assert_eq!(
            store.repo_settings().await.unwrap(),
            [
                RepoSetting {
                    repo: repo.clone(),
                    installation_id: 9,
                    enabled: false
                },
                RepoSetting {
                    repo,
                    installation_id: 10,
                    enabled: true
                },
            ]
        );
    }

    #[sqlx::test(migrations = false)]
    async fn lanes_and_events_round_trip(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
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
            summary: Default::default(),
        }]);
        store.save_lane(&key, &lane).await.unwrap();
        assert_eq!(store.load_lane(&key).await.unwrap(), lane);
        assert_eq!(store.lanes(1).await.unwrap(), [(key.clone(), lane)]);
        assert_eq!(store.lanes(2).await.unwrap(), []);

        store.record(&key, 4, "enqueued", "hi").await.unwrap();
        store.record(&key, 5, "enqueued", "other").await.unwrap();
        let events = store.events(1, &key.repo, Some(4), 10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message, "hi");
        assert!(events[0].at.ends_with('Z'), "{}", events[0].at);
        assert_eq!(store.events(1, &key.repo, None, 10).await.unwrap().len(), 2);
        assert_eq!(store.events(2, &key.repo, None, 10).await.unwrap().len(), 0);
    }

    #[sqlx::test(migrations = false)]
    async fn workspaces_upsert_and_filter(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let w = |id, account: &str| Workspace {
            installation_id: id,
            account: account.into(),
            account_type: "Organization".into(),
            avatar_url: String::new(),
        };
        store
            .save_workspaces(&[w(1, "acme"), w(2, "beta")])
            .await
            .unwrap();
        store
            .save_workspaces(&[w(1, "acme-renamed")])
            .await
            .unwrap();
        assert_eq!(
            store.workspaces(&[1]).await.unwrap(),
            [w(1, "acme-renamed")]
        );
        assert_eq!(store.workspaces(&[3]).await.unwrap(), []);
    }
}
