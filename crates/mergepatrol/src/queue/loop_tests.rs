//! The queue loop against a fake GitHub, a scripted rebase, and an in-memory store. Time is
//! paused, so the waits for checks take no real time.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

use mergepatrol_core::{CheckFailure, LabelSpec, Labels, ResolverSettings, Step, StepTimes};
use mergepatrol_executor::LocalExecutor;
use mergepatrol_executor::proxy::Upstreams;
use mergepatrol_github::{
    BranchMerge, Checks, HeadRepository, RepoOwner, RequiredChecks, StackRef,
};
use mergepatrol_store::{
    EventRecord, ExitQuery, ExitRecord, ExitStats, RepoState, SecretRecord, Session, StepStats,
    Workspace,
};

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
    /// Each stack's PRs, bottom to top.
    stacks: BTreeMap<PrNumber, Vec<PrNumber>>,
    /// How many PR reads fail next, as in a GitHub outage.
    outage: u32,
    /// Whether the next merge never answers, as when a restart cuts the wait short. `Merged`:
    /// GitHub merges it meanwhile; `Pending`: not yet.
    merge_hangs: Option<MergeHang>,
    /// Each merged PR's merge commit.
    merge_commits: HashMap<PrNumber, String>,
    /// Heads that need a rebase onto the trunk. Every other head is on it.
    behind: Vec<String>,
    /// Heads whose workflow runs wait for a maintainer's approval.
    awaiting_approval: Vec<String>,
    /// PRs that change workflow files.
    change_workflows: Vec<PrNumber>,
}

#[derive(Clone, Copy)]
enum MergeHang {
    Merged,
    Pending,
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
                stack: None,
                maintainer_can_modify: false,
                head_repository: None,
                details: Default::default(),
            },
        );
    }

    /// A PR from a user's fork, whose author allows edits by maintainers.
    fn add_fork_pr(&mut self, number: PrNumber) {
        self.add_pr(number);
        let pr = self.prs.get_mut(&number).unwrap();
        pr.is_cross_repository = true;
        pr.maintainer_can_modify = true;
        pr.head_repository = Some(HeadRepository {
            name_with_owner: "someone/r".into(),
            owner: RepoOwner {
                kind: "User".into(),
            },
        });
    }

    /// Stacks the PRs `layers`, bottom to top, as stack `number`. Only `labeled` keep the label.
    fn add_stack(&mut self, number: PrNumber, layers: &[PrNumber], labeled: &[PrNumber]) {
        let mut base = "main".to_owned();
        for n in layers {
            self.add_pr(*n);
            let pr = self.prs.get_mut(n).unwrap();
            pr.base_ref_name = base;
            pr.stack = Some(StackRef {
                number,
                base_ref_name: "main".into(),
            });
            if !labeled.contains(n) {
                pr.labels.clear();
            }
            base = pr.head_ref_name.clone();
        }
        self.stacks.insert(number, layers.to_vec());
    }

    /// The layers of `number`'s stack up to it, or `number` alone.
    fn up_to(&self, number: PrNumber) -> Vec<PrNumber> {
        match &self.prs[&number].stack {
            Some(stack) => {
                let layers = &self.stacks[&stack.number];
                let top = layers.iter().position(|n| *n == number).unwrap();
                layers[..=top].to_vec()
            }
            None => vec![number],
        }
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

    /// As the asynchronous merge does: a stacked PR merges with every layer below it, or none.
    fn merge_now(&self, number: PrNumber, sha: &str, _: &str) -> Result<MergeOutcome> {
        let mut s = self.state.lock().unwrap();
        if s.head(number) != sha {
            return Ok(MergeOutcome::Rejected("head moved".into()));
        }
        let layers: Vec<PrNumber> = s
            .up_to(number)
            .into_iter()
            .filter(|n| !s.merged.contains(n))
            .collect();
        // As a ruleset does: every required check must have passed on each layer's head.
        for n in &layers {
            let checks = s.checks_of(&s.head(*n));
            if let Some(missing) = s
                .required
                .iter()
                .find(|r| checks.get(*r) != Some(&CheckState::Passed))
            {
                return Ok(MergeOutcome::Rejected(format!(
                    "Required status check \"{missing}\" is expected."
                )));
            }
            if s.rejects.contains(n) {
                return Ok(MergeOutcome::Rejected("Base branch was modified".into()));
            }
        }
        s.merges += 1;
        let mut tree = s.tree(&s.trunk.clone());
        for n in &layers {
            tree = format!("{tree}-{n}");
            if s.diverges.contains(n) {
                tree.push('x');
            }
        }
        s.trunk = format!("merge-{number}");
        let trunk = s.trunk.clone();
        s.trees.insert(trunk.clone(), tree);
        for n in &layers {
            s.merge_commits.insert(*n, trunk.clone());
        }
        s.merged.extend(layers);
        Ok(MergeOutcome::Merged {
            sha: s.trunk.clone(),
        })
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
        if s.outage > 0 {
            s.outage -= 1;
            bail!("GET /pulls/{number}: 502 Bad Gateway");
        }
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

    async fn stack_layers(&self, _: &RepoName, stack: PrNumber) -> Result<Vec<PrNumber>> {
        let s = self.state.lock().unwrap();
        Ok(s.stacks[&stack]
            .iter()
            .copied()
            .filter(|n| !s.merged.contains(n))
            .collect())
    }

    async fn branch_sha(&self, _: &RepoName, branch: &str) -> Result<String> {
        let s = self.state.lock().unwrap();
        Ok(if branch == "main" {
            s.trunk.clone()
        } else {
            s.branches[branch].clone()
        })
    }

    async fn pull_head(&self, _: &RepoName, number: PrNumber) -> Result<String> {
        Ok(self.state.lock().unwrap().head(number))
    }

    async fn behind(&self, _: &RepoName, _: &str, head: &str) -> Result<bool> {
        Ok(self.state.lock().unwrap().behind.iter().any(|h| h == head))
    }

    async fn awaiting_approval(&self, _: &RepoName, sha: &str) -> Result<bool> {
        let s = self.state.lock().unwrap();
        Ok(s.awaiting_approval.iter().any(|h| h == sha))
    }

    async fn changes_under(&self, _: &RepoName, number: PrNumber, dir: &str) -> Result<bool> {
        assert_eq!(dir, ".github/workflows/");
        Ok(self
            .state
            .lock()
            .unwrap()
            .change_workflows
            .contains(&number))
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
        method: &str,
    ) -> Result<MergeOutcome> {
        let hang = self.state.lock().unwrap().merge_hangs.take();
        if let Some(hang) = hang {
            if let MergeHang::Merged = hang {
                self.merge_now(number, sha, method)?;
            }
            std::future::pending::<()>().await;
        }
        self.merge_now(number, sha, method)
    }

    async fn merged_as(
        &self,
        _: &RepoName,
        number: PrNumber,
        head: &str,
    ) -> Result<Option<String>> {
        let s = self.state.lock().unwrap();
        Ok(s.merge_commits
            .get(&number)
            .filter(|_| s.head(number) == head)
            .cloned())
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
        let layers = s.up_to(number);
        if layers.iter().any(|n| s.conflicts.contains(n)) {
            return Ok(None);
        }
        let mut tree = s.tree(&s.branches[branch]);
        for n in layers {
            tree = format!("{tree}-{n}");
        }
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
        unit: &Unit,
        _: &RepoSettings,
    ) -> Result<Event> {
        let n = {
            let mut n = self.rebases.lock().unwrap();
            *n += 1;
            assert!(
                *n <= 5,
                "rebased {} {n} times: the job is looping",
                unit.refs()
            );
            *n
        };
        if let Some(reason) = &self.fail {
            return Ok(Event::RebaseFailed(reason.clone()));
        }
        let mut s = self.gh.state.lock().unwrap();
        let mut head = String::new();
        for pr in &unit.layers {
            head = format!("{}-r{n}", pr.head_ref_name);
            s.push(pr.number, &head);
        }
        Ok(Event::Rebased {
            head,
            base: s.trunk.clone(),
        })
    }

    async fn fix(
        &self,
        _: &LaneKey,
        _: &RepoSnapshot,
        unit: &Unit,
        layer: usize,
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
        let mut top = format!("{head}-f{n}");
        s.push(unit.layers[layer].number, &top);
        if self.fix_works {
            let passing = BTreeMap::from([("ci".to_owned(), CheckState::Passed)]);
            s.checks_on.insert(top.clone(), passing);
        }
        // The layers above go onto the fix.
        for pr in &unit.layers[layer + 1..] {
            top = format!("{}-s{n}", s.head(pr.number));
            s.push(pr.number, &top);
        }
        Ok(Event::Fixed { head: top })
    }
}

/// Lanes, events and exits in memory. The queue loop uses nothing else of the store.
#[derive(Default)]
struct Mem {
    lanes: Mutex<HashMap<LaneKey, Lane>>,
    events: Mutex<Vec<(PrNumber, String, String)>>,
    /// Each exit's PR, outcome, and title.
    exits: Mutex<Vec<(PrNumber, String, String)>>,
    config_error: Mutex<Option<String>>,
    /// The head each PR had when the trigger label went on, as the webhook told.
    label_heads: Mutex<HashMap<PrNumber, String>>,
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
    async fn record_exit(
        &self,
        _: &LaneKey,
        pr: PrNumber,
        outcome: &str,
        _: &str,
        summary: &PrSummary,
    ) -> Result<Option<StepTimes>> {
        let exit = (pr, outcome.into(), summary.title.clone());
        self.exits.lock().unwrap().push(exit);
        Ok(None)
    }
    async fn exits(&self, _: &ExitQuery<'_>) -> Result<Vec<ExitRecord>> {
        unreachable!()
    }
    async fn exit_stats(&self, _: u64, _: u32, _: u32) -> Result<Vec<ExitStats>> {
        unreachable!()
    }
    async fn step_stats(&self, _: u64, _: Option<&RepoName>, _: u32) -> Result<Vec<StepStats>> {
        unreachable!()
    }
    async fn merged_by_day(&self, _: u64, _: Option<&RepoName>, _: u32) -> Result<Vec<u64>> {
        unreachable!()
    }
    async fn put_secret(&self, _: Scope, _: &str, _: SecretRecord) -> Result<()> {
        unreachable!()
    }
    /// None: no self-hosted App, so no webhooks unless a test sets the hosted one.
    async fn secret(&self, _: Scope, _: &str) -> Result<Option<SecretRecord>> {
        Ok(None)
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
    async fn set_label_head(
        &self,
        _: u64,
        _: &RepoName,
        _: PrNumber,
        _: &str,
        _: &str,
    ) -> Result<()> {
        unreachable!()
    }
    async fn label_head(
        &self,
        _: u64,
        _: &RepoName,
        pr: PrNumber,
        label: &str,
    ) -> Result<Option<String>> {
        assert_eq!(label, TRIGGER);
        Ok(self.label_heads.lock().unwrap().get(&pr).cloned())
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
            job_proxy_url: "http://localhost:8080".into(),
            job_proxy_bind: None,
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
        proxy: Arc::new(ApiProxy::new(Upstreams::default())),
        jobs: Semaphore::new(1),
        wakers: Arc::default(),
        analytics: Analytics::default(),
        prs: Default::default(),
        job_errors: Default::default(),
        unpinned: Default::default(),
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
        self.try_tick().await.unwrap()
    }

    async fn try_tick(&self) -> Result<Ran> {
        tick(&self.ctx, &*self.gh, &self.rebase, &repo()).await
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

    fn exits(&self) -> Vec<(PrNumber, String, String)> {
        self.store.exits.lock().unwrap().clone()
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
async fn only_the_sandbox_takes_a_job_slot() {
    // Other repos' jobs hold every slot. The scripted sandbox takes none, so the PR's wait for
    // its checks, and its merge, must not either.
    let mut run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.checks.insert("ci".into(), CheckState::Pending);
        s.required = vec!["ci".into()];
    }));
    run.ctx.jobs = Semaphore::new(0);
    run.gh.then(|_| {});
    run.gh.then(|s| {
        s.checks.insert("ci".into(), CheckState::Passed);
    });
    let ran = tokio::time::timeout(Duration::from_secs(3600), run.tick()).await;
    assert!(matches!(ran, Ok(Ran::Job)), "the job waited for a slot");
    assert_eq!(run.merged(), [1]);
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

#[tokio::test(start_paused = true)]
async fn a_required_check_that_never_reports_ejects_without_a_fix() {
    let run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.required = vec!["ci".into(), "e2e".into()];
    }));
    let start = tokio::time::Instant::now();
    run.tick().await;
    assert!(start.elapsed() >= CHECKS_TIMEOUT);
    assert_eq!(
        run.kinds(1),
        ["enqueued", "rebasing", "published", "ejected"]
    );
    assert!(run.rebase.fixed().is_empty());
    assert_eq!(run.labels(1), [FAILED]);
    let comment = run.gh.with(|s| s.comments.last().unwrap().1.clone());
    assert!(
        comment.contains("required checks did not finish within 3 hours: e2e"),
        "{comment}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_batch_whose_checks_never_finish_counts_as_failed() {
    let run = Run::batching(Fake::new(|s| {
        prs(&[1, 2])(s);
        s.required = vec!["ci".into(), "e2e".into()];
        // Each PR passed both on its own; the batch never gets `e2e`.
        let both = BTreeMap::from([
            ("ci".to_owned(), CheckState::Passed),
            ("e2e".to_owned(), CheckState::Passed),
        ]);
        for n in [1, 2] {
            s.checks_on.insert(format!("pr-{n}-v0"), both.clone());
        }
    }));
    run.tick().await;
    let failed: Vec<String> = run
        .store
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|(n, kind, _)| *n == 1 && kind == "batch-failed")
        .map(|(.., message)| message.clone())
        .collect();
    assert_eq!(
        failed,
        ["required checks did not finish within 3 hours: e2e"]
    );
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

/// The history splits a PR's time by the events the queue records: a renamed event would leave
/// its step at zero.
#[tokio::test(start_paused = true)]
async fn each_step_of_a_job_has_an_event_that_begins_it() {
    let run = Run::new(Fake::new(failing_ci));
    run.tick().await;
    let kinds = run.kinds(1);
    let events: Vec<(f64, &str)> = kinds
        .iter()
        .enumerate()
        .map(|(i, k)| (i as f64, k.as_str()))
        .collect();
    let times = mergepatrol_core::step_times(&events);
    for step in [Step::Waiting, Step::Rebasing, Step::Checks, Step::Fixing] {
        assert!(times[&step] > 0.0, "{step:?} in {kinds:?}");
    }
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
async fn a_github_outage_keeps_the_pr_in_its_place() {
    let run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.outage = 1;
    }));
    let Err(err) = run.try_tick().await else {
        panic!("the tick passed during the outage");
    };
    assert!(format!("{err:#}").contains("502"), "{err:#}");
    assert_eq!(run.lane(), [(1, "Active".into())]);
    assert_eq!(run.labels(1), [TRIGGER]);

    assert!(matches!(run.tick().await, Ran::Job));
    assert_eq!(run.merged(), [1]);
    assert_eq!(run.exits().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_pr_queued_again_gets_a_new_window_for_errors() {
    let run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.outage = 1;
    }));
    assert!(run.try_tick().await.is_err());
    run.gh
        .with_mut(|s| s.prs.get_mut(&1).unwrap().labels.clear());
    run.tick().await;
    tokio::time::advance(JOB_ERRORS_FOR).await;
    run.gh.with_mut(|s| {
        s.prs.get_mut(&1).unwrap().labels.push(TRIGGER.into());
        s.outage = 1;
    });
    assert!(run.try_tick().await.is_err());
    assert_eq!(run.lane(), [(1, "Active".into())]);
}

#[tokio::test(start_paused = true)]
async fn a_pr_whose_job_keeps_failing_is_ejected() {
    let run = Run::new(Fake::new(|s| {
        s.add_pr(1);
        s.outage = u32::MAX;
    }));
    assert!(run.try_tick().await.is_err());
    tokio::time::advance(JOB_ERRORS_FOR - Duration::from_secs(1)).await;
    assert!(run.try_tick().await.is_err());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(run.tick().await, Ran::Job));
    assert_eq!(run.kinds(1), ["enqueued", "ejected"]);
    assert_eq!(run.labels(1), [FAILED]);
}

#[tokio::test(start_paused = true)]
async fn the_history_keeps_each_pr_that_leaves_the_queue() {
    let mut run = Run::new(Fake::new(|s| (1..=3).for_each(|n| s.add_pr(n))));
    run.tick().await;
    run.rebase.fail = Some("conflict".into());
    run.tick().await;
    run.gh
        .with_mut(|s| s.prs.get_mut(&3).unwrap().labels.clear());
    run.tick().await;
    let exit = |n: PrNumber, outcome: &str| (n, outcome.into(), format!("PR {n}"));
    assert_eq!(
        run.exits(),
        [exit(1, "merged"), exit(2, "ejected"), exit(3, "dequeued")]
    );
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
            ..Default::default()
        },
        Candidate {
            number: 2,
            ..Default::default()
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

/// Ticks until the next merge, which a restart cuts short: GitHub merges it meanwhile, or not.
async fn cut_short_while_merging(run: &Run, hang: MergeHang) {
    run.gh.with_mut(|s| s.merge_hangs = Some(hang));
    let cut = tokio::time::timeout(Duration::from_secs(3600), run.tick()).await;
    assert!(cut.is_err(), "the tick ran on");
    assert!(run.gh.with(|s| s.merge_hangs.is_none()), "no merge yet");
}

#[tokio::test(start_paused = true)]
async fn a_merge_that_lands_during_a_restart_is_seen_through() {
    let run = Run::new(Fake::new(|s| s.add_pr(1)));
    cut_short_while_merging(&run, MergeHang::Merged).await;
    run.tick().await;
    assert_eq!(
        run.kinds(1),
        ["enqueued", "rebasing", "published", "merged"]
    );
    assert_eq!(run.exits(), [(1, "merged".into(), "PR 1".into())]);
    assert!(run.labels(1).is_empty());
    let last = run.gh.with(|s| s.comments.last().cloned());
    assert_eq!(last, Some((1, "Merged as merge-1.".into())));
    assert!(run.lane().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_merge_still_pending_after_a_restart_is_waited_for() {
    let run = Run::new(Fake::new(|s| s.add_pr(1)));
    cut_short_while_merging(&run, MergeHang::Pending).await;
    run.tick().await;
    assert_eq!(run.merged(), [1]);
    assert_eq!(run.gh.with(|s| s.merges), 1);
    // Not rebased again.
    assert_eq!(run.rebase.count(), 1);
    assert_eq!(
        run.kinds(1),
        ["enqueued", "rebasing", "published", "merged"]
    );
}

#[tokio::test(start_paused = true)]
async fn a_stack_that_merges_during_a_restart_is_seen_through() {
    let run = Run::new(Fake::new(|s| s.add_stack(10, &[1, 2], &[2])));
    cut_short_while_merging(&run, MergeHang::Merged).await;
    run.tick().await;
    for n in [1, 2] {
        assert_eq!(
            run.kinds(n),
            ["enqueued", "rebasing", "published", "merged"]
        );
        assert!(run.labels(n).is_empty());
    }
    assert!(run.lane().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_batch_merge_that_lands_during_a_restart_is_seen_through() {
    let run = Run::batching(Fake::new(prs(&[1, 2])));
    cut_short_while_merging(&run, MergeHang::Merged).await;
    run.drain().await;
    assert_eq!(run.merged(), [1, 2]);
    assert_eq!(run.kinds(1), ["enqueued", "batch", "merged"]);
    assert!(run.labels(1).is_empty());
    assert!(run.lane().is_empty());
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

#[tokio::test(start_paused = true)]
async fn labeling_one_layer_queues_its_stack_which_merges_all_at_once() {
    let run = Run::new(Fake::new(|s| s.add_stack(10, &[1, 2, 3], &[2])));
    assert!(matches!(run.tick().await, Ran::Job));
    assert_eq!(run.merged(), [1, 2, 3]);
    assert_eq!(run.gh.with(|s| s.merges), 1);
    assert_eq!(run.gh.with(|s| s.tree(&s.trunk)), "t-1-2-3");
    for n in [1, 2, 3] {
        assert_eq!(
            run.kinds(n),
            ["enqueued", "rebasing", "published", "merged"]
        );
        assert!(run.labels(n).is_empty());
    }
    assert!(run.lane().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_stack_waits_while_any_layer_is_not_ready() {
    let run = Run::new(Fake::new(|s| {
        s.add_stack(10, &[1, 2], &[1, 2]);
        s.prs.get_mut(&1).unwrap().is_draft = true;
    }));
    assert!(matches!(run.tick().await, Ran::Idle));
    assert_eq!(
        run.lane(),
        [(
            10,
            "Held(\"#1 is not ready: the PR is a draft\")".to_owned()
        )]
    );
    let held = run.gh.with(|s| {
        s.comments
            .iter()
            .any(|(n, c)| *n == 2 && c.starts_with("On hold: #1 is not ready"))
    });
    assert!(held);
    run.gh
        .with_mut(|s| s.prs.get_mut(&1).unwrap().is_draft = false);
    run.tick().await;
    assert_eq!(run.merged(), [1, 2]);
}

#[tokio::test(start_paused = true)]
async fn a_green_stack_is_tested_in_a_batch_and_merged_at_once() {
    let run = Run::batching(Fake::new(|s| {
        prs(&[1])(s);
        s.add_stack(10, &[2, 3], &[3]);
        s.add_pr(4);
    }));
    run.tick().await;
    assert_eq!(run.merged(), [1, 2, 3, 4]);
    assert_eq!(run.rebase.count(), 0);
    // 1, the stack, then 4.
    assert_eq!(run.gh.with(|s| s.merges), 3);
    assert_eq!(run.gh.with(|s| s.tree(&s.trunk)), "t-1-2-3-4");
    assert_eq!(
        run.gh.with(|s| s.drafts.clone()),
        ["PR 1 (+3 in a Mergepatrol batch)"]
    );
    assert_eq!(
        run.gh.with(|s| s.deleted.clone()),
        ["mergepatrol/batch-1-10-4"]
    );
    assert_eq!(run.kinds(2), ["enqueued", "batch", "merged"]);
    let comment = run.gh.with(|s| {
        s.comments
            .iter()
            .any(|(n, c)| *n == 2 && c == "Testing together with #1, #4 in #1001.")
    });
    assert!(comment);
}

#[tokio::test(start_paused = true)]
async fn a_stack_that_breaks_a_batch_is_one_suspect_and_is_ejected_whole() {
    let run = Run::batching(Fake::new(|s| {
        prs(&[1])(s);
        s.add_stack(10, &[2, 3], &[3]);
        s.add_pr(4);
        // Each layer passes on its own head; what has 3 merged in fails.
        s.breaks = Some(3);
    }));
    run.drain().await;
    assert_eq!(run.merged(), [1, 4]);
    assert_eq!(run.labels(2), [FAILED]);
    assert_eq!(run.labels(3), [FAILED]);
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
async fn a_failed_check_on_a_layer_is_fixed_there_and_the_layers_above_go_onto_the_fix() {
    let run = Run::new(Fake::new(|s| {
        s.add_stack(10, &[1, 2], &[2]);
        s.required = vec!["ci".into()];
        s.checks_on
            .insert("pr-1-r1".into(), [("ci".into(), CheckState::Failed)].into());
    }));
    run.tick().await;
    assert_eq!(run.rebase.fixed(), [["ci"]]);
    assert_eq!(run.merged(), [1, 2]);
    assert_eq!(run.gh.with(|s| s.head(1)), "pr-1-r1-f1");
    assert_eq!(run.gh.with(|s| s.head(2)), "pr-2-r1-s1");
    let comment = run.gh.with(|s| {
        s.comments
            .iter()
            .any(|(n, c)| *n == 2 && c.starts_with("Required checks failed on #1: ci."))
    });
    assert!(comment);
}

#[tokio::test(start_paused = true)]
async fn a_push_to_a_lower_layer_while_checks_run_rebases_the_stack_again() {
    let run = Run::new(Fake::new(|s| {
        s.add_stack(10, &[1, 2], &[2]);
        s.required = vec!["ci".into()];
        s.checks_on.insert(
            "pr-2-r1".into(),
            [("ci".into(), CheckState::Pending)].into(),
        );
    }));
    // Layer 1's checks, then layer 2's, when its author pushes to layer 1.
    run.gh.then(|_| {});
    run.gh.then(|s| s.push(1, "pr-1-v1"));
    run.tick().await;
    assert_eq!(run.rebase.count(), 2);
    assert_eq!(run.merged(), [1, 2]);
    assert!(run.kinds(1).contains(&"head-moved".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn a_batch_with_a_stack_picks_up_where_it_was_after_a_restart() {
    let run = Run::batching(Fake::new(|s| {
        prs(&[1])(s);
        s.add_stack(10, &[2, 3], &[3]);
    }));
    const BATCH: &str = "mergepatrol/batch-1-10@t-1-2-3";
    run.gh.with_mut(|s| {
        s.checks_on
            .insert(BATCH.into(), [("ci".into(), CheckState::Pending)].into())
    });
    let cut = tokio::time::timeout(CHECKS_POLL * 4, run.tick()).await;
    assert!(cut.is_err(), "the batch is still testing");
    run.gh.with_mut(|s| s.checks_on.remove(BATCH));
    run.tick().await;
    assert_eq!(run.merged(), [1, 2, 3]);
    assert_eq!(run.gh.with(|s| s.drafts.len()), 1);
    assert_eq!(
        run.kinds(2),
        ["enqueued", "batch", "batch-resumed", "merged"]
    );
}

#[tokio::test(start_paused = true)]
async fn a_stack_that_gains_a_pr_while_it_runs_runs_again_whole() {
    let run = Run::new(Fake::new(|s| {
        s.add_stack(10, &[1, 2], &[2]);
        s.required = vec!["ci".into()];
        s.checks_on.insert(
            "pr-2-r1".into(),
            [("ci".into(), CheckState::Pending)].into(),
        );
    }));
    // While the checks of layer 2 run, its author stacks 3 on top.
    run.gh.then(|_| {});
    run.gh.then(|s| {
        s.add_pr(3);
        let pr = s.prs.get_mut(&3).unwrap();
        pr.labels.clear();
        pr.base_ref_name = "pr-2".into();
        pr.stack = Some(StackRef {
            number: 10,
            base_ref_name: "main".into(),
        });
        s.stacks.get_mut(&10).unwrap().push(3);
        s.checks_on.remove("pr-2-r1");
    });
    run.tick().await;
    assert!(run.merged().is_empty());
    assert_eq!(run.kinds(1).last().unwrap(), "restarted");
    assert_eq!(run.lane(), [(10, "Waiting".to_owned())]);
    run.tick().await;
    assert_eq!(run.merged(), [1, 2, 3]);
    assert_eq!(run.gh.with(|s| s.merges), 1);
}

fn fork_pr(s: &mut State) {
    s.required = vec!["ci".into()];
    s.add_fork_pr(1);
}

fn last_comment(run: &Run) -> String {
    run.gh.with(|s| s.comments.last().unwrap().1.clone())
}

#[tokio::test(start_paused = true)]
async fn a_fork_pr_is_rebased_checked_and_merged() {
    let run = Run::new(Fake::new(|s| {
        fork_pr(s);
        // The PR API lags behind the queue's own publish to the fork.
        s.lag_reads = 2;
    }));
    run.drain().await;
    assert_eq!(run.merged(), [1]);
    assert_eq!(
        run.kinds(1),
        ["enqueued", "rebasing", "published", "merged"]
    );
}

#[tokio::test(start_paused = true)]
async fn a_fork_pr_is_never_tested_in_a_batch() {
    let run = Run::batching(Fake::new(|s| {
        prs(&[1, 2])(s);
        s.add_fork_pr(3);
        s.add_pr(4);
    }));
    run.drain().await;
    assert_eq!(run.merged(), [1, 2, 3, 4]);
    // The batch stops before the fork PR, which runs alone. The PR after it runs alone too: a
    // batch needs two.
    assert_eq!(
        run.gh.with(|s| s.drafts.clone()),
        ["PR 1 (+1 in a Mergepatrol batch)"]
    );
    assert!(run.kinds(3).contains(&"rebasing".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn a_push_to_a_fork_pr_after_it_was_queued_takes_it_out_of_the_queue() {
    let run = Run::new(Fake::new(|s| {
        fork_pr(s);
        s.checks.insert("ci".into(), CheckState::Pending);
    }));
    run.gh.then(|s| s.push(1, "untrusted"));
    run.drain().await;
    assert!(run.merged().is_empty());
    assert_eq!(&run.kinds(1)[3..], ["pushed", "dequeued"]);
    assert!(run.labels(1).is_empty(), "{:?}", run.labels(1));
    let comment = last_comment(&run);
    assert!(
        comment.contains("Dequeued: new commits were pushed"),
        "{comment}"
    );
    assert!(
        run.exits()
            .iter()
            .any(|(n, o, _)| *n == 1 && o == "dequeued")
    );
}

#[tokio::test(start_paused = true)]
async fn a_fork_pr_pushed_to_since_its_label_went_on_is_not_queued() {
    let run = Run::new(Fake::new(fork_pr));
    run.store
        .label_heads
        .lock()
        .unwrap()
        .insert(1, "what-the-maintainer-saw".into());
    // The pull ref may only lag behind the label: it waits a while first.
    assert!(matches!(run.tick().await, Ran::Idle));
    assert!(run.kinds(1).is_empty());
    tokio::time::advance(PIN_WAIT).await;
    assert!(matches!(run.tick().await, Ran::Idle));
    assert_eq!(run.kinds(1), ["pushed"]);
    assert!(run.labels(1).is_empty());
    assert!(run.merged().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_fork_pr_goes_by_its_pull_ref_which_the_pr_lags_behind() {
    let run = Run::new(Fake::new(|s| {
        fork_pr(s);
        // Pushed just before the label went on: GraphQL still shows the head before.
        s.branches.insert("pr-1".into(), "pr-1-v1".into());
    }));
    run.store
        .label_heads
        .lock()
        .unwrap()
        .insert(1, "pr-1-v1".into());
    run.drain().await;
    assert_eq!(run.merged(), [1]);
}

/// A Mergepatrol that GitHub delivers webhooks to.
fn with_webhooks(run: &mut Run) {
    run.ctx.config.public_url = "https://mergepatrol.example".into();
    run.ctx.hosted_app = Some(AppCredentials {
        id: 1,
        slug: "mp".into(),
        client_id: "c".into(),
        client_secret: "s".into(),
        webhook_secret: Some("hush".into()),
        pem: String::new(),
        owner: "o".into(),
        html_url: "https://github.com/apps/mp".into(),
    });
}

#[tokio::test(start_paused = true)]
async fn with_webhooks_a_fork_pr_waits_for_the_labeled_webhook() {
    let mut run = Run::new(Fake::new(fork_pr));
    with_webhooks(&mut run);
    // Polled before the webhook came: the author may have pushed since the label.
    assert!(matches!(run.tick().await, Ran::Idle));
    assert!(run.lane().is_empty());
    run.store
        .label_heads
        .lock()
        .unwrap()
        .insert(1, "pr-1-v0".into());
    run.drain().await;
    assert_eq!(run.merged(), [1]);
}

#[tokio::test(start_paused = true)]
async fn a_fork_pr_labeled_again_waits_afresh_for_its_webhook() {
    let mut run = Run::new(Fake::new(fork_pr));
    with_webhooks(&mut run);
    run.tick().await;
    run.gh
        .with_mut(|s| s.prs.get_mut(&1).unwrap().labels.clear());
    run.tick().await;
    tokio::time::advance(PIN_WAIT).await;
    run.gh
        .with_mut(|s| s.prs.get_mut(&1).unwrap().labels.push(TRIGGER.into()));
    // Polled before the new label's webhook.
    run.tick().await;
    assert!(run.kinds(1).is_empty());
    assert_eq!(run.labels(1), [TRIGGER]);
}

#[tokio::test(start_paused = true)]
async fn with_webhooks_a_fork_pr_whose_webhook_never_comes_leaves() {
    let mut run = Run::new(Fake::new(fork_pr));
    with_webhooks(&mut run);
    run.tick().await;
    tokio::time::advance(PIN_WAIT).await;
    run.tick().await;
    assert_eq!(run.kinds(1), ["unpinned"]);
    assert!(run.labels(1).is_empty());
    assert!(last_comment(&run).contains("no webhook for its label"));
}

#[tokio::test(start_paused = true)]
async fn a_fork_pr_queues_at_the_head_its_label_went_on_at() {
    let run = Run::new(Fake::new(fork_pr));
    run.store
        .label_heads
        .lock()
        .unwrap()
        .insert(1, "pr-1-v0".into());
    run.drain().await;
    assert_eq!(run.merged(), [1]);
}

#[tokio::test(start_paused = true)]
async fn no_agent_fixes_a_fork_pr_whose_checks_fail() {
    let run = Run::new(Fake::new(|s| {
        fork_pr(s);
        s.checks.insert("ci".into(), CheckState::Failed);
    }));
    run.drain().await;
    assert!(run.rebase.fixed().is_empty());
    assert_eq!(run.kinds(1).last().unwrap(), "ejected");
    assert_eq!(run.labels(1), [FAILED]);
    assert!(last_comment(&run).contains("required checks failed: ci"));
}

#[tokio::test(start_paused = true)]
async fn a_fork_pr_that_needs_a_rebase_waits_until_its_author_allows_edits() {
    let run = Run::new(Fake::new(|s| {
        fork_pr(s);
        s.prs.get_mut(&1).unwrap().maintainer_can_modify = false;
        s.behind.push("pr-1-v0".into());
    }));
    assert!(matches!(run.tick().await, Ran::Idle));
    assert_eq!(run.kinds(1), ["enqueued", "held"]);
    assert!(last_comment(&run).contains("only once its author allows edits by maintainers"));
    run.tick().await;
    assert_eq!(run.gh.with(|s| s.comments.len()), 1);

    run.gh
        .with_mut(|s| s.prs.get_mut(&1).unwrap().maintainer_can_modify = true);
    run.drain().await;
    assert_eq!(run.merged(), [1]);
    assert_eq!(&run.kinds(1)[2..4], ["released", "rebasing"]);
}

#[tokio::test(start_paused = true)]
async fn a_fork_pr_on_the_trunk_needs_no_edits_by_maintainers() {
    let run = Run::new(Fake::new(|s| {
        fork_pr(s);
        s.prs.get_mut(&1).unwrap().maintainer_can_modify = false;
    }));
    run.drain().await;
    assert_eq!(run.merged(), [1]);
}

#[tokio::test(start_paused = true)]
async fn a_fork_pr_behind_the_trunk_from_an_organization_is_ejected() {
    let run = Run::new(Fake::new(|s| {
        fork_pr(s);
        let pr = s.prs.get_mut(&1).unwrap();
        pr.maintainer_can_modify = false;
        pr.head_repository.as_mut().unwrap().owner.kind = "Organization".into();
        s.behind.push("pr-1-v0".into());
    }));
    run.drain().await;
    assert_eq!(run.labels(1), [FAILED]);
    let comment = last_comment(&run);
    assert!(
        comment.contains("a fork that an organization owns"),
        "{comment}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_fork_pr_behind_the_trunk_that_changes_workflows_is_ejected() {
    let run = Run::new(Fake::new(|s| {
        fork_pr(s);
        s.behind.push("pr-1-v0".into());
        s.change_workflows.push(1);
    }));
    run.drain().await;
    assert!(run.merged().is_empty());
    assert_eq!(run.labels(1), [FAILED]);
    assert!(last_comment(&run).contains("changes workflow files"));
}

#[tokio::test(start_paused = true)]
async fn a_fork_pr_whose_ci_waits_for_approval_is_held_until_a_maintainer_approves() {
    let run = Run::new(Fake::new(|s| {
        fork_pr(s);
        // GitHub holds the runs on the head the queue publishes, and shows them as failed.
        s.awaiting_approval.push("pr-1-r1".into());
        let failed = BTreeMap::from([("ci".to_owned(), CheckState::Failed)]);
        s.checks_on.insert("pr-1-r1".into(), failed);
    }));
    assert!(matches!(run.tick().await, Ran::Job));
    assert_eq!(run.lane(), [(1, format!("Held({AWAITING_APPROVAL:?})"))]);
    assert!(last_comment(&run).contains(AWAITING_APPROVAL));
    // It stays held, without a word more.
    let comments = run.gh.with(|s| s.comments.len());
    assert!(matches!(run.tick().await, Ran::Idle));
    assert_eq!(run.gh.with(|s| s.comments.len()), comments);

    run.gh.with_mut(|s| {
        s.awaiting_approval.clear();
        s.checks_on.clear();
    });
    run.drain().await;
    assert_eq!(run.merged(), [1]);
}

#[tokio::test(start_paused = true)]
async fn a_stack_with_a_pr_from_a_fork_is_ejected() {
    let run = Run::new(Fake::new(|s| {
        s.add_stack(10, &[1, 2], &[1]);
        s.prs.get_mut(&2).unwrap().is_cross_repository = true;
    }));
    run.drain().await;
    assert!(run.merged().is_empty());
    assert_eq!(run.labels(1), [FAILED]);
}
