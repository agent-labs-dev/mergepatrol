//! What the queue last read of each repo from GitHub, and what changed there since: the queue
//! reads GitHub again only for that.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use mergepatrol_core::{PrNumber, RepoName};
use serde_json::Value;
use sqlx::Row;

use crate::PgStore;

/// A repo's reads. Each counts the changes it saw: a newer count means it is stale.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoReads {
    /// Changes to the repo itself: its default branch, its labels.
    pub changes: i64,
    /// The config file: `None` in it for none.
    pub config: Option<Read<Option<String>>>,
    /// The labels it was read for, and the repository, without its PRs.
    pub repository: Option<Read<(Value, Value)>>,
    /// The queued PRs, and those changed since they were read.
    pub prs: Vec<PrRead>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Read<T> {
    /// The repo's `changes` when the read began.
    pub changes: i64,
    pub age: Duration,
    pub value: T,
}

impl<T> Read<T> {
    /// Whether nothing changed since, and it is younger than `max_age`.
    pub fn fresh(&self, changes: i64, max_age: Duration) -> bool {
        self.changes == changes && self.age < max_age
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PrRead {
    pub number: PrNumber,
    pub changes: i64,
    /// `changes` when `node` was read.
    pub read: i64,
    /// `None`: never read.
    pub node: Option<Value>,
}

impl PrRead {
    pub fn fresh(&self) -> bool {
        self.node.is_some() && self.read == self.changes
    }
}

/// A PR read, at the `changes` it began at. `None` for a PR that is not queued: it goes,
/// unless it changed since.
pub struct PrWrite {
    pub number: PrNumber,
    pub changes: i64,
    pub node: Option<Value>,
}

#[async_trait]
pub trait ReadCache: Send + Sync {
    /// Something changed on GitHub: the repo itself with `None`, or one of its PRs. Only a repo
    /// the queue reads keeps track.
    async fn github_changed(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: Option<PrNumber>,
    ) -> Result<()>;
    /// Starts keeping track of the repo's changes, if it did not yet.
    async fn github_reads(&self, installation_id: u64, repo: &RepoName) -> Result<RepoReads>;
    async fn save_config_read(
        &self,
        installation_id: u64,
        repo: &RepoName,
        changes: i64,
        config: Option<&str>,
    ) -> Result<()>;
    async fn save_repository_read(
        &self,
        installation_id: u64,
        repo: &RepoName,
        changes: i64,
        labels: &Value,
        repository: &Value,
    ) -> Result<()>;
    async fn save_pr_reads(
        &self,
        installation_id: u64,
        repo: &RepoName,
        prs: &[PrWrite],
    ) -> Result<()>;
}

#[async_trait]
impl ReadCache for PgStore {
    async fn github_changed(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: Option<PrNumber>,
    ) -> Result<()> {
        let query = match pr {
            None => sqlx::query(
                "UPDATE github_repos SET changes = changes + 1
                 WHERE installation_id = $1 AND repo = $2",
            ),
            Some(_) => sqlx::query(
                "INSERT INTO github_prs (installation_id, repo, number)
                 SELECT installation_id, repo, $3 FROM github_repos
                 WHERE installation_id = $1 AND repo = $2
                 ON CONFLICT (installation_id, repo, number)
                 DO UPDATE SET changes = github_prs.changes + 1",
            ),
        };
        query
            .bind(installation_id as i64)
            .bind(repo.to_string())
            .bind(pr.map(|n| n as i64))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn github_reads(&self, installation_id: u64, repo: &RepoName) -> Result<RepoReads> {
        let row = sqlx::query(
            "WITH created AS (
                 INSERT INTO github_repos (installation_id, repo) VALUES ($1, $2)
                 ON CONFLICT DO NOTHING
                 RETURNING *
             )
             SELECT *, EXTRACT(EPOCH FROM now() - config_read_at)::float8 AS config_age,
                 EXTRACT(EPOCH FROM now() - repository_read_at)::float8 AS repository_age
             FROM (SELECT * FROM created UNION ALL
                   SELECT * FROM github_repos WHERE installation_id = $1 AND repo = $2) r",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .fetch_one(&self.pool)
        .await?;
        let age = |col: &str| {
            Duration::from_secs_f64(row.get::<Option<f64>, _>(col).unwrap_or(0.0).max(0.0))
        };
        let config = row
            .get::<Option<i64>, _>("config_read")
            .map(|changes| Read {
                changes,
                age: age("config_age"),
                value: row.get("config"),
            });
        let repository = row
            .get::<Option<i64>, _>("repository_read")
            .map(|changes| Read {
                changes,
                age: age("repository_age"),
                value: (row.get("labels"), row.get("repository")),
            });
        let prs = sqlx::query(
            "SELECT number, changes, read, node FROM github_prs
             WHERE installation_id = $1 AND repo = $2 ORDER BY number",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| PrRead {
            number: r.get::<i64, _>("number") as PrNumber,
            changes: r.get("changes"),
            read: r.get("read"),
            node: r.get("node"),
        })
        .collect();
        Ok(RepoReads {
            changes: row.get("changes"),
            config,
            repository,
            prs,
        })
    }

    async fn save_config_read(
        &self,
        installation_id: u64,
        repo: &RepoName,
        changes: i64,
        config: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE github_repos SET config_read = $3, config_read_at = now(), config = $4
             WHERE installation_id = $1 AND repo = $2",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(changes)
        .bind(config)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn save_repository_read(
        &self,
        installation_id: u64,
        repo: &RepoName,
        changes: i64,
        labels: &Value,
        repository: &Value,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE github_repos SET repository_read = $3, repository_read_at = now(),
                 labels = $4, repository = $5
             WHERE installation_id = $1 AND repo = $2",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(changes)
        .bind(labels)
        .bind(repository)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn save_pr_reads(
        &self,
        installation_id: u64,
        repo: &RepoName,
        prs: &[PrWrite],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for pr in prs {
            let query = match &pr.node {
                // A PR new to the table starts at `changes` 0: a webhook since made it 1.
                Some(node) => sqlx::query(
                    "INSERT INTO github_prs (installation_id, repo, number, changes, read, node)
                     VALUES ($1, $2, $3, $4, $4, $5)
                     ON CONFLICT (installation_id, repo, number)
                     DO UPDATE SET read = excluded.read, node = excluded.node",
                )
                .bind(installation_id as i64)
                .bind(repo.to_string())
                .bind(pr.number as i64)
                .bind(pr.changes)
                .bind(node),
                None => sqlx::query(
                    "DELETE FROM github_prs
                     WHERE installation_id = $1 AND repo = $2 AND number = $3 AND changes = $4",
                )
                .bind(installation_id as i64)
                .bind(repo.to_string())
                .bind(pr.number as i64)
                .bind(pr.changes),
            };
            query.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;

    fn repo() -> RepoName {
        RepoName::parse("o/r").unwrap()
    }

    fn write(number: PrNumber, changes: i64, node: Option<Value>) -> PrWrite {
        PrWrite {
            number,
            changes,
            node,
        }
    }

    #[sqlx::test(migrations = false)]
    async fn a_change_during_a_read_leaves_it_stale(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let start = store.github_reads(1, &repo()).await.unwrap();
        assert_eq!(start, RepoReads::default());
        store.github_changed(1, &repo(), None).await.unwrap();
        store
            .save_config_read(1, &repo(), start.changes, Some("yml"))
            .await
            .unwrap();
        let reads = store.github_reads(1, &repo()).await.unwrap();
        let config = reads.config.unwrap();
        assert_eq!(config.value.as_deref(), Some("yml"));
        assert!(!config.fresh(reads.changes, Duration::from_secs(60)));
        store
            .save_config_read(1, &repo(), reads.changes, None)
            .await
            .unwrap();
        let reads = store.github_reads(1, &repo()).await.unwrap();
        let config = reads.config.unwrap();
        assert_eq!(config.value, None);
        assert!(config.fresh(reads.changes, Duration::from_secs(60)));
        assert!(!config.fresh(reads.changes, Duration::ZERO));
    }

    #[sqlx::test(migrations = false)]
    async fn only_a_repo_the_queue_reads_keeps_its_changes(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let other = RepoName::parse("o/other").unwrap();
        store.github_changed(1, &other, None).await.unwrap();
        store.github_changed(1, &other, Some(7)).await.unwrap();
        assert_eq!(
            store.github_reads(1, &other).await.unwrap(),
            RepoReads::default()
        );
    }

    #[sqlx::test(migrations = false)]
    async fn a_changed_pr_reads_again_and_one_not_queued_goes(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        store.github_reads(1, &repo()).await.unwrap();
        store
            .save_pr_reads(1, &repo(), &[write(1, 0, Some(json!({"n": 1})))])
            .await
            .unwrap();
        // Labeled: a PR the queue had not read.
        store.github_changed(1, &repo(), Some(2)).await.unwrap();
        store.github_changed(1, &repo(), Some(1)).await.unwrap();
        let prs = store.github_reads(1, &repo()).await.unwrap().prs;
        assert_eq!(
            prs,
            [
                PrRead {
                    number: 1,
                    changes: 1,
                    read: 0,
                    node: Some(json!({"n": 1}))
                },
                PrRead {
                    number: 2,
                    changes: 1,
                    read: 0,
                    node: None
                },
            ]
        );
        assert!(prs.iter().all(|pr| !pr.fresh()));

        // PR 1 changed again while it was read: it stays, stale.
        store.github_changed(1, &repo(), Some(1)).await.unwrap();
        store
            .save_pr_reads(
                1,
                &repo(),
                &[write(1, 1, None), write(2, 1, Some(json!({"n": 2})))],
            )
            .await
            .unwrap();
        let prs = store.github_reads(1, &repo()).await.unwrap().prs;
        assert_eq!(prs.len(), 2);
        assert!(!prs[0].fresh());
        assert!(prs[1].fresh());

        store
            .save_pr_reads(1, &repo(), &[write(1, 2, None)])
            .await
            .unwrap();
        let prs = store.github_reads(1, &repo()).await.unwrap().prs;
        assert_eq!(prs.iter().map(|p| p.number).collect::<Vec<_>>(), [2]);
    }
}
