//! What hosted billing keeps: each workspace's Stripe subscription, its seats, and the fair-use
//! counts of public repos.

use anyhow::Result;
use async_trait::async_trait;
use sqlx::Row;

use crate::PgStore;

/// A workspace's Stripe subscription, as Stripe last told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    pub customer: String,
    pub id: String,
    /// The subscription's one item: the seats.
    pub item: String,
    pub status: String,
    pub quantity: u32,
    /// Unix seconds.
    pub trial_end: Option<i64>,
}

/// `Pr` for a PR that joined a queue, `Job` for a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FairUse {
    Pr,
    Job,
}

impl FairUse {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pr => "pr",
            Self::Job => "job",
        }
    }
}

/// A subscription whose quantity is not its workspace's seats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsynced {
    pub installation_id: u64,
    pub subscription: Subscription,
    pub seats: u32,
}

#[async_trait]
pub trait BillingStore: Send + Sync {
    async fn subscription(&self, installation_id: u64) -> Result<Option<Subscription>>;
    async fn set_subscription(&self, installation_id: u64, s: &Subscription) -> Result<()>;
    async fn set_quantity(&self, installation_id: u64, quantity: u32) -> Result<()>;
    /// Gives `login` a seat: whether they had none.
    async fn add_seat(&self, installation_id: u64, login: &str, at: i64) -> Result<bool>;
    /// Frees `login`'s seat: whether they had one.
    async fn remove_seat(&self, installation_id: u64, login: &str) -> Result<bool>;
    /// The seats' logins, in order.
    async fn seats(&self, installation_id: u64) -> Result<Vec<String>>;
    /// The live subscriptions whose quantity is not their seats, at least one.
    async fn unsynced(&self) -> Result<Vec<Unsynced>>;
    async fn record_fair_use(&self, installation_id: u64, kind: FairUse, at: i64) -> Result<()>;
    /// How many of `kind` the workspace recorded since `since`.
    async fn fair_use_since(&self, installation_id: u64, kind: FairUse, since: i64) -> Result<u64>;
    async fn prune_fair_use(&self, before: i64) -> Result<u64>;
}

fn subscription(r: &sqlx::postgres::PgRow) -> Subscription {
    Subscription {
        customer: r.get("stripe_customer"),
        id: r.get("stripe_subscription"),
        item: r.get("stripe_item"),
        status: r.get("status"),
        quantity: r.get::<i32, _>("quantity") as u32,
        trial_end: r.get("trial_end"),
    }
}

#[async_trait]
impl BillingStore for PgStore {
    async fn subscription(&self, installation_id: u64) -> Result<Option<Subscription>> {
        let row = sqlx::query("SELECT * FROM billing WHERE installation_id = $1")
            .bind(installation_id as i64)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(subscription))
    }

    async fn set_subscription(&self, installation_id: u64, s: &Subscription) -> Result<()> {
        sqlx::query(
            "INSERT INTO billing (installation_id, stripe_customer, stripe_subscription,
                stripe_item, status, quantity, trial_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (installation_id) DO UPDATE SET stripe_customer = excluded.stripe_customer,
                stripe_subscription = excluded.stripe_subscription,
                stripe_item = excluded.stripe_item, status = excluded.status,
                quantity = excluded.quantity, trial_end = excluded.trial_end",
        )
        .bind(installation_id as i64)
        .bind(&s.customer)
        .bind(&s.id)
        .bind(&s.item)
        .bind(&s.status)
        .bind(s.quantity as i32)
        .bind(s.trial_end)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn set_quantity(&self, installation_id: u64, quantity: u32) -> Result<()> {
        sqlx::query("UPDATE billing SET quantity = $2 WHERE installation_id = $1")
            .bind(installation_id as i64)
            .bind(quantity as i32)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn add_seat(&self, installation_id: u64, login: &str, at: i64) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO seats (installation_id, login, added_at) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(installation_id as i64)
        .bind(login)
        .bind(at)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    async fn remove_seat(&self, installation_id: u64, login: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM seats WHERE installation_id = $1 AND login = $2")
            .bind(installation_id as i64)
            .bind(login)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() == 1)
    }

    async fn seats(&self, installation_id: u64) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT login FROM seats WHERE installation_id = $1 ORDER BY lower(login)",
        )
        .bind(installation_id as i64)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn unsynced(&self) -> Result<Vec<Unsynced>> {
        let rows = sqlx::query(
            "SELECT b.*, GREATEST(count(s.login), 1)::INT AS seats
             FROM billing b LEFT JOIN seats s USING (installation_id)
             WHERE b.status IN ('trialing', 'active', 'past_due')
             GROUP BY b.installation_id
             HAVING GREATEST(count(s.login), 1) <> b.quantity",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| Unsynced {
                installation_id: r.get::<i64, _>("installation_id") as u64,
                subscription: subscription(r),
                seats: r.get::<i32, _>("seats") as u32,
            })
            .collect())
    }

    async fn record_fair_use(&self, installation_id: u64, kind: FairUse, at: i64) -> Result<()> {
        sqlx::query("INSERT INTO fair_use (installation_id, kind, at) VALUES ($1, $2, $3)")
            .bind(installation_id as i64)
            .bind(kind.as_str())
            .bind(at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn fair_use_since(&self, installation_id: u64, kind: FairUse, since: i64) -> Result<u64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM fair_use WHERE installation_id = $1 AND kind = $2 AND at >= $3",
        )
        .bind(installation_id as i64)
        .bind(kind.as_str())
        .bind(since)
        .fetch_one(&self.pool)
        .await?;
        Ok(n as u64)
    }

    async fn prune_fair_use(&self, before: i64) -> Result<u64> {
        let done = sqlx::query("DELETE FROM fair_use WHERE at < $1")
            .bind(before)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    fn sub(status: &str, quantity: u32) -> Subscription {
        Subscription {
            customer: "cus".into(),
            id: "sub".into(),
            item: "si".into(),
            status: status.into(),
            quantity,
            trial_end: None,
        }
    }

    #[sqlx::test(migrations = false)]
    async fn a_subscription_round_trips(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        assert_eq!(store.subscription(1).await.unwrap(), None);
        let trial = Subscription {
            trial_end: Some(100),
            ..sub("trialing", 2)
        };
        store.set_subscription(1, &trial).await.unwrap();
        assert_eq!(store.subscription(1).await.unwrap(), Some(trial));
        store.set_subscription(1, &sub("active", 2)).await.unwrap();
        store.set_quantity(1, 3).await.unwrap();
        assert_eq!(store.subscription(1).await.unwrap(), Some(sub("active", 3)));
    }

    #[sqlx::test(migrations = false)]
    async fn a_seat_is_taken_once_and_freed(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        assert!(store.add_seat(1, "bob", 5).await.unwrap());
        assert!(!store.add_seat(1, "bob", 6).await.unwrap());
        assert!(store.add_seat(1, "Ada", 7).await.unwrap());
        assert!(store.add_seat(2, "cy", 7).await.unwrap());
        assert_eq!(store.seats(1).await.unwrap(), ["Ada", "bob"]);
        assert!(store.remove_seat(1, "bob").await.unwrap());
        assert!(!store.remove_seat(1, "bob").await.unwrap());
        assert_eq!(store.seats(1).await.unwrap(), ["Ada"]);
    }

    #[sqlx::test(migrations = false)]
    async fn live_subscriptions_follow_their_seats_at_least_one(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        // No seats, one billed: in sync.
        store
            .set_subscription(1, &sub("trialing", 1))
            .await
            .unwrap();
        // Two seats, one billed.
        store.set_subscription(2, &sub("active", 1)).await.unwrap();
        store.add_seat(2, "ada", 0).await.unwrap();
        store.add_seat(2, "bob", 0).await.unwrap();
        // Ended: nothing to bill.
        store
            .set_subscription(3, &sub("canceled", 1))
            .await
            .unwrap();
        store.add_seat(3, "ada", 0).await.unwrap();
        store.add_seat(3, "bob", 0).await.unwrap();
        let unsynced = store.unsynced().await.unwrap();
        let ids: Vec<_> = unsynced
            .iter()
            .map(|u| (u.installation_id, u.seats))
            .collect();
        assert_eq!(ids, [(2, 2)]);
        store.set_quantity(2, 2).await.unwrap();
        assert!(store.unsynced().await.unwrap().is_empty());
    }

    #[sqlx::test(migrations = false)]
    async fn fair_use_counts_the_window_of_one_workspace(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        for at in [10, 20, 30] {
            store.record_fair_use(1, FairUse::Pr, at).await.unwrap();
        }
        store.record_fair_use(1, FairUse::Job, 30).await.unwrap();
        store.record_fair_use(2, FairUse::Pr, 30).await.unwrap();
        assert_eq!(store.fair_use_since(1, FairUse::Pr, 20).await.unwrap(), 2);
        assert_eq!(store.fair_use_since(1, FairUse::Job, 0).await.unwrap(), 1);
        assert_eq!(store.prune_fair_use(20).await.unwrap(), 1);
        assert_eq!(store.fair_use_since(1, FairUse::Pr, 0).await.unwrap(), 2);
    }
}
