//! Billing for the hosted Mergepatrol, through Stripe. Off without Stripe's keys, so a
//! self-hosted Mergepatrol has no limits.
//!
//! Public repos are free, within a fair-use limit on each workspace: `FAIR_USE_PRS` PRs joining
//! a queue, and `FAIR_USE_JOBS` jobs, an hour. Private repos need a subscription, which starts
//! with a Stripe trial of `TRIAL_DAYS` that needs no card: `PRICE_USD` a month per seat. A
//! developer takes a seat when their PR first joins a private repo's queue, and keeps it until
//! freed in the dashboard; the subscription's quantity follows the seats. Either way, the
//! workspace brings its own model API key.
//!
//! Over the limit, or without a subscription, the repo's waiting PRs go on hold: they keep their
//! place, and go on once the hour is under the limit, or the workspace subscribes.

mod stripe;

use std::sync::Arc;

use anyhow::{Result, bail};
use mergepatrol_store::{BillingStore, FairUse, Subscription};

use stripe::Stripe;
pub use stripe::StripeConfig;

pub const PRICE_USD: u32 = 15;
pub const TRIAL_DAYS: u32 = 14;
pub const FAIR_USE_PRS: u64 = 100;
pub const FAIR_USE_JOBS: u64 = 50;
const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;

/// Whether a workspace's private repos queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    /// Unix seconds.
    Trial {
        ends_at: i64,
    },
    Subscribed,
    /// No live subscription. `tried`: it had one, and with it its trial.
    None {
        tried: bool,
    },
}

impl Access {
    pub fn of(subscription: Option<&Subscription>) -> Self {
        let Some(s) = subscription else {
            return Self::None { tried: false };
        };
        match s.status.as_str() {
            "trialing" => Self::Trial {
                ends_at: s.trial_end.unwrap_or_default(),
            },
            // Stripe retries a failed payment while `past_due`: the queue goes on meanwhile.
            "active" | "past_due" => Self::Subscribed,
            _ => Self::None { tried: true },
        }
    }

    fn live(&self) -> bool {
        !matches!(self, Self::None { .. })
    }
}

/// Why a repo's waiting PRs are on hold: `reason` for the dashboard, `comment` for the PR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    pub reason: String,
    pub comment: String,
}

/// A workspace's billing, for the dashboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub access: Access,
    /// The seats' logins.
    pub seats: Vec<String>,
    /// It has a Stripe customer: the portal can open.
    pub customer: bool,
}

pub struct Billing {
    store: Arc<dyn BillingStore>,
    stripe: Stripe,
    /// Where users reach the dashboard.
    public_url: String,
}

impl Billing {
    pub fn new(store: Arc<dyn BillingStore>, config: StripeConfig, public_url: &str) -> Self {
        Self {
            store,
            stripe: Stripe::new(config),
            public_url: public_url.trim_end_matches('/').to_owned(),
        }
    }

    /// Why the waiting PRs of a repo of `account`'s workspace go on hold, if they do.
    pub async fn gate(
        &self,
        installation_id: u64,
        account: &str,
        private: bool,
        now: i64,
    ) -> Result<Option<Hold>> {
        if !private {
            let since = now - HOUR;
            let prs = self
                .store
                .fair_use_since(installation_id, FairUse::Pr, since)
                .await?;
            let jobs = self
                .store
                .fair_use_since(installation_id, FairUse::Job, since)
                .await?;
            return Ok(fair_use_hold(account, prs, jobs));
        }
        let subscription = self.store.subscription(installation_id).await?;
        Ok(match Access::of(subscription.as_ref()) {
            Access::None { tried } => Some(self.unsubscribed_hold(account, tried)),
            _ => None,
        })
    }

    fn unsubscribed_hold(&self, account: &str, tried: bool) -> Hold {
        let url = self.settings_url(account);
        let what = match tried {
            false => format!(
                "Mergepatrol on private repos needs a subscription. Start a free trial of {TRIAL_DAYS} days in the [dashboard]({url}), with no card"
            ),
            true => format!(
                "the Mergepatrol subscription for private repos has ended. Subscribe again in the [dashboard]({url})"
            ),
        };
        Hold {
            reason: "private repos need a Mergepatrol subscription".into(),
            comment: format!(
                "On hold: {what}. Public repos stay free, and private ones cost ${PRICE_USD} a month per seat: a developer whose PRs Mergepatrol queues. It keeps its place in the queue, and goes on once subscribed."
            ),
        }
    }

    fn settings_url(&self, account: &str) -> String {
        format!("{}/{account}/settings", self.public_url)
    }

    /// A PR by `authors` joined a queue: a stack's layers may have several. On a private repo,
    /// each takes a seat, subscribed or not yet, so a subscription starts with the seats it
    /// needs.
    pub async fn queued(
        &self,
        installation_id: u64,
        private: bool,
        authors: &[String],
        now: i64,
    ) -> Result<()> {
        if !private {
            return self
                .store
                .record_fair_use(installation_id, FairUse::Pr, now)
                .await;
        }
        for login in authors {
            if self.store.add_seat(installation_id, login, now).await? {
                tracing::info!(installation = installation_id, %login, "seat taken");
            }
        }
        Ok(())
    }

    /// A job started, on a repo that is `private` or not.
    pub async fn job(&self, installation_id: u64, private: bool, now: i64) -> Result<()> {
        if private {
            return Ok(());
        }
        self.store
            .record_fair_use(installation_id, FairUse::Job, now)
            .await
    }

    /// Sets each live subscription's quantity to its seats, where they differ: how many.
    pub async fn sync(&self) -> Result<usize> {
        let unsynced = self.store.unsynced().await?;
        for u in &unsynced {
            self.stripe.set_seats(&u.subscription, u.seats).await?;
            self.store.set_quantity(u.installation_id, u.seats).await?;
            tracing::info!(
                installation = u.installation_id,
                from = u.subscription.quantity,
                to = u.seats,
                "seats billed"
            );
        }
        Ok(unsynced.len())
    }

    /// Deletes the fair-use counts older than a day: how many.
    pub async fn prune(&self, now: i64) -> Result<u64> {
        self.store.prune_fair_use(now - DAY).await
    }

    pub async fn status(&self, installation_id: u64) -> Result<Status> {
        let subscription = self.store.subscription(installation_id).await?;
        Ok(Status {
            access: Access::of(subscription.as_ref()),
            seats: self.store.seats(installation_id).await?,
            customer: subscription.is_some(),
        })
    }

    /// Frees `login`'s seat: whether they had one. They take one again with their next PR.
    pub async fn free_seat(&self, installation_id: u64, login: &str) -> Result<bool> {
        self.store.remove_seat(installation_id, login).await
    }

    /// A Checkout page that subscribes the workspace of `account`, for the seats it has, with a
    /// trial unless it had one: its URL.
    pub async fn checkout(&self, installation_id: u64, account: &str) -> Result<String> {
        let subscription = self.store.subscription(installation_id).await?;
        if Access::of(subscription.as_ref()).live() {
            bail!("already subscribed");
        }
        let seats = self.store.seats(installation_id).await?.len().max(1) as u32;
        let back = self.settings_url(account);
        self.stripe
            .checkout(
                installation_id,
                subscription.as_ref().map(|s| s.customer.as_str()),
                seats,
                subscription.is_none().then_some(TRIAL_DAYS),
                &format!("{back}?billing=subscribed"),
                &back,
            )
            .await
    }

    /// The customer portal of the workspace of `account`: its URL. `None` before it subscribed.
    pub async fn portal(&self, installation_id: u64, account: &str) -> Result<Option<String>> {
        let Some(s) = self.store.subscription(installation_id).await? else {
            return Ok(None);
        };
        let back = self.settings_url(account);
        Ok(Some(self.stripe.portal(&s.customer, &back).await?))
    }

    /// Whether `signature`, a webhook's `Stripe-Signature` header, signs `body`.
    pub fn verify(&self, signature: &str, body: &[u8], now: i64) -> bool {
        self.stripe.verify(signature, body, now)
    }

    /// A verified webhook: a subscription changed. Its state is read from Stripe again, so
    /// deliveries may come in any order, or twice.
    pub async fn webhook(&self, event: &serde_json::Value) -> Result<()> {
        let object = &event["data"]["object"];
        let subscription = match event["type"].as_str().unwrap_or_default() {
            "checkout.session.completed" => object["subscription"].as_str(),
            t if t.starts_with("customer.subscription.") => object["id"].as_str(),
            _ => None,
        };
        let Some(id) = subscription else {
            return Ok(());
        };
        let (s, installation_id) = self.stripe.subscription(id).await?;
        // The Stripe account is shared with other products, whose subscriptions name no installation.
        let Some(installation_id) = installation_id else {
            tracing::debug!(subscription = %s.id, "a subscription without an installation");
            return Ok(());
        };
        self.store.set_subscription(installation_id, &s).await?;
        tracing::info!(installation = installation_id, status = %s.status, "subscription");
        Ok(())
    }
}

fn fair_use_hold(account: &str, prs: u64, jobs: u64) -> Option<Hold> {
    (prs >= FAIR_USE_PRS || jobs >= FAIR_USE_JOBS).then(|| Hold {
        reason: "over the fair-use limit of public repos".into(),
        comment: format!(
            "On hold: `{account}`'s public repos queued {FAIR_USE_PRS} PRs, or ran {FAIR_USE_JOBS} jobs, in the last hour: Mergepatrol's fair-use limit. It keeps its place in the queue, and goes on once the last hour is under the limit."
        ),
    })
}

#[cfg(test)]
mod tests;
