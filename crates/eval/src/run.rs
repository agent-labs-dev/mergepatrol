//! Runs MergeLoop's review on each case as production does: a review `JobSpec` for the case's
//! PR, run by `mergepatrol job` through `LocalExecutor`, calling the model through MergeLoop's
//! own API proxy. So the prompt, the agent, and how its evidence runs are the shipped ones.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{StreamExt, stream};
use mergepatrol_core::{Provider, Spend};
use mergepatrol_executor::protocol::{
    ApiAccess, JobOutcome, JobResult, JobSpec, REVIEW_FILE, ResolverSpec, ReviewOutput, Task,
};
use mergepatrol_executor::proxy::{Api, ApiProxy, Credential, REQUESTS_PER_JOB, Upstreams};
use mergepatrol_executor::{Executor, LocalExecutor};
use serde::{Deserialize, Serialize};

use crate::case::Case;
use crate::score::{CaseRun, Reviewed};

/// The resolver's limits, as the queue gives a review.
const TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_TURNS: u32 = 100;
/// Longer than the resolver's and its evidence's own limits, as the orchestrator's.
const JOB_TIMEOUT: Duration = Duration::from_secs(40 * 60);

/// Which agent reviews: what a repo's `agent:` config sets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub provider: Provider,
    pub model: String,
    pub effort: String,
}

/// A run's file: what reviewed, and each case's review.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunFile {
    /// What it tries, e.g. `main` or `shorter-prompt`.
    pub label: String,
    pub agent: Agent,
    pub cases: Vec<CaseRun>,
}

pub struct Runner {
    /// The `mergepatrol` binary whose review it runs.
    pub job_bin: PathBuf,
    pub agent: Agent,
    /// The provider's API key.
    pub key: String,
    /// Cases reviewed at once.
    pub jobs: usize,
    /// Where each case's job writes its output.
    pub work: PathBuf,
}

impl Runner {
    pub async fn run(&self, cases: &[Case]) -> Result<Vec<CaseRun>> {
        let proxy = Arc::new(ApiProxy::new(Upstreams::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let proxy_url = format!("http://{}", listener.local_addr()?);
        let router = proxy.clone().router();
        tokio::spawn(async move { axum::serve(listener, router).await });

        let executor = LocalExecutor {
            program: self.job_bin.clone(),
            timeout: JOB_TIMEOUT,
        };
        let runs = stream::iter(cases)
            .map(|case| self.review(&executor, &proxy, &proxy_url, case))
            .buffered(self.jobs.max(1))
            .collect()
            .await;
        Ok(runs)
    }

    async fn review(
        &self,
        executor: &LocalExecutor,
        proxy: &Arc<ApiProxy>,
        proxy_url: &str,
        case: &Case,
    ) -> CaseRun {
        let Agent {
            provider, model, ..
        } = &self.agent;
        let credential = Credential::ApiKey(self.key.clone().into());
        let granted = proxy.grant(*provider, credential, REQUESTS_PER_JOB);
        let api = ApiAccess {
            base_url: Api::of(*provider, model).base_url(proxy_url),
            token: granted.token.clone(),
        };
        let out = self.work.join(&case.id);
        eprintln!("{}: reviewing {}#{}", case.id, case.repo, case.pr);
        let result = run_job(executor, &spec(case, &self.agent, api), &out).await;
        let run = case_run(case, result);
        match &run.reviewed {
            Reviewed::Done { findings, .. } => {
                eprintln!("{}: {} findings", case.id, findings.len())
            }
            Reviewed::Failed { reason } => eprintln!("{}: failed: {reason}", case.id),
        }
        run
    }
}

/// The job the queue would run to review the case's PR.
pub fn spec(case: &Case, agent: &Agent, api: ApiAccess) -> JobSpec {
    JobSpec {
        repo: case.repo.clone(),
        clone_url: format!("https://github.com/{}.git", case.repo),
        // A public repo: the job fetches without auth, and has no token to revoke.
        token: String::new(),
        head_ref: format!("refs/pull/{}/head", case.pr),
        head_sha: case.head.as_str().to_owned(),
        trunk: case.trunk.clone(),
        trunk_sha: case.base.as_str().to_owned(),
        base_ref: None,
        upstream: None,
        pr_title: case.title.clone(),
        pr_body: case.body.clone(),
        verify: case.verify.clone(),
        leave_conflicts: false,
        resolver: ResolverSpec {
            provider: agent.provider,
            model: agent.model.clone(),
            effort: agent.effort.clone(),
            timeout_secs: TIMEOUT.as_secs(),
            max_turns: MAX_TURNS,
            api: Some(api),
            settings_url: None,
        },
        task: Task::Review {
            answered: Vec::new(),
            sent_back: Vec::new(),
            since: None,
            reviewed: None,
            depth: mergepatrol_core::Depth::Standard,
        },
    }
}

/// What the job left: its result, and the review it wrote, if it wrote one.
type JobRan = Result<(JobResult, Option<ReviewOutput>)>;

async fn run_job(executor: &LocalExecutor, spec: &JobSpec, out: &Path) -> JobRan {
    // A run before this one may have left its output.
    let _ = tokio::fs::remove_dir_all(out).await;
    let result = executor.run("eval", spec, out).await?;
    let review = match tokio::fs::read(out.join(REVIEW_FILE)).await {
        Ok(json) => Some(serde_json::from_slice(&json).context("the job wrote a bad review")?),
        Err(_) => None,
    };
    Ok((result, review))
}

/// The case's review, from what its job left.
pub fn case_run(case: &Case, ran: JobRan) -> CaseRun {
    let (cost_usd, reviewed) = match ran {
        Err(e) => (None, failed(format!("{e:#}"))),
        Ok((result, review)) => {
            let cost = result
                .events
                .iter()
                .filter_map(|e| Spend::parse(&e.kind, &e.message))
                .filter_map(|s| s.cost_usd)
                .reduce(|a, b| a + b);
            let reviewed = match (result.outcome, review) {
                (JobOutcome::Failed { reason }, _) => failed(reason),
                (JobOutcome::Done, None) => failed("the job wrote no review".into()),
                (JobOutcome::Done, Some(r)) => Reviewed::Done {
                    summary: r.summary,
                    findings: r.findings,
                },
            };
            (cost, reviewed)
        }
    };
    CaseRun {
        id: case.id.clone(),
        head: case.head.clone(),
        reviewed,
        cost_usd,
    }
}

fn failed(reason: String) -> Reviewed {
    Reviewed::Failed { reason }
}

#[cfg(test)]
mod tests {
    use mergepatrol_executor::protocol::JobEvent;

    use super::*;

    fn case() -> Case {
        let sha = |c: &str| c.repeat(40);
        Case::parse(
            "pr-9",
            &format!(
                "repo: o/r\npr: 9\nbase: {}\nhead: {}\ntitle: T\nbody: B\nexpected: clean\n",
                sha("a"),
                sha("b")
            ),
        )
        .unwrap()
    }

    fn agent() -> Agent {
        Agent {
            provider: Provider::Anthropic,
            model: "m".into(),
            effort: "high".into(),
        }
    }

    fn api() -> ApiAccess {
        ApiAccess {
            base_url: "http://proxy/anthropic".into(),
            token: "job-token".into(),
        }
    }

    fn event(kind: &str, message: &str) -> JobEvent {
        JobEvent {
            kind: kind.into(),
            message: message.into(),
        }
    }

    fn done(events: Vec<JobEvent>) -> JobResult {
        JobResult {
            outcome: JobOutcome::Done,
            events,
        }
    }

    #[test]
    fn the_job_reviews_the_prs_head_from_where_it_left_the_trunk() {
        let s = spec(&case(), &agent(), api());
        assert_eq!(s.clone_url, "https://github.com/o/r.git");
        assert_eq!(s.head_ref, "refs/pull/9/head");
        assert_eq!((s.head_sha, s.trunk_sha), ("b".repeat(40), "a".repeat(40)));
        assert_eq!((s.trunk.as_str(), s.pr_title.as_str()), ("main", "T"));
        assert_eq!(s.pr_body, "B");
        assert!(s.token.is_empty(), "the job revokes any token it gets");
        assert_eq!(
            s.task,
            Task::Review {
                answered: Vec::new(),
                sent_back: Vec::new(),
                since: None,
                reviewed: None,
                depth: mergepatrol_core::Depth::Standard,
            }
        );
        let r = s.resolver;
        assert_eq!((r.model.as_str(), r.effort.as_str()), ("m", "high"));
        assert_eq!((r.timeout_secs, r.max_turns), (15 * 60, 100));
        assert_eq!(r.api.unwrap().token, "job-token");
    }

    #[test]
    fn a_review_keeps_its_findings_and_what_the_agent_spent() {
        let review = ReviewOutput {
            summary: "S".into(),
            findings: Vec::new(),
        };
        let events = vec![
            event("reviewing", "x"),
            event("reviewed", "12 turns, $0.46, by m at high effort"),
            event("unreviewed", "3 turns, $0.04, by m at high effort"),
        ];
        let run = case_run(&case(), Ok((done(events), Some(review))));
        assert_eq!(
            (run.id.as_str(), run.head.as_str()),
            ("pr-9", "b".repeat(40).as_str())
        );
        assert_eq!(
            run.reviewed,
            Reviewed::Done {
                summary: "S".into(),
                findings: Vec::new()
            }
        );
        assert!((run.cost_usd.unwrap() - 0.50).abs() < 1e-9);
        let unpriced = case_run(&case(), Ok((done(Vec::new()), None)));
        assert_eq!(unpriced.cost_usd, None);
    }

    #[test]
    fn a_job_that_failed_or_wrote_no_review_failed() {
        let reason = |ran: JobRan| match case_run(&case(), ran).reviewed {
            Reviewed::Failed { reason } => reason,
            r => panic!("{r:?}"),
        };
        let failed = JobResult {
            outcome: JobOutcome::Failed {
                reason: "the agent gave up".into(),
            },
            events: Vec::new(),
        };
        assert_eq!(reason(Ok((failed, None))), "the agent gave up");
        assert_eq!(
            reason(Ok((done(Vec::new()), None))),
            "the job wrote no review"
        );
        assert_eq!(reason(Err(anyhow::anyhow!("crashed"))), "crashed");
    }
}

/// A stand-in for the `mergepatrol` binary, for tests.
#[cfg(test)]
pub(crate) mod fake {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    /// Stands in for `mergepatrol job --out <dir>`: keeps the spec it was given, then reviews
    /// PR 1 and crashes on any other.
    const FAKE_JOB: &str = r#"#!/bin/sh
out="$3"
mkdir -p "$out"
cat > "$out/spec.json"
env > "$out/env"
if grep -q '"head_ref":"refs/pull/1/head"' "$out/spec.json"; then
  echo '{"summary":"S","findings":[]}' > "$out/review.json"
  echo '{"outcome":{"type":"done"},"events":[{"kind":"reviewed","message":"3 turns, $0.25, by m at medium effort"}]}' > "$out/result.json"
else
  echo boom >&2
  exit 3
fi
"#;

    /// Writes it into `dir`, and returns its path.
    pub(crate) fn job(dir: &Path) -> PathBuf {
        let bin = dir.join("mergepatrol");
        std::fs::write(&bin, FAKE_JOB).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }
}

#[cfg(test)]
mod runner_tests {
    use super::*;

    fn case(pr: u64) -> Case {
        Case::parse(
            &format!("pr-{pr}"),
            &format!(
                "repo: o/r\npr: {pr}\nbase: {}\nhead: {}\ntitle: T\nexpected: clean\n",
                "a".repeat(40),
                "b".repeat(40)
            ),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn each_case_runs_as_a_review_job_through_the_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let job_bin = fake::job(dir.path());
        let work = dir.path().join("work");
        let runner = Runner {
            job_bin,
            agent: Agent {
                provider: Provider::Anthropic,
                model: "m".into(),
                effort: "medium".into(),
            },
            key: "k".into(),
            jobs: 2,
            work: work.clone(),
        };
        let runs = runner.run(&[case(1), case(2)]).await.unwrap();

        assert_eq!(runs[0].id, "pr-1");
        assert_eq!(
            runs[0].reviewed,
            Reviewed::Done {
                summary: "S".into(),
                findings: Vec::new()
            }
        );
        assert_eq!(runs[0].cost_usd, Some(0.25));
        let Reviewed::Failed { reason } = &runs[1].reviewed else {
            panic!("{:?}", runs[1]);
        };
        assert!(reason.contains("boom"), "{reason}");

        let spec: JobSpec =
            serde_json::from_slice(&std::fs::read(work.join("pr-1/spec.json")).unwrap()).unwrap();
        let api = spec.resolver.api.unwrap();
        assert!(
            api.base_url.starts_with("http://127.0.0.1:"),
            "{}",
            api.base_url
        );
        assert!(api.base_url.ends_with("/anthropic"), "{}", api.base_url);
        assert_ne!(
            api.token, "k",
            "the job gets a token for the proxy, never the key"
        );
    }
}
