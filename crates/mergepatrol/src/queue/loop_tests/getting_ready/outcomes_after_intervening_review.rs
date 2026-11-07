//! An intervening review must not hide an earlier request's outcome.

use super::*;

#[tokio::test]
async fn a_fix_after_escalation_settles_the_requests_sent_back_before_it() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    let mut sent_back = vec![world.conclude(100).await];
    for head in ["second", "third"] {
        world.gh.with_mut(|s| s.push(1, head));
        sent_back.push(world.conclude(100).await);
    }
    world.gh.with_mut(|s| s.push(1, "escalated"));
    let escalated = world.conclude(100).await;
    let escalated = world.store.review(escalated).await.unwrap().unwrap();
    assert_eq!(escalated.status, ReviewStatus::Asks);
    assert!(escalated.findings[0].asked);
    assert!(!escalated.findings[0].sent_back);
    assert!(escalated.answers.is_empty());

    // The author fixes it on the next push before a human has answered the escalation.
    world.gh.with_mut(|s| s.push(1, "fixed"));
    let head = world.pr(1).head_ref_oid;
    let current = world
        .store
        .start_review(1, &repo(), 1, &head)
        .await
        .unwrap();
    let settings = world.ctx.config.repo_defaults.clone();
    world
        .conclude_review(
            &current,
            Finding {
                lean: Lean::Fine,
                settled: Some(Settled::Fixed),
                ..finding(100)
            },
            &settings,
        )
        .await;
    for id in sent_back {
        let original = world.store.review(id).await.unwrap().unwrap();
        assert_eq!(
            original.findings[0].outcome,
            Some(Outcome::Fixed),
            "review {id}: escalation must not hide the author's eventual fix"
        );
    }
}

#[tokio::test]
async fn a_fix_after_an_intervening_failed_review_settles_the_original_request() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    let first = world.conclude(100).await;

    world.gh.with_mut(|s| s.push(1, "review-failed"));
    let head = world.pr(1).head_ref_oid;
    let failed = world
        .store
        .start_review(1, &repo(), 1, &head)
        .await
        .unwrap();
    let settings = world.ctx.config.repo_defaults.clone();
    conclude(
        &world.ctx,
        &world.installation,
        &world.key(),
        &world.pr(1),
        &failed,
        &settings,
        &ReviewSettings::default(),
        Err("The review agent could not run".into()),
    )
    .await
    .unwrap();
    let failed = world.store.review(failed.id).await.unwrap().unwrap();
    assert_eq!(failed.status, ReviewStatus::Asks);
    assert!(failed.findings.is_empty());

    world.gh.with_mut(|s| s.push(1, "fixed"));
    let head = world.pr(1).head_ref_oid;
    let current = world
        .store
        .start_review(1, &repo(), 1, &head)
        .await
        .unwrap();
    world
        .conclude_review(
            &current,
            Finding {
                lean: Lean::Fine,
                settled: Some(Settled::Fixed),
                ..finding(100)
            },
            &settings,
        )
        .await;

    let original = world.store.review(first).await.unwrap().unwrap();
    assert_eq!(
        original.findings[0].outcome,
        Some(Outcome::Fixed),
        "the explicit fix must settle the older request even when the intervening review failed"
    );
}
