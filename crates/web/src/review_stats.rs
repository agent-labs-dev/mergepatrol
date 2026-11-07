//! How the agent's review does, as the Claude Code team measures its own: what became of each
//! change it asked for, how many reviews a PR took to pass, and how much it asked of people.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use mergepatrol_core::{Lean, Outcome, ReviewStatus};
use mergepatrol_store::ReviewRecord;
use serde::Serialize;
use ts_rs::TS;

use crate::api::HistoryQuery;
use crate::auth::Caller;
use crate::{ApiError, AppState};

/// What became of the changes the review asked for: sent back to the author, or asked of a
/// human. `open` ones are not settled yet, or were raised again.
#[derive(Clone, Debug, Default, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct ChangeOutcomesView {
    pub fixed: u32,
    pub wrong: u32,
    pub intended: u32,
    pub dismissed: u32,
    pub open: u32,
}

/// How many reviews the PRs took to pass the review, the queue's first gate.
#[derive(Clone, Debug, Default, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct RoundsView {
    /// PRs whose review passed.
    pub passed: u32,
    /// Of those, on their first review.
    pub first_review: u32,
    /// Within two reviews: at most one round of fixing.
    pub one_round: u32,
    /// After five reviews or more.
    pub five_plus: u32,
    /// Reviews to pass, on average. `None` with none passed.
    pub mean_reviews: Option<f64>,
    /// PRs reviewed that have not passed.
    pub not_passed: u32,
}

/// What the review asked of people.
#[derive(Clone, Debug, Default, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct HumanTimeView {
    /// PRs whose review asked a human.
    pub asked_prs: u32,
    /// Reviews a human answered.
    pub answered: u32,
    /// How long a review waited for a human's answer, the median. `None` with none answered.
    pub wait_p50_secs: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct ReviewStatsView {
    /// The PRs first reviewed in the last `days`, each with its whole history.
    pub days: u32,
    pub prs: u32,
    pub changes: ChangeOutcomesView,
    pub rounds: RoundsView,
    pub humans: HumanTimeView,
}

/// The review's numbers over these reviews, of any PRs, in any order. A PR's reviews count up
/// to its first approval: a later one is a rebase carrying it, findings and all, or a review of
/// what the queue's agent changed since.
fn review_stats(reviews: &[ReviewRecord], days: u32) -> ReviewStatsView {
    let mut prs: BTreeMap<(&str, &str, u64), Vec<&ReviewRecord>> = BTreeMap::new();
    for r in reviews {
        prs.entry((&r.repo.owner, &r.repo.name, r.number))
            .or_default()
            .push(r);
    }
    let prs: Vec<_> = prs
        .into_values()
        .map(|mut of| {
            of.sort_by_key(|r| r.id);
            if let Some(i) = of.iter().position(|r| r.status == ReviewStatus::Approved) {
                of.truncate(i + 1);
            }
            of
        })
        .collect();
    ReviewStatsView {
        days,
        prs: prs.len() as u32,
        changes: changes(prs.iter().flatten().copied()),
        rounds: rounds(prs.iter().map(|of| reviews_to_pass(of))),
        humans: humans(&prs),
    }
}

fn changes<'a>(reviews: impl Iterator<Item = &'a ReviewRecord>) -> ChangeOutcomesView {
    let mut v = ChangeOutcomesView::default();
    let asked_for = reviews
        .flat_map(|r| &r.findings)
        .filter(|j| j.finding.lean == Lean::Change && (j.sent_back || j.withdrawn || j.asked));
    for j in asked_for {
        *v.of(j.outcome) += 1;
    }
    v
}

impl ChangeOutcomesView {
    fn of(&mut self, outcome: Option<Outcome>) -> &mut u32 {
        match outcome {
            Some(Outcome::Fixed) => &mut self.fixed,
            Some(Outcome::Wrong) => &mut self.wrong,
            Some(Outcome::Intended) => &mut self.intended,
            Some(Outcome::Dismissed) => &mut self.dismissed,
            None => &mut self.open,
        }
    }
}

/// How many of a PR's reviews, oldest first, it took to pass: its finished ones, up to its
/// first approval. `None` if none approved it.
fn reviews_to_pass(of: &[&ReviewRecord]) -> Option<u32> {
    let mut n = 0;
    for r in of {
        match r.status {
            ReviewStatus::Approved => return Some(n + 1),
            ReviewStatus::Changes | ReviewStatus::Asks => n += 1,
            _ => {}
        }
    }
    None
}

fn rounds(each: impl Iterator<Item = Option<u32>>) -> RoundsView {
    let mut v = RoundsView::default();
    let mut total = 0;
    for n in each {
        let Some(n) = n else {
            v.not_passed += 1;
            continue;
        };
        v.passed += 1;
        total += n;
        v.first_review += u32::from(n == 1);
        v.one_round += u32::from(n <= 2);
        v.five_plus += u32::from(n >= 5);
    }
    v.mean_reviews = (v.passed > 0).then(|| f64::from(total) / f64::from(v.passed));
    v
}

fn humans(prs: &[Vec<&ReviewRecord>]) -> HumanTimeView {
    let asked = |r: &&ReviewRecord| r.status == ReviewStatus::Asks || r.answered_by.is_some();
    let mut waits: Vec<u32> = prs.iter().flatten().filter_map(|r| r.waited_secs).collect();
    waits.sort_unstable();
    HumanTimeView {
        asked_prs: prs.iter().filter(|of| of.iter().any(asked)).count() as u32,
        answered: prs
            .iter()
            .flatten()
            .filter(|r| r.answered_by.is_some())
            .count() as u32,
        wait_p50_secs: waits.get(waits.len() / 2).copied(),
    }
}

pub(crate) async fn review_stats_view(
    State(app): State<AppState>,
    caller: Caller,
    Path(id): Path<u64>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<ReviewStatsView>, ApiError> {
    let (id, readable) = caller.workspace_repos(&app, id).await?;
    let (repo, days) = (q.repo()?, q.days()?);
    let mut reviews = app.store.reviews_of_prs_since(id, days).await?;
    reviews.retain(|r| readable.contains(&r.repo) && repo.as_ref().is_none_or(|p| *p == r.repo));
    Ok(Json(review_stats(&reviews, days)))
}

#[cfg(test)]
mod tests {
    use mergepatrol_core::{Bar, Finding, Judged, Kind, RepoName};

    use super::*;

    fn change(id: &str, sent_back: bool, outcome: Option<Outcome>) -> Judged {
        Judged {
            finding: Finding {
                id: id.into(),
                kind: Kind::Other,
                title: id.into(),
                file: None,
                line: None,
                confidence: 90,
                lean: Lean::Change,
                finding: String::new(),
                unsure: String::new(),
                checked: Vec::new(),
                would_clear: String::new(),
                suggestion: String::new(),
                evidence: None,
                needs_human: false,
                verified: None,
                settled: None,
                also_at: Vec::new(),
                code: Vec::new(),
                related: Vec::new(),
            },
            asked: !sent_back,
            sent_back,
            withdrawn: false,
            threaded: false,
            bar: Bar::From(80),
            outcome,
        }
    }

    /// Review `id` of PR `number`.
    fn review(id: i64, number: u64, status: ReviewStatus, findings: Vec<Judged>) -> ReviewRecord {
        ReviewRecord {
            id,
            installation_id: 1,
            repo: RepoName::parse("o/r").unwrap(),
            number,
            head_sha: format!("h{id}"),
            since_head: None,
            status,
            claim: 0,
            error: None,
            attempts: 0,
            summary: String::new(),
            findings,
            asked: Vec::new(),
            answers: BTreeMap::new(),
            answered_by: None,
            note: String::new(),
            questions: Vec::new(),
            waited_secs: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    use ReviewStatus::{Approved, Asks, Changes, Pending, Stale};

    #[test]
    fn each_change_counts_by_what_became_of_it() {
        let fine = Judged {
            finding: Finding {
                lean: Lean::Fine,
                ..change("f9", false, None).finding
            },
            ..change("f9", false, None)
        };
        let withdrawn = Judged {
            sent_back: false,
            withdrawn: true,
            asked: false,
            ..change("f4", true, Some(Outcome::Intended))
        };
        let cleared = Judged {
            sent_back: false,
            asked: false,
            ..change("f6", true, None)
        };
        let s = review_stats(
            &[
                review(
                    1,
                    7,
                    Changes,
                    vec![
                        change("f1", true, Some(Outcome::Fixed)),
                        change("f2", true, Some(Outcome::Wrong)),
                        change("f3", false, Some(Outcome::Dismissed)),
                        withdrawn,
                        change("f5", true, None),
                        // A note that held nothing, and a question about what is fine: no
                        // change asked for.
                        cleared,
                        fine,
                    ],
                ),
                review(
                    2,
                    7,
                    Approved,
                    vec![change("f1", true, Some(Outcome::Fixed))],
                ),
            ],
            30,
        );
        assert_eq!(
            s.changes,
            ChangeOutcomesView {
                fixed: 2,
                wrong: 1,
                intended: 1,
                dismissed: 1,
                open: 1,
            }
        );
    }

    #[test]
    fn a_pr_passes_on_its_first_approval_counting_only_finished_reviews() {
        let s = review_stats(
            &[
                // #1 passes on its first review.
                review(1, 1, Approved, Vec::new()),
                // #2 on its second; a re-review after it passed does not count.
                review(3, 2, Approved, Vec::new()),
                review(2, 2, Changes, Vec::new()),
                review(9, 2, Changes, Vec::new()),
                // #3 on its fifth: a stale or pending one is no review.
                review(10, 3, Changes, Vec::new()),
                review(11, 3, Stale, Vec::new()),
                review(12, 3, Asks, Vec::new()),
                review(13, 3, Changes, Vec::new()),
                review(14, 3, Changes, Vec::new()),
                review(15, 3, Approved, Vec::new()),
                // #4 has not passed.
                review(20, 4, Changes, Vec::new()),
                review(21, 4, Pending, Vec::new()),
            ],
            30,
        );
        assert_eq!(s.prs, 4);
        assert_eq!(
            s.rounds,
            RoundsView {
                passed: 3,
                first_review: 1,
                one_round: 2,
                five_plus: 1,
                mean_reviews: Some(8.0 / 3.0),
                not_passed: 1,
            }
        );
    }

    #[test]
    fn a_review_after_the_pr_passed_counts_for_nothing() {
        let asked = ReviewRecord {
            answered_by: Some("mona".into()),
            ..review(
                2,
                1,
                Approved,
                vec![change("f2", false, Some(Outcome::Dismissed))],
            )
        };
        // A rebase carries the approval to a new head, findings and all.
        let carried = ReviewRecord {
            id: 3,
            head_sha: "rebased".into(),
            ..asked.clone()
        };
        let s = review_stats(
            &[
                review(
                    1,
                    1,
                    Changes,
                    vec![change("f1", true, Some(Outcome::Fixed))],
                ),
                asked,
                carried,
            ],
            30,
        );
        assert_eq!(
            s.changes,
            ChangeOutcomesView {
                fixed: 1,
                dismissed: 1,
                ..ChangeOutcomesView::default()
            }
        );
        assert_eq!((s.humans.asked_prs, s.humans.answered), (1, 1));
    }

    #[test]
    fn a_pr_in_another_repo_is_another_pr() {
        let mut other = review(2, 1, Approved, Vec::new());
        other.repo = RepoName::parse("o/other").unwrap();
        let s = review_stats(&[review(1, 1, Approved, Vec::new()), other], 30);
        assert_eq!((s.prs, s.rounds.first_review), (2, 2));
    }

    #[test]
    fn people_are_counted_by_the_prs_they_were_asked_about_and_how_long_they_took() {
        let answered = |id, number, secs| ReviewRecord {
            answered_by: Some("mona".into()),
            waited_secs: Some(secs),
            ..review(id, number, Approved, Vec::new())
        };
        let s = review_stats(
            &[
                answered(1, 1, 60),
                review(2, 2, Asks, Vec::new()),
                answered(3, 2, 600),
                answered(4, 3, 120),
                review(5, 4, Approved, Vec::new()),
            ],
            30,
        );
        assert_eq!(
            s.humans,
            HumanTimeView {
                asked_prs: 3,
                answered: 3,
                wait_p50_secs: Some(120),
            }
        );
        let none = review_stats(&[review(1, 1, Approved, Vec::new())], 30);
        assert_eq!(none.humans, HumanTimeView::default());
        assert_eq!(none.rounds.mean_reviews, Some(1.0));
        assert_eq!(review_stats(&[], 30).rounds.mean_reviews, None);
    }
}
