//! The queue loop against a fake GitHub, a scripted rebase, and an in-memory store. Time is
//! paused, so the waits for checks take no real time.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

use mergepatrol_core::{CheckFailure, LabelSpec, Labels, ResolverSettings};
use mergepatrol_executor::LocalExecutor;
use mergepatrol_github::{BranchMerge, Checks, RequiredChecks};
use mergepatrol_store::{EventRecord, RepoState, SecretRecord, Session, Workspace};

use super::*;
use crate::config::ExecutorConfig;

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
    actions: Mutex<VecDeque<Action>>,
}

type Action = Box<dyn FnOnce(&mut State) + Send>;

#[derive(Default)]
struct State {
    /// `.mergepatrol/mergepatrol.yml` on the trunk.
    config: Option<String>,
    trunk: String,
    branches: HashMap<String, String>,
    prs: BTreeMap<PrNumber, PullRequest>,
    /// The PR API lags behind a branch update for this many reads, as GitHub's does.
    lag_reads: u32,
    stale: HashMap<PrNumber, (String, u32)>,
    required: Vec<String>,
    strict: bool,
    /// The checks on every commit, unless `checks_on` has the commit.
    checks: BTreeMap<String, CheckState>,
    checks_on: HashMap<String, BTreeMap<String, CheckState>>,
    comments: Vec<(PrNumber, String)>,
    merges: u32,
    /// Merged PRs stay, closed, as on GitHub.
    merged: Vec<PrNumber>,
    /// Trees by commit, as the PRs merged into them in order: `t-1-2`. Unknown commits are `t`.
    trees: HashMap<String, String>,
    /// The PR whose change breaks CI: every commit that has it fails `ci`.
    breaks: Option<PrNumber>,
    /// PRs that conflict when merged into a batch.
    conflicts: Vec<PrNumber>,
    /// PRs whose merge GitHub rejects.
    rejects: Vec<PrNumber>,
    /// PRs whose squash merge comes out as another tree than the batch's.
    diverges: Vec<PrNumber>,
    /// The titles of the draft PRs opened, and the branches deleted.
    drafts: Vec<String>,
    deleted: Vec<String>,
    /// The labels the repo has.
    repo_labels: Vec<String>,
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
                details: Default::default(),
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

    fn checks_of(&self, sha: &str) -> BTreeMap<String, CheckState> {
        let mut by_name = self.checks_on.get(sha).unwrap_or(&self.checks).clone();
        if self.broken(sha) {
            by_name.insert("ci".into(), CheckState::Failed);
        }
        by_name
    }

    fn tree(&self, sha: &str) -> String {
        self.trees.get(sha).cloned().unwrap_or_else(|| "t".into())
    }

    /// Whether `sha` has the breaking PR: merged into its tree, or rebased as its head.
    fn broken(&self, sha: &str) -> bool {
        self.breaks.is_some_and(|b| {
            self.tree(sha).split('-').any(|n| n == b.to_string())
                || sha.starts_with(&format!("pr-{b}-r"))
        })
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

    fn with_mut<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        f(&mut self.state.lock().unwrap())
    }
}

#[async_trait]
impl GitHub for Fake {
    fn installation_id(&self) -> u64 {
        1
    }

    async fn config_file(&self, _: &RepoName) -> Result<Option<String>> {
        Ok(self.state.lock().unwrap().config.clone())
    }

    async fn snapshot(&self, _: &RepoName, labels: &Labels) -> Result<RepoSnapshot> {
        let s = self.state.lock().unwrap();
        let label = &labels.trigger;
        Ok(RepoSnapshot {
            id: "R".into(),
            default_branch: "main".into(),
            trunk_sha: s.trunk.clone(),
            merge_method: "merge".into(),
            labeled: s
                .prs
                .values()
                .filter(|pr| pr.has_label(label) && !s.merged.contains(&pr.number))
                .cloned()
                .collect(),
            missing_labels: [&labels.trigger, &labels.priority, &labels.failed]
                .into_iter()
                .filter(|l| !s.repo_labels.contains(l))
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

    async fn required_checks(&self, _: &RepoName, _: &str) -> Result<RequiredChecks> {
        let s = self.state.lock().unwrap();
        Ok(RequiredChecks {
            names: s.required.clone(),
            strict: s.strict,
        })
    }

    async fn checks(&self, _: &RepoName, sha: &str) -> Result<Checks> {
        let action = self.actions.lock().unwrap().pop_front();
        let mut s = self.state.lock().unwrap();
        if let Some(action) = action {
            action(&mut s);
        }
        Ok(Checks {
            by_name: s.checks_of(sha),
        })
    }

    async fn check_failures(
        &self,
        _: &RepoName,
        _: &str,
        names: &[String],
    ) -> Result<Vec<CheckFailure>> {
        Ok(names
            .iter()
            .map(|name| CheckFailure {
                name: name.clone(),
                details: "it broke".into(),
            })
            .collect())
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
        // As a ruleset does: every required check must have passed on the commit.
        let checks = s.checks_of(sha);
        if let Some(missing) = s
            .required
            .iter()
            .find(|r| checks.get(*r) != Some(&CheckState::Passed))
        {
            return Ok(MergeOutcome::Rejected(format!(
                "Required status check \"{missing}\" is expected."
            )));
        }
        if s.rejects.contains(&number) {
            return Ok(MergeOutcome::Rejected("Base branch was modified".into()));
        }
        s.merges += 1;
        let mut tree = format!("{}-{number}", s.tree(&s.trunk.clone()));
        if s.diverges.contains(&number) {
            tree.push('x');
        }
        s.trunk = format!("merge-{number}");
        let trunk = s.trunk.clone();
        s.trees.insert(trunk, tree);
        s.merged.push(number);
        Ok(MergeOutcome::Merged {
            sha: s.trunk.clone(),
        })
    }

    async fn create_branch(&self, _: &RepoName, branch: &str, sha: &str) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        assert!(!s.branches.contains_key(branch), "{branch} exists");
        s.branches.insert(branch.into(), sha.into());
        Ok(())
    }

    async fn delete_branch(&self, _: &RepoName, branch: &str) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        if s.branches.remove(branch).is_some() {
            s.deleted.push(branch.into());
        }
        Ok(())
    }

    async fn merge_into_branch(
        &self,
        _: &RepoName,
        branch: &str,
        head: &str,
        _: &str,
    ) -> Result<Option<BranchMerge>> {
        let mut s = self.state.lock().unwrap();
        let number = *s
            .prs
            .keys()
            .find(|n| s.head(**n) == head)
            .expect("a PR head");
        if s.conflicts.contains(&number) {
            return Ok(None);
        }
        let tree = format!("{}-{number}", s.tree(&s.branches[branch]));
        let sha = format!("{branch}@{tree}");
        s.trees.insert(sha.clone(), tree.clone());
        s.branches.insert(branch.into(), sha.clone());
        Ok(Some(BranchMerge { sha, tree }))
    }

    async fn open_draft_pr(
        &self,
        _: &RepoName,
        head: &str,
        base: &str,
        title: &str,
        _: &str,
    ) -> Result<PrNumber> {
        let mut s = self.state.lock().unwrap();
        assert!(s.branches.contains_key(head) && base == "main");
        s.drafts.push(title.into());
        Ok(1000 + s.drafts.len() as PrNumber)
    }

    async fn commit_tree(&self, _: &RepoName, sha: &str) -> Result<String> {
        Ok(self.state.lock().unwrap().tree(sha))
    }

    async fn create_label(&self, _: &RepoName, label: &LabelSpec<'_>) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        assert!(!s.repo_labels.iter().any(|l| l == label.name));
        s.repo_labels.push(label.name.into());
        Ok(())
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

/// Publishes `<branch>-r<n>` onto the trunk, as a job would, or fails as told. A fix publishes
/// `<head>-f<n>`, whose checks pass if `fix_works`.
struct Scripted {
    gh: Arc<Fake>,
    rebases: Mutex<u32>,
    fail: Option<String>,
    fixes: Mutex<Vec<Vec<String>>>,
    fix_works: bool,
    fix_fails: Option<String>,
}

impl Scripted {
    fn new(gh: &Arc<Fake>) -> Self {
        Self {
            gh: gh.clone(),
            rebases: Mutex::new(0),
            fail: None,
            fixes: Mutex::new(Vec::new()),
            fix_works: true,
            fix_fails: None,
        }
    }

    /// The failed checks each fix was asked to fix.
    fn fixed(&self) -> Vec<Vec<String>> {
        self.fixes.lock().unwrap().clone()
    }

    fn count(&self) -> u32 {
        *self.rebases.lock().unwrap()
    }
}

#[async_trait]
impl Sandbox for Scripted {
    async fn rebase(
        &self,
        _: &LaneKey,
        _: &RepoSnapshot,
        pr: &PullRequest,
        _: &RepoSettings,
    ) -> Result<Event> {
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

    async fn fix(
        &self,
        _: &LaneKey,
        _: &RepoSnapshot,
        pr: &PullRequest,
        _: &RepoSettings,
        head: &str,
        _: &str,
        failures: &[CheckFailure],
    ) -> Result<Event> {
        let n = {
            let mut fixes = self.fixes.lock().unwrap();
            fixes.push(failures.iter().map(|f| f.name.clone()).collect());
            fixes.len()
        };
        if let Some(reason) = &self.fix_fails {
            return Ok(Event::FixFailed(reason.clone()));
        }
        let mut s = self.gh.state.lock().unwrap();
        let fixed = format!("{head}-f{n}");
        s.push(pr.number, &fixed);
        if self.fix_works {
            let passing = BTreeMap::from([("ci".to_owned(), CheckState::Passed)]);
            s.checks_on.insert(fixed.clone(), passing);
        }
        Ok(Event::Fixed { head: fixed })
    }
}

/// Lanes and events in memory. The queue loop uses nothing else of the store.
#[derive(Default)]
struct Mem {
    lanes: Mutex<HashMap<LaneKey, Lane>>,
    events: Mutex<Vec<(PrNumber, String, String)>>,
    config_error: Mutex<Option<String>>,
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
    async fn repos(&self) -> Result<Vec<RepoState>> {
        unreachable!()
    }
    async fn set_repo_enabled(&self, _: u64, _: &RepoName, _: Option<bool>) -> Result<()> {
        unreachable!()
    }
    async fn set_has_config(&self, _: u64, _: &RepoName, _: bool) -> Result<()> {
        unreachable!()
    }
    async fn replace_has_config(&self, _: u64, _: &[RepoName]) -> Result<()> {
        unreachable!()
    }
    async fn set_config_error(&self, _: u64, _: &RepoName, error: Option<&str>) -> Result<()> {
        *self.config_error.lock().unwrap() = error.map(str::to_owned);
        Ok(())
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
            repo_defaults: RepoSettings {
                label: TRIGGER.into(),
                // One PR at a time, unless a test batches.
                batch_size: 1,
                resolver: ResolverSettings {
                    model: "m".into(),
                    effort: "medium".into(),
                },
                verify: Vec::new(),
                fix_ci: Default::default(),
            },
            data_dir: PathBuf::new(),
            poll: Duration::from_secs(60),
            bind: "127.0.0.1:0".parse().unwrap(),
            max_jobs: 1,
            executor: ExecutorConfig::Local,
            posthog_key: None,
            posthog_host: String::new(),
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
        analytics: Analytics::default(),
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

    fn batching(gh: Arc<Fake>) -> Self {
        let mut run = Self::new(gh);
        run.ctx.config.repo_defaults.batch_size = 8;
        run
    }

    /// Ticks until the lane is idle.
    async fn drain(&self) {
        for _ in 0..20 {
            if matches!(self.tick().await, Ran::Idle) {
                return;
            }
        }
        panic!("the lane does not drain: {:?}", self.lane());
    }

    fn merged(&self) -> Vec<PrNumber> {
        self.gh.with(|s| s.merged.clone())
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
async fn the_config_file_sets_the_label() {
    let run = Run::new(Fake::new(|s| {
        s.config = Some("version: 1\nlabel: ship\n".into());
        s.add_pr(1);
        s.add_pr(2);
        s.prs.get_mut(&2).unwrap().labels = vec!["ship".into()];
    }));
    run.drain().await;
    assert_eq!(run.merged(), [2]);
}

#[tokio::test(start_paused = true)]
async fn the_queue_labels_are_created_once() {
    let run = Run::new(Fake::new(|s| {
        s.config = Some("version: 1\nlabel: ship\n".into());
        s.repo_labels = vec!["ship:priority".into()];
    }));
    run.tick().await;
    run.tick().await;
    assert_eq!(
        run.gh.with(|s| s.repo_labels.clone()),
        ["ship:priority", "ship", "ship:failed"]
    );
}

#[tokio::test(start_paused = true)]
async fn a_config_file_that_does_not_parse_pauses_the_queue() {
    let run = Run::new(Fake::new(|s| {
        s.config = Some("version: 1\nbatchsize: 2\n".into());
        s.add_pr(1);
    }));
    assert!(matches!(run.tick().await, Ran::Idle));
    assert!(run.kinds(1).is_empty());
    let error = run.store.config_error.lock().unwrap().clone();
    assert!(error.is_some_and(|e| e.contains("batchsize")));

    run.gh.state.lock().unwrap().config = Some("version: 1\n".into());
    run.drain().await;
    assert_eq!(run.merged(), [1]);
    assert_eq!(*run.store.config_error.lock().unwrap(), None);
}

#[tokio::test(start_paused = true)]
async fn a_merged_pr_loses_the_queue_labels() {
    let run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.prs
            .get_mut(&1)
            .unwrap()
            .labels
            .push(format!("{TRIGGER}:priority"));
    }));
    run.tick().await;
    assert_eq!(run.kinds(1).last().unwrap(), "merged");
    assert!(run.labels(1).is_empty(), "{:?}", run.labels(1));
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
        s.config = Some("version: 1\nfix_ci: { enabled: false }\n".into());
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

fn failing_ci(s: &mut State) {
    s.add_pr(1);
    s.required = vec!["ci".into()];
    s.checks.insert("ci".into(), CheckState::Failed);
}

#[tokio::test(start_paused = true)]
async fn a_failed_required_check_is_fixed_then_merged() {
    let run = Run::new(Fake::new(failing_ci));
    run.tick().await;
    assert_eq!(run.rebase.fixed(), [["ci"]]);
    assert_eq!(
        run.kinds(1),
        [
            "enqueued",
            "rebasing",
            "published",
            "fixing",
            "published",
            "merged"
        ]
    );
    assert_eq!(run.merged(), [1]);
}

/// The commit has only a quick app's check at first. CI's `ci` turns up `after` polls later.
fn quick_check_first(ci: CheckState, after: usize, run: &Run) {
    run.gh
        .with_mut(|s| s.checks = BTreeMap::from([("socket".into(), CheckState::Passed)]));
    for _ in 0..after {
        run.gh.then(|_| {});
    }
    run.gh.then(move |s| {
        s.checks.insert("ci".into(), ci);
    });
}

#[tokio::test(start_paused = true)]
async fn a_ruleset_added_while_a_pr_waits_holds_its_merge() {
    let run = Run::new(Fake::new(|s| s.add_pr(1)));
    run.gh.then(|s| s.required = vec!["ci".into()]);
    quick_check_first(CheckState::Passed, 10, &run);
    run.tick().await;
    assert_eq!(run.merged(), [1]);
}

#[tokio::test(start_paused = true)]
async fn fixes_that_do_not_mend_the_checks_eject_after_the_attempts() {
    let mut run = Run::new(Fake::new(failing_ci));
    run.rebase.fix_works = false;
    run.tick().await;
    assert_eq!(run.rebase.fixed().len(), 2);
    assert_eq!(run.labels(1), [FAILED]);
    let comment = run.gh.with(|s| s.comments.last().unwrap().1.clone());
    assert!(
        comment.contains("required checks failed: ci, after 2 fixes"),
        "{comment}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_config_file_sets_the_fix_attempts() {
    let mut run = Run::new(Fake::new(|s| {
        failing_ci(s);
        s.config = Some("version: 1\nfix_ci: { attempts: 1 }\n".into());
    }));
    run.rebase.fix_works = false;
    run.tick().await;
    assert_eq!(run.rebase.fixed().len(), 1);
    assert_eq!(run.labels(1), [FAILED]);
}

#[tokio::test(start_paused = true)]
async fn a_fix_the_agent_gives_up_on_ejects_with_its_reason() {
    let mut run = Run::new(Fake::new(failing_ci));
    run.rebase.fix_fails = Some("the agent gave up: a flaky test".into());
    run.tick().await;
    assert_eq!(run.labels(1), [FAILED]);
    let comment = run.gh.with(|s| s.comments.last().unwrap().1.clone());
    assert!(
        comment.contains("the fix failed: the agent gave up: a flaky test"),
        "{comment}"
    );
}

#[tokio::test(start_paused = true)]
async fn without_required_checks_a_quick_green_check_waits_for_the_rest() {
    let run = Run::new(Fake::new(|s| {
        s.config = Some("version: 1\nfix_ci: { enabled: false }\n".into());
        s.add_pr(1);
    }));
    quick_check_first(CheckState::Failed, 3, &run);
    run.tick().await;
    assert!(run.merged().is_empty());
    assert_eq!(run.kinds(1).last().unwrap(), "ejected");
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
        [(1, "Held(\"waiting for a required review\")".to_owned())]
    );
}

#[tokio::test(start_paused = true)]
async fn a_pr_that_needs_a_review_is_held_and_says_so_until_approved() {
    let run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.prs.get_mut(&1).unwrap().review_decision = Some("REVIEW_REQUIRED".into());
    }));
    assert!(matches!(run.tick().await, Ran::Idle));
    assert_eq!(run.kinds(1), ["enqueued", "held"]);
    let comment = run.gh.with(|s| s.comments.last().unwrap().1.clone());
    assert!(comment.contains("On hold: waiting for a required review"));
    // Each tick after says nothing new.
    run.tick().await;
    assert_eq!(run.gh.with(|s| s.comments.len()), 1);

    run.gh
        .state
        .lock()
        .unwrap()
        .prs
        .get_mut(&1)
        .unwrap()
        .review_decision = Some("APPROVED".into());
    assert!(matches!(run.tick().await, Ran::Job));
    assert_eq!(&run.kinds(1)[2..4], ["released", "rebasing"]);
    assert_eq!(run.merged(), [1]);
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

fn prs(numbers: &[PrNumber]) -> impl FnOnce(&mut State) {
    let numbers = numbers.to_vec();
    move |s| {
        s.required = vec!["ci".into()];
        for n in numbers {
            s.add_pr(n);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn green_prs_are_tested_together_and_merged_without_a_rebase() {
    let run = Run::batching(Fake::new(prs(&[1, 2, 3])));
    assert!(matches!(run.tick().await, Ran::Job));
    assert_eq!(run.merged(), [1, 2, 3]);
    assert_eq!(run.rebase.count(), 0);
    assert_eq!(run.gh.with(|s| s.tree(&s.trunk)), "t-1-2-3");
    assert_eq!(
        run.gh.with(|s| s.drafts.clone()),
        ["PR 1 (+2 in a Mergepatrol batch)"]
    );
    // The batch's branch is gone, which closes its draft PR.
    assert_eq!(
        run.gh.with(|s| s.deleted.clone()),
        ["mergepatrol/batch-1-2-3"]
    );
    assert_eq!(run.kinds(2), ["enqueued", "batch", "merged"]);
    assert!(run.lane().is_empty());
    let lane = run
        .store
        .lanes
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    assert_eq!(lane.batch_branch, None);
    assert_eq!(lane.batch_pr, None);
}

#[tokio::test(start_paused = true)]
async fn a_failed_batch_is_bisected_to_the_pr_that_broke_it() {
    let run = Run::batching(Fake::new(|s| {
        prs(&[1, 2, 3, 4])(s);
        s.breaks = Some(3);
    }));
    run.drain().await;
    assert_eq!(run.merged(), [1, 2, 4]);
    assert_eq!(run.labels(3), [FAILED]);
    // 1-2-3-4 failed, 1-2 passed, 3-4 failed, 3 alone failed, its fixes too, then 4 alone.
    assert_eq!(run.gh.with(|s| s.drafts.len()), 3);
    assert_eq!(
        run.kinds(3),
        [
            "enqueued",
            "batch",
            "batch-failed",
            "batch",
            "batch-failed",
            "rebasing",
            "published",
            "fixing",
            "published",
            "fixing",
            "published",
            "ejected"
        ]
    );
    let lane = run
        .store
        .lanes
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    assert_eq!(lane.bisect, None);
}

#[tokio::test(start_paused = true)]
async fn a_strict_trunk_merges_one_pr_at_a_time() {
    let run = Run::batching(Fake::new(|s| {
        prs(&[1, 2])(s);
        s.strict = true;
    }));
    run.drain().await;
    assert_eq!(run.merged(), [1, 2]);
    assert_eq!(run.rebase.count(), 2);
    assert!(run.gh.with(|s| s.drafts.is_empty()));
}

#[tokio::test(start_paused = true)]
async fn a_pr_whose_own_checks_have_not_passed_runs_alone() {
    let run = Run::batching(Fake::new(|s| {
        prs(&[1, 2, 3])(s);
        let head = s.head(1);
        s.checks_on
            .insert(head, [("ci".into(), CheckState::Failed)].into());
    }));
    run.tick().await;
    // Rebased onto the trunk, where its checks run again.
    assert_eq!(run.merged(), [1]);
    assert_eq!(run.rebase.count(), 1);
    run.tick().await;
    assert_eq!(run.merged(), [1, 2, 3]);
    assert_eq!(run.rebase.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_batch_stops_at_the_first_conflict() {
    let run = Run::batching(Fake::new(|s| {
        prs(&[1, 2, 3, 4])(s);
        s.conflicts = vec![3];
    }));
    run.tick().await;
    assert_eq!(run.merged(), [1, 2]);
    assert_eq!(
        run.gh.with(|s| s.drafts.clone()),
        ["PR 1 (+1 in a Mergepatrol batch)"]
    );
    // 3 conflicts even as the first of a batch, so it is rebased alone.
    run.tick().await;
    assert_eq!(run.merged(), [1, 2, 3]);
    assert_eq!(run.rebase.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_trunk_moving_while_a_batch_is_tested_builds_it_again() {
    let run = Run::batching(Fake::new(prs(&[1, 2])));
    // The checks of both PRs are read, then the batch's.
    const BATCH: &str = "mergepatrol/batch-1-2@t-1-2";
    run.gh.then(|_| {});
    run.gh.then(|_| {});
    run.gh.then(|s| {
        s.checks_on
            .insert(BATCH.into(), [("ci".into(), CheckState::Pending)].into());
        s.trunk = "outside".into();
    });
    run.gh.then(|s| {
        s.checks_on.remove(BATCH);
    });
    run.tick().await;
    assert!(run.merged().is_empty());
    assert_eq!(run.kinds(1), ["enqueued", "batch", "batch-stopped"]);
    assert_eq!(run.lane().len(), 2);
    run.tick().await;
    assert_eq!(run.merged(), [1, 2]);
}

#[tokio::test(start_paused = true)]
async fn a_merge_that_leaves_another_tree_stops_the_batch() {
    let run = Run::batching(Fake::new(|s| {
        prs(&[1, 2, 3])(s);
        s.diverges = vec![1];
    }));
    run.tick().await;
    assert_eq!(run.merged(), [1]);
    assert_eq!(run.kinds(1).last().unwrap(), "diverged");
    assert_eq!(
        run.lane(),
        [(2, "Waiting".to_owned()), (3, "Waiting".to_owned())]
    );
}

#[tokio::test(start_paused = true)]
async fn a_rejected_merge_ejects_that_pr_and_stops_the_batch() {
    let run = Run::batching(Fake::new(|s| {
        prs(&[1, 2, 3])(s);
        s.rejects = vec![2];
    }));
    let start = Instant::now();
    run.tick().await;
    assert_eq!(run.merged(), [1]);
    assert_eq!(run.labels(2), [FAILED]);
    assert!(start.elapsed() >= MERGE_RETRY_AFTER * MERGE_RETRIES);
    assert_eq!(run.lane(), [(3, "Waiting".to_owned())]);
}

#[tokio::test(start_paused = true)]
async fn a_batch_cut_short_by_a_restart_is_built_again() {
    let run = Run::batching(Fake::new(prs(&[1, 2])));
    let key = LaneKey {
        installation_id: 1,
        repo: repo(),
        trunk: "main".into(),
    };
    let mut lane = Lane::default();
    lane.sync(&[
        Candidate {
            number: 1,
            priority: false,
            hold: None,
            summary: Default::default(),
        },
        Candidate {
            number: 2,
            priority: false,
            hold: None,
            summary: Default::default(),
        },
    ]);
    lane.activate(1);
    lane.activate(2);
    lane.batch_branch = Some("mergepatrol/batch-1-2".into());
    run.store.lanes.lock().unwrap().insert(key, lane);
    run.gh
        .state
        .lock()
        .unwrap()
        .branches
        .insert("mergepatrol/batch-1-2".into(), "stale".into());
    run.tick().await;
    assert_eq!(run.merged(), [1, 2]);
}

/// Ticks until the batch of 1 and 2 waits for its checks, then drops the tick, as a restart
/// does.
async fn cut_short_while_testing(run: &Run) {
    const BATCH: &str = "mergepatrol/batch-1-2@t-1-2";
    run.gh.with_mut(|s| {
        s.checks_on
            .insert(BATCH.into(), [("ci".into(), CheckState::Pending)].into())
    });
    let cut = tokio::time::timeout(CHECKS_POLL * 4, run.tick()).await;
    assert!(cut.is_err(), "the batch is still testing");
    run.gh.with_mut(|s| s.checks_on.remove(BATCH));
}

#[tokio::test(start_paused = true)]
async fn a_batch_cut_short_by_a_restart_picks_up_where_it_was() {
    let run = Run::batching(Fake::new(prs(&[1, 2])));
    cut_short_while_testing(&run).await;
    run.tick().await;
    assert_eq!(run.merged(), [1, 2]);
    // The same branch and draft PR, which CI kept testing.
    assert_eq!(run.gh.with(|s| s.drafts.len()), 1);
    assert_eq!(
        run.gh.with(|s| s.deleted.clone()),
        ["mergepatrol/batch-1-2"]
    );
    assert_eq!(
        run.kinds(1),
        ["enqueued", "batch", "batch-resumed", "merged"]
    );
}

#[tokio::test(start_paused = true)]
async fn a_batch_whose_trunk_moved_during_a_restart_is_built_again() {
    let run = Run::batching(Fake::new(prs(&[1, 2])));
    cut_short_while_testing(&run).await;
    run.gh.with_mut(|s| s.trunk = "outside".into());
    run.tick().await;
    assert_eq!(run.merged(), [1, 2]);
    assert_eq!(run.gh.with(|s| s.drafts.len()), 2);
    assert_eq!(run.kinds(1), ["enqueued", "batch", "batch", "merged"]);
}
