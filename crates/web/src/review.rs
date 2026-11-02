//! The agent's review of a queued PR on the dashboard: what it asks a human about, the human's
//! answers and questions, and each workspace's review settings with the agent's track record.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use mergepatrol_core::{
    Answer, Bar, CodeLine, Judged, Kind, Lean, LineKind, PrNumber, Question, Related, RepoName,
    ReviewOverrides, ReviewSettings, ReviewStatus,
};
use mergepatrol_github::setup::{self, LineComment, Verdict};
use mergepatrol_store::{ReviewAnswers, ReviewRecord};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::api::{approval, installation, repo_name};
use crate::auth::Caller;
use crate::{ApiError, AppState};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum KindView {
    Security,
    Migrations,
    Dependencies,
    PublicApi,
    Other,
}

impl From<Kind> for KindView {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Security => KindView::Security,
            Kind::Migrations => KindView::Migrations,
            Kind::Dependencies => KindView::Dependencies,
            Kind::PublicApi => KindView::PublicApi,
            Kind::Other => KindView::Other,
        }
    }
}

impl From<KindView> for Kind {
    fn from(k: KindView) -> Self {
        match k {
            KindView::Security => Kind::Security,
            KindView::Migrations => Kind::Migrations,
            KindView::Dependencies => Kind::Dependencies,
            KindView::PublicApi => Kind::PublicApi,
            KindView::Other => Kind::Other,
        }
    }
}

/// How sure the agent must be to clear a kind of finding alone: a percent from 50 to 100, or
/// `always_ask`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BarView {
    pub kind: KindView,
    #[ts(type = "number | \"always_ask\"")]
    pub bar: Bar,
}

fn bar_views(bars: &BTreeMap<Kind, Bar>) -> Vec<BarView> {
    bars.iter()
        .map(|(k, b)| BarView {
            kind: (*k).into(),
            bar: *b,
        })
        .collect()
}

fn bars_of(views: &[BarView]) -> BTreeMap<Kind, Bar> {
    views.iter().map(|v| (v.kind.into(), v.bar)).collect()
}

/// What a repo's config file sets of the review, over the workspace's settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RepoReviewView {
    /// `None` follows the workspace.
    pub enabled: Option<bool>,
    /// The kinds whose bar the repo sets. The others follow the workspace.
    pub bars: Vec<BarView>,
    /// Whether its gate waits for every review thread to be resolved. Unset follows the
    /// workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub require_resolved: Option<bool>,
}

impl From<ReviewOverrides> for RepoReviewView {
    fn from(o: ReviewOverrides) -> Self {
        RepoReviewView {
            enabled: o.enabled,
            bars: bar_views(&o.bars),
            require_resolved: o.require_resolved,
        }
    }
}

impl From<RepoReviewView> for ReviewOverrides {
    fn from(v: RepoReviewView) -> Self {
        ReviewOverrides {
            enabled: v.enabled,
            bars: bars_of(&v.bars),
            require_resolved: v.require_resolved,
        }
    }
}

/// The workspace's review settings, the defaults, and how the agent's confidence held up.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct ReviewSettingsView {
    /// Whether the repos' queues review, unless a repo's config file says otherwise.
    pub enabled: bool,
    /// Every kind's bar.
    pub bars: Vec<BarView>,
    /// Whether the gate waits for every review thread on a PR to be resolved.
    pub require_resolved: bool,
    pub defaults: Vec<BarView>,
    /// Each kind's findings over `days`, by confidence.
    pub history: Vec<KindHistoryView>,
    pub days: u32,
}

/// One kind's findings, in bands of 5% of confidence from 50%.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct KindHistoryView {
    pub kind: KindView,
    pub bands: Vec<BandView>,
}

#[derive(Default, Serialize, TS)]
#[ts(export)]
pub struct BandView {
    /// The band's lowest confidence: 50, 55, … 95. 95 holds 100 too.
    pub from: u32,
    pub findings: u32,
    /// Of them, the ones the agent leaned to change: at or over the bar, they go back to the
    /// author.
    pub changes: u32,
    /// Of them, the ones a human answered.
    pub answered: u32,
    /// Of those, the ones the human decided otherwise than the agent leaned.
    pub overruled: u32,
}

#[derive(Deserialize, TS)]
#[ts(export)]
pub struct SetReviewSettings {
    pub enabled: bool,
    pub bars: Vec<BarView>,
    /// Unset keeps what is saved.
    #[serde(default)]
    #[ts(optional)]
    pub require_resolved: Option<bool>,
}

/// How far back the track record goes.
const HISTORY_DAYS: u32 = 90;

pub(crate) async fn review_settings(
    State(app): State<AppState>,
    caller: Caller,
    Path(id): Path<u64>,
) -> Result<Json<ReviewSettingsView>, ApiError> {
    let id = caller.workspace(id)?;
    settings_view(&app, id).await
}

async fn settings_view(app: &AppState, id: u64) -> Result<Json<ReviewSettingsView>, ApiError> {
    let settings = app.store.review_settings(id).await?.unwrap_or_default();
    let full = settings.with(&ReviewOverrides::default());
    let reviews = app.store.reviews_since(id, HISTORY_DAYS).await?;
    Ok(Json(ReviewSettingsView {
        enabled: full.enabled,
        bars: bar_views(&full.bars),
        require_resolved: full.require_resolved,
        defaults: bar_views(&ReviewSettings::default().bars),
        history: history(&reviews),
        days: HISTORY_DAYS,
    }))
}

/// Each kind's findings by confidence, and how often a human overruled the agent.
fn history(reviews: &[ReviewRecord]) -> Vec<KindHistoryView> {
    Kind::ALL
        .into_iter()
        .map(|kind| {
            let mut bands: Vec<BandView> = (0..10)
                .map(|i| BandView {
                    from: 50 + 5 * i,
                    ..Default::default()
                })
                .collect();
            for r in reviews {
                for j in r.findings.iter().filter(|j| j.finding.kind == kind) {
                    let i = (j.finding.confidence.clamp(50, 99) as usize - 50) / 5;
                    let band = &mut bands[i];
                    band.findings += 1;
                    if j.finding.lean == Lean::Change {
                        band.changes += 1;
                    }
                    let Some(answer) = r.answers.get(&j.finding.id) else {
                        continue;
                    };
                    band.answered += 1;
                    let agreed = matches!(
                        (j.finding.lean, answer),
                        (Lean::Fine, Answer::Fine) | (Lean::Change, Answer::Change { .. })
                    );
                    if !agreed {
                        band.overruled += 1;
                    }
                }
            }
            KindHistoryView {
                kind: kind.into(),
                bands,
            }
        })
        .collect()
}

pub(crate) async fn set_review_settings(
    State(app): State<AppState>,
    caller: Caller,
    Path(id): Path<u64>,
    Json(body): Json<SetReviewSettings>,
) -> Result<Json<ReviewSettingsView>, ApiError> {
    let id = caller.workspace(id)?;
    let saved = app.store.review_settings(id).await?.unwrap_or_default();
    let mut settings = ReviewSettings {
        enabled: body.enabled,
        require_resolved: body.require_resolved.unwrap_or(saved.require_resolved),
        ..ReviewSettings::default()
    };
    settings.bars.extend(bars_of(&body.bars));
    app.store.set_review_settings(id, &settings).await?;
    tracing::info!(
        installation = id,
        enabled = settings.enabled,
        "set the review settings"
    );
    settings_view(&app, id).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentReviewStatusView {
    Pending,
    Running,
    Asks,
    Approved,
    Changes,
    Stale,
}

impl From<ReviewStatus> for AgentReviewStatusView {
    fn from(s: ReviewStatus) -> Self {
        match s {
            ReviewStatus::Pending => Self::Pending,
            ReviewStatus::Running => Self::Running,
            ReviewStatus::Asks => Self::Asks,
            ReviewStatus::Approved => Self::Approved,
            ReviewStatus::Changes => Self::Changes,
            ReviewStatus::Stale => Self::Stale,
        }
    }
}

/// The agent's review of a PR's head.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct AgentReviewView {
    pub status: AgentReviewStatusView,
    /// The head it reviewed. Answers name it, so they never land on a newer one.
    pub head_sha: String,
    /// Why the agent could not review: a human reviews the whole PR instead.
    pub error: Option<String>,
    pub summary: String,
    /// What the agent asks about first, then what it sent back to the author, then the rest.
    pub findings: Vec<FindingView>,
    /// Who the agent asks, by login. Empty: anyone who may approve the PR.
    pub asked: Vec<String>,
    pub answers: Vec<AnswerView>,
    pub answered_by: Option<String>,
    pub note: String,
    pub questions: Vec<QuestionView>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct FindingView {
    pub id: String,
    pub kind: KindView,
    pub title: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    /// In percent.
    pub confidence: u32,
    pub lean: LeanView,
    pub finding: String,
    pub unsure: String,
    pub checked: Vec<String>,
    pub would_clear: String,
    pub suggestion: String,
    pub code: Vec<CodeLineView>,
    pub related: Vec<RelatedView>,
    /// The agent asks a human about it.
    pub asked: bool,
    /// Sent back to the PR's author: the PR waits for a push that fixes it.
    pub sent_back: bool,
    /// The agent withdrew it after the author's reply.
    pub withdrawn: bool,
    /// The change needs a call the author cannot make from the PR.
    pub needs_human: bool,
    /// It has a review thread on the PR, which its author resolves.
    pub threaded: bool,
    #[ts(type = "number | \"always_ask\"")]
    pub bar: Bar,
}

#[derive(Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum LeanView {
    Fine,
    Change,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct CodeLineView {
    pub number: Option<u32>,
    /// `context`, `added` or `removed`.
    pub kind: String,
    pub text: String,
    pub hot: bool,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct RelatedView {
    pub file: String,
    pub line: Option<u32>,
    pub why: String,
    pub code: Vec<CodeLineView>,
}

#[derive(Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AnswerView {
    /// The finding's id.
    pub finding: String,
    pub call: LeanView,
    /// What to change. Required with `change`.
    #[serde(default)]
    pub note: String,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct QuestionView {
    pub id: String,
    pub finding: String,
    pub question: String,
    pub by: String,
    pub answer: Option<String>,
    pub error: Option<String>,
}

fn code_view(lines: Vec<CodeLine>) -> Vec<CodeLineView> {
    lines
        .into_iter()
        .map(|l| CodeLineView {
            number: l.number,
            kind: match l.kind {
                LineKind::Context => "context",
                LineKind::Added => "added",
                LineKind::Removed => "removed",
            }
            .into(),
            text: l.text,
            hot: l.hot,
        })
        .collect()
}

impl From<Judged> for FindingView {
    fn from(j: Judged) -> Self {
        let f = j.finding;
        FindingView {
            id: f.id,
            kind: f.kind.into(),
            title: f.title,
            file: f.file,
            line: f.line,
            confidence: f.confidence as u32,
            lean: match f.lean {
                Lean::Fine => LeanView::Fine,
                Lean::Change => LeanView::Change,
            },
            finding: f.finding,
            unsure: f.unsure,
            checked: f.checked,
            would_clear: f.would_clear,
            suggestion: f.suggestion,
            needs_human: f.needs_human,
            code: code_view(f.code),
            related: f
                .related
                .into_iter()
                .map(|r: Related| RelatedView {
                    file: r.file,
                    line: r.line,
                    why: r.why,
                    code: code_view(r.code),
                })
                .collect(),
            asked: j.asked,
            sent_back: j.sent_back,
            withdrawn: j.withdrawn,
            threaded: j.threaded,
            bar: j.bar,
        }
    }
}

impl From<Question> for QuestionView {
    fn from(q: Question) -> Self {
        QuestionView {
            id: q.id,
            finding: q.finding,
            question: q.question,
            by: q.by,
            answer: q.answer,
            error: q.error,
        }
    }
}

impl From<ReviewRecord> for AgentReviewView {
    fn from(r: ReviewRecord) -> Self {
        let mut findings = r.findings;
        // What it asks first, then what it sent back, then the rest, each as it reported them.
        findings.sort_by_key(|f| (!f.asked, !f.sent_back));
        AgentReviewView {
            status: r.status.into(),
            head_sha: r.head_sha,
            error: r.error,
            summary: r.summary,
            findings: findings.into_iter().map(FindingView::from).collect(),
            asked: r.asked,
            answers: r
                .answers
                .into_iter()
                .map(|(finding, a)| match a {
                    Answer::Fine => AnswerView {
                        finding,
                        call: LeanView::Fine,
                        note: String::new(),
                    },
                    Answer::Change { note } => AnswerView {
                        finding,
                        call: LeanView::Change,
                        note,
                    },
                })
                .collect(),
            answered_by: r.answered_by,
            note: r.note,
            questions: r.questions.into_iter().map(QuestionView::from).collect(),
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// The PR's newest review, or `null` before its first.
pub(crate) async fn agent_review(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
) -> Result<Json<Option<AgentReviewView>>, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let review = app.store.latest_review(id, &repo, number).await?;
    Ok(Json(review.map(AgentReviewView::from)))
}

#[derive(Deserialize, TS)]
#[ts(export)]
pub struct AnswerAgentReview {
    /// The head the answers are about, as the review names it.
    pub head_sha: String,
    /// One per finding the review asks about.
    pub answers: Vec<AnswerView>,
    /// For the author, on top of the answers. Optional.
    #[serde(default)]
    pub note: String,
    /// Ask for changes to a PR the agent could not review, and so asks nothing about: the
    /// `note` says what. Ignored when the review asks about findings.
    #[serde(default)]
    pub request_changes: bool,
}

/// Hidden in answers so deliveries are not read as new replies. Keep the pre-rebrand markers:
/// an old revision still serves webhooks while the new revision rolls out.
pub(crate) const ANSWERED_MARK: &str = "Answered Mergepatrol's review.";
pub(crate) const LINE_MARK: &str = "<sub>On Mergepatrol's review:";

/// Answers what the review asks: posts a review of the PR as the caller, with each change asked
/// for on its line, and lets the PR on in the queue, or holds it for a push.
pub(crate) async fn answer_agent_review(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
    Json(body): Json<AnswerAgentReview>,
) -> Result<Json<AgentReviewView>, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let token = caller.github_token(&app)?;
    let review = current(&app, id, &repo, number, &body.head_sha).await?;
    if review.status != ReviewStatus::Asks {
        return Err(ApiError::Status(
            StatusCode::CONFLICT,
            "The review asks nothing now.".into(),
        ));
    }
    let answers = answers_of(&review, &body)?;
    let changes = match review.asks().count() {
        0 => body.request_changes,
        _ => answers.values().any(|a| matches!(a, Answer::Change { .. })),
    };
    if changes && answers.is_empty() && body.note.trim().is_empty() {
        return Err(ApiError::bad_request("Say what to change."));
    }
    // What the agent sent back to the author still holds the PR for their push: answering the
    // asks approves it only when nothing is left.
    let owed = review.findings.iter().any(|f| f.sent_back);
    let by = reviewer(&caller, &app, &repo).await?;

    let mut text = String::new();
    if !body.note.trim().is_empty() {
        text.push_str(body.note.trim());
        text.push_str("\n\n");
    }
    let fine: Vec<&Judged> = review
        .asks()
        .filter(|f| matches!(answers.get(&f.finding.id), Some(Answer::Fine)))
        .collect();
    if !fine.is_empty() {
        text.push_str("Fine as they are, of what MergeLoop's review asked about:\n");
        for f in fine {
            text.push_str(&format!("- {}\n", f.finding.title));
        }
    }
    let mut comments = Vec::new();
    let mut unplaced = String::new();
    for f in review.asks() {
        let Some(Answer::Change { note }) = answers.get(&f.finding.id) else {
            continue;
        };
        let body = format!(
            "{note}\n\n<sub>On MergeLoop's review: {}</sub><!-- {LINE_MARK} -->",
            f.finding.title
        );
        match (&f.finding.file, f.finding.line) {
            (Some(path), Some(line)) => comments.push(LineComment { path, line, body }),
            _ => unplaced.push_str(&format!("\n\n**{}**\n{note}", f.finding.title)),
        }
    }
    text.push_str(&unplaced);
    text.push_str(&format!(
        "\n\n<sub>Answered MergeLoop's review.</sub><!-- {ANSWERED_MARK} -->"
    ));
    // GitHub takes neither an approval nor a request for changes from the PR's author, so
    // theirs goes as a comment. The gate holds a PR sent back by its `Changes` status, not by
    // GitHub's review, so the author's request holds it for their next push all the same.
    let gh = installation(&app, id).await?;
    let verdict = match changes {
        true if gh.pull_request(&repo, number).await?.authored_by(&by) => Verdict::Comment,
        true => Verdict::RequestChanges,
        false if owed => Verdict::Comment,
        false => approval(&gh, &repo, number, &by)
            .await?
            .unwrap_or(Verdict::Comment),
    };
    setup::review_lines_as(
        &token,
        &repo.to_string(),
        number,
        &review.head_sha,
        verdict,
        text.trim(),
        &comments,
    )
    .await
    .map_err(|e| {
        if let Some(setup::Rejected(why)) = e.downcast_ref() {
            ApiError::Status(StatusCode::UNPROCESSABLE_ENTITY, why.clone())
        } else if e.is::<setup::Refused>() {
            ApiError::Status(
                StatusCode::FORBIDDEN,
                format!("GitHub won't let you review PRs in {repo}."),
            )
        } else if e.is::<setup::Revoked>() {
            ApiError::Status(StatusCode::UNAUTHORIZED, "Sign in again first.".into())
        } else {
            e.into()
        }
    })?;
    let answered = ReviewAnswers {
        answers,
        by: by.clone(),
        note: body.note.trim().to_owned(),
        status: match changes || owed {
            true => ReviewStatus::Changes,
            false => ReviewStatus::Approved,
        },
    };
    if !app.store.answer_review(review.id, &answered).await? {
        return Err(ApiError::Status(
            StatusCode::CONFLICT,
            "Someone answered the review first.".into(),
        ));
    }
    tracing::info!(installation = id, %repo, number, changes, "answered the agent's review");
    app.store.inbox_reviewed(id, &repo, number, &by).await?;
    app.wake.wake(id, repo.clone());
    let review = app.store.review(review.id).await?.ok_or_else(no_review)?;
    Ok(Json(review.into()))
}

/// The agent's questions are for the people who may change the repo: GitHub takes a review
/// from anyone who can read it, but the gate takes answers, and spends the workspace's model
/// on questions, only from those who can write.
async fn reviewer(caller: &Caller, app: &AppState, repo: &RepoName) -> Result<String, ApiError> {
    let token = caller.github_token(app)?;
    let writes = setup::can_write(&token, &repo.to_string())
        .await
        .map_err(|e| match e.is::<setup::Revoked>() {
            true => ApiError::Status(StatusCode::UNAUTHORIZED, "Sign in again first.".into()),
            false => e.into(),
        })?;
    if !writes {
        return Err(ApiError::Status(
            StatusCode::FORBIDDEN,
            format!("You need write access to {repo} to answer its review."),
        ));
    }
    Ok(match caller {
        Caller::User(user) => user.login().to_owned(),
        Caller::Token(t) => setup::user(t).await?.login,
    })
}

fn no_review() -> ApiError {
    ApiError::Status(
        StatusCode::NOT_FOUND,
        "The agent has not reviewed this PR.".into(),
    )
}

/// The PR's newest review, which must be of `head`.
async fn current(
    app: &AppState,
    id: u64,
    repo: &RepoName,
    number: PrNumber,
    head: &str,
) -> Result<ReviewRecord, ApiError> {
    let review = app
        .store
        .latest_review(id, repo, number)
        .await?
        .ok_or_else(no_review)?;
    if review.head_sha != head {
        return Err(ApiError::Status(
            StatusCode::CONFLICT,
            "The PR moved on: its new head has a review of its own.".into(),
        ));
    }
    Ok(review)
}

/// The answers by finding: one for each finding the review asks about, and no other.
fn answers_of(
    review: &ReviewRecord,
    body: &AnswerAgentReview,
) -> Result<BTreeMap<String, Answer>, ApiError> {
    let mut answers = BTreeMap::new();
    for a in &body.answers {
        if !review.asks().any(|f| f.finding.id == a.finding) {
            return Err(ApiError::bad_request(&format!(
                "The review does not ask about {}.",
                a.finding
            )));
        }
        let answer = match a.call {
            LeanView::Fine => Answer::Fine,
            LeanView::Change if a.note.trim().is_empty() => {
                return Err(ApiError::bad_request("Say what to change."));
            }
            LeanView::Change => Answer::Change {
                note: a.note.trim().to_owned(),
            },
        };
        answers.insert(a.finding.clone(), answer);
    }
    if let Some(missing) = review.asks().find(|f| !answers.contains_key(&f.finding.id)) {
        return Err(ApiError::bad_request(&format!(
            "Answer every finding: {} has no answer.",
            missing.finding.id
        )));
    }
    Ok(answers)
}

#[derive(Deserialize, TS)]
#[ts(export)]
pub struct AskAgent {
    pub head_sha: String,
    /// The finding's id.
    pub finding: String,
    pub question: String,
}

/// Puts a question to the agent about a finding. The agent reads the code again to answer: the
/// answer shows on the review in a minute or two.
pub(crate) async fn ask_agent(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
    Json(body): Json<AskAgent>,
) -> Result<Json<QuestionView>, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let question = body.question.trim();
    if question.is_empty() {
        return Err(ApiError::bad_request("Ask something."));
    }
    let review = current(&app, id, &repo, number, &body.head_sha).await?;
    let Some(finding) = review
        .findings
        .iter()
        .find(|f| f.finding.id == body.finding)
    else {
        return Err(ApiError::bad_request("No such finding."));
    };
    // A human's question while it asks, or the author's reply to a finding sent back to them.
    if review.status != ReviewStatus::Asks && !finding.sent_back {
        return Err(ApiError::Status(
            StatusCode::CONFLICT,
            "The review asks nothing now, and holds nothing for the author.".into(),
        ));
    }
    let by = reviewer(&caller, &app, &repo).await?;
    let q = app
        .store
        .add_question(review.id, &body.finding, question, &by)
        .await?;
    Ok(Json(q.into()))
}

#[derive(Deserialize, TS)]
#[ts(export)]
pub struct AskReviewers {
    /// GitHub logins.
    pub reviewers: Vec<String>,
}

/// Asks others to answer the review in place of those it asked: requests their reviews on
/// GitHub, as the caller.
pub(crate) async fn ask_reviewers(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
    Json(body): Json<AskReviewers>,
) -> Result<Json<AgentReviewView>, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let token = caller.github_token(&app)?;
    let logins: Vec<String> = body
        .reviewers
        .iter()
        .map(|l| l.trim().trim_start_matches('@').to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    if logins.is_empty() {
        return Err(ApiError::bad_request("Name someone to ask."));
    }
    let review = app
        .store
        .latest_review(id, &repo, number)
        .await?
        .ok_or_else(no_review)?;
    setup::request_reviewers_as(&token, &repo.to_string(), number, &logins)
        .await
        .map_err(|e| {
            if let Some(setup::Rejected(why)) = e.downcast_ref() {
                ApiError::Status(StatusCode::UNPROCESSABLE_ENTITY, why.clone())
            } else if e.is::<setup::Refused>() {
                ApiError::Status(
                    StatusCode::FORBIDDEN,
                    format!("GitHub won't let you ask for reviews in {repo}."),
                )
            } else {
                e.into()
            }
        })?;
    app.store.set_asked(review.id, &logins).await?;
    app.store.inbox_changed(id, &repo, number).await?;
    let review = app.store.review(review.id).await?.ok_or_else(no_review)?;
    Ok(Json(review.into()))
}

/// What a lane shows of a queued PR's review.
#[derive(Clone, Serialize, TS)]
#[ts(export)]
pub struct ReviewBriefView {
    pub status: AgentReviewStatusView,
    /// Findings the agent asks about.
    pub asks: u32,
    /// Findings it sent back to the author, still to fix.
    pub sent_back: u32,
    /// Findings it cleared alone.
    pub cleared: u32,
    pub asked: Vec<String>,
    pub answered_by: Option<String>,
    /// The agent could not review.
    pub failed: bool,
}

impl From<&ReviewRecord> for ReviewBriefView {
    fn from(r: &ReviewRecord) -> Self {
        let asks = r.asks().count() as u32;
        let sent_back = r.findings.iter().filter(|f| f.sent_back).count() as u32;
        ReviewBriefView {
            status: r.status.into(),
            asks,
            sent_back,
            cleared: r.findings.len() as u32 - asks - sent_back,
            asked: r.asked.clone(),
            answered_by: r.answered_by.clone(),
            failed: r.error.is_some() && r.status == ReviewStatus::Asks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mergepatrol_core::Finding;

    fn judged(id: &str, confidence: u8, lean: Lean, asked: bool) -> Judged {
        Judged {
            finding: Finding {
                id: id.into(),
                kind: Kind::Security,
                title: id.into(),
                file: None,
                line: None,
                confidence,
                lean,
                finding: String::new(),
                unsure: String::new(),
                checked: Vec::new(),
                would_clear: String::new(),
                suggestion: String::new(),
                needs_human: false,
                code: Vec::new(),
                related: Vec::new(),
            },
            asked,
            sent_back: false,
            withdrawn: false,
            threaded: false,
            bar: Bar::From(90),
        }
    }

    fn record(findings: Vec<Judged>, answers: BTreeMap<String, Answer>) -> ReviewRecord {
        ReviewRecord {
            id: 1,
            installation_id: 1,
            repo: RepoName::parse("o/r").unwrap(),
            number: 1,
            head_sha: "h".into(),
            status: ReviewStatus::Asks,
            error: None,
            attempts: 0,
            summary: String::new(),
            findings,
            asked: Vec::new(),
            answers,
            answered_by: None,
            note: String::new(),
            questions: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn the_track_record_counts_where_humans_overruled_the_agent() {
        let r = record(
            vec![
                judged("a", 52, Lean::Fine, true),
                judged("b", 58, Lean::Change, true),
                judged("c", 97, Lean::Fine, false),
                judged("d", 100, Lean::Fine, false),
            ],
            BTreeMap::from([
                ("a".into(), Answer::Change { note: "n".into() }),
                ("b".into(), Answer::Change { note: "n".into() }),
            ]),
        );
        let h = history(&[r]);
        let security = &h[0].bands;
        assert_eq!((security[0].findings, security[0].overruled), (1, 1));
        assert_eq!((security[1].answered, security[1].overruled), (1, 0));
        assert_eq!(security[9].findings, 2, "100% is in the top band");
        assert!(
            h[1..]
                .iter()
                .all(|k| k.bands.iter().all(|b| b.findings == 0))
        );
    }

    #[test]
    fn answers_cover_exactly_what_the_review_asks() {
        let r = record(
            vec![
                judged("a", 52, Lean::Fine, true),
                judged("c", 97, Lean::Fine, false),
            ],
            BTreeMap::new(),
        );
        let body = |answers: Vec<AnswerView>| AnswerAgentReview {
            head_sha: "h".into(),
            answers,
            note: String::new(),
            request_changes: false,
        };
        let a = |id: &str, call, note: &str| AnswerView {
            finding: id.into(),
            call,
            note: note.into(),
        };
        assert!(answers_of(&r, &body(vec![])).is_err(), "a missing answer");
        assert!(
            answers_of(
                &r,
                &body(vec![a("a", LeanView::Fine, ""), a("c", LeanView::Fine, "")])
            )
            .is_err(),
            "not asked"
        );
        assert!(
            answers_of(&r, &body(vec![a("a", LeanView::Change, " ")])).is_err(),
            "a change says what"
        );
        let Ok(got) = answers_of(&r, &body(vec![a("a", LeanView::Change, "fix")])) else {
            panic!("a change that says what is an answer");
        };
        assert_eq!(
            got,
            BTreeMap::from([("a".into(), Answer::Change { note: "fix".into() })])
        );
    }
}
