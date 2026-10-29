//! The queue loop against a fake GitHub, a scripted rebase, and an in-memory store. Time is
//! paused, so the waits for checks take no real time.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

use mergepatrol_executor::LocalExecutor;
use mergepatrol_github::Checks;
use mergepatrol_store::{EventRecord, RepoSetting, SecretRecord, Session, Workspace};

use super::*;
use crate::config::{ExecutorConfig, Labels};

const TRIGGER: &str = "mergepatrol";
const FAILED: &str = "mergepatrol:failed";

fn repo() -> RepoName {
    RepoName::parse("o/r").unwrap()
}

/// GitHub as the queue sees it. `actions` run one per `checks` call, before it answers: they
/// are how a test changes GitHub while a job waits for checks.
#[derive(Default)]
struct Fake {
    state: Mutex<State>,
    actions: Mutex<VecDeque<Box<dyn FnOnce(&mut State) + Send>>>,
}

#[derive(Default)]
struct State {
    trunk: String,
    branches: HashMap<String, String>,
    prs: BTreeMap<PrNumber, PullRequest>,
    /// The PR API lags behind a branch update for this many reads, as GitHub's does.
    lag_reads: u32,
    stale: HashMap<PrNumber, (String, u32)>,
    required: Vec<String>,
    /// The checks on every commit, unless `checks_on` has the commit.
    checks: BTreeMap<String, CheckState>,
    checks_on: HashMap<String, BTreeMap<String, CheckState>>,
    comments: Vec<(PrNumber, String)>,
    merges: u32,
}

impl State {
    fn add_pr(&mut self, number: PrNumber) {
        let branch = format!("pr-{number}");
        let head = format!("{branch}-v0");
        self.branches.insert(branch.clone(), head.clone());
        self.prs.insert(
            number,
            PullRequest {
                number,
                title: format!("PR {number}"),
                body: String::new(),
                is_draft: false,
                is_cross_repository: false,
                review_decision: None,
                base_ref_name: "main".into(),
                head_ref_name: branch,
                head_ref_oid: head,
                labels: vec![TRIGGER.into()],
            },
        );
    }

    /// Moves a PR's branch, as a push does. The PR API sees it `lag_reads` reads later.
    fn push(&mut self, number: PrNumber, sha: &str) {
        let pr = self.prs.get_mut(&number).unwrap();
        self.branches.insert(pr.head_ref_name.clone(), sha.into());
        if self.lag_reads > 0 {
            let old = pr.head_ref_oid.clone();
            self.stale.insert(number, (old, self.lag_reads));
        }
        pr.head_ref_oid = sha.into();
    }

    fn head(&self, number: PrNumber) -> String {
        self.branches[&self.prs[&number].head_ref_name].clone()
    }
}

impl Fake {
    fn new(f: impl FnOnce(&mut State)) -> Arc<Self> {
        let fake = Self::default();
        {
            let mut s = fake.state.lock().unwrap();
            s.trunk = "trunk-0".into();
            s.checks.insert("ci".into(), CheckState::Passed);
            f(&mut s);
        }
        Arc::new(fake)
    }

    /// While a job waits for checks: one change to GitHub per `checks` call.
    fn then(&self, action: impl FnOnce(&mut State) + Send + 'static) {
        self.actions.lock().unwrap().push_back(Box::new(action));
    }

    fn with<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(&self.state.lock().unwrap())
    }
}

#[async_trait]
impl GitHub for Fake {
    fn installation_id(&self) -> u64 {
        1
    }

    async fn snapshot(&self, _: &RepoName, label: &str) -> Result<RepoSnapshot> {
        let s = self.state.lock().unwrap();
        Ok(RepoSnapshot {
            id: "R".into(),
            default_branch: "main".into(),
            trunk_sha: s.trunk.clone(),
            merge_method: "merge".into(),
            labeled: s
                .prs
                .values()
                .filter(|pr| pr.has_label(label))
                .cloned()
                .collect(),
        })
    }

    async fn pull_request(&self, _: &RepoName, number: PrNumber) -> Result<PullRequest> {
        let mut s = self.state.lock().unwrap();
        let mut pr = s.prs[&number].clone();
        if let Some((old, reads)) = s.stale.get_mut(&number) {
            pr.head_ref_oid = old.clone();
            *reads -= 1;
            if *reads == 0 {
                s.stale.remove(&number);
            }
        }
        Ok(pr)
    }

    async fn branch_sha(&self, _: &RepoName, branch: &str) -> Result<String> {
        let s = self.state.lock().unwrap();
        Ok(if branch == "main" {
            s.trunk.clone()
        } else {
            s.branches[branch].clone()
        })
    }

    async fn required_checks(&self, _: &RepoName, _: &str) -> Result<Vec<String>> {
        Ok(self.state.lock().unwrap().required.clone())
    }

    async fn checks(&self, _: &RepoName, sha: &str) -> Result<Checks> {
        let action = self.actions.lock().unwrap().pop_front();
        let mut s = self.state.lock().unwrap();
        if let Some(action) = action {
            action(&mut s);
        }
        let by_name = s.checks_on.get(sha).unwrap_or(&s.checks).clone();
        Ok(Checks { by_name })
    }

    async fn merge(
        &self,
        _: &RepoName,
        number: PrNumber,
        sha: &str,
        _: &str,
    ) -> Result<MergeOutcome> {
        let mut s = self.state.lock().unwrap();
        if s.head(number) != sha {
            return Ok(MergeOutcome::Rejected("head moved".into()));
        }
        s.merges += 1;
        s.trunk = format!("merge-{number}");
        s.prs.remove(&number);
        Ok(MergeOutcome::Merged {
            sha: s.trunk.clone(),
        })
    }

    async fn add_label(&self, _: &RepoName, number: PrNumber, label: &str) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.prs.get_mut(&number).unwrap().labels.push(label.into());
        Ok(())
    }

    async fn remove_label(&self, _: &RepoName, number: PrNumber, label: &str) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.prs
            .get_mut(&number)
            .unwrap()
            .labels
            .retain(|l| l != label);
        Ok(())
    }

    async fn sticky_comment(&self, _: &RepoName, number: PrNumber, body: &str) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .comments
            .push((number, body.into()));
        Ok(())
    }
}

/// Publishes `<branch>-r<n>` onto the trunk, as a job would, or fails as told.
struct Scripted {
    gh: Arc<Fake>,
    rebases: Mutex<u32>,
    fail: Option<String>,
}

impl Scripted {
    fn new(gh: &Arc<Fake>) -> Self {
        Self {
            gh: gh.clone(),
            rebases: Mutex::new(0),
            fail: None,
        }
    }

    fn count(&self) -> u32 {
        *self.rebases.lock().unwrap()
    }
}

#[async_trait]
impl Rebase for Scripted {
    async fn rebase(&self, _: &LaneKey, _: &RepoSnapshot, pr: &PullRequest) -> Result<Event> {
        let n = {
            let mut n = self.rebases.lock().unwrap();
            *n += 1;
            assert!(
                *n <= 5,
                "rebased PR {} {n} times: the job is looping",
                pr.number
            );
            *n
        };
        if let Some(reason) = &self.fail {
            return Ok(Event::RebaseFailed(reason.clone()));
        }
        let mut s = self.gh.state.lock().unwrap();
        let head = format!("{}-r{n}", pr.head_ref_name);
        s.push(pr.number, &head);
        Ok(Event::Rebased {
            head,
            base: s.trunk.clone(),
        })
    }
}

/// Lanes and events in memory. The queue loop uses nothing else of the store.
#[derive(Default)]
struct Mem {
    lanes: Mutex<HashMap<LaneKey, Lane>>,
    events: Mutex<Vec<(PrNumber, String, String)>>,
}

#[async_trait]
impl Store for Mem {
    async fn load_lane(&self, key: &LaneKey) -> Result<Lane> {
        Ok(self
            .lanes
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .unwrap_or_default())
    }
    async fn save_lane(&self, key: &LaneKey, lane: &Lane) -> Result<()> {
        self.lanes.lock().unwrap().insert(key.clone(), lane.clone());
        Ok(())
    }
    async fn record(&self, _: &LaneKey, pr: PrNumber, kind: &str, message: &str) -> Result<()> {
        self.events
            .lock()
            .unwrap()
            .push((pr, kind.into(), message.into()));
        Ok(())
    }
    async fn lanes(&self, _: u64) -> Result<Vec<(LaneKey, Lane)>> {
        unreachable!()
    }
    async fn events(
        &self,
        _: u64,
        _: &RepoName,
        _: Option<PrNumber>,
        _: u32,
    ) -> Result<Vec<EventRecord>> {
        unreachable!()
    }
    async fn put_secret(&self, _: Scope, _: &str, _: SecretRecord) -> Result<()> {
        unreachable!()
    }
    async fn secret(&self, _: Scope, _: &str) -> Result<Option<SecretRecord>> {
        unreachable!()
    }
    async fn delete_secret(&self, _: Scope, _: &str) -> Result<()> {
        unreachable!()
    }
    async fn repo_settings(&self) -> Result<Vec<RepoSetting>> {
        unreachable!()
    }
    async fn set_repo_enabled(&self, _: u64, _: &RepoName, _: bool) -> Result<()> {
        unreachable!()
    }
    async fn save_workspaces(&self, _: &[Workspace]) -> Result<()> {
        unreachable!()
    }
    async fn workspaces(&self, _: &[u64]) -> Result<Vec<Workspace>> {
        unreachable!()
    }
    async fn create_session(&self, _: &str, _: &Session) -> Result<()> {
        unreachable!()
    }
    async fn session(&self, _: &str) -> Result<Option<Session>> {
        unreachable!()
    }
    async fn delete_session(&self, _: &str) -> Result<()> {
        unreachable!()
    }
}

fn ctx(store: Arc<Mem>) -> Ctx {
    Ctx {
        config: Config {
            secret_key: String::new(),
            database_url: String::new(),
            hosted_app: None,
            public_url: "http://localhost:8080".into(),
            labels: Labels {
                trigger: TRIGGER.into(),
                priority: format!("{TRIGGER}:priority"),
                failed: FAILED.into(),
            },
            data_dir: PathBuf::new(),
            poll: Duration::from_secs(60),
            bind: "127.0.0.1:0".parse().unwrap(),
            model: String::new(),
            effort: String::new(),
            max_jobs: 1,
            executor: ExecutorConfig::Local,
        },
        hosted_app: None,
        vault: Arc::new(Vault::new(store.clone(), &format!("{}=", "A".repeat(43))).unwrap()),
        store,
        executor: Arc::new(LocalExecutor {
            program: PathBuf::new(),
            timeout: Duration::ZERO,
        }),
        jobs: Semaphore::new(1),
        wakers: Arc::default(),
    }
}

struct Run {
    ctx: Ctx,
    store: Arc<Mem>,
    gh: Arc<Fake>,
    rebase: Scripted,
}

impl Run {
    fn new(gh: Arc<Fake>) -> Self {
        let store = Arc::new(Mem::default());
        Self {
            ctx: ctx(store.clone()),
            rebase: Scripted::new(&gh),
            store,
            gh,
        }
    }

    async fn tick(&self) -> Ran {
        tick(&self.ctx, &*self.gh, &self.rebase, &repo())
            .await
            .unwrap()
    }

    /// The event kinds of one PR, in order.
    fn kinds(&self, number: PrNumber) -> Vec<String> {
        self.store
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, ..)| *n == number)
            .map(|(_, kind, _)| kind.clone())
            .collect()
    }

    fn lane(&self) -> Vec<(PrNumber, String)> {
        let lanes = self.store.lanes.lock().unwrap();
        lanes
            .values()
            .flat_map(|l| &l.entries)
            .map(|e| (e.number, format!("{:?}", e.state)))
            .collect()
    }

    fn labels(&self, number: PrNumber) -> Vec<String> {
        self.gh.with(|s| s.prs[&number].labels.clone())
    }
}

#[tokio::test(start_paused = true)]
async fn a_labeled_pr_is_rebased_checked_and_merged() {
    let run = Run::new(Fake::new(|s| s.add_pr(1)));
    assert!(matches!(run.tick().await, Ran::Job));
    assert_eq!(
        run.kinds(1),
        ["enqueued", "rebasing", "published", "merged"]
    );
    assert_eq!(run.gh.with(|s| s.trunk.clone()), "merge-1");
    assert!(run.lane().is_empty());
    assert!(matches!(run.tick().await, Ran::Idle));
}

#[tokio::test(start_paused = true)]
async fn our_own_publish_is_not_an_author_push() {
    // GitHub's PR API shows the old head for a read after we publish.
    let run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.lag_reads = 1;
    }));
    run.tick().await;
    assert_eq!(run.rebase.count(), 1);
    assert_eq!(
        run.kinds(1),
        ["enqueued", "rebasing", "published", "merged"]
    );
}

#[tokio::test(start_paused = true)]
async fn an_author_push_while_checks_run_rebases_again() {
    let run = Run::new(Fake::new(|s| s.add_pr(1)));
    run.gh.then(|s| {
        s.checks_on
            .insert(s.head(1), [("ci".into(), CheckState::Pending)].into());
        s.push(1, "author-push");
    });
    run.tick().await;
    assert_eq!(run.rebase.count(), 2);
    assert_eq!(
        run.kinds(1),
        [
            "enqueued",
            "rebasing",
            "published",
            "head-moved",
            "rebasing",
            "published",
            "merged"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn the_trunk_moving_twice_outside_the_queue_ejects() {
    let run = Run::new(Fake::new(|s| s.add_pr(1)));
    for n in 1..=2 {
        run.gh.then(move |s| {
            s.checks_on
                .insert(s.head(1), [("ci".into(), CheckState::Pending)].into());
            s.trunk = format!("outside-{n}");
        });
    }
    run.tick().await;
    assert_eq!(
        run.kinds(1),
        [
            "enqueued",
            "rebasing",
            "published",
            "base-moved",
            "rebasing",
            "published",
            "base-moved",
            "ejected"
        ]
    );
    assert_eq!(run.labels(1), [FAILED]);
    assert!(run.lane().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_failed_required_check_ejects() {
    let run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.required = vec!["ci".into()];
        s.checks.insert("ci".into(), CheckState::Failed);
    }));
    run.tick().await;
    assert_eq!(run.kinds(1).last().unwrap(), "ejected");
    assert_eq!(run.labels(1), [FAILED]);
    let comment = run.gh.with(|s| s.comments.last().unwrap().1.clone());
    assert!(comment.contains("required checks failed: ci"), "{comment}");
}

#[tokio::test(start_paused = true)]
async fn a_failed_rebase_ejects() {
    let mut run = Run::new(Fake::new(|s| s.add_pr(1)));
    run.rebase.fail = Some("conflict in a.txt".into());
    run.tick().await;
    assert_eq!(run.kinds(1), ["enqueued", "rebasing", "ejected"]);
    assert_eq!(run.labels(1), [FAILED]);
}

#[tokio::test(start_paused = true)]
async fn removing_the_label_cancels_the_job() {
    let run = Run::new(Fake::new(|s| s.add_pr(1)));
    run.gh.then(|s| {
        s.checks_on
            .insert(s.head(1), [("ci".into(), CheckState::Pending)].into());
        s.prs.get_mut(&1).unwrap().labels.clear();
    });
    run.tick().await;
    assert_eq!(run.kinds(1).last().unwrap(), "cancelled");
    assert_eq!(run.gh.with(|s| s.merges), 0);
    assert!(run.lane().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_dismissed_approval_holds_the_pr_in_its_place() {
    let run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.add_pr(2);
    }));
    run.gh.then(|s| {
        s.prs.get_mut(&1).unwrap().review_decision = Some("REVIEW_REQUIRED".into());
    });
    run.tick().await;
    assert_eq!(run.kinds(1).last().unwrap(), "held");
    // The held PR keeps its place, and the next one runs.
    assert!(matches!(run.tick().await, Ran::Job));
    assert_eq!(run.kinds(2).last().unwrap(), "merged");
    assert_eq!(
        run.lane(),
        [(1, "Held(\"waiting for an approval\")".to_owned())]
    );
}

#[tokio::test(start_paused = true)]
async fn with_no_checks_at_all_it_waits_the_grace_then_merges() {
    let run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.checks.clear();
    }));
    let start = Instant::now();
    run.tick().await;
    assert!(start.elapsed() >= NO_CHECKS_GRACE, "{:?}", start.elapsed());
    assert_eq!(run.kinds(1).last().unwrap(), "merged");
}

#[tokio::test(start_paused = true)]
async fn queued_prs_merge_one_after_the_other() {
    let run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.add_pr(2);
    }));
    assert!(matches!(run.tick().await, Ran::Job));
    assert_eq!(run.gh.with(|s| s.trunk.clone()), "merge-1");
    assert!(matches!(run.tick().await, Ran::Job));
    assert_eq!(run.gh.with(|s| s.trunk.clone()), "merge-2");
    assert!(matches!(run.tick().await, Ran::Idle));
}
