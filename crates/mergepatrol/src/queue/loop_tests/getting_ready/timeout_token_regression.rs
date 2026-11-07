//! A timed-out job may already have fetched and revoked its read-only token.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

struct FetchOnce {
    agent: Arc<Agent>,
    consumed: Mutex<HashSet<String>>,
}

#[async_trait]
impl Executor for FetchOnce {
    async fn run(&self, id: &str, spec: &JobSpec, out: &Path) -> Result<JobResult> {
        // job::run revokes spec.token after fetching, before it starts the rebase.
        // Model a timeout during that rebase, rather than before the fetch.
        if !self.consumed.lock().unwrap().insert(spec.token.clone()) {
            return Ok(JobResult {
                outcome: JobOutcome::Failed {
                    reason: "fetch refused: the preceding job revoked this clone token".into(),
                },
                events: Vec::new(),
            });
        }
        self.agent.run(id, spec, out).await
    }
}

#[tokio::test]
async fn a_timeout_fallback_fetches_with_a_fresh_read_token() {
    let mut world = World::one_pr("feature.txt", "other.txt").await;
    let (answers, origin) = (world.gh.clone(), world.origin.clone());
    let minted = Arc::new(AtomicUsize::new(0));
    let tokens = minted.clone();
    world.installation = mergepatrol_github::fake::answered_by(move |method, path, body| {
        if path.ends_with("/access_tokens") {
            let token = format!("read-{}", tokens.fetch_add(1, Ordering::SeqCst));
            return (201, json!({ "token": token }).to_string());
        }
        answer(&answers, &origin, method, path, body)
    })
    .await;
    Arc::get_mut(&mut world.ctx).unwrap().executor = Arc::new(FetchOnce {
        agent: world.agent.clone(),
        consumed: Default::default(),
    });
    *world.agent.times_out.lock().unwrap() = 1;

    let unit = Unit {
        number: 1,
        base: "main".into(),
        layers: vec![world.pr(1)],
        stacked: false,
    };
    let settings = &world.ctx.config.repo_defaults;
    let snap = GitHub::snapshot(&world.installation, &repo(), &settings.labels())
        .await
        .unwrap();
    let event = rebase_and_publish(
        &world.ctx,
        &world.installation,
        &world.key(),
        &snap,
        &unit,
        0,
        settings,
        true,
        false,
    )
    .await
    .unwrap();
    assert!(matches!(event, Event::Rebased { .. }), "{event:?}");
    assert_eq!(
        *world.agent.updates.lock().unwrap(),
        [Update::Either, Update::Merge]
    );
}
