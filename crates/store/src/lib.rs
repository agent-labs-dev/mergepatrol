//! Persistence, in Postgres. Migrations are in `migrations/`, embedded, and run at startup.

mod vault;

pub use vault::{ANTHROPIC_API_KEY, GITHUB_APP, SecretStatus, Vault, api_key_secret};

use anyhow::{Context, Result};
use async_trait::async_trait;
use mergepatrol_core::{Lane, PrNumber, PrSummary, RepoName, Step, StepTimes, step_times};
use serde::Serialize;
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgConnection, PgPool};

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

/// A time a PR left the queue: how, and the PR as it was then.
#[derive(Clone, Debug, PartialEq)]
pub struct ExitRecord {
    /// Newer exits have higher IDs: the cursor for the next page.
    pub id: i64,
    pub repo: String,
    pub trunk: String,
    pub pr: PrNumber,
    /// The event kind it left with: `merged`, `ejected`, `dequeued` or `cancelled`.
    pub outcome: String,
    /// The merge commit, or why.
    pub detail: String,
    pub summary: PrSummary,
    /// `None` for a PR ejected before it joined, such as one from a fork.
    pub enqueued_at: Option<String>,
    /// `None` with `enqueued_at`.
    pub steps: Option<StepTimes>,
    pub at: String,
}

/// Seconds the merged PRs spent in one step, over a window.
#[derive(Clone, Debug, PartialEq)]
pub struct StepStats {
    pub step: Step,
    pub mean_secs: f64,
    pub p50_secs: f64,
    pub p95_secs: f64,
}

/// Which exits to list, the newest first.
#[derive(Clone, Debug)]
pub struct ExitQuery<'a> {
    pub installation_id: u64,
    /// `None` for every repo of the workspace.
    pub repo: Option<&'a RepoName>,
    /// Outcomes to keep. Empty keeps all.
    pub outcomes: &'a [&'a str],
    /// Only exits from the last `days`.
    pub days: u32,
    /// Only exits older than this ID: the page after the one that ended with it.
    pub before: Option<i64>,
    pub limit: u32,
}

/// The exits of one repo, or of every repo in the workspace, over a stretch of time.
#[derive(Clone, Debug, PartialEq)]
pub struct ExitStats {
    /// `None` for every repo together.
    pub repo: Option<String>,
    pub merged: u64,
    pub ejected: u64,
    /// Dequeued or cancelled: the label came off, or the PR closed.
    pub removed: u64,
    /// Seconds from joining the queue to merging, of the merged PRs. `None` without any.
    pub p50_secs: Option<f64>,
    pub p95_secs: Option<f64>,
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
    /// A PR left the queue. Its time in the queue runs from its last `enqueued` event. Returns
    /// where that time went: none for a PR that left before it joined.
    async fn record_exit(
        &self,
        key: &LaneKey,
        pr: PrNumber,
        outcome: &str,
        detail: &str,
        summary: &PrSummary,
    ) -> Result<Option<StepTimes>>;
    async fn exits(&self, query: &ExitQuery<'_>) -> Result<Vec<ExitRecord>>;
    /// Each repo's, then every repo's together, from `from_days` ago to `to_days` ago.
    async fn exit_stats(
        &self,
        installation_id: u64,
        from_days: u32,
        to_days: u32,
    ) -> Result<Vec<ExitStats>>;
    /// Of the PRs merged in the last `days`, in the order of `Step::ALL`. Empty without any.
    async fn step_stats(
        &self,
        installation_id: u64,
        repo: Option<&RepoName>,
        days: u32,
    ) -> Result<Vec<StepStats>>;
    /// The PRs merged on each of the last `days` UTC days, today last. `None` for every repo.
    async fn merged_by_day(
        &self,
        installation_id: u64,
        repo: Option<&RepoName>,
        days: u32,
    ) -> Result<Vec<u64>>;
    async fn put_secret(&self, scope: Scope, name: &str, secret: SecretRecord) -> Result<()>;
    async fn secret(&self, scope: Scope, name: &str) -> Result<Option<SecretRecord>>;
    async fn delete_secret(&self, scope: Scope, name: &str) -> Result<()>;
    async fn secret_names(&self) -> Result<Vec<(Scope, String)>>;
    /// Keeps the hint and `updated_at`. Does nothing if the ciphertext is no longer `was`.
    async fn replace_ciphertext(
        &self,
        scope: Scope,
        name: &str,
        was: &[u8],
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    ) -> Result<()>;

    /// The repos Mergepatrol knows of, in every workspace. Repos without a row are off.
    async fn repos(&self) -> Result<Vec<RepoState>>;
    /// `None` follows the config file.
    async fn set_repo_enabled(
        &self,
        installation_id: u64,
        repo: &RepoName,
        enabled: Option<bool>,
    ) -> Result<()>;
    async fn set_has_config(&self, installation_id: u64, repo: &RepoName, has: bool) -> Result<()>;
    /// All the repos of an installation that have the config file. The rest have not.
    async fn replace_has_config(
        &self,
        installation_id: u64,
        with_config: &[RepoName],
    ) -> Result<()>;
    /// `None` clears it.
    async fn set_config_error(
        &self,
        installation_id: u64,
        repo: &RepoName,
        error: Option<&str>,
    ) -> Result<()>;

    /// A fork PR's head when `label` went on it, as the `labeled` webhook told.
    async fn set_label_head(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        label: &str,
        head_sha: &str,
    ) -> Result<()>;
    async fn label_head(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        label: &str,
    ) -> Result<Option<String>>;

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

    pub fn from_key(key: &str) -> Result<Self> {
        match key.strip_prefix("installation:") {
            Some(id) => Ok(Self::Installation(id.parse()?)),
            None if key == "instance" => Ok(Self::Instance),
            None => anyhow::bail!("unknown secret scope {key:?}"),
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
pub struct RepoState {
    pub repo: RepoName,
    pub installation_id: u64,
    /// Set in the dashboard. `None` follows `has_config`.
    pub enabled: Option<bool>,
    /// The default branch has the config file, as of the last look.
    pub has_config: bool,
    /// Why the queue is paused: the config file does not parse.
    pub config_error: Option<String>,
}

impl RepoState {
    /// Whether the repo's queue runs.
    pub fn on(&self) -> bool {
        self.enabled.unwrap_or(self.has_config)
    }
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

    /// The queue lock, unless another instance holds it.
    pub async fn try_queue_lock(&self) -> Result<Option<QueueLock>> {
        let mut conn = PgConnection::connect_with(&self.pool.connect_options())
            .await
            .context("connect to Postgres for the queue lock")?;
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(QUEUE_LOCK)
            .fetch_one(&mut conn)
            .await?;
        Ok(locked.then_some(QueueLock { conn }))
    }
}

/// The advisory lock that the instance running the queues holds: "mergepat".
const QUEUE_LOCK: i64 = 0x6d65_7267_6570_6174;

/// Lets one instance run the queues: two would race each other on every PR. Postgres holds it
/// for as long as its connection lives, so a crashed instance lets it go.
pub struct QueueLock {
    conn: PgConnection,
}

impl QueueLock {
    /// Errs when the connection is gone, and with it the lock.
    pub async fn check(&mut self) -> Result<()> {
        self.conn
            .ping()
            .await
            .context("the queue lock's connection")
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

    async fn record_exit(
        &self,
        key: &LaneKey,
        pr: PrNumber,
        outcome: &str,
        detail: &str,
        summary: &PrSummary,
    ) -> Result<Option<StepTimes>> {
        // From the time it last joined: none for a PR ejected before it joined.
        let events = sqlx::query(
            "SELECT extract(epoch FROM at)::FLOAT8 AS at, kind FROM events
             WHERE installation_id = $1 AND repo = $2 AND pr = $3 AND id >= (
                SELECT max(id) FROM events
                WHERE installation_id = $1 AND repo = $2 AND pr = $3 AND kind = 'enqueued'
             )
             ORDER BY id",
        )
        .bind(key.installation_id as i64)
        .bind(key.repo.to_string())
        .bind(pr as i64)
        .fetch_all(&self.pool)
        .await?;
        let events: Vec<(f64, String)> = events
            .into_iter()
            .map(|row| (row.get("at"), row.get("kind")))
            .collect();
        let events: Vec<(f64, &str)> = events.iter().map(|(at, k)| (*at, k.as_str())).collect();
        let enqueued_at = events.first().map(|(at, _)| *at);
        let steps = enqueued_at.map(|_| step_times(&events));
        sqlx::query(
            "INSERT INTO exits
                (installation_id, repo, trunk, pr, outcome, detail, summary, enqueued_at, steps)
             VALUES ($1, $2, $3, $4, $5, $6, $7, to_timestamp($8), $9)",
        )
        .bind(key.installation_id as i64)
        .bind(key.repo.to_string())
        .bind(&key.trunk)
        .bind(pr as i64)
        .bind(outcome)
        .bind(detail)
        .bind(serde_json::to_value(summary)?)
        .bind(enqueued_at)
        .bind(steps.as_ref().map(serde_json::to_value).transpose()?)
        .execute(&self.pool)
        .await?;
        Ok(steps)
    }

    async fn exits(&self, q: &ExitQuery<'_>) -> Result<Vec<ExitRecord>> {
        let rows = sqlx::query(
            r#"SELECT id, repo, trunk, pr, outcome, detail, summary, steps,
                to_char(enqueued_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS enqueued_at,
                to_char(at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS at
             FROM exits
             WHERE installation_id = $1 AND ($2::TEXT IS NULL OR repo = $2)
                AND (cardinality($3::TEXT[]) = 0 OR outcome = ANY($3))
                AND at > now() - make_interval(days => $4)
                AND ($5::BIGINT IS NULL OR id < $5)
             ORDER BY id DESC LIMIT $6"#,
        )
        .bind(q.installation_id as i64)
        .bind(q.repo.map(|r| r.to_string()))
        .bind(q.outcomes)
        .bind(q.days as i32)
        .bind(q.before)
        .bind(q.limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(ExitRecord {
                    id: row.get("id"),
                    repo: row.get("repo"),
                    trunk: row.get("trunk"),
                    pr: row.get::<i64, _>("pr") as u64,
                    outcome: row.get("outcome"),
                    detail: row.get("detail"),
                    summary: serde_json::from_value(row.get("summary"))?,
                    steps: row
                        .get::<Option<serde_json::Value>, _>("steps")
                        .map(serde_json::from_value)
                        .transpose()?,
                    enqueued_at: row.get("enqueued_at"),
                    at: row.get("at"),
                })
            })
            .collect()
    }

    async fn exit_stats(
        &self,
        installation_id: u64,
        from_days: u32,
        to_days: u32,
    ) -> Result<Vec<ExitStats>> {
        let rows = sqlx::query(
            "SELECT repo,
                count(*) FILTER (WHERE outcome = 'merged') AS merged,
                count(*) FILTER (WHERE outcome = 'ejected') AS ejected,
                count(*) FILTER (WHERE outcome IN ('dequeued', 'cancelled')) AS removed,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM at - enqueued_at)::FLOAT8)
                    FILTER (WHERE outcome = 'merged') AS p50,
                percentile_cont(0.95) WITHIN GROUP (ORDER BY extract(epoch FROM at - enqueued_at)::FLOAT8)
                    FILTER (WHERE outcome = 'merged') AS p95
             FROM exits
             WHERE installation_id = $1
                AND at > now() - make_interval(days => $2)
                AND at <= now() - make_interval(days => $3)
             GROUP BY GROUPING SETS ((repo), ())
             ORDER BY repo NULLS LAST",
        )
        .bind(installation_id as i64)
        .bind(from_days as i32)
        .bind(to_days as i32)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| ExitStats {
                repo: row.get("repo"),
                merged: row.get::<i64, _>("merged") as u64,
                ejected: row.get::<i64, _>("ejected") as u64,
                removed: row.get::<i64, _>("removed") as u64,
                p50_secs: row.get("p50"),
                p95_secs: row.get("p95"),
            })
            .collect())
    }

    async fn step_stats(
        &self,
        installation_id: u64,
        repo: Option<&RepoName>,
        days: u32,
    ) -> Result<Vec<StepStats>> {
        let rows = sqlx::query(
            "SELECT s.key AS step, avg(s.value::FLOAT8) AS mean,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY s.value::FLOAT8) AS p50,
                percentile_cont(0.95) WITHIN GROUP (ORDER BY s.value::FLOAT8) AS p95
             FROM exits, jsonb_each_text(steps) AS s
             WHERE installation_id = $1 AND ($2::TEXT IS NULL OR repo = $2)
                AND outcome = 'merged' AND at > now() - make_interval(days => $3)
             GROUP BY s.key",
        )
        .bind(installation_id as i64)
        .bind(repo.map(|r| r.to_string()))
        .bind(days as i32)
        .fetch_all(&self.pool)
        .await?;
        let mut stats: Vec<StepStats> = rows
            .into_iter()
            .filter_map(|row| {
                Some(StepStats {
                    step: Step::parse(row.get("step"))?,
                    mean_secs: row.get("mean"),
                    p50_secs: row.get("p50"),
                    p95_secs: row.get("p95"),
                })
            })
            .collect();
        stats.sort_by_key(|s| s.step);
        Ok(stats)
    }

    async fn merged_by_day(
        &self,
        installation_id: u64,
        repo: Option<&RepoName>,
        days: u32,
    ) -> Result<Vec<u64>> {
        let rows = sqlx::query(
            "SELECT count(e.id) AS merged
             FROM generate_series(0, $3 - 1) AS ago
             LEFT JOIN exits e ON e.installation_id = $1 AND ($2::TEXT IS NULL OR e.repo = $2)
                AND e.outcome = 'merged'
                AND e.at >= date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                    - make_interval(days => ago)
                AND e.at < date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                    - make_interval(days => ago - 1)
             GROUP BY ago ORDER BY ago DESC",
        )
        .bind(installation_id as i64)
        .bind(repo.map(|r| r.to_string()))
        .bind(days as i32)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| row.get::<i64, _>("merged") as u64)
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

    async fn secret_names(&self) -> Result<Vec<(Scope, String)>> {
        sqlx::query("SELECT scope, name FROM secrets ORDER BY scope, name")
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|row| Ok((Scope::from_key(row.get("scope"))?, row.get("name"))))
            .collect()
    }

    async fn replace_ciphertext(
        &self,
        scope: Scope,
        name: &str,
        was: &[u8],
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE secrets SET nonce = $4, ciphertext = $5
             WHERE scope = $1 AND name = $2 AND ciphertext = $3",
        )
        .bind(scope.key())
        .bind(name)
        .bind(was)
        .bind(nonce)
        .bind(ciphertext)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn repos(&self) -> Result<Vec<RepoState>> {
        let rows = sqlx::query(
            "SELECT repo, installation_id, enabled, has_config, config_error FROM repos
             ORDER BY installation_id, repo",
        )
        .fetch_all(&self.pool)
        .await?;
        // A row that does not name a repo is skipped, not an error: it would stop every queue.
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let name: String = row.get("repo");
                let Some(repo) = RepoName::parse(&name) else {
                    tracing::warn!(repo = %name, "skipping a repo row that names no repo");
                    return None;
                };
                Some(RepoState {
                    repo,
                    installation_id: row.get::<i64, _>("installation_id") as u64,
                    enabled: row.get("enabled"),
                    has_config: row.get("has_config"),
                    config_error: row.get("config_error"),
                })
            })
            .collect())
    }

    async fn set_repo_enabled(
        &self,
        installation_id: u64,
        repo: &RepoName,
        enabled: Option<bool>,
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

    async fn set_label_head(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        label: &str,
        head_sha: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO label_heads (installation_id, repo, pr, label, head_sha)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (installation_id, repo, pr, label) DO UPDATE SET head_sha = excluded.head_sha",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(pr as i64)
        .bind(label)
        .bind(head_sha)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn label_head(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        label: &str,
    ) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT head_sha FROM label_heads
             WHERE installation_id = $1 AND repo = $2 AND pr = $3 AND label = $4",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(pr as i64)
        .bind(label)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn set_has_config(&self, installation_id: u64, repo: &RepoName, has: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO repos (installation_id, repo, has_config) VALUES ($1, $2, $3)
             ON CONFLICT (installation_id, repo) DO UPDATE SET has_config = excluded.has_config",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(has)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn replace_has_config(
        &self,
        installation_id: u64,
        with_config: &[RepoName],
    ) -> Result<()> {
        let names: Vec<String> = with_config.iter().map(|r| r.to_string()).collect();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE repos SET has_config = (repo = ANY($2))
             WHERE installation_id = $1 AND has_config <> (repo = ANY($2))",
        )
        .bind(installation_id as i64)
        .bind(&names)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO repos (installation_id, repo, has_config)
             SELECT $1, unnest($2::TEXT[]), true
             ON CONFLICT (installation_id, repo) DO NOTHING",
        )
        .bind(installation_id as i64)
        .bind(&names)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn set_config_error(
        &self,
        installation_id: u64,
        repo: &RepoName,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE repos SET config_error = $3
             WHERE installation_id = $1 AND repo = $2 AND config_error IS DISTINCT FROM $3",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(error)
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
    async fn one_instance_at_a_time_holds_the_queue_lock(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let mut first = store.try_queue_lock().await.unwrap().expect("a free lock");
        assert!(store.try_queue_lock().await.unwrap().is_none());
        first.check().await.unwrap();
        // As when the instance stops: its connection closes.
        drop(first);
        let mut again = None;
        for _ in 0..50 {
            again = store.try_queue_lock().await.unwrap();
            if again.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(again.is_some(), "the lock was not let go");
    }

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
    async fn a_label_head_is_the_latest_labeling(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let repo = RepoName::parse("o/r").unwrap();
        assert_eq!(store.label_head(1, &repo, 7, "mp").await.unwrap(), None);
        store.set_label_head(1, &repo, 7, "mp", "a").await.unwrap();
        store.set_label_head(1, &repo, 7, "mp", "b").await.unwrap();
        store
            .set_label_head(1, &repo, 7, "other", "c")
            .await
            .unwrap();
        assert_eq!(
            store
                .label_head(1, &repo, 7, "mp")
                .await
                .unwrap()
                .as_deref(),
            Some("b")
        );
        assert_eq!(store.label_head(2, &repo, 7, "mp").await.unwrap(), None);
    }

    #[sqlx::test(migrations = false)]
    async fn a_row_that_names_no_repo_does_not_hide_the_others(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let a = RepoName::parse("o/a").unwrap();
        store.set_repo_enabled(9, &a, Some(true)).await.unwrap();
        // As `PUT /api/w/9/repos/o%2Fx/y` stored it.
        let bad = RepoName {
            owner: "o/x".into(),
            name: "y".into(),
        };
        store.set_repo_enabled(9, &bad, Some(true)).await.unwrap();
        let repos: Vec<String> = store
            .repos()
            .await
            .unwrap()
            .iter()
            .map(|s| s.repo.to_string())
            .collect();
        assert_eq!(repos, ["o/a"]);
    }

    #[sqlx::test(migrations = false)]
    async fn a_repo_is_on_with_the_config_file_unless_overridden(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let a = RepoName::parse("o/a").unwrap();
        let b = RepoName::parse("o/b").unwrap();
        let on = |states: &[RepoState]| -> Vec<(u64, String, bool)> {
            states
                .iter()
                .map(|s| (s.installation_id, s.repo.to_string(), s.on()))
                .collect()
        };

        store
            .replace_has_config(9, &[a.clone(), b.clone()])
            .await
            .unwrap();
        store.set_repo_enabled(9, &b, Some(false)).await.unwrap();
        // The same repo name under another installation is its own row.
        store.set_repo_enabled(10, &a, Some(true)).await.unwrap();
        assert_eq!(
            on(&store.repos().await.unwrap()),
            [
                (9, "o/a".into(), true),
                (9, "o/b".into(), false),
                (10, "o/a".into(), true),
            ]
        );

        // `a` lost its file; `b` follows its file again.
        store
            .replace_has_config(9, std::slice::from_ref(&b))
            .await
            .unwrap();
        store.set_repo_enabled(9, &b, None).await.unwrap();
        store.set_config_error(9, &b, Some("bad")).await.unwrap();
        let states = store.repos().await.unwrap();
        assert_eq!(
            on(&states),
            [
                (9, "o/a".into(), false),
                (9, "o/b".into(), true),
                (10, "o/a".into(), true),
            ]
        );
        assert_eq!(states[1].config_error.as_deref(), Some("bad"));
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
            ..Default::default()
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
    async fn exits_keep_the_pr_and_its_time_in_the_queue(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let key = |repo: &str| LaneKey {
            installation_id: 1,
            repo: RepoName::parse(repo).unwrap(),
            trunk: "main".into(),
        };
        let (a, b) = (key("o/a"), key("o/b"));
        let summary = PrSummary {
            title: "Fix it".into(),
            ..Default::default()
        };
        store.record(&a, 1, "enqueued", "").await.unwrap();
        store.record(&a, 1, "rebasing", "").await.unwrap();
        store.record(&a, 1, "published", "").await.unwrap();
        // An hour in the queue, all of it waiting.
        sqlx::query("UPDATE events SET at = now() - interval '1 hour' WHERE kind = 'enqueued'")
            .execute(&pool)
            .await
            .unwrap();
        let steps = store
            .record_exit(&a, 1, "merged", "abc", &summary)
            .await
            .unwrap();
        assert!(steps.is_some_and(|s| s[&Step::Waiting] > 3599.0));
        store.record(&b, 2, "enqueued", "").await.unwrap();
        store
            .record_exit(&b, 2, "ejected", "no", &summary)
            .await
            .unwrap();
        // Ejected before it joined.
        let steps = store
            .record_exit(&b, 3, "ejected", "fork", &summary)
            .await
            .unwrap();
        assert_eq!(steps, None);
        store
            .record_exit(&b, 4, "dequeued", "", &summary)
            .await
            .unwrap();

        let query = ExitQuery {
            installation_id: 1,
            repo: None,
            outcomes: &[],
            days: 30,
            before: None,
            limit: 10,
        };
        async fn prs(store: &PgStore, q: ExitQuery<'_>) -> Vec<PrNumber> {
            let exits = store.exits(&q).await.unwrap();
            exits.iter().map(|e| e.pr).collect()
        }
        let exits = store.exits(&query).await.unwrap();
        assert_eq!(exits.iter().map(|e| e.pr).collect::<Vec<_>>(), [4, 3, 2, 1]);
        assert_eq!(exits[3].summary, summary);
        assert_eq!(exits[3].detail, "abc");
        assert!(exits[3].enqueued_at.is_some());
        assert_eq!(exits[1].enqueued_at, None);
        assert_eq!(exits[1].steps, None);
        let waited = exits[3].steps.as_ref().unwrap()[&Step::Waiting];
        assert!((3599.0..3700.0).contains(&waited), "{waited}");

        let steps = store.step_stats(1, None, 30).await.unwrap();
        assert_eq!(steps.iter().map(|s| s.step).collect::<Vec<_>>(), Step::ALL);
        assert_eq!(steps[0].p50_secs, waited);
        assert!(
            store
                .step_stats(1, Some(&b.repo), 30)
                .await
                .unwrap()
                .is_empty()
        );
        let repo_a = ExitQuery {
            repo: Some(&a.repo),
            ..query.clone()
        };
        assert_eq!(prs(&store, repo_a).await, [1]);
        let ejected = ExitQuery {
            outcomes: &["ejected"],
            ..query.clone()
        };
        assert_eq!(prs(&store, ejected).await, [3, 2]);
        let next_page = ExitQuery {
            before: Some(exits[1].id),
            limit: 1,
            ..query.clone()
        };
        assert_eq!(prs(&store, next_page).await, [2]);
        let other = ExitQuery {
            installation_id: 2,
            ..query.clone()
        };
        assert_eq!(prs(&store, other).await, Vec::<PrNumber>::new());

        let stats = store.exit_stats(1, 30, 0).await.unwrap();
        let counts: Vec<_> = stats
            .iter()
            .map(|s| (s.repo.as_deref(), s.merged, s.ejected, s.removed))
            .collect();
        assert_eq!(
            counts,
            [
                (Some("o/a"), 1, 0, 0),
                (Some("o/b"), 0, 2, 1),
                (None, 1, 2, 1)
            ]
        );
        let p50 = stats[0].p50_secs.unwrap();
        assert!((3599.0..3700.0).contains(&p50), "{p50}");
        assert_eq!(stats[1].p50_secs, None);
        // Nothing the 30 days before: only the row for every repo, empty.
        let before = store.exit_stats(1, 60, 30).await.unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!((before[0].repo.as_deref(), before[0].merged), (None, 0));

        assert_eq!(store.merged_by_day(1, None, 3).await.unwrap(), [0, 0, 1]);
        assert_eq!(
            store.merged_by_day(1, Some(&b.repo), 2).await.unwrap(),
            [0, 0]
        );
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
