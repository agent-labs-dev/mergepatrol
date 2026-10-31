//! The agent's reviews of queued PRs, one per PR head, and each workspace's review settings.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use mergepatrol_core::{
    Answer, Judged, PrNumber, Question, RepoName, ReviewSettings, ReviewStatus,
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
    pub status: ReviewStatus,
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
    /// `Asks` or `Approved`.
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
    /// Ends a running review with what the agent found. False if it is no longer running.
    async fn finish_review(&self, id: i64, done: &ReviewDone) -> Result<bool>;
    /// Puts a running review back to wait `after`, once more, for an error outside the PR.
    async fn retry_review(&self, id: i64, error: &str, after: Duration) -> Result<()>;
    /// The PR moved on before the review finished.
    async fn stale_review(&self, id: i64) -> Result<()>;
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
    /// Who the review asks, changed.
    async fn set_asked(&self, id: i64, asked: &[String]) -> Result<()>;
    /// A human answered the review's asks. False if it no longer asks.
    async fn answer_review(&self, id: i64, answers: &ReviewAnswers) -> Result<bool>;
    /// Marks the review as having told the PR, on GitHub, to answer it on the dashboard. False
    /// if it told it already, or it no longer asks.
    async fn nudge_review(&self, id: i64) -> Result<bool>;
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
    async fn answer_question(
        &self,
        id: i64,
        question: &str,
        answer: std::result::Result<String, String>,
    ) -> Result<()>;
    /// The workspace's reviews of the last `days`, newest first.
    async fn reviews_since(&self, installation_id: u64, days: u32) -> Result<Vec<ReviewRecord>>;
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
        r#"id, installation_id, repo, number, head_sha, status, error, attempts,
    summary, findings, asked, answers, answered_by, note, questions,
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
        status: ReviewStatus::parse(&status).with_context(|| format!("bad status {status:?}"))?,
        error: row.get("error"),
        attempts: row.get::<i32, _>("attempts") as u32,
        summary: row.get("summary"),
        findings: serde_json::from_value(row.get("findings"))?,
        asked: row.get("asked"),
        answers: serde_json::from_value(row.get("answers"))?,
        answered_by: row.get("answered_by"),
        note: row.get("note"),
        questions: serde_json::from_value(row.get("questions"))?,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

fn records(rows: &[PgRow]) -> Result<Vec<ReviewRecord>> {
    rows.iter().map(record).collect()
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
            "UPDATE agent_reviews SET status = 'running', claimed_at = now(), updated_at = now()
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

    async fn finish_review(&self, id: i64, done: &ReviewDone) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE agent_reviews SET status = $2, error = $3, summary = $4, findings = $5,
                 asked = $6, claimed_at = NULL, retry_at = NULL, updated_at = now()
             WHERE id = $1 AND status = 'running'",
        )
        .bind(id)
        .bind(done.status.as_str())
        .bind(&done.error)
        .bind(&done.summary)
        .bind(serde_json::to_value(&done.findings)?)
        .bind(&done.asked)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    async fn retry_review(&self, id: i64, error: &str, after: Duration) -> Result<()> {
        sqlx::query(
            "UPDATE agent_reviews SET status = 'pending', error = $2, attempts = attempts + 1,
                 claimed_at = NULL, retry_at = now() + make_interval(secs => $3),
                 updated_at = now()
             WHERE id = $1 AND status = 'running'",
        )
        .bind(id)
        .bind(error)
        .bind(after.as_secs_f64())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn stale_review(&self, id: i64) -> Result<()> {
        sqlx::query(
            "UPDATE agent_reviews SET status = 'stale', claimed_at = NULL, updated_at = now()
             WHERE id = $1",
        )
        .bind(id)
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

    async fn set_asked(&self, id: i64, asked: &[String]) -> Result<()> {
        sqlx::query("UPDATE agent_reviews SET asked = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(asked)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn answer_review(&self, id: i64, a: &ReviewAnswers) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE agent_reviews SET status = $2, answers = $3, answered_by = $4, note = $5,
                 updated_at = now()
             WHERE id = $1 AND status = 'asks'",
        )
        .bind(id)
        .bind(a.status.as_str())
        .bind(serde_json::to_value(&a.answers)?)
        .bind(&a.by)
        .bind(&a.note)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
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
        let row = sqlx::query(concat!("UPDATE agent_reviews SET claimed_at = now()
             WHERE id = (
                 SELECT id FROM agent_reviews
                 WHERE status = 'asks' AND jsonb_path_exists(questions, '$[*] ? (!exists(@.answer) && !exists(@.error))')
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
        question: &str,
        answer: std::result::Result<String, String>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT questions FROM agent_reviews WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
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
        Ok(())
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
                code: Vec::new(),
                related: Vec::new(),
            },
            asked,
            bar: Bar::From(90),
        }
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
        assert!(store.finish_review(r.id, &done).await.unwrap());
        assert!(
            !store.finish_review(r.id, &done).await.unwrap(),
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
            .answer_question(r.id, "q1", Ok("because".into()))
            .await
            .unwrap();
        let (_, next) = store.claim_question(stale).await.unwrap().unwrap();
        assert_eq!(next.id, "q2");
        store
            .answer_question(r.id, "q2", Err("no key".into()))
            .await
            .unwrap();
        assert_eq!(store.claim_question(stale).await.unwrap(), None);

        // A PR is told once per review where its answers go.
        assert!(store.nudge_review(r.id).await.unwrap());
        assert!(!store.nudge_review(r.id).await.unwrap(), "told once");

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
        store.stale_review(stale.id).await.unwrap();
        let again = store.start_review(1, &repo(), 9, "x").await.unwrap();
        assert_eq!((again.id, again.status), (stale.id, ReviewStatus::Pending));

        // Sent back: no approval to carry over a rebase.
        store
            .carry_review(1, &repo(), 7, "aaa", "rebased")
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
        store.claim_review(stale).await.unwrap().unwrap();
        store
            .retry_review(r.id, "the sandbox broke", Duration::from_secs(60))
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
    async fn an_approval_holds_for_the_heads_the_queue_makes(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let r = store.start_review(1, &repo(), 7, "aaa").await.unwrap();
        store.claim_review(Duration::from_secs(600)).await.unwrap();
        let done = ReviewDone {
            status: ReviewStatus::Approved,
            error: None,
            summary: "s".into(),
            findings: vec![judged("f1", false)],
            asked: Vec::new(),
        };
        store.finish_review(r.id, &done).await.unwrap();
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
