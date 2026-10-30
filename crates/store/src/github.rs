//! What the queue last read of each repo from GitHub, and what changed there since: the queue
//! reads GitHub again only for that.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use mergepatrol_core::{PrNumber, RepoName};
use serde_json::Value;
use sqlx::Row;

use crate::PgStore;

/// What changed on GitHub, for the queue to read again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// The repo itself: its default branch, its labels.
    Repo,
    Pr(PrNumber),
    /// One of the other reads, by key.
    Value(String),
    /// Every other read whose key starts so. `""` for all of them.
    ValuesUnder(String),
}

/// A repo's reads. Each counts the changes it saw: a newer count means it is stale.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoReads {
    /// Changes to the repo itself: its default branch, its labels.
    pub changes: i64,
    /// How long ago the repo last changed. `None`: never.
    pub changed: Option<Duration>,
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
    pub changed: Option<Duration>,
    /// `changes` when `node` was read.
    pub read: i64,
    /// `None`: never read.
    pub node: Option<Value>,
}

/// One of a repo's other reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ValueRead {
    pub changes: i64,
    pub changed: Option<Duration>,
    /// `changes` when `value` was read, how long ago, and the value. `None`: never read.
    pub read: Option<Read<Value>>,
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
    /// Something changed on GitHub. Only a repo the queue reads keeps track, and of its other
    /// reads, only those it read.
    async fn github_changed(
        &self,
        installation_id: u64,
        repo: &RepoName,
        change: &Change,
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
    /// Starts keeping track of the key's changes, if it did not yet.
    async fn github_value(
        &self,
        installation_id: u64,
        repo: &RepoName,
        key: &str,
    ) -> Result<ValueRead>;
    async fn save_github_value(
        &self,
        installation_id: u64,
        repo: &RepoName,
        key: &str,
        changes: i64,
        value: &Value,
    ) -> Result<()>;
}

#[async_trait]
impl ReadCache for PgStore {
    async fn github_changed(
        &self,
        installation_id: u64,
        repo: &RepoName,
        change: &Change,
    ) -> Result<()> {
        let sql = match change {
            Change::Repo => {
                "UPDATE github_repos SET changes = changes + 1, changed_at = now()
                 WHERE installation_id = $1 AND repo = $2"
            }
            Change::Pr(_) => {
                "INSERT INTO github_prs (installation_id, repo, number, changed_at)
                 SELECT installation_id, repo, $3, now() FROM github_repos
                 WHERE installation_id = $1 AND repo = $2
                 ON CONFLICT (installation_id, repo, number)
                 DO UPDATE SET changes = github_prs.changes + 1, changed_at = now()"
            }
            // A key not read yet keeps when it changed: a first read soon after is not kept.
            Change::Value(_) => {
                "INSERT INTO github_values (installation_id, repo, key, changes, changed_at)
                 SELECT installation_id, repo, $3, 1, now() FROM github_repos
                 WHERE installation_id = $1 AND repo = $2
                 ON CONFLICT (installation_id, repo, key)
                 DO UPDATE SET changes = github_values.changes + 1, changed_at = now()"
            }
            Change::ValuesUnder(_) => {
                "WITH marked AS (
                     UPDATE github_values SET changes = changes + 1, changed_at = now()
                     WHERE installation_id = $1 AND repo = $2 AND starts_with(key, $3)
                 )
                 INSERT INTO github_value_prefixes (installation_id, repo, prefix, changed_at)
                 SELECT installation_id, repo, $3, now() FROM github_repos
                 WHERE installation_id = $1 AND repo = $2
                 ON CONFLICT (installation_id, repo, prefix) DO UPDATE SET changed_at = now()"
            }
        };
        let query = sqlx::query(sql)
            .bind(installation_id as i64)
            .bind(repo.to_string());
        let query = match change {
            Change::Repo => query,
            Change::Pr(number) => query.bind(*number as i64),
            Change::Value(key) | Change::ValuesUnder(key) => query.bind(key.as_str()),
        };
        let mut tx = self.pool.begin().await?;
        // Against a key's first read: one of the two sees the other.
        if let Change::ValuesUnder(_) = change {
            lock_repo(&mut tx, installation_id, repo).await?;
        }
        query.execute(&mut *tx).await?;
        tx.commit().await?;
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
                 EXTRACT(EPOCH FROM now() - repository_read_at)::float8 AS repository_age,
                 EXTRACT(EPOCH FROM now() - changed_at)::float8 AS changed_age
             FROM (SELECT * FROM created UNION ALL
                   SELECT * FROM github_repos WHERE installation_id = $1 AND repo = $2) r",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .fetch_one(&self.pool)
        .await?;
        let age = |col: &str| since(row.get(col)).unwrap_or_default();
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
            "SELECT number, changes, read, node,
                 EXTRACT(EPOCH FROM now() - changed_at)::float8 AS changed_age
             FROM github_prs WHERE installation_id = $1 AND repo = $2 ORDER BY number",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| PrRead {
            number: r.get::<i64, _>("number") as PrNumber,
            changes: r.get("changes"),
            changed: since(r.get("changed_age")),
            read: r.get("read"),
            node: r.get("node"),
        })
        .collect();
        Ok(RepoReads {
            changes: row.get("changes"),
            changed: since(row.get("changed_age")),
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

    async fn github_value(
        &self,
        installation_id: u64,
        repo: &RepoName,
        key: &str,
    ) -> Result<ValueRead> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO github_repos (installation_id, repo) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .execute(&mut *tx)
        .await?;
        // Against a prefix mark: one of the two sees the other.
        lock_repo(&mut tx, installation_id, repo).await?;
        sqlx::query(
            "INSERT INTO github_values (installation_id, repo, key, changed_at)
             VALUES ($1, $2, $3, (SELECT max(changed_at) FROM github_value_prefixes
                 WHERE installation_id = $1 AND repo = $2 AND starts_with($3, prefix)))
             ON CONFLICT DO NOTHING",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(key)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(
            "SELECT changes, read, value,
                 EXTRACT(EPOCH FROM now() - changed_at)::float8 AS changed_age,
                 EXTRACT(EPOCH FROM now() - read_at)::float8 AS read_age
             FROM github_values WHERE installation_id = $1 AND repo = $2 AND key = $3",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(key)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let read = match (row.get::<Option<i64>, _>("read"), row.get("value")) {
            (Some(changes), Some(value)) => Some(Read {
                changes,
                age: since(row.get("read_age")).unwrap_or_default(),
                value,
            }),
            _ => None,
        };
        Ok(ValueRead {
            changes: row.get("changes"),
            changed: since(row.get("changed_age")),
            read,
        })
    }

    async fn save_github_value(
        &self,
        installation_id: u64,
        repo: &RepoName,
        key: &str,
        changes: i64,
        value: &Value,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE github_values SET read = $4, read_at = now(), value = $5
             WHERE installation_id = $1 AND repo = $2 AND key = $3",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(key)
        .bind(changes)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Holds the repo's row until the transaction ends. Not `FOR UPDATE`, which would hold off the
/// foreign key checks of the rows that point at it.
async fn lock_repo(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    installation_id: u64,
    repo: &RepoName,
) -> Result<()> {
    sqlx::query(
        "SELECT 1 FROM github_repos WHERE installation_id = $1 AND repo = $2 FOR NO KEY UPDATE",
    )
    .bind(installation_id as i64)
    .bind(repo.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Seconds ago, as Postgres counts them.
fn since(secs: Option<f64>) -> Option<Duration> {
    secs.map(|s| Duration::from_secs_f64(s.max(0.0)))
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
        store
            .github_changed(1, &repo(), &Change::Repo)
            .await
            .unwrap();
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
        store
            .github_changed(1, &other, &Change::Repo)
            .await
            .unwrap();
        store
            .github_changed(1, &other, &Change::Pr(7))
            .await
            .unwrap();
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
        store
            .github_changed(1, &repo(), &Change::Pr(2))
            .await
            .unwrap();
        store
            .github_changed(1, &repo(), &Change::Pr(1))
            .await
            .unwrap();
        let prs = store.github_reads(1, &repo()).await.unwrap().prs;
        let seen: Vec<_> = prs
            .iter()
            .map(|p| (p.number, p.changes, p.read, p.node.clone()))
            .collect();
        assert_eq!(seen, [(1, 1, 0, Some(json!({"n": 1}))), (2, 1, 0, None)]);
        assert!(
            prs.iter()
                .all(|pr| pr.changed.unwrap() < Duration::from_secs(60))
        );
        assert!(prs.iter().all(|pr| !pr.fresh()));

        // PR 1 changed again while it was read: it stays, stale.
        store
            .github_changed(1, &repo(), &Change::Pr(1))
            .await
            .unwrap();
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

    #[sqlx::test(migrations = false)]
    async fn a_value_reads_again_once_its_key_changes(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let first = store.github_value(1, &repo(), "checks:a").await.unwrap();
        assert_eq!(first, ValueRead::default());
        store
            .save_github_value(1, &repo(), "checks:a", 0, &json!("pending"))
            .await
            .unwrap();
        store.github_value(1, &repo(), "ref:main").await.unwrap();
        let read = store.github_value(1, &repo(), "checks:a").await.unwrap();
        assert_eq!(
            read.read.as_ref().map(|r| (r.changes, r.value.clone())),
            Some((0, json!("pending")))
        );
        assert_eq!(read.changed, None);

        // A key the queue has not read yet keeps when it changed.
        store
            .github_changed(1, &repo(), &Change::Value("checks:b".into()))
            .await
            .unwrap();
        store
            .github_changed(1, &repo(), &Change::Value("checks:a".into()))
            .await
            .unwrap();
        let read = store.github_value(1, &repo(), "checks:a").await.unwrap();
        assert_eq!((read.changes, read.read.unwrap().changes), (1, 0));
        assert!(read.changed.is_some());
        let unread = store.github_value(1, &repo(), "checks:b").await.unwrap();
        assert_eq!((unread.changes, unread.read), (1, None));
        assert!(unread.changed.is_some());
        assert_eq!(
            store
                .github_value(1, &repo(), "ref:main")
                .await
                .unwrap()
                .changes,
            0
        );

        store
            .github_changed(1, &repo(), &Change::ValuesUnder("".into()))
            .await
            .unwrap();
        assert_eq!(
            store
                .github_value(1, &repo(), "checks:a")
                .await
                .unwrap()
                .changes,
            2
        );
        assert_eq!(
            store
                .github_value(1, &repo(), "ref:main")
                .await
                .unwrap()
                .changes,
            1
        );
    }

    #[sqlx::test(migrations = false)]
    async fn a_key_first_read_after_its_prefix_changed_knows_when(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        store.github_reads(1, &repo()).await.unwrap();
        store
            .github_changed(1, &repo(), &Change::ValuesUnder("ref:".into()))
            .await
            .unwrap();
        let moved = store.github_value(1, &repo(), "ref:topic").await.unwrap();
        assert!(moved.changed.is_some());
        let other = store.github_value(1, &repo(), "checks:a").await.unwrap();
        assert_eq!(other.changed, None);
    }

    #[sqlx::test(migrations = false)]
    async fn a_prefix_mark_waits_for_a_first_read_under_way(pool: PgPool) {
        let store = std::sync::Arc::new(PgStore::migrate(pool.clone()).await.unwrap());
        store.github_reads(1, &repo()).await.unwrap();
        // A first read holds the repo's row while it creates its key.
        let mut read = pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM github_repos FOR NO KEY UPDATE")
            .execute(&mut *read)
            .await
            .unwrap();
        let marking = {
            let store = store.clone();
            tokio::spawn(async move {
                let change = Change::ValuesUnder("ref:".into());
                store.github_changed(1, &repo(), &change).await.unwrap();
            })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!marking.is_finished());
        read.commit().await.unwrap();
        marking.await.unwrap();
    }

    #[sqlx::test(migrations = false)]
    async fn a_key_mark_does_not_wait_for_a_first_read_under_way(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        store.github_reads(1, &repo()).await.unwrap();
        // The lock a first read takes: a key's row may still point at the repo's meanwhile.
        let mut read = pool.begin().await.unwrap();
        lock_repo(&mut read, 1, &repo()).await.unwrap();
        let (repo, change) = (repo(), Change::Value("checks:new".into()));
        let marked = store.github_changed(1, &repo, &change);
        tokio::time::timeout(Duration::from_secs(2), marked)
            .await
            .expect("not held off by the lock")
            .unwrap();
        read.commit().await.unwrap();
    }
}
