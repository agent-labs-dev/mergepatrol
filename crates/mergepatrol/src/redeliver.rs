//! Has GitHub send the webhook deliveries that failed again, as when Mergepatrol was down or
//! restarting: the queue reads only what webhooks say changed, so a lost one leaves a read stale
//! until the queue reads the whole repo again.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use mergepatrol_github::{App, HookDelivery};

use crate::queue::{Ctx, app_credentials, webhooks};

const REDELIVER_EVERY: Duration = Duration::from_secs(5 * 60);
/// A delivery that fails this often fails for a reason a redelivery does not fix.
const TRIES: u32 = 3;

/// The failed deliveries, by `guid`: the latest attempt, and how many times it was redelivered.
#[derive(Default)]
struct Failed {
    /// The newest delivery seen.
    last: Option<u64>,
    pending: HashMap<String, (u64, u32)>,
    /// Tried `TRIES` times: a later failure of theirs does not go again.
    given_up: HashSet<String>,
}

impl Failed {
    /// Takes in the deliveries since the last call, newest first. The deliveries to send again,
    /// by `guid`.
    fn take(&mut self, deliveries: &[HookDelivery]) -> Vec<(String, u64)> {
        for d in deliveries.iter().rev() {
            if d.delivered() {
                self.pending.remove(&d.guid);
            } else if !self.given_up.contains(&d.guid) {
                let tries = self.pending.get(&d.guid).map_or(0, |(_, tries)| *tries);
                self.pending.insert(d.guid.clone(), (d.id, tries));
            }
        }
        if let Some(newest) = deliveries.first() {
            self.last = Some(newest.id);
        }
        let given_up = &mut self.given_up;
        self.pending.retain(|guid, (id, tries)| {
            if *tries >= TRIES {
                tracing::warn!(
                    guid,
                    delivery = *id,
                    "a webhook delivery keeps failing, giving up"
                );
                given_up.insert(guid.clone());
            }
            *tries < TRIES
        });
        let mut ids: Vec<(String, u64)> = self
            .pending
            .iter()
            .map(|(guid, (id, _))| (guid.clone(), *id))
            .collect();
        ids.sort_by_key(|(_, id)| *id);
        ids
    }

    /// GitHub took the redelivery: one try.
    fn sent(&mut self, guid: &str) {
        if let Some((_, tries)) = self.pending.get_mut(guid) {
            *tries += 1;
        }
    }
}

pub async fn redeliver(ctx: Arc<Ctx>) {
    let mut failed = Failed::default();
    loop {
        if let Err(e) = redeliver_once(&ctx, &mut failed).await {
            tracing::warn!(error = %format!("{e:#}"), "could not redeliver failed webhooks");
        }
        tokio::time::sleep(REDELIVER_EVERY).await;
    }
}

async fn redeliver_once(ctx: &Ctx, failed: &mut Failed) -> Result<()> {
    let (Some(creds), true) = (app_credentials(ctx).await?, webhooks(ctx).await?) else {
        return Ok(());
    };
    let app = App::from_credentials(&creds)?;
    let deliveries = app.hook_deliveries(failed.last).await?;
    for (guid, id) in failed.take(&deliveries) {
        tracing::info!(delivery = id, "redelivering a failed webhook");
        match app.redeliver(id).await {
            Ok(()) => failed.sent(&guid),
            Err(e) => {
                tracing::warn!(delivery = id, error = %format!("{e:#}"), "could not redeliver a webhook")
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivery(id: u64, guid: &str, status_code: u16) -> HookDelivery {
        HookDelivery {
            id,
            guid: guid.into(),
            status_code,
            event: "push".into(),
        }
    }

    /// The ids to send again, each sent: `ok` says which GitHub took.
    fn redeliver(
        failed: &mut Failed,
        deliveries: &[HookDelivery],
        ok: impl Fn(u64) -> bool,
    ) -> Vec<u64> {
        let mut ids = Vec::new();
        for (guid, id) in failed.take(deliveries) {
            ids.push(id);
            if ok(id) {
                failed.sent(&guid);
            }
        }
        ids
    }

    #[test]
    fn a_failed_delivery_goes_again_until_it_succeeds() {
        let mut failed = Failed::default();
        let all = |_| true;
        // Newest first: `b` failed, then succeeded on its own redelivery.
        let first = [
            delivery(4, "b", 204),
            delivery(3, "c", 0),
            delivery(2, "b", 502),
            delivery(1, "a", 500),
        ];
        assert_eq!(redeliver(&mut failed, &first, all), [1, 3]);
        assert_eq!(failed.last, Some(4));
        // `a`'s redelivery succeeds, `c`'s fails again.
        let second = [delivery(6, "a", 204), delivery(5, "c", 503)];
        assert_eq!(redeliver(&mut failed, &second, all), [5]);
        assert_eq!(redeliver(&mut failed, &[], all), [5]);
        assert!(
            redeliver(&mut failed, &[], all).is_empty(),
            "three tries at most"
        );
        // The last try's failure shows late.
        assert!(redeliver(&mut failed, &[delivery(7, "c", 502)], all).is_empty());
    }

    #[test]
    fn a_redelivery_github_did_not_take_is_no_try() {
        let mut failed = Failed::default();
        let first = [delivery(2, "b", 500), delivery(1, "a", 500)];
        // GitHub refuses `a`'s, and takes `b`'s.
        for _ in 0..3 {
            assert_eq!(redeliver(&mut failed, &first, |id| id != 1), [1, 2]);
        }
        assert_eq!(redeliver(&mut failed, &[], |id| id != 1), [1]);
    }
}
