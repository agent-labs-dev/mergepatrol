use super::*;
use mergepatrol_store::PgStore;
use sqlx::PgPool;

const NOW: i64 = 1_793_275_200;

fn billing(store: Arc<PgStore>) -> Billing {
    let config = StripeConfig {
        secret_key: "sk_test".into(),
        webhook_secret: "whsec".into(),
        price: "price".into(),
    };
    Billing::new(store, config, "https://mergepatrol.test/")
}

fn sub(status: &str) -> Subscription {
    Subscription {
        customer: "cus".into(),
        id: "sub".into(),
        item: "si".into(),
        status: status.into(),
        quantity: 1,
        trial_end: Some(NOW),
    }
}

#[test]
fn a_subscription_is_live_while_trialing_or_paying() {
    assert_eq!(Access::of(None), Access::None { tried: false });
    assert_eq!(
        Access::of(Some(&sub("trialing"))),
        Access::Trial { ends_at: NOW }
    );
    for status in ["active", "past_due"] {
        assert_eq!(
            Access::of(Some(&sub(status))),
            Access::Subscribed,
            "{status}"
        );
    }
    for status in ["canceled", "unpaid", "paused", "incomplete_expired"] {
        let access = Access::of(Some(&sub(status)));
        assert_eq!(access, Access::None { tried: true }, "{status}");
    }
}

#[test]
fn fair_use_holds_at_either_limit() {
    assert_eq!(
        fair_use_hold("o", FAIR_USE_PRS - 1, FAIR_USE_JOBS - 1),
        None
    );
    assert!(fair_use_hold("o", FAIR_USE_PRS, 0).is_some());
    assert!(fair_use_hold("o", 0, FAIR_USE_JOBS).is_some());
}

#[sqlx::test(migrations = false)]
async fn a_private_repo_holds_until_subscribed(pool: PgPool) {
    let store = Arc::new(PgStore::migrate(pool).await.unwrap());
    let b = billing(store.clone());
    let hold = b.gate(1, "o", true, NOW).await.unwrap().unwrap();
    assert!(hold.comment.contains("free trial of 14 days"));
    assert!(hold.comment.contains("https://mergepatrol.test/o/settings"));
    // Public repos stay free.
    assert_eq!(b.gate(1, "o", false, NOW).await.unwrap(), None);

    store.set_subscription(1, &sub("trialing")).await.unwrap();
    assert_eq!(b.gate(1, "o", true, NOW).await.unwrap(), None);
    store.set_subscription(1, &sub("canceled")).await.unwrap();
    let hold = b.gate(1, "o", true, NOW).await.unwrap().unwrap();
    assert!(hold.comment.contains("has ended"));
}

#[sqlx::test(migrations = false)]
async fn private_repo_authors_take_seats_until_freed(pool: PgPool) {
    let store = Arc::new(PgStore::migrate(pool).await.unwrap());
    let b = billing(store.clone());
    let authors = ["bob".to_owned(), "ada".to_owned()];
    // Before a subscription, so it starts with them.
    b.queued(1, true, &authors, NOW).await.unwrap();
    b.queued(1, true, &authors[..1], NOW + 1).await.unwrap();
    // Public repos count toward fair use, not seats.
    b.queued(1, false, &["cy".to_owned()], NOW).await.unwrap();
    assert_eq!(b.status(1).await.unwrap().seats, ["ada", "bob"]);
    assert!(b.free_seat(1, "bob").await.unwrap());
    assert_eq!(b.status(1).await.unwrap().seats, ["ada"]);
}

#[sqlx::test(migrations = false)]
async fn public_repos_hold_once_the_hour_is_full(pool: PgPool) {
    let store = Arc::new(PgStore::migrate(pool).await.unwrap());
    let b = billing(store.clone());
    for i in 0..FAIR_USE_JOBS {
        b.job(1, false, NOW - HOUR + 1 + i as i64).await.unwrap();
        // Private repos' jobs are paid for.
        b.job(2, true, NOW).await.unwrap();
    }
    assert!(b.gate(1, "o", false, NOW).await.unwrap().is_some());
    assert_eq!(b.gate(2, "p", false, NOW).await.unwrap(), None);
    // An hour after the first job, the window has room again.
    assert_eq!(b.gate(1, "o", false, NOW + 2).await.unwrap(), None);
}
