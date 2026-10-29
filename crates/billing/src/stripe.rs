//! The few Stripe calls billing makes: Checkout and the customer portal, reading a
//! subscription and changing its seats, and webhook signatures.

use anyhow::{Context, Result, bail};
use hmac::{Hmac, KeyInit, Mac};
use mergepatrol_store::Subscription;
use serde_json::Value;
use sha2::Sha256;

const API: &str = "https://api.stripe.com";
/// How old a webhook's signed time may be: Stripe's own libraries' default.
const WEBHOOK_TOLERANCE_SECS: i64 = 5 * 60;

pub struct StripeConfig {
    /// A restricted key will do: it needs Checkout, the customer portal, and subscriptions.
    pub secret_key: String,
    /// The webhook endpoint's signing secret, `whsec_…`.
    pub webhook_secret: String,
    /// The monthly price per seat: `price_…`.
    pub price: String,
}

pub(crate) struct Stripe {
    config: StripeConfig,
    http: reqwest::Client,
}

impl Stripe {
    pub fn new(config: StripeConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    async fn post(&self, path: &str, form: &[(&str, String)]) -> Result<Value> {
        let res = self
            .http
            .post(format!("{API}{path}"))
            .bearer_auth(&self.config.secret_key)
            .form(form)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        json(res, path).await
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let res = self
            .http
            .get(format!("{API}{path}"))
            .bearer_auth(&self.config.secret_key)
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        json(res, path).await
    }

    /// A Checkout page that subscribes the workspace to `seats` seats, after a trial of
    /// `trial_days` that needs no card: its URL.
    pub async fn checkout(
        &self,
        installation_id: u64,
        customer: Option<&str>,
        seats: u32,
        trial_days: Option<u32>,
        success_url: &str,
        cancel_url: &str,
    ) -> Result<String> {
        let id = installation_id.to_string();
        let mut form = vec![
            ("mode", "subscription".to_owned()),
            ("line_items[0][price]", self.config.price.clone()),
            ("line_items[0][quantity]", seats.to_string()),
            ("success_url", success_url.to_owned()),
            ("cancel_url", cancel_url.to_owned()),
            ("client_reference_id", id.clone()),
            ("subscription_data[metadata][installation_id]", id),
        ];
        if let Some(days) = trial_days {
            form.extend([
                ("subscription_data[trial_period_days]", days.to_string()),
                ("payment_method_collection", "if_required".to_owned()),
                // No card by the end of the trial: the subscription ends.
                (
                    "subscription_data[trial_settings][end_behavior][missing_payment_method]",
                    "cancel".to_owned(),
                ),
            ]);
        }
        if let Some(customer) = customer {
            form.push(("customer", customer.to_owned()));
        }
        let session = self.post("/v1/checkout/sessions", &form).await?;
        url(&session)
    }

    /// The customer portal, where a customer adds a card, or cancels: its URL.
    pub async fn portal(&self, customer: &str, return_url: &str) -> Result<String> {
        let form = [
            ("customer", customer.to_owned()),
            ("return_url", return_url.to_owned()),
        ];
        let session = self.post("/v1/billing_portal/sessions", &form).await?;
        url(&session)
    }

    /// The subscription, and the installation its metadata names: `None` for one made
    /// elsewhere.
    pub async fn subscription(&self, id: &str) -> Result<(Subscription, Option<u64>)> {
        let s = self.get(&format!("/v1/subscriptions/{id}")).await?;
        let item = &s["items"]["data"][0];
        let subscription = Subscription {
            id: s["id"].as_str().context("subscription id")?.to_owned(),
            customer: s["customer"]
                .as_str()
                .context("subscription customer")?
                .to_owned(),
            item: item["id"].as_str().context("subscription item")?.to_owned(),
            status: s["status"]
                .as_str()
                .context("subscription status")?
                .to_owned(),
            quantity: item["quantity"].as_u64().unwrap_or(1) as u32,
            trial_end: s["trial_end"].as_i64(),
        };
        let installation_id = s["metadata"]["installation_id"]
            .as_str()
            .and_then(|id| id.parse().ok());
        Ok((subscription, installation_id))
    }

    /// Bills `s` for `seats` from now. A seat added is charged for the rest of the period,
    /// prorated; a seat freed is not refunded, and drops off at the next period.
    pub async fn set_seats(&self, s: &Subscription, seats: u32) -> Result<()> {
        let proration = match seats > s.quantity {
            true => "create_prorations",
            false => "none",
        };
        let form = [
            ("items[0][id]", s.item.clone()),
            ("items[0][quantity]", seats.to_string()),
            ("proration_behavior", proration.to_owned()),
        ];
        self.post(&format!("/v1/subscriptions/{}", s.id), &form)
            .await?;
        Ok(())
    }

    /// Whether `header`, the `Stripe-Signature` of a webhook, signs `body` and is recent.
    pub fn verify(&self, header: &str, body: &[u8], now: i64) -> bool {
        verify(&self.config.webhook_secret, header, body, now)
    }
}

async fn json(res: reqwest::Response, path: &str) -> Result<Value> {
    let status = res.status();
    let body: Value = res
        .json()
        .await
        .with_context(|| format!("{path}: not JSON"))?;
    if !status.is_success() {
        let message = body["error"]["message"].as_str().unwrap_or("no message");
        bail!("{path}: Stripe answered {status}: {message}");
    }
    Ok(body)
}

fn url(session: &Value) -> Result<String> {
    Ok(session["url"].as_str().context("session url")?.to_owned())
}

/// The header is `t=<time>,v1=<signature>`, with a `v1` per signing secret while one rolls.
fn verify(secret: &str, header: &str, body: &[u8], now: i64) -> bool {
    let mut time = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        match part.split_once('=') {
            Some(("t", t)) => time = t.parse::<i64>().ok(),
            Some(("v1", s)) => signatures.extend(hex::decode(s).ok()),
            _ => {}
        }
    }
    let Some(time) = time else {
        return false;
    };
    if (now - time).abs() > WEBHOOK_TOLERANCE_SECS {
        return false;
    }
    signatures.iter().any(|s| {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key");
        mac.update(format!("{time}.").as_bytes());
        mac.update(body);
        mac.verify_slice(s).is_ok()
    })
}

#[cfg(test)]
pub(crate) fn sign(secret: &str, body: &[u8], time: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{time}.").as_bytes());
    mac.update(body);
    format!("t={time},v1={}", hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_webhook_is_signed_with_the_secret_and_recently() {
        let body = br#"{"type":"customer.subscription.updated"}"#;
        let header = sign("whsec", body, 1_000);
        assert!(verify("whsec", &header, body, 1_000 + 60));
        assert!(!verify("other", &header, body, 1_000));
        assert!(!verify("whsec", &header, b"{}", 1_000));
        assert!(!verify("whsec", &header, body, 1_000 + 3_600));
        // One of the signatures matches while a secret rolls.
        let rolling = format!("{header},v1=00ff");
        assert!(verify("whsec", &rolling, body, 1_000));
        assert!(!verify("whsec", "v1=00ff", body, 1_000));
    }
}
