//! Settling a recurring finding must settle each request counted by review stats.

use std::collections::BTreeMap;
use std::time::Duration;

use mergepatrol_core::{Answer, Judged, Outcome, RepoName, ReviewStatus};
use mergepatrol_store::{PgStore, ReviewAnswers, ReviewDone, ReviewStore};
use serde_json::json;
use sqlx::PgPool;

fn repo() -> RepoName {
    RepoName::parse("o/r").unwrap()
}

async fn finished(store: &PgStore, head: &str, asked: bool) -> i64 {
    let r = store.start_review(1, &repo(), 8, head).await.unwrap();
    let claim = store
        .claim_review(Duration::from_secs(600))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.id, r.id);
    let finding: Judged = serde_json::from_value(json!({
        "id": "f1",
        "kind": "other",
        "title": "Retry never stops",
        "confidence": 100,
        "lean": "change",
        "finding": "The retry loop has no bound.",
        "sent_back": !asked,
        "asked": asked,
        "threaded": true,
        "bar": 80
    }))
    .unwrap();
    let done = ReviewDone {
        status: if asked {
            ReviewStatus::Asks
        } else {
            ReviewStatus::Changes
        },
        error: None,
        summary: String::new(),
        findings: vec![finding],
        asked: Vec::new(),
    };
    assert!(store.finish_review(r.id, claim.claim, &done).await.unwrap());
    r.id
}

async fn both_settled(store: &PgStore, first: i64, second: i64, outcome: Outcome) {
    let reviews = store.reviews_of(1, &repo(), 8, 10).await.unwrap();
    assert_eq!(reviews.len(), 2);
    for id in [second, first] {
        let r = reviews.iter().find(|r| r.id == id).unwrap();
        assert_eq!(
            r.findings[0].outcome,
            Some(outcome),
            "review {id} still counts an open request after the recurring finding was settled"
        );
    }
}

#[sqlx::test(migrations = false)]
async fn withdrawing_a_recurring_finding_settles_every_request(pool: PgPool) {
    let store = PgStore::migrate(pool).await.unwrap();
    let first = finished(&store, "first", false).await;
    let second = finished(&store, "second", false).await;
    store
        .withdraw_finding(second, "f1", Outcome::Wrong)
        .await
        .unwrap();
    both_settled(&store, first, second, Outcome::Wrong).await;
}

#[sqlx::test(migrations = false)]
async fn an_intended_recurring_finding_settles_every_request(pool: PgPool) {
    let store = PgStore::migrate(pool).await.unwrap();
    let first = finished(&store, "first", false).await;
    let second = finished(&store, "second", false).await;
    store
        .withdraw_finding(second, "f1", Outcome::Intended)
        .await
        .unwrap();
    both_settled(&store, first, second, Outcome::Intended).await;
}

#[sqlx::test(migrations = false)]
async fn resolving_a_recurring_thread_settles_every_request(pool: PgPool) {
    let store = PgStore::migrate(pool).await.unwrap();
    let first = finished(&store, "first", false).await;
    let second = finished(&store, "second", false).await;
    assert!(store.approve_resolved(second).await.unwrap());
    both_settled(&store, first, second, Outcome::Dismissed).await;
}

#[sqlx::test(migrations = false)]
async fn a_human_dismissing_a_recurring_finding_settles_every_request(pool: PgPool) {
    let store = PgStore::migrate(pool).await.unwrap();
    let first = finished(&store, "first", false).await;
    let second = finished(&store, "second", true).await;
    assert!(
        store
            .answer_review(
                second,
                &ReviewAnswers {
                    answers: BTreeMap::from([("f1".into(), Answer::Fine)]),
                    by: "mona".into(),
                    note: String::new(),
                    status: ReviewStatus::Approved,
                },
            )
            .await
            .unwrap()
    );
    both_settled(&store, first, second, Outcome::Dismissed).await;
}
