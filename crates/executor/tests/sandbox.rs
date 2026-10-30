//! A real rebase in a real GKE sandbox. Run from a pod in the cluster that the job template's
//! network policy lets in (`app: mergepatrol` in the namespace `mergepatrol`), as the
//! orchestrator's account:
//!
//!   POD_IP=<the pod's IP> cargo test -p mergepatrol-executor --test sandbox -- --ignored
//!
//! The job rebases fork PR #3 of the public agent-labs-dev/mergepatrol-fork-lab, which merges
//! cleanly, so it needs no model API.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mergepatrol_executor::Executor;
use mergepatrol_executor::protocol::{BUNDLE_FILE, JobOutcome, JobSpec, ResolverSpec};
use mergepatrol_executor::remote::{Mailbox, RemoteExecutor};
use mergepatrol_executor::sandbox::{Kube, START_PORT, SandboxLaunch};

#[tokio::test]
#[ignore = "needs a GKE cluster with Agent Sandbox; see the top of this file"]
async fn a_rebase_runs_in_a_gke_sandbox() {
    let mailbox = Arc::new(Mailbox::default());
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8081").await.unwrap();
    tokio::spawn(axum::serve(listener, mailbox.clone().router()).into_future());
    let timeout = Duration::from_secs(15 * 60);
    let executor = RemoteExecutor {
        mailbox,
        base_url: format!("http://{}:8081", std::env::var("POD_IP").unwrap()),
        launch: Box::new(SandboxLaunch {
            kube: Kube::in_cluster().unwrap(),
            namespace: "mergepatrol-jobs".into(),
            warm_pool: "mergepatrol-job".into(),
            timeout,
            start_port: START_PORT,
        }),
        timeout,
    };
    let spec = JobSpec {
        repo: "agent-labs-dev/mergepatrol-fork-lab".into(),
        clone_url: "https://github.com/agent-labs-dev/mergepatrol-fork-lab.git".into(),
        token: String::new(),
        head_ref: "refs/pull/3/head".into(),
        head_sha: "732e7c5f550c743ed85251099dd35b93874de321".into(),
        trunk: "main".into(),
        trunk_sha: "43f9e3989e0154aeac96922034bd0a0af10c3df6".into(),
        base_ref: None,
        upstream: None,
        pr_title: "fork: only.txt".into(),
        pr_body: String::new(),
        verify: Vec::new(),
        resolver: ResolverSpec {
            provider: Default::default(),
            model: "claude-opus-5-5".into(),
            effort: "medium".into(),
            timeout_secs: 60,
            max_turns: 5,
            api: None,
        },
        task: Default::default(),
    };
    let out = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let result = executor.run("e2e", &spec, out.path()).await.unwrap();
    eprintln!("the job took {:?}", start.elapsed());
    assert_eq!(result.outcome, JobOutcome::Done, "{:?}", result.events);
    assert!(out.path().join(BUNDLE_FILE).exists());
    // The claim's deletion runs on its own: it lands before the process exits.
    tokio::time::sleep(Duration::from_secs(2)).await;
}
