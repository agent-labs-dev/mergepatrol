//! The agent's reviews of queued PRs, one per PR head, and each workspace's review settings.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use mergepatrol_core::{
    Answer, Judged, Outcome, PrNumber, Question, RepoName, ReviewSettings, ReviewStatus,
    carry_back, dismiss_sent_back, newly_settled, settle_answers,
};
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::PgStore;

/// A review as stored.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewRecord {
    pub id: i64,
    pub installation_id: u64,
    pub repo: RepoName,
    pub number: PrNumber,
    pub head_sha: String,
    /// The approved head it goes on from: only what the queue's agent changed since needs
    /// reading. `None` reads the whole PR.
    pub since_head: Option<String>,
    pub status: ReviewStatus,
    /// Which taking of it this is: a worker's writes count only while it is the latest.
    pub claim: i64,
    /// Why the agent could not review: a human reviews the whole PR.
    pub error: Option<String>,
    pub attempts: u32,
    pub summary: String,
    pub findings: Vec<Judged>,
    pub asked: Vec<String>,
    /// By finding id.
    pub answers: BTreeMap<String, Answer>,
    pub answered_by: Option<String>,
    pub note: String,
    pub questions: Vec<Question>,
    /// How long it waited on a human, from asking to their answer. `None` until they answer.
    pub waited_secs: Option<u32>,
    /// RFC 3339.
    pub created_at: String,
    pub updated_at: String,
}

impl ReviewRecord {
    /// The findings the agent asks a human about.
    pub fn asks(&self) -> impl Iterator<Item = &Judged> {
        self.findings.iter().filter(|f| f.asked)
    }
}

/// What a finished run of the agent found.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewDone {
    /// `Asks`, `Changes` or `Approved`.
    pub status: ReviewStatus,
    pub error: Option<String>,
    pub summary: String,
    pub findings: Vec<Judged>,
    pub asked: Vec<String>,
}

/// A human's answers to a review's asks.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewAnswers {
    pub answers: BTreeMap<String, Answer>,
    pub by: String,
    pub note: String,
    /// `Approved` or `Changes`.
    pub status: ReviewStatus,
}

#[async_trait]
pub trait ReviewStore: Send + Sync {
    async fn review(&self, id: i64) -> Result<Option<ReviewRecord>>;
    /// The PR's newest review, of whatever head.
    async fn latest_review(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
    ) -> Result<Option<ReviewRecord>>;
    /// The newest review of each of these PRs that has one.
    async fn latest_reviews(
        &self,
        installation_id: u64,
        repo: &RepoName,
        prs: &[PrNumber],
    ) -> Result<Vec<ReviewRecord>>;
    /// The PR's reviews, newest first, up to `limit`.
    async fn reviews_of(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        limit: u32,
    ) -> Result<Vec<ReviewRecord>>;
    /// The review of the PR at `head`: the one there is, or a new one, pending. A stale one is
    /// pending again.
    async fn start_review(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        head: &str,
    ) -> Result<ReviewRecord>;
    /// Takes the next review to run: a pending one, or one whose run has gone `stale` since a
    /// worker took it, as a restart leaves it. It is running once taken.
    async fn claim_review(&self, stale: Duration) -> Result<Option<ReviewRecord>>;
    /// Ends a running review with what the agent found. False if it is no longer running, or
    /// the worker no longer holds `claim`.
    async fn finish_review(&self, id: i64, claim: i64, done: &ReviewDone) -> Result<bool>;
    /// Puts a running review back to wait `after`, once more, for an error outside the PR.
    async fn retry_review(&self, id: i64, claim: i64, error: &str, after: Duration) -> Result<()>;
    /// The PR moved on before the review finished.
    async fn stale_review(&self, id: i64, claim: i64) -> Result<()>;
    /// Lets go of every review and question a worker took, for the next to take them now, and
    /// ends those claims: only the worker that starts with the queue lock may run them.
    async fn release_reviews(&self) -> Result<u64>;
    /// The queue moved the PR from `before` to `after`, a head it made: a rebase, or a fix of
    /// its checks. An approval of `before` holds for `after`, which is not reviewed again.
    async fn carry_review(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        before: &str,
        after: &str,
    ) -> Result<()>;
    /// The queue's agent changed an approved PR from `before` to `after`, which waits for a
    /// review of that change alone. It stays stale until `start_review`, as a tick does once the
    /// PR's checks let it on. A review not yet approved passes on the approval it goes from.
    async fn review_queue_change(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        before: &str,
        after: &str,
    ) -> Result<()>;
    /// Who the review asks, changed.
    async fn set_asked(&self, id: i64, asked: &[String]) -> Result<()>;
    /// A human answered the review's asks. False if it no longer asks.
    async fn answer_review(&self, id: i64, answers: &ReviewAnswers) -> Result<bool>;
    /// Marks the review as having told the PR, on GitHub, to answer it on the dashboard. False
    /// if it told it already, or it no longer asks.
    async fn nudge_review(&self, id: i64) -> Result<bool>;
    /// Undoes `nudge_review`, as when posting the note failed, so a later reply tries again.
    async fn release_nudge(&self, id: i64) -> Result<()>;
    /// A human's question for the agent. Returns it, with its id.
    async fn add_question(
        &self,
        id: i64,
        finding: &str,
        question: &str,
        by: &str,
    ) -> Result<Question>;
    /// Takes the next review with a question waiting, as for `claim_review`.
    async fn claim_question(&self, stale: Duration) -> Result<Option<(ReviewRecord, Question)>>;
    /// The agent's answer to a question, or why it has none.
    /// False if the worker no longer holds `claim`.
    async fn answer_question(
        &self,
        id: i64,
        claim: i64,
        question: &str,
        answer: std::result::Result<String, String>,
    ) -> Result<bool>;
    /// The agent withdrew a finding it sent back to the author, on their reply, as `outcome`
    /// says. A review that held the PR for the author alone, and now holds it for nothing,
    /// approves it. Returns the review's status after, or `None` if the finding was not sent
    /// back.
    async fn withdraw_finding(
        &self,
        id: i64,
        finding: &str,
        outcome: Outcome,
    ) -> Result<Option<ReviewStatus>>;
    /// What became of these findings of the review, by id, as the next head's review says. A
    /// finding keeps the first outcome it was given.
    async fn settle_findings(&self, id: i64, outcomes: &[(String, Outcome)]) -> Result<()>;
    /// These findings of the review have a thread on the PR.
    async fn mark_threaded(&self, id: i64, findings: &[String]) -> Result<()>;
    /// The author resolved every thread of a review that held the PR for them alone: it
    /// approves, and what it still sent back was dismissed. False if it no longer holds for
    /// them alone.
    async fn approve_resolved(&self, id: i64) -> Result<bool>;
    /// The workspace's reviews of the last `days`, newest first.
    async fn reviews_since(&self, installation_id: u64, days: u32) -> Result<Vec<ReviewRecord>>;
    /// Every review of the workspace's PRs first reviewed in the last `days`, newest first: a
    /// PR's whole history, never the end of one.
    async fn reviews_of_prs_since(
        &self,
        installation_id: u64,
        days: u32,
    ) -> Result<Vec<ReviewRecord>>;
    /// `None`: the workspace never saved any, and has the defaults.
    async fn review_settings(&self, installation_id: u64) -> Result<Option<ReviewSettings>>;
    async fn set_review_settings(
        &self,
        installation_id: u64,
        settings: &ReviewSettings,
    ) -> Result<()>;
}

/// A review's columns, for `concat!`: sqlx takes only literal SQL.
macro_rules! columns {
    () => {
        r#"id, installation_id, repo, number, head_sha, since_head, status, claim, error, attempts,
    summary, findings, asked, answers, answered_by, note, questions,
    EXTRACT(EPOCH FROM answered_at - asked_at)::INT AS waited_secs,
    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS created_at,
    to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') AS updated_at"#
    };
}

fn record(row: &PgRow) -> Result<ReviewRecord> {
    let repo: String = row.get("repo");
    let status: String = row.get("status");
    Ok(ReviewRecord {
        id: row.get("id"),
        installation_id: row.get::<i64, _>("installation_id") as u64,
        repo: RepoName::parse(&repo).with_context(|| format!("bad repo {repo:?}"))?,
        number: row.get::<i64, _>("number") as u64,
        head_sha: row.get("head_sha"),
        since_head: row.get("since_head"),
        status: ReviewStatus::parse(&status).with_context(|| format!("bad status {status:?}"))?,
        claim: row.get("claim"),
        error: row.get("error"),
        attempts: row.get::<i32, _>("attempts") as u32,
        summary: row.get("summary"),
        findings: serde_json::from_value(row.get("findings"))?,
        asked: row.get("asked"),
        answers: serde_json::from_value(row.get("answers"))?,
        answered_by: row.get("answered_by"),
        note: row.get("note"),
        questions: serde_json::from_value(row.get("questions"))?,
        waited_secs: row
            .get::<Option<i32>, _>("waited_secs")
            .map(|s| s.max(0) as u32),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

fn records(rows: &[PgRow]) -> Result<Vec<ReviewRecord>> {
    rows.iter().map(record).collect()
}

impl PgStore {
    /// Changes a review's findings under its row's lock. `edit` gets them with the review's
    /// status and whether a human answered it, and returns the status to save with them, or
    /// `None` to save nothing.
    async fn edit_findings(
        &self,
        id: i64,
        edit: impl FnOnce(&mut Vec<Judged>, ReviewStatus, bool) -> Option<ReviewStatus> + Send,
    ) -> Result<Option<ReviewStatus>> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT findings, status, answered_by FROM agent_reviews WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let mut findings: Vec<Judged> = serde_json::from_value(row.get("findings"))?;
        let status: String = row.get("status");
        let status =
            ReviewStatus::parse(&status).with_context(|| format!("bad status {status:?}"))?;
        let answered = row.get::<Option<String>, _>("answered_by").is_some();
        let before = findings.clone();
        let Some(status) = edit(&mut findings, status, answered) else {
            return Ok(None);
        };
        sqlx::query(
            "UPDATE agent_reviews SET findings = $2, status = $3, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(serde_json::to_value(&findings)?)
        .bind(status.as_str())
        .execute(&mut *tx)
        .await?;
        carry_back_outcomes(&mut tx, id, &newly_settled(&before, &findings)).await?;
        tx.commit().await?;
        Ok(Some(status))
    }
}

/// What review `id` just settled, given to the same open findings of the PR's earlier reviews:
/// each time a finding was raised is settled with it. Locks from newer to older, as every caller
/// holds `id` first.
async fn carry_back_outcomes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: i64,
    settled: &[(String, Outcome)],
) -> Result<()> {
    if settled.is_empty() {
        return Ok(());
    }
    let rows = sqlx::query(
        "SELECT e.id, e.findings, e.answers FROM agent_reviews e, agent_reviews r
         WHERE r.id = $1 AND e.installation_id = r.installation_id AND e.repo = r.repo
           AND e.number = r.number AND e.id < r.id
         ORDER BY e.id DESC FOR UPDATE OF e",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await?;
    for row in rows {
        let mut findings: Vec<Judged> = serde_json::from_value(row.get("findings"))?;
        let answers: BTreeMap<String, Answer> = serde_json::from_value(row.get("answers"))?;
        if carry_back(&mut findings, &answers, settled) {
            sqlx::query("UPDATE agent_reviews SET findings = $2, updated_at = now() WHERE id = $1")
                .bind(row.get::<i64, _>("id"))
                .bind(serde_json::to_value(&findings)?)
                .execute(&mut **tx)
                .await?;
        }
    }
    Ok(())
}

#[async_trait]
impl ReviewStore for PgStore {
    async fn review(&self, id: i64) -> Result<Option<ReviewRecord>> {
        let row = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM agent_reviews WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(record).transpose()
    }

    async fn latest_review(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
    ) -> Result<Option<ReviewRecord>> {
        Ok(self
            .reviews_of(installation_id, repo, pr, 1)
            .await?
            .into_iter()
            .next())
    }

    async fn latest_reviews(
        &self,
        installation_id: u64,
        repo: &RepoName,
        prs: &[PrNumber],
    ) -> Result<Vec<ReviewRecord>> {
        let prs: Vec<i64> = prs.iter().map(|&n| n as i64).collect();
        let rows = sqlx::query(concat!(
            "SELECT DISTINCT ON (number) ",
            columns!(),
            " FROM agent_reviews
             WHERE installation_id = $1 AND repo = $2 AND number = ANY($3)
             ORDER BY number, id DESC"
        ))
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(&prs)
        .fetch_all(&self.pool)
        .await?;
        records(&rows)
    }

    async fn reviews_of(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        limit: u32,
    ) -> Result<Vec<ReviewRecord>> {
        let rows = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM agent_reviews
             WHERE installation_id = $1 AND repo = $2 AND number = $3
             ORDER BY id DESC LIMIT $4"
        ))
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(pr as i64)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        records(&rows)
    }

    async fn start_review(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        head: &str,
    ) -> Result<ReviewRecord> {
        // A stale review of the head waits for the agent again.
        sqlx::query(
            "INSERT INTO agent_reviews (installation_id, repo, number, head_sha)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (installation_id, repo, number, head_sha) DO UPDATE
             SET status = 'pending', claimed_at = NULL, updated_at = now()
             WHERE agent_reviews.status = 'stale'",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(pr as i64)
        .bind(head)
        .execute(&self.pool)
        .await?;
        let row = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM agent_reviews
             WHERE installation_id = $1 AND repo = $2 AND number = $3 AND head_sha = $4"
        ))
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(pr as i64)
        .bind(head)
        .fetch_one(&self.pool)
        .await?;
        record(&row)
    }

    async fn claim_review(&self, stale: Duration) -> Result<Option<ReviewRecord>> {
        let row = sqlx::query(concat!(
            "UPDATE agent_reviews SET status = 'running', claim = claim + 1, claimed_at = now(),
                 updated_at = now()
             WHERE id = (
                 SELECT id FROM agent_reviews
                 WHERE (status = 'pending' AND (retry_at IS NULL OR retry_at <= now()))
                    OR (status = 'running' AND claimed_at < now() - make_interval(secs => $1))
                 ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED
             )
             RETURNING ",
            columns!()
        ))
        .bind(stale.as_secs_f64())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(record).transpose()
    }

    async fn finish_review(&self, id: i64, claim: i64, done: &ReviewDone) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE agent_reviews SET status = $2, error = $3, summary = $4, findings = $5,
                 asked = $6, claimed_at = NULL, retry_at = NULL, updated_at = now(),
                 asked_at = CASE WHEN $2 = 'asks' THEN now() END
             WHERE id = $1 AND status = 'running' AND claim = $7",
        )
        .bind(id)
        .bind(done.status.as_str())
        .bind(&done.error)
        .bind(&done.summary)
        .bind(serde_json::to_value(&done.findings)?)
        .bind(&done.asked)
        .bind(claim)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    async fn retry_review(&self, id: i64, claim: i64, error: &str, after: Duration) -> Result<()> {
        sqlx::query(
            "UPDATE agent_reviews SET status = 'pending', error = $2, attempts = attempts + 1,
                 claimed_at = NULL, retry_at = now() + make_interval(secs => $3),
                 updated_at = now()
             WHERE id = $1 AND status = 'running' AND claim = $4",
        )
        .bind(id)
        .bind(error)
        .bind(after.as_secs_f64())
        .bind(claim)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn release_reviews(&self) -> Result<u64> {
        let released = sqlx::query(
            "UPDATE agent_reviews
             SET status = CASE status WHEN 'running' THEN 'pending' ELSE status END,
                 claim = claim + 1, claimed_at = NULL, updated_at = now()
             WHERE claimed_at IS NOT NULL",
        )
        .execute(&self.pool)
        .await?;
        Ok(released.rows_affected())
    }

    async fn stale_review(&self, id: i64, claim: i64) -> Result<()> {
        sqlx::query(
            "UPDATE agent_reviews SET status = 'stale', claimed_at = NULL,
                 updated_at = now()
             WHERE id = $1 AND claim = $2",
        )
        .bind(id)
        .bind(claim)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn carry_review(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        before: &str,
        after: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO agent_reviews (installation_id, repo, number, head_sha, status, summary,
                 findings, asked, answers, answered_by, note, questions)
             SELECT installation_id, repo, number, $5, status, summary, findings, asked, answers,
                 answered_by, note, questions
             FROM agent_reviews
             WHERE installation_id = $1 AND repo = $2 AND number = $3 AND head_sha = $4
               AND status = 'approved'
             ON CONFLICT DO NOTHING",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(pr as i64)
        .bind(before)
        .bind(after)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn review_queue_change(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        before: &str,
        after: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO agent_reviews
                 (installation_id, repo, number, head_sha, status, since_head)
             SELECT installation_id, repo, number, $5, 'stale',
                 CASE status WHEN 'approved' THEN head_sha ELSE since_head END
             FROM agent_reviews
             WHERE installation_id = $1 AND repo = $2 AND number = $3 AND head_sha = $4
               AND (status = 'approved' OR since_head IS NOT NULL)
             ON CONFLICT DO NOTHING",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(pr as i64)
        .bind(before)
        .bind(after)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn set_asked(&self, id: i64, asked: &[String]) -> Result<()> {
        sqlx::query("UPDATE agent_reviews SET asked = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(asked)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn answer_review(&self, id: i64, a: &ReviewAnswers) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT findings FROM agent_reviews WHERE id = $1 AND status = 'asks' FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let mut findings: Vec<Judged> = serde_json::from_value(row.get("findings"))?;
        let before = findings.clone();
        settle_answers(&mut findings, &a.answers);
        sqlx::query(
            "UPDATE agent_reviews SET status = $2, answers = $3, answered_by = $4, note = $5,
                 findings = $6, answered_at = now(), updated_at = now()
             WHERE id = $1",
        )
        .bind(id)
        .bind(a.status.as_str())
        .bind(serde_json::to_value(&a.answers)?)
        .bind(&a.by)
        .bind(&a.note)
        .bind(serde_json::to_value(&findings)?)
        .execute(&mut *tx)
        .await?;
        carry_back_outcomes(&mut tx, id, &newly_settled(&before, &findings)).await?;
        tx.commit().await?;
        Ok(true)
    }

    async fn nudge_review(&self, id: i64) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE agent_reviews SET nudged_at = now()
             WHERE id = $1 AND status = 'asks' AND nudged_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    async fn release_nudge(&self, id: i64) -> Result<()> {
        sqlx::query("UPDATE agent_reviews SET nudged_at = NULL WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn add_question(
        &self,
        id: i64,
        finding: &str,
        question: &str,
        by: &str,
    ) -> Result<Question> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT questions FROM agent_reviews WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let mut questions: Vec<Question> = serde_json::from_value(row.get("questions"))?;
        let q = Question {
            id: format!("q{}", questions.len() + 1),
            finding: finding.to_owned(),
            question: question.to_owned(),
            by: by.to_owned(),
            answer: None,
            error: None,
        };
        questions.push(q.clone());
        // A worker answering an earlier question keeps it: it takes this one once done.
        sqlx::query("UPDATE agent_reviews SET questions = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(serde_json::to_value(&questions)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(q)
    }

    async fn claim_question(&self, stale: Duration) -> Result<Option<(ReviewRecord, Question)>> {
        let row = sqlx::query(concat!("UPDATE agent_reviews SET claim = claim + 1, claimed_at = now()
             WHERE id = (
                 SELECT id FROM agent_reviews
                 WHERE status IN ('asks', 'changes') AND jsonb_path_exists(questions, '$[*] ? (!exists(@.answer) && !exists(@.error))')
                   AND (claimed_at IS NULL OR claimed_at < now() - make_interval(secs => $1))
                 ORDER BY updated_at LIMIT 1 FOR UPDATE SKIP LOCKED
             )
             RETURNING ", columns!()))
        .bind(stale.as_secs_f64())
        .fetch_optional(&self.pool)
        .await?;
        let Some(review) = row.as_ref().map(record).transpose()? else {
            return Ok(None);
        };
        let Some(q) = review.questions.iter().find(|q| q.pending()).cloned() else {
            return Ok(None);
        };
        Ok(Some((review, q)))
    }

    async fn answer_question(
        &self,
        id: i64,
        claim: i64,
        question: &str,
        answer: std::result::Result<String, String>,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT questions FROM agent_reviews WHERE id = $1 AND claim = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(claim)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let mut questions: Vec<Question> = serde_json::from_value(row.get("questions"))?;
        if let Some(q) = questions.iter_mut().find(|q| q.id == question) {
            match answer {
                Ok(a) => q.answer = Some(a),
                Err(e) => q.error = Some(e),
            }
        }
        sqlx::query(
            "UPDATE agent_reviews SET questions = $2, claimed_at = NULL, updated_at = now()
             WHERE id = $1",
        )
        .bind(id)
        .bind(serde_json::to_value(&questions)?)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    async fn withdraw_finding(
        &self,
        id: i64,
        finding: &str,
        outcome: Outcome,
    ) -> Result<Option<ReviewStatus>> {
        self.edit_findings(id, |findings, status, answered| {
            let f = findings
                .iter_mut()
                .find(|f| f.finding.id == finding && f.sent_back)?;
            f.sent_back = false;
            f.withdrawn = true;
            f.settle(outcome);
            // Only the agent's own hold lifts: a human's request for changes stands.
            Some(match status {
                ReviewStatus::Changes if !answered => {
                    mergepatrol_core::review_status(findings, false)
                }
                other => other,
            })
        })
        .await
    }

    async fn mark_threaded(&self, id: i64, ids: &[String]) -> Result<()> {
        self.edit_findings(id, |findings, status, _| {
            for f in findings.iter_mut().filter(|f| ids.contains(&f.finding.id)) {
                f.threaded = true;
            }
            Some(status)
        })
        .await?;
        Ok(())
    }

    async fn settle_findings(&self, id: i64, outcomes: &[(String, Outcome)]) -> Result<()> {
        self.edit_findings(id, |findings, status, _| {
            for (finding, outcome) in outcomes {
                if let Some(f) = findings.iter_mut().find(|f| &f.finding.id == finding) {
                    f.settle(*outcome);
                }
            }
            Some(status)
        })
        .await?;
        Ok(())
    }

    async fn approve_resolved(&self, id: i64) -> Result<bool> {
        let approved = self
            .edit_findings(id, |findings, status, answered| {
                (status == ReviewStatus::Changes && !answered).then(|| {
                    dismiss_sent_back(findings);
                    ReviewStatus::Approved
                })
            })
            .await?;
        Ok(approved.is_some())
    }

    async fn reviews_since(&self, installation_id: u64, days: u32) -> Result<Vec<ReviewRecord>> {
        let rows = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM agent_reviews
             WHERE installation_id = $1 AND created_at > now() - make_interval(days => $2)
             ORDER BY id DESC"
        ))
        .bind(installation_id as i64)
        .bind(days as i32)
        .fetch_all(&self.pool)
        .await?;
        records(&rows)
    }

    async fn reviews_of_prs_since(
        &self,
        installation_id: u64,
        days: u32,
    ) -> Result<Vec<ReviewRecord>> {
        let rows = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM agent_reviews
             WHERE installation_id = $1 AND (repo, number) IN (
                 SELECT repo, number FROM agent_reviews WHERE installation_id = $1
                 GROUP BY repo, number
                 HAVING min(created_at) > now() - make_interval(days => $2))
             ORDER BY id DESC"
        ))
        .bind(installation_id as i64)
        .bind(days as i32)
        .fetch_all(&self.pool)
        .await?;
        records(&rows)
    }

    async fn review_settings(&self, installation_id: u64) -> Result<Option<ReviewSettings>> {
        let row = sqlx::query("SELECT settings FROM review_settings WHERE installation_id = $1")
            .bind(installation_id as i64)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| Ok(serde_json::from_value(r.get("settings"))?))
            .transpose()
    }

    async fn set_review_settings(
        &self,
        installation_id: u64,
        settings: &ReviewSettings,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO review_settings (installation_id, settings) VALUES ($1, $2)
             ON CONFLICT (installation_id) DO UPDATE SET settings = $2, updated_at = now()",
        )
        .bind(installation_id as i64)
        .bind(serde_json::to_value(settings)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mergepatrol_core::{Bar, Finding, Kind, Lean};
    use sqlx::PgPool;

    fn repo() -> RepoName {
        RepoName::parse("o/r").unwrap()
    }

    fn asks() -> ReviewDone {
        ReviewDone {
            status: ReviewStatus::Asks,
            error: None,
            summary: "s".into(),
            findings: vec![judged("f1", true)],
            asked: vec!["mona".into()],
        }
    }

    fn judged(id: &str, asked: bool) -> Judged {
        Judged {
            finding: Finding {
                id: id.into(),
                kind: Kind::Security,
                title: "t".into(),
                file: Some("a.rs".into()),
                line: Some(3),
                confidence: 60,
                lean: Lean::Fine,
                finding: "f".into(),
                unsure: "u".into(),
                checked: vec!["c".into()],
                would_clear: "w".into(),
                suggestion: "s".into(),
                evidence: Some(mergepatrol_core::Evidence {
                    command: "cargo test".into(),
                    failed: true,
                    ..Default::default()
                }),
                needs_human: false,
                verified: None,
                settled: None,
                code: Vec::new(),
                related: Vec::new(),
                also_at: Vec::new(),
            },
            asked,
            sent_back: false,
            withdrawn: false,
            threaded: false,
            bar: Bar::From(90),
            outcome: None,
        }
    }

    /// A finished review of `head`, with `findings`.
    async fn finished(
        store: &PgStore,
        head: &str,
        status: ReviewStatus,
        findings: Vec<Judged>,
    ) -> i64 {
        let r = store.start_review(1, &repo(), 8, head).await.unwrap();
        let claim = store
            .claim_review(Duration::from_secs(600))
            .await
            .unwrap()
            .unwrap()
            .claim;
        let done = ReviewDone {
            status,
            error: None,
            summary: "s".into(),
            findings,
            asked: Vec::new(),
        };
        assert!(store.finish_review(r.id, claim, &done).await.unwrap());
        r.id
    }

    fn outcomes(r: &ReviewRecord) -> Vec<Option<Outcome>> {
        r.findings.iter().map(|f| f.outcome).collect()
    }

    #[sqlx::test(migrations = false)]
    async fn a_window_takes_the_prs_first_reviewed_in_it_with_all_their_reviews(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        // #1 was first reviewed 40 days ago, and a rebase carried its approval today.
        let old = store.start_review(1, &repo(), 1, "a").await.unwrap();
        let carried = store.start_review(1, &repo(), 1, "b").await.unwrap();
        sqlx::query(
            "UPDATE agent_reviews SET created_at = now() - interval '40 days' WHERE id = $1",
        )
        .bind(old.id)
        .execute(&pool)
        .await
        .unwrap();
        // #2 was first reviewed 10 days ago, and again today.
        let first = store.start_review(1, &repo(), 2, "c").await.unwrap();
        let again = store.start_review(1, &repo(), 2, "d").await.unwrap();
        sqlx::query(
            "UPDATE agent_reviews SET created_at = now() - interval '10 days' WHERE id = $1",
        )
        .bind(first.id)
        .execute(&pool)
        .await
        .unwrap();
        // Another workspace's is not this one's.
        store.start_review(2, &repo(), 3, "e").await.unwrap();

        let ids = |rs: Vec<ReviewRecord>| rs.iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(
            ids(store.reviews_of_prs_since(1, 30).await.unwrap()),
            [again.id, first.id],
            "#2's whole history, newest first, and none of #1's"
        );
        assert_eq!(
            ids(store.reviews_of_prs_since(1, 60).await.unwrap()),
            [again.id, first.id, carried.id, old.id]
        );
    }

    #[sqlx::test(migrations = false)]
    async fn each_change_keeps_what_became_of_it_and_how_long_a_human_took(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let change = |id: &str, asked: bool| Judged {
            finding: Finding {
                lean: Lean::Change,
                ..judged(id, asked).finding
            },
            sent_back: !asked,
            ..judged(id, asked)
        };
        let id = finished(
            &store,
            "aaa",
            ReviewStatus::Asks,
            vec![change("f1", true), change("f2", true)],
        )
        .await;
        assert_eq!(store.review(id).await.unwrap().unwrap().waited_secs, None);
        let answers = ReviewAnswers {
            answers: BTreeMap::from([
                ("f1".to_owned(), Answer::Fine),
                (
                    "f2".to_owned(),
                    Answer::Change {
                        note: String::new(),
                    },
                ),
            ]),
            by: "mona".into(),
            note: String::new(),
            status: ReviewStatus::Changes,
        };
        assert!(store.answer_review(id, &answers).await.unwrap());
        let r = store.review(id).await.unwrap().unwrap();
        assert_eq!(outcomes(&r), [Some(Outcome::Dismissed), None]);
        assert!(r.waited_secs.is_some(), "a human answered");

        // The next head's review settles the rest, once.
        let fixed = [("f2".to_owned(), Outcome::Fixed)];
        store.settle_findings(id, &fixed).await.unwrap();
        let wrong = [("f2".to_owned(), Outcome::Wrong)];
        store.settle_findings(id, &wrong).await.unwrap();
        let r = store.review(id).await.unwrap().unwrap();
        assert_eq!(
            outcomes(&r),
            [Some(Outcome::Dismissed), Some(Outcome::Fixed)]
        );

        // Withdrawn on the author's reply, and the rest resolved with no push.
        let id = finished(
            &store,
            "bbb",
            ReviewStatus::Changes,
            vec![change("f1", false), change("f2", false)],
        )
        .await;
        store
            .withdraw_finding(id, "f1", Outcome::Intended)
            .await
            .unwrap();
        store.mark_threaded(id, &["f2".to_owned()]).await.unwrap();
        assert!(store.approve_resolved(id).await.unwrap());
        assert!(!store.approve_resolved(id).await.unwrap(), "approved once");
        let r = store.review(id).await.unwrap().unwrap();
        let threaded: Vec<bool> = r.findings.iter().map(|f| f.threaded).collect();
        assert_eq!(threaded, [false, true]);
        assert_eq!(
            outcomes(&r),
            [Some(Outcome::Intended), Some(Outcome::Dismissed)]
        );
        assert_eq!(r.waited_secs, None, "no human was asked");
    }

    #[sqlx::test(migrations = false)]
    async fn a_withdrawn_finding_lets_the_pr_on_once_nothing_is_sent_back(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let r = store.start_review(1, &repo(), 8, "bbb").await.unwrap();
        let claim = store
            .claim_review(Duration::from_secs(600))
            .await
            .unwrap()
            .unwrap()
            .claim;
        let back = |id: &str| Judged {
            sent_back: true,
            ..judged(id, false)
        };
        let done = ReviewDone {
            status: ReviewStatus::Changes,
            error: None,
            summary: "s".into(),
            findings: vec![back("f1"), back("f2"), judged("f3", false)],
            asked: Vec::new(),
        };
        assert!(store.finish_review(r.id, claim, &done).await.unwrap());
        // The author's reply to a finding sent back is taken like a human's question.
        store
            .add_question(r.id, "f1", "the caller holds the lock", "bot")
            .await
            .unwrap();
        let (taken, q) = store
            .claim_question(Duration::from_secs(600))
            .await
            .unwrap()
            .unwrap();
        assert_eq!((taken.id, q.id.as_str()), (r.id, "q1"));
        // Not sent back: nothing to withdraw.
        assert_eq!(
            store
                .withdraw_finding(r.id, "f3", Outcome::Wrong)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .withdraw_finding(r.id, "f1", Outcome::Wrong)
                .await
                .unwrap(),
            Some(ReviewStatus::Changes),
            "f2 still holds it"
        );
        assert_eq!(
            store
                .withdraw_finding(r.id, "f2", Outcome::Wrong)
                .await
                .unwrap(),
            Some(ReviewStatus::Approved)
        );
        let r = store.review(r.id).await.unwrap().unwrap();
        assert_eq!(r.status, ReviewStatus::Approved);
        assert!(r.findings[..2].iter().all(|f| f.withdrawn && !f.sent_back));
        assert_eq!(
            outcomes(&r),
            [Some(Outcome::Wrong), Some(Outcome::Wrong), None]
        );
    }

    #[sqlx::test(migrations = false)]
    async fn a_review_runs_once_per_head_and_waits_for_its_answers(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let r = store.start_review(1, &repo(), 7, "aaa").await.unwrap();
        assert_eq!(r.status, ReviewStatus::Pending);
        // The same head is the same review.
        assert_eq!(
            store.start_review(1, &repo(), 7, "aaa").await.unwrap().id,
            r.id
        );

        let stale = Duration::from_secs(600);
        let claimed = store.claim_review(stale).await.unwrap().unwrap();
        assert_eq!((claimed.id, claimed.status), (r.id, ReviewStatus::Running));
        assert_eq!(store.claim_review(stale).await.unwrap(), None, "taken once");

        let done = ReviewDone {
            status: ReviewStatus::Asks,
            error: None,
            summary: "s".into(),
            findings: vec![judged("f1", true), judged("f2", false)],
            asked: vec!["mona".into()],
        };
        assert!(
            store
                .finish_review(r.id, claimed.claim, &done)
                .await
                .unwrap()
        );
        assert!(
            !store
                .finish_review(r.id, claimed.claim, &done)
                .await
                .unwrap(),
            "not running"
        );
        let r = store.latest_review(1, &repo(), 7).await.unwrap().unwrap();
        assert_eq!(r.status, ReviewStatus::Asks);
        assert_eq!(r.asks().count(), 1);
        assert_eq!(r.findings, done.findings);

        let q = store
            .add_question(r.id, "f1", "why?", "mona")
            .await
            .unwrap();
        assert_eq!(q.id, "q1");
        let (taken, tq) = store.claim_question(stale).await.unwrap().unwrap();
        assert_eq!((taken.id, tq.id.as_str()), (r.id, "q1"));
        // Asked while the worker answers q1: no second worker takes q1 again.
        store
            .add_question(r.id, "f1", "and?", "mona")
            .await
            .unwrap();
        assert_eq!(store.claim_question(stale).await.unwrap(), None);
        store
            .answer_question(r.id, taken.claim, "q1", Ok("because".into()))
            .await
            .unwrap();
        let (again, next) = store.claim_question(stale).await.unwrap().unwrap();
        assert_eq!(next.id, "q2");
        store
            .answer_question(r.id, again.claim, "q2", Err("no key".into()))
            .await
            .unwrap();
        assert_eq!(store.claim_question(stale).await.unwrap(), None);

        // A PR is told once per review where its answers go.
        assert!(store.nudge_review(r.id).await.unwrap());
        assert!(!store.nudge_review(r.id).await.unwrap(), "told once");
        // A note that failed to post is tried again.
        store.release_nudge(r.id).await.unwrap();
        assert!(store.nudge_review(r.id).await.unwrap());

        let answers = ReviewAnswers {
            answers: BTreeMap::from([(
                "f1".to_owned(),
                Answer::Change {
                    note: "fix it".into(),
                },
            )]),
            by: "mona".into(),
            note: String::new(),
            status: ReviewStatus::Changes,
        };
        assert!(store.answer_review(r.id, &answers).await.unwrap());
        assert!(
            !store.answer_review(r.id, &answers).await.unwrap(),
            "answered once"
        );
        let r = store.review(r.id).await.unwrap().unwrap();
        assert_eq!(r.status, ReviewStatus::Changes);
        assert_eq!(r.answers, answers.answers);
        assert_eq!(r.questions[0].answer.as_deref(), Some("because"));

        // A review gone stale, as its worker read an older head, is started again.
        let stale = store.start_review(1, &repo(), 9, "x").await.unwrap();
        store.stale_review(stale.id, stale.claim).await.unwrap();
        let again = store.start_review(1, &repo(), 9, "x").await.unwrap();
        assert_eq!((again.id, again.status), (stale.id, ReviewStatus::Pending));

        // Sent back: no approval to carry over a rebase.
        store
            .carry_review(1, &repo(), 7, "aaa", "rebased")
            .await
            .unwrap();
        assert_eq!(store.reviews_of(1, &repo(), 7, 10).await.unwrap().len(), 1);

        // Nor anything for the queue's agent to go on from.
        store
            .review_queue_change(1, &repo(), 7, "aaa", "fixed")
            .await
            .unwrap();
        assert_eq!(store.reviews_of(1, &repo(), 7, 10).await.unwrap().len(), 1);

        // A push: a new head, a new review, which the latest one is.
        let next = store.start_review(1, &repo(), 7, "bbb").await.unwrap();
        assert_ne!(next.id, r.id);
        let latest = store.latest_reviews(1, &repo(), &[7]).await.unwrap();
        let ids: Vec<i64> = latest.iter().map(|r| r.id).collect();
        assert_eq!(ids, [next.id], "#7's newest, and not #9's");
        assert_eq!(store.reviews_of(1, &repo(), 7, 10).await.unwrap().len(), 2);
        assert_eq!(
            store.reviews_since(1, 30).await.unwrap().len(),
            3,
            "#7's two, #9's one"
        );
    }

    #[sqlx::test(migrations = false)]
    async fn a_failed_run_waits_before_it_is_tried_again(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let r = store.start_review(1, &repo(), 7, "aaa").await.unwrap();
        let stale = Duration::from_secs(600);
        let claim = store.claim_review(stale).await.unwrap().unwrap().claim;
        store
            .retry_review(r.id, claim, "the sandbox broke", Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(store.claim_review(stale).await.unwrap(), None, "not yet");
        let r = store.review(r.id).await.unwrap().unwrap();
        assert_eq!((r.status, r.attempts), (ReviewStatus::Pending, 1));
        // A run a restart cut short is taken again once stale.
        let other = store.start_review(1, &repo(), 8, "ccc").await.unwrap();
        store.claim_review(stale).await.unwrap().unwrap();
        assert_eq!(
            store
                .claim_review(Duration::ZERO)
                .await
                .unwrap()
                .map(|r| r.id),
            Some(other.id)
        );
    }

    #[sqlx::test(migrations = false)]
    async fn the_next_worker_takes_what_a_restart_cut_short_at_once(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let stale = Duration::from_secs(600);
        let cut = store.start_review(1, &repo(), 7, "aaa").await.unwrap();
        store.claim_review(stale).await.unwrap().unwrap();
        let asked = store.start_review(1, &repo(), 8, "bbb").await.unwrap();
        let claim = store.claim_review(stale).await.unwrap().unwrap().claim;
        let done = asks();
        store.finish_review(asked.id, claim, &done).await.unwrap();
        store
            .add_question(asked.id, "f1", "why?", "mona")
            .await
            .unwrap();
        store.claim_question(stale).await.unwrap().unwrap();
        assert_eq!(store.claim_review(stale).await.unwrap(), None, "taken");
        assert_eq!(store.claim_question(stale).await.unwrap(), None, "taken");

        assert_eq!(store.release_reviews().await.unwrap(), 2);

        let again = store.claim_review(stale).await.unwrap().unwrap();
        assert_eq!(again.id, cut.id);
        let (r, _) = store.claim_question(stale).await.unwrap().unwrap();
        assert_eq!((r.id, r.status), (asked.id, ReviewStatus::Asks));
    }

    #[sqlx::test(migrations = false)]
    async fn a_worker_that_lost_the_queue_lock_changes_nothing(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let stale = Duration::from_secs(600);
        let r = store.start_review(1, &repo(), 7, "aaa").await.unwrap();
        let old = store.claim_review(stale).await.unwrap().unwrap().claim;
        let asked = store.start_review(1, &repo(), 8, "bbb").await.unwrap();
        let claim = store.claim_review(stale).await.unwrap().unwrap().claim;
        let done = asks();
        store.finish_review(asked.id, claim, &done).await.unwrap();
        let q = store
            .add_question(asked.id, "f1", "why?", "mona")
            .await
            .unwrap();
        let (taken, _) = store.claim_question(stale).await.unwrap().unwrap();

        // The next instance takes the lock and both runs, while the old worker runs on.
        store.release_reviews().await.unwrap();
        let new = store.claim_review(stale).await.unwrap().unwrap().claim;
        let (retaken, _) = store.claim_question(stale).await.unwrap().unwrap();

        assert!(!store.finish_review(r.id, old, &done).await.unwrap());
        let later = Duration::from_secs(60);
        store.retry_review(r.id, old, "late", later).await.unwrap();
        store.stale_review(r.id, old).await.unwrap();
        let held = store.review(r.id).await.unwrap().unwrap();
        assert_eq!((held.status, held.attempts), (ReviewStatus::Running, 0));
        let late = Ok("late".to_owned());
        let answered = store.answer_question(asked.id, taken.claim, &q.id, late);
        assert!(!answered.await.unwrap());
        let open = store.review(asked.id).await.unwrap().unwrap();
        assert!(open.questions[0].pending());

        assert!(store.finish_review(r.id, new, &done).await.unwrap());
        let answer = Ok("because".to_owned());
        let answered = store.answer_question(asked.id, retaken.claim, &q.id, answer);
        assert!(answered.await.unwrap());
    }

    #[sqlx::test(migrations = false)]
    async fn the_queues_agent_changing_an_approved_pr_starts_a_review_of_that_change(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let r = store.start_review(1, &repo(), 7, "aaa").await.unwrap();
        let claim = store
            .claim_review(Duration::from_secs(600))
            .await
            .unwrap()
            .unwrap()
            .claim;
        let done = ReviewDone {
            status: ReviewStatus::Approved,
            error: None,
            summary: "s".into(),
            findings: vec![judged("f1", false)],
            asked: Vec::new(),
        };
        store.finish_review(r.id, claim, &done).await.unwrap();
        store
            .review_queue_change(1, &repo(), 7, "aaa", "fixed")
            .await
            .unwrap();
        let next = store.latest_review(1, &repo(), 7).await.unwrap().unwrap();
        assert_eq!(next.status, ReviewStatus::Stale, "not before its checks");
        assert_eq!(next.since_head.as_deref(), Some("aaa"));
        assert_eq!(store.claim_review(Duration::ZERO).await.unwrap(), None);
        assert!(next.findings.is_empty());

        // A second fix before that review ends goes on from the same approval.
        store
            .review_queue_change(1, &repo(), 7, "fixed", "fixed again")
            .await
            .unwrap();
        let last = store.latest_review(1, &repo(), 7).await.unwrap().unwrap();
        assert_eq!(last.head_sha, "fixed again");
        assert_eq!(last.since_head.as_deref(), Some("aaa"));

        // Once that review approves, the next fix goes on from it.
        let started = store
            .start_review(1, &repo(), 7, "fixed again")
            .await
            .unwrap();
        assert_eq!(started.since_head.as_deref(), Some("aaa"), "it keeps that");
        assert_eq!(started.status, ReviewStatus::Pending);
        let claim = store
            .claim_review(Duration::from_secs(600))
            .await
            .unwrap()
            .unwrap()
            .claim;
        store.finish_review(last.id, claim, &done).await.unwrap();
        store
            .review_queue_change(1, &repo(), 7, "fixed again", "fixed once more")
            .await
            .unwrap();
        let next = store.latest_review(1, &repo(), 7).await.unwrap().unwrap();
        assert_eq!(next.since_head.as_deref(), Some("fixed again"));
    }

    #[sqlx::test(migrations = false)]
    async fn an_approval_holds_for_the_heads_the_queue_makes(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let r = store.start_review(1, &repo(), 7, "aaa").await.unwrap();
        let claim = store
            .claim_review(Duration::from_secs(600))
            .await
            .unwrap()
            .unwrap()
            .claim;
        let done = ReviewDone {
            status: ReviewStatus::Approved,
            error: None,
            summary: "s".into(),
            findings: vec![judged("f1", false)],
            asked: Vec::new(),
        };
        store.finish_review(r.id, claim, &done).await.unwrap();
        store
            .carry_review(1, &repo(), 7, "aaa", "rebased")
            .await
            .unwrap();
        let carried = store.latest_review(1, &repo(), 7).await.unwrap().unwrap();
        assert_eq!(carried.head_sha, "rebased");
        assert_eq!(carried.status, ReviewStatus::Approved);
        assert_eq!(carried.findings, done.findings);
        // Nothing waits for the agent.
        assert_eq!(store.claim_review(Duration::ZERO).await.unwrap(), None);
    }

    #[sqlx::test(migrations = false)]
    async fn settings_default_until_saved(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        assert_eq!(store.review_settings(1).await.unwrap(), None);
        let mut s = ReviewSettings {
            enabled: true,
            ..Default::default()
        };
        s.bars.insert(Kind::Other, Bar::From(70));
        store.set_review_settings(1, &s).await.unwrap();
        assert_eq!(store.review_settings(1).await.unwrap(), Some(s));
    }
}
