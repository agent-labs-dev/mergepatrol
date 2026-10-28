//! Persistence. `SqliteStore` is the only backend for now. Postgres comes with the hosted version.

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use mergepatrol_core::Candidate;

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
