//! A finding can take more than one author push to fix.

use super::*;

#[tokio::test]
async fn a_later_fix_settles_every_review_that_sent_the_same_finding_back() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    let first = world.conclude(100).await;

    world.gh.with_mut(|s| s.push(1, "still-broken"));
    let second = world.conclude(100).await;

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

    // Both records describe the same finding, whose fix is now known.
    for id in [second, first] {
        let review = world.store.review(id).await.unwrap().unwrap();
        assert_eq!(
            review.findings[0].outcome,
            Some(Outcome::Fixed),
            "review {id} still has no outcome after the finding was fixed"
        );
    }
}
