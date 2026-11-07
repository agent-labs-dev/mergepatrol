//! Getting a PR ready, end to end: the real jobs, with their locks and their publishing, against
//! a GitHub on localhost and an agent that needs no model. The GitHub answers from the loop tests'
//! `Fake`, and keeps its objects in a real repo, which the queue's git cache fetches from. Each
//! part is real git and sockets, so time runs: a job waits out `prep::drain`'s two seconds.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::{Command, Stdio};

use base64::Engine;
use mergepatrol_core::FIXED_BY;
use mergepatrol_executor::protocol::JobResult;
use mergepatrol_git::cache_path;
use serde_json::{Value, json};
use tokio::sync::Barrier;

use super::*;
use crate::job::LEFT_CONFLICT;
use crate::review::{conclude, fix_sent_back, hold_for_fix};
use mergepatrol_executor::protocol::ReviewOutput;

/// How long a scenario may take to end. It ends far sooner, or it is stuck.
const SETTLES_WITHIN: Duration = Duration::from_secs(60);

fn git(dir: &Path, args: &[&str]) -> String {
    git_with(dir, args, &[], None)
}

fn git_with(dir: &Path, args: &[&str], env: &[(&str, &str)], input: Option<&[u8]>) -> String {
    use std::io::Write;
    let mut child = Command::new("git")
        .args(["-c", "user.name=Ada", "-c", "user.email=ada@example.com"])
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.unwrap_or_default())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn commit(dir: &Path, file: &str, content: &str, message: &str) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// What the agent does on the job in front of it, beside its work, as a test scripts it.
type During = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The job as the sandbox would run it, with a fake in place of the model: a rebase that resolves
/// each conflict, or a fix that adds a commit.
struct Agent {
    origin: PathBuf,
    /// Each job takes the next, to run while the agent works.
    during: Mutex<VecDeque<During>>,
    /// Why the agent cannot do any job, if it cannot.
    fails: Mutex<Option<String>>,
    tasks: Mutex<Vec<Task>>,
}

impl Agent {
    fn work(&self, spec: &JobSpec, dir: &Path) -> std::result::Result<Vec<JobEvent>, String> {
        let url = self.origin.to_str().unwrap();
        git(dir, &["clone", "-q", url, "."]);
        git(dir, &["checkout", "-q", "--detach", &spec.head_sha]);
        if !matches!(spec.task, Task::Rebase) {
            commit(dir, "fix.txt", "fixed\n", &format!("Fix it\n\n{FIXED_BY}"));
            return Ok(Vec::new());
        }
        let upstream = match &spec.upstream {
            Some(upstream) => upstream.clone(),
            None => git(dir, &["merge-base", &spec.trunk_sha, "HEAD"]),
        };
        let rebased = Command::new("git")
            .args(["-c", "user.name=Ada", "-c", "user.email=ada@example.com"])
            .args(["rebase", "-q", "--onto", &spec.trunk_sha, &upstream])
            .current_dir(dir)
            .output()
            .unwrap();
        if rebased.status.success() {
            return Ok(Vec::new());
        }
        let conflicts = git(dir, &["diff", "--name-only", "--diff-filter=U"]);
        if spec.leave_conflicts {
            return Err(format!("{LEFT_CONFLICT} in {conflicts}"));
        }
        for file in conflicts.lines() {
            std::fs::write(dir.join(file), "resolved\n").unwrap();
        }
        git(dir, &["add", "-A"]);
        git_with(
            dir,
            &["rebase", "--continue"],
            &[("GIT_EDITOR", "true")],
            None,
        );
        Ok(vec![JobEvent {
            kind: "resolved".into(),
            message: conflicts,
        }])
    }
}

#[async_trait]
impl Executor for Agent {
    async fn run(&self, _: &str, spec: &JobSpec, out: &Path) -> Result<JobResult> {
        self.tasks.lock().unwrap().push(spec.task.clone());
        let during = self.during.lock().unwrap().pop_front();
        if let Some(during) = during {
            during.await;
        }
        let failed = |reason, events| JobResult {
            outcome: JobOutcome::Failed { reason },
            events,
        };
        if let Some(reason) = self.fails.lock().unwrap().clone() {
            return Ok(failed(reason, Vec::new()));
        }
        let dir = tempfile::tempdir()?;
        let events = match self.work(spec, dir.path()) {
            Ok(events) => events,
            Err(reason) => return Ok(failed(reason, Vec::new())),
        };
        let exclude = match spec.task {
            Task::Rebase => &spec.trunk_sha,
            _ => &spec.head_sha,
        };
        tokio::fs::create_dir_all(out).await?;
        let head = git(dir.path(), &["rev-parse", "HEAD"]);
        Git.create_bundle(dir.path(), &out.join(BUNDLE_FILE), &head, exclude)
            .await?;
        Ok(JobResult {
            outcome: JobOutcome::Done,
            events,
        })
    }
}

/// A PR as GraphQL gives it.
fn pr_node(pr: &PullRequest) -> Value {
    let none = json!({ "nodes": [] });
    let labels: Vec<Value> = pr.labels.iter().map(|l| json!({ "name": l })).collect();
    json!({
        "number": pr.number,
        "state": pr.state,
        "title": pr.title,
        "body": pr.body,
        "isDraft": pr.is_draft,
        "isCrossRepository": pr.is_cross_repository,
        "reviewDecision": null,
        "baseRefName": pr.base_ref_name,
        "headRefName": pr.head_ref_name,
        "headRefOid": pr.head_ref_oid,
        "labels": { "nodes": labels },
        "stack": pr.stack.as_ref().map(|s| {
            json!({ "number": s.number, "baseRefName": s.base_ref_name })
        }),
        "mergeable": if pr.conflicting { "CONFLICTING" } else { "MERGEABLE" },
        "url": "", "createdAt": "", "additions": 0, "deletions": 0, "changedFiles": 0,
        "author": null,
        "latestOpinionatedReviews": none,
        "reviewRequests": none,
        "commits": none,
    })
}

/// A repository as a snapshot gives it. A job reads only what is about the repo.
fn repository_node(trunk: &str) -> Value {
    let label = json!({ "name": TRIGGER });
    json!({
        "id": "R",
        "isPrivate": false,
        "trigger": label, "priority": label, "failed": label,
        "viewerDefaultMergeMethod": "MERGE",
        "squashMergeAllowed": true, "rebaseMergeAllowed": true, "mergeCommitAllowed": true,
        "defaultBranchRef": { "name": "main", "target": { "oid": trunk } },
        "pullRequests": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } },
    })
}

/// `updateRefs`: every branch moves, or none does.
fn update_refs(gh: &Fake, origin: &Path, updates: &[Value]) -> Value {
    let str_of = |u: &Value, key: &str| u[key].as_str().unwrap().to_owned();
    if updates.iter().any(|u| {
        let name = str_of(u, "name");
        git(origin, &["rev-parse", &name]) != str_of(u, "beforeOid")
    }) {
        return json!({ "errors": [{ "message": "a ref moved" }] });
    }
    let mut s = gh.state.lock().unwrap();
    for u in updates {
        let (name, after) = (str_of(u, "name"), str_of(u, "afterOid"));
        git(origin, &["update-ref", &name, &after]);
        let branch = name.strip_prefix("refs/heads/").unwrap();
        s.branches.insert(branch.into(), after.clone());
        if let Some(pr) = s.prs.values_mut().find(|p| p.head_ref_name == branch) {
            pr.head_ref_oid = after;
            pr.conflicting = false;
        }
    }
    json!({ "data": { "updateRefs": { "clientMutationId": null } } })
}

fn graphql(gh: &Fake, origin: &Path, body: &Value) -> Value {
    let (query, vars) = (body["query"].as_str().unwrap(), &body["variables"]);
    if query.contains("updateRefs") {
        return update_refs(gh, origin, vars["updates"].as_array().unwrap());
    }
    let s = gh.state.lock().unwrap();
    if query.contains("reviewThreads(first: 100) { nodes { id isResolved") {
        // Thread `i` starts with comment `i + 1`; its replies are numbered after it.
        let comment = |id: usize, body: &str| json!({ "nodes": [{ "databaseId": id, "body": body, "author": { "login": "mergeloop" } }] });
        let nodes: Vec<Value> = s
            .threads
            .iter()
            .enumerate()
            .map(|(i, (first, replies, resolved))| {
                let last = match replies.last() {
                    Some(reply) => comment(1000 * (i + 1) + replies.len(), reply),
                    None => comment(i + 1, first),
                };
                json!({ "id": format!("t{i}"), "isResolved": resolved,
                        "first": comment(i + 1, first), "last": last })
            })
            .collect();
        let threads = json!({ "reviewThreads": { "nodes": nodes } });
        return json!({ "data": { "repository": { "pullRequest": threads } } });
    }
    if query.contains("ReviewThread(input") {
        drop(s);
        let i: usize = vars["id"].as_str().unwrap()[1..].parse().unwrap();
        let resolved = query.contains("resolveReviewThread") && !query.contains("unresolve");
        gh.with_mut(|s| s.threads[i].2 = resolved);
        return json!({ "data": {} });
    }
    if query.contains("pullRequests(") {
        return json!({ "data": { "repository": repository_node(&s.trunk) } });
    }
    // The workflows a review of the author's agent reads its checks from: none.
    if query.contains("$workflows") {
        return json!({ "data": { "repository": { "object": null } } });
    }
    // A file of the default branch, as a review that asks a human reads CODEOWNERS: none.
    if query.contains("file(path: $path)") {
        let target = json!({ "oid": s.trunk, "file": null });
        let branch = json!({ "name": "main", "target": target });
        return json!({ "data": { "repository": { "defaultBranchRef": branch } } });
    }
    let pr = &s.prs[&vars["number"].as_u64().unwrap()];
    if query.contains("commits(first: 100") {
        let log = git(
            origin,
            &["log", "-n", "20", "--format=%B%x1e", &pr.head_ref_oid],
        );
        let nodes: Vec<Value> = log
            .split('\u{1e}')
            .rev()
            .map(|m| json!({ "commit": { "message": m.trim() } }))
            .collect();
        let page = json!({ "hasNextPage": false, "endCursor": null });
        let commits = json!({ "commits": { "nodes": nodes, "pageInfo": page } });
        return json!({ "data": { "repository": { "pullRequest": commits } } });
    }
    json!({ "data": { "repository": { "pullRequest": pr_node(pr) } } })
}

/// The Git Data API, on the repo at `origin`: `base_tree` with `tree`'s entries applied.
fn create_tree(origin: &Path, body: &Value) -> String {
    let index = tempfile::tempdir().unwrap();
    let index = index.path().join("index");
    let env = [("GIT_INDEX_FILE", index.to_str().unwrap())];
    let base = body["base_tree"].as_str().unwrap();
    git_with(origin, &["read-tree", base], &env, None);
    for entry in body["tree"].as_array().unwrap() {
        let path = entry["path"].as_str().unwrap();
        let info = match entry["sha"].as_str() {
            Some(sha) => format!("{},{sha},{path}", entry["mode"].as_str().unwrap()),
            None => String::new(),
        };
        let args = match info.is_empty() {
            true => vec!["update-index", "--force-remove", path],
            false => vec!["update-index", "--add", "--cacheinfo", &info],
        };
        git_with(origin, &args, &env, None);
    }
    git_with(origin, &["write-tree"], &env, None)
}

/// GitHub's answer to one request. Refs go to the `Fake` too, which is what a tick reads.
fn answer(gh: &Fake, origin: &Path, method: &str, path: &str, body: &str) -> (u16, String) {
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let sha_of = |sha: String| json!({ "sha": sha }).to_string();
    if path.ends_with("/access_tokens") {
        return (201, json!({ "token": "read" }).to_string());
    }
    if path == "/graphql" {
        let pr_read = body["query"]
            .as_str()
            .unwrap()
            .contains("pullRequest(number");
        let down = pr_read
            && gh.with_mut(|s| {
                let down = s.outage > 0;
                s.outage = s.outage.saturating_sub(1);
                down
            });
        if down {
            return (404, "{}".into());
        }
        return (200, graphql(gh, origin, &body).to_string());
    }
    let Some(rest) = path.strip_prefix("/repos/o/r/") else {
        return (404, "{}".into());
    };
    if let Some(branch) = rest.strip_prefix("git/ref/heads/") {
        let s = gh.state.lock().unwrap();
        let sha = match branch {
            "main" => Some(&s.trunk),
            _ => s.branches.get(branch),
        };
        return match sha {
            Some(sha) => (200, json!({ "object": { "sha": sha } }).to_string()),
            None => (404, "{}".into()),
        };
    }
    if let Some(commit) = rest.strip_prefix("commits/") {
        let sha = commit.split('/').next().unwrap();
        let failed: Vec<Value> = gh
            .with(|s| s.checks_of(sha))
            .into_iter()
            .filter(|(_, state)| *state == CheckState::Failed)
            .map(|(name, _)| json!({ "id": 1, "name": name, "conclusion": "failure" }))
            .collect();
        let total = failed.len();
        let runs = json!({ "check_runs": failed, "total_count": total });
        return (200, runs.to_string());
    }
    if let Some(stack) = rest.strip_prefix("stacks/") {
        let s = gh.state.lock().unwrap();
        let layers = &s.stacks[&stack.parse::<PrNumber>().unwrap()];
        let prs: Vec<Value> = layers
            .iter()
            .map(|n| json!({ "number": n, "state": s.prs[n].state.to_lowercase() }))
            .collect();
        return (200, json!({ "pull_requests": prs }).to_string());
    }
    if let Some(number) = rest
        .strip_prefix("pulls/")
        .and_then(|r| r.strip_suffix("/reviews"))
    {
        let comments = body["comments"].as_array().cloned().unwrap_or_default();
        let text = body["body"].as_str().unwrap_or_default().to_owned();
        gh.with_mut(|s| {
            s.reviews
                .push((number.parse().unwrap(), text, comments.len()));
            for c in &comments {
                let first = c["body"].as_str().unwrap().to_owned();
                s.threads.push((first, Vec::new(), false));
            }
        });
        return (200, "{}".into());
    }
    if let Some(reply) = rest
        .strip_prefix("pulls/1/comments/")
        .and_then(|r| r.strip_suffix("/replies"))
    {
        let i = reply.parse::<usize>().unwrap() - 1;
        let text = body["body"].as_str().unwrap().to_owned();
        gh.with_mut(|s| s.threads[i].1.push(text));
        return (201, "{}".into());
    }
    match (method, rest) {
        ("POST", "git/blobs") => {
            let content = body["content"].as_str().unwrap();
            let content = base64::engine::general_purpose::STANDARD
                .decode(content)
                .unwrap();
            let args = ["hash-object", "-w", "--stdin"];
            let sha = git_with(origin, &args, &[], Some(&content));
            (201, sha_of(sha))
        }
        ("POST", "git/trees") => (201, sha_of(create_tree(origin, &body))),
        ("POST", "git/commits") => {
            let (tree, parent) = (
                body["tree"].as_str().unwrap(),
                body["parents"][0].as_str().unwrap(),
            );
            let message = body["message"].as_str().unwrap().as_bytes();
            let args = ["commit-tree", tree, "-p", parent, "-F", "-"];
            (201, sha_of(git_with(origin, &args, &[], Some(message))))
        }
        ("POST", "git/refs") => {
            let (name, sha) = (body["ref"].as_str().unwrap(), body["sha"].as_str().unwrap());
            git(origin, &["update-ref", name, sha]);
            let branch = name.strip_prefix("refs/heads/").unwrap();
            gh.with_mut(|s| s.branches.insert(branch.into(), sha.into()));
            (201, "{}".into())
        }
        ("DELETE", _) => {
            let name = format!("refs/{}", rest.strip_prefix("git/refs/").unwrap());
            let branch = name.strip_prefix("refs/heads/").unwrap();
            if gh.with_mut(|s| s.branches.remove(branch)).is_none() {
                return (422, "{}".into());
            }
            git(origin, &["update-ref", "-d", &name]);
            (204, String::new())
        }
        _ => (404, "{}".into()),
    }
}

/// PR `number`, at the head its branch `pr-<number>` has in the repo.
fn add_pr(s: &mut State, number: PrNumber, origin: &Path) {
    s.add_pr(number);
    point_at_branch(s, number, origin);
}

fn point_at_branch(s: &mut State, number: PrNumber, origin: &Path) {
    let branch = format!("pr-{number}");
    let head = git(origin, &["rev-parse", &branch]);
    s.branches.insert(branch, head.clone());
    s.prs.get_mut(&number).unwrap().head_ref_oid = head;
}

/// A queue with its real jobs: its ticks read the `Fake`, the jobs reach it over HTTP.
struct World {
    _tmp: tempfile::TempDir,
    origin: PathBuf,
    ctx: Arc<Ctx>,
    store: Arc<Mem>,
    gh: Arc<Fake>,
    installation: Installation,
    agent: Arc<Agent>,
}

impl World {
    /// `origin` builds the repo, on `main`, and `setup` what GitHub says of it, with the trunk at
    /// its last commit. Two jobs may run at once.
    async fn new(origin_of: impl FnOnce(&Path), setup: impl FnOnce(&mut State, &Path)) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);
        origin_of(&origin);

        let gh = Fake::new(|s| {
            s.required = vec!["ci".into()];
            s.trunk = git(&origin, &["rev-parse", "main"]);
            setup(s, &origin);
        });
        let (answers, at) = (gh.clone(), origin.clone());
        let installation = mergepatrol_github::fake::answered_by(move |method, path, body| {
            answer(&answers, &at, method, path, body)
        })
        .await;

        let agent = Arc::new(Agent {
            origin: origin.clone(),
            during: Default::default(),
            fails: Default::default(),
            tasks: Default::default(),
        });
        let store = Arc::new(Mem::default());
        let mut ctx = ctx(store.clone());
        ctx.config.data_dir = tmp.path().join("data");
        ctx.executor = agent.clone();
        ctx.jobs = Semaphore::new(2);
        // The cache's `origin` is GitHub's URL, whatever the test: it is sent here.
        let cache = cache_path(&ctx.config.data_dir, "o", "r");
        std::fs::create_dir_all(&cache).unwrap();
        git(&cache, &["init", "--bare", "-q"]);
        let instead = format!("url.{}.insteadOf", origin.display());
        git(&cache, &["config", &instead, &clone_url(&repo())]);

        Self {
            _tmp: tmp,
            origin,
            ctx: Arc::new(ctx),
            store,
            gh,
            installation,
            agent,
        }
    }

    /// One PR off `main`, on a repo with a `shared.txt`. `pr_file` and `trunk_file` are what each
    /// side changes: the same file conflicts.
    async fn one_pr(pr_file: &'static str, trunk_file: &'static str) -> Self {
        Self::new(
            |o| {
                commit(o, "shared.txt", "base\n", "start");
                git(o, &["checkout", "-q", "-b", "pr-1"]);
                commit(o, pr_file, "from the pr\n", "the PR's change");
                git(o, &["checkout", "-q", "main"]);
                commit(o, trunk_file, "from main\n", "the trunk's change");
            },
            |s, o| add_pr(s, 1, o),
        )
        .await
    }

    /// `prep::drain`, until it is dropped. It forgets what was asked before it started: a test
    /// starts it first.
    async fn drain(&self) -> Spawned {
        let (ctx, gh) = (self.ctx.clone(), self.installation.clone());
        let drain = Spawned(tokio::spawn(prep::drain(ctx, gh, repo())));
        tokio::task::yield_now().await;
        drain
    }

    async fn tick(&self) {
        let sandbox = JobSandbox {
            ctx: &self.ctx,
            gh: &self.installation,
        };
        tick(&self.ctx, &*self.gh, &sandbox, &repo()).await.unwrap();
    }

    /// Ticks until `done`.
    async fn tick_until(&self, done: impl Fn(&Self) -> bool) {
        let since = Instant::now();
        while !done(self) {
            assert!(
                since.elapsed() < SETTLES_WITHIN,
                "stuck: {:?}, {:?}",
                self.lane(),
                self.kinds(1)
            );
            self.tick().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Waits for `done`, with no tick to help it.
    async fn until(&self, done: impl Fn(&Self) -> bool) {
        let since = Instant::now();
        while !done(self) {
            assert!(since.elapsed() < SETTLES_WITHIN, "stuck: {:?}", self.lane());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn merged(&self) -> Vec<PrNumber> {
        self.gh.with(|s| s.merged.clone())
    }

    fn lane(&self) -> Vec<(PrNumber, String)> {
        let lanes = self.store.lanes.lock().unwrap();
        lanes
            .values()
            .flat_map(|l| &l.entries)
            .map(|e| (e.number, format!("{:?}", e.state)))
            .collect()
    }

    fn kinds(&self, number: PrNumber) -> Vec<String> {
        let events = self.store.events.lock().unwrap();
        events
            .iter()
            .filter(|(n, ..)| *n == number)
            .map(|(_, kind, _)| kind.clone())
            .collect()
    }

    fn did(&self, number: PrNumber, kind: &str) -> bool {
        self.kinds(number).iter().any(|k| k == kind)
    }

    fn tasks(&self) -> Vec<Task> {
        self.agent.tasks.lock().unwrap().clone()
    }

    /// The branch as the repo has it, which only a publish moves.
    fn branch(&self, name: &str) -> String {
        git(&self.origin, &["rev-parse", name])
    }

    fn key(&self) -> LaneKey {
        LaneKey {
            installation_id: 1,
            repo: repo(),
            trunk: "main".into(),
        }
    }

    fn pr(&self, number: PrNumber) -> PullRequest {
        self.gh.with(|s| s.prs[&number].clone())
    }

    /// `f` runs while the agent works on the next job.
    fn during(&self, f: impl Future<Output = ()> + Send + 'static) {
        self.agent.during.lock().unwrap().push_back(Box::pin(f));
    }

    /// The review of PR 1 finished and sent a finding back to its author: its id.
    async fn sent_back(&self) -> i64 {
        let head = self.pr(1).head_ref_oid;
        let r = self.store.start_review(1, &repo(), 1, &head).await;
        let r = r.unwrap();
        self.store.with_finding(1, |f| f.sent_back = true);
        self.store.finish_review(1, ReviewStatus::Changes);
        r.id
    }

    /// What the review worker does with a review that sent findings back.
    async fn fix_review(&self, review: i64) {
        let (key, pr) = (self.key(), self.pr(1));
        let held = hold_for_fix(&self.ctx, &self.installation, &key, &pr);
        let Some(held) = held.await.unwrap() else {
            return;
        };
        let settings = &self.ctx.config.repo_defaults;
        fix_sent_back(
            &self.ctx,
            &self.installation,
            &key,
            &pr,
            settings,
            review,
            held,
        )
        .await
        .unwrap();
    }

    /// The review of PR 1's head ends with a finding the agent is `confidence` sure of: sent
    /// back to the author when it is sure enough. The review's id.
    async fn conclude(&self, confidence: u8) -> i64 {
        let settings = self.ctx.config.repo_defaults.clone();
        self.conclude_with(confidence, &settings).await
    }

    /// `conclude`, in a repo with these settings.
    async fn conclude_with(&self, confidence: u8, settings: &RepoSettings) -> i64 {
        let head = self.pr(1).head_ref_oid;
        let r = self.store.start_review(1, &repo(), 1, &head).await.unwrap();
        self.conclude_review(r, confidence, settings).await
    }

    /// `conclude_with`, for a review already started: of a head PR 1 may have left since.
    async fn conclude_review(
        &self,
        r: mergepatrol_store::ReviewRecord,
        confidence: u8,
        settings: &RepoSettings,
    ) -> i64 {
        let finding = Finding {
            id: "f1".into(),
            kind: Kind::Other,
            title: "Cap the retry".into(),
            file: Some("feature.txt".into()),
            line: Some(1),
            confidence,
            lean: Lean::Change,
            finding: "It spins.".into(),
            unsure: String::new(),
            checked: Vec::new(),
            would_clear: String::new(),
            suggestion: String::new(),
            evidence: Some(mergepatrol_core::Evidence {
                command: "cargo test".into(),
                failed: true,
                ..Default::default()
            }),
            needs_human: false,
            code: Vec::new(),
            related: Vec::new(),
        };
        let output = ReviewOutput {
            summary: "s".into(),
            findings: vec![finding],
        };
        let (key, pr) = (self.key(), self.pr(1));
        let bars = ReviewSettings::default();
        conclude(
            &self.ctx,
            &self.installation,
            &key,
            &pr,
            &r,
            settings,
            &bars,
            Ok(output),
        )
        .await
        .unwrap();
        r.id
    }

    /// The reviews posted on PR 1, as how many threads each starts.
    fn posted(&self) -> Vec<usize> {
        self.gh
            .with(|s| s.reviews.iter().map(|(_, _, n)| *n).collect())
    }

    fn fixing(&self, review: i64) -> bool {
        self.store.fixing.lock().unwrap().contains(&review)
    }

    fn review_on(&self) {
        *self.store.review_settings.lock().unwrap() = Some(ReviewSettings {
            enabled: true,
            ..ReviewSettings::default()
        });
    }
}

fn checks_fail(s: &mut State, number: PrNumber) {
    let failed = BTreeMap::from([("ci".to_owned(), CheckState::Failed)]);
    s.checks_on.insert(s.head(number), failed);
}

#[tokio::test]
async fn a_conflict_the_lane_finds_is_rebased_off_it_then_merges() {
    let world = World::one_pr("shared.txt", "shared.txt").await;
    let before = world.pr(1).head_ref_oid;
    let _drain = world.drain().await;
    world.tick().await;
    assert_eq!(world.lane(), [(1, "Held(PrepRebasing)".into())]);
    assert_eq!(world.branch("pr-1"), before, "the lane leaves it as it is");

    world.tick_until(|w| !w.merged().is_empty()).await;
    assert_eq!(world.tasks(), [Task::Rebase, Task::Rebase]);
    assert!(world.did(1, "resolved"), "{:?}", world.kinds(1));
    assert!(world.did(1, "published"));
    assert!(!world.did(1, "ejected"));
    let head = world.branch("pr-1");
    let shown = git(&world.origin, &["show", &format!("{head}:shared.txt")]);
    assert_eq!(shown, "resolved");
    let on = git(&world.origin, &["rev-parse", &format!("{head}~1")]);
    assert_eq!(on, git(&world.origin, &["rev-parse", "main"]));
}

#[tokio::test]
async fn a_conflict_github_shows_is_rebased_before_the_lane_sees_it() {
    let world = World::one_pr("shared.txt", "shared.txt").await;
    world.gh.with_mut(|s| {
        s.prs.get_mut(&1).unwrap().conflicting = true;
    });
    let _drain = world.drain().await;
    world.tick_until(|w| !w.merged().is_empty()).await;
    assert_eq!(world.tasks(), [Task::Rebase], "the lane needs no agent");
}

#[tokio::test]
async fn a_failed_check_is_fixed_off_the_lane_then_merges() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.gh.with_mut(|s| checks_fail(s, 1));
    let _drain = world.drain().await;
    world.tick().await;
    assert_eq!(world.lane(), [(1, "Held(PrepFixing)".into())]);

    world.tick_until(|w| !w.merged().is_empty()).await;
    let Task::FixChecks { failures } = &world.tasks()[0] else {
        panic!("{:?}", world.tasks());
    };
    assert_eq!(failures[0].name, "ci");
    assert!(failures[0].details.contains("failure"), "{failures:?}");
    assert!(world.did(1, "fixing"));
    // The lane then rebased it, with the fix on top of the PR's own commit.
    let log = git(&world.origin, &["log", "--format=%B", "main..pr-1"]);
    assert!(log.starts_with("Fix it") && log.contains(FIXED_BY), "{log}");
    assert!(log.contains("the PR's change"), "{log}");
}

/// A repo whose PRs' authors run the agent. Its review, which needs no model key there, is off:
/// no review worker runs here.
const AUTHORS_AGENT: &str = "agent:\n  runs: local\nreview:\n  enabled: false\n";

#[tokio::test]
async fn with_the_authors_agent_a_pr_is_reviewed_without_a_model_key() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world
        .gh
        .with_mut(|s| s.config = Some("agent:\n  runs: local\n".into()));
    world.tick().await;
    assert_eq!(world.lane(), [(1, "Held(Reviewing)".into())]);
    let reviews = world.store.reviews.lock().unwrap().clone();
    assert_eq!(
        reviews.len(),
        1,
        "a review of its head, for the author's agent"
    );
}

#[tokio::test]
async fn with_the_authors_agent_a_conflict_waits_for_their_push_then_merges() {
    let world = World::one_pr("shared.txt", "shared.txt").await;
    world.gh.with_mut(|s| s.config = Some(AUTHORS_AGENT.into()));
    let before = world.pr(1).head_ref_oid;
    let _drain = world.drain().await;
    for _ in 0..3 {
        world.tick().await;
    }
    let held = format!("Held(AuthorRebase {{ head: {before:?} }})");
    assert_eq!(world.lane(), [(1, held)]);
    assert_eq!(world.branch("pr-1"), before, "nothing pushed to it");

    // The author's agent rebases it, keeping the PR's side, and pushes.
    git(&world.origin, &["checkout", "-q", "pr-1"]);
    git(&world.origin, &["rebase", "-q", "-X", "theirs", "main"]);
    git(&world.origin, &["checkout", "-q", "main"]);
    world.gh.with_mut(|s| point_at_branch(s, 1, &world.origin));
    world.tick_until(|w| !w.merged().is_empty()).await;
    // Only the lane's plain rebases, which leave a conflict as it is.
    assert!(
        world.tasks().iter().all(|t| *t == Task::Rebase),
        "{:?}",
        world.tasks()
    );
    assert!(
        !world.did(1, "resolved"),
        "MergeLoop's agent resolved nothing"
    );
    assert!(!world.did(1, "ejected"));
}

#[tokio::test]
async fn with_the_authors_agent_a_failed_check_waits_for_their_fix_then_merges() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.gh.with_mut(|s| {
        s.config = Some(AUTHORS_AGENT.into());
        checks_fail(s, 1);
    });
    let _drain = world.drain().await;
    for _ in 0..3 {
        world.tick().await;
    }
    let head = world.pr(1).head_ref_oid;
    let held = format!(r#"Held(AuthorFix {{ head: {head:?}, checks: ["ci"] }})"#);
    assert_eq!(world.lane(), [(1, held)]);
    assert!(!world.did(1, "fixing"));

    // The author's agent commits a fix on top, and pushes: its checks pass.
    git(&world.origin, &["checkout", "-q", "pr-1"]);
    commit(&world.origin, "feature.txt", "fixed\n", "Fix the check");
    git(&world.origin, &["checkout", "-q", "main"]);
    world.gh.with_mut(|s| point_at_branch(s, 1, &world.origin));
    world.tick_until(|w| !w.merged().is_empty()).await;
    let fixed = |t: &Task| matches!(t, Task::FixChecks { .. });
    assert!(!world.tasks().iter().any(fixed), "{:?}", world.tasks());
    assert!(!world.did(1, "fixing"));
}

#[tokio::test]
async fn with_the_authors_agent_a_failure_the_lane_finds_goes_back_to_them_without_fixes() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.gh.with_mut(|s| {
        // MergeLoop's fixes are off: the author's still go on.
        s.config = Some(format!("{AUTHORS_AGENT}fix_ci:\n  enabled: false\n"));
        s.checks.insert("ci".into(), CheckState::Failed);
        let passed = BTreeMap::from([("ci".to_owned(), CheckState::Passed)]);
        s.checks_on.insert(s.head(1), passed);
    });
    let _drain = world.drain().await;
    for _ in 0..3 {
        world.tick().await;
    }
    // On the head the lane rebased, whose checks failed.
    let head = world.pr(1).head_ref_oid;
    let held = format!(r#"Held(AuthorFix {{ head: {head:?}, checks: ["ci"] }})"#);
    assert_eq!(world.lane(), [(1, held)], "{:?}", world.kinds(1));
    assert!(!world.did(1, "ejected"));
}

#[tokio::test]
async fn review_a_local_fix_that_still_fails_makes_a_new_need() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.gh.with_mut(|s| {
        s.config = Some(AUTHORS_AGENT.into());
        checks_fail(s, 1);
    });
    world.tick().await;
    let hold_message = || {
        let lanes = world.store.lanes.lock().unwrap();
        let entry = &lanes[&world.key()].entries[0];
        let EntryState::Held(hold) = &entry.state else {
            panic!("expected an author hold, got {:?}", entry.state);
        };
        assert!(hold.of_author());
        hold.to_string()
    };
    let before = (hold_message(), world.store.events.lock().unwrap().clone());

    // The agent follows the fix-checks brief, but its attempted fix still fails CI.
    // CI can finish between queue ticks, so the queue need not observe Pending.
    git(&world.origin, &["checkout", "-q", "pr-1"]);
    commit(
        &world.origin,
        "feature.txt",
        "attempted fix\n",
        "Try fixing CI",
    );
    git(&world.origin, &["checkout", "-q", "main"]);
    world.gh.with_mut(|s| {
        point_at_branch(s, 1, &world.origin);
        checks_fail(s, 1);
    });
    world.tick().await;
    world.tick().await;

    // next_need hashes the hold and newest queue event, with no head SHA. If both
    // stay unchanged, the agent running `then --after <id>` waits forever.
    let after = (hold_message(), world.store.events.lock().unwrap().clone());
    assert_ne!(
        after, before,
        "a new head that still fails must change next_need's inputs so the author can act again"
    );
}

#[tokio::test]
async fn a_failure_the_lane_finds_is_fixed_off_it_then_merges() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.gh.with_mut(|s| {
        s.checks.insert("ci".into(), CheckState::Failed);
        let passed = BTreeMap::from([("ci".to_owned(), CheckState::Passed)]);
        s.checks_on.insert(s.head(1), passed);
    });
    let _drain = world.drain().await;
    world.tick().await;
    assert_eq!(world.lane(), [(1, "Held(PrepFixing)".into())]);
    assert!(!world.did(1, "fixing"), "the lane leaves it as it is");

    // Only the head the lane rebased fails: the fix's passes.
    let rebased = world.branch("pr-1");
    world.gh.with_mut(|s| {
        s.checks.insert("ci".into(), CheckState::Passed);
        let failed = BTreeMap::from([("ci".to_owned(), CheckState::Failed)]);
        s.checks_on.insert(rebased, failed);
    });
    world.tick_until(|w| !w.merged().is_empty()).await;
    assert!(
        matches!(world.tasks()[..], [.., Task::FixChecks { .. }]),
        "{:?}",
        world.tasks()
    );
    assert!(world.did(1, "fixing"));
    assert!(!world.did(1, "ejected"));
    let log = git(&world.origin, &["log", "--format=%B", "main..pr-1"]);
    assert!(log.starts_with("Fix it") && log.contains(FIXED_BY), "{log}");
}

#[tokio::test]
async fn a_review_s_findings_are_fixed_and_reviewed_again() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    assert_eq!(world.lane(), [(1, "Held(Reviewing)".into())]);
    let before = world.pr(1).head_ref_oid;

    let review = world.sent_back().await;
    world.fix_review(review).await;
    let Task::FixReview { findings } = &world.tasks()[0] else {
        panic!("{:?}", world.tasks());
    };
    assert_eq!(findings[0].id, "f1");
    let head = world.branch("pr-1");
    assert_ne!(head, before);
    let message = git(&world.origin, &["log", "-1", "--format=%B", &head]);
    assert!(message.contains(FIXED_BY), "{message}");
    assert!(world.did(1, "review-fixing") && world.did(1, "review-fixed"));
    let first = world.store.review(review).await.unwrap().unwrap();
    assert!(first.agent_fixed, "the author's rounds are not spent");

    // The next tick reviews what the agent pushed, and that review lets it go on.
    world.tick().await;
    assert_eq!(world.store.newest_review(1).0, head);
    assert_eq!(world.lane(), [(1, "Held(Reviewing)".into())]);
    assert!(world.merged().is_empty());
    world.store.finish_review(1, ReviewStatus::Approved);
    world.tick_until(|w| !w.merged().is_empty()).await;
}

#[tokio::test]
async fn a_fix_the_agent_cannot_make_leaves_the_findings_to_the_author() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    *world.agent.fails.lock().unwrap() = Some("the agent gave up".into());
    let before = world.pr(1).head_ref_oid;
    let review = world.sent_back().await;
    world.fix_review(review).await;
    assert!(world.did(1, "review-unfixed"), "{:?}", world.kinds(1));
    assert_eq!(world.branch("pr-1"), before);
    let first = world.store.review(review).await.unwrap().unwrap();
    assert!(!first.agent_fixed);
}

#[tokio::test]
async fn a_pr_with_no_fixes_left_gets_no_review_fix() {
    let world = World::new(
        |o| {
            commit(o, "shared.txt", "base\n", "start");
            git(o, &["checkout", "-q", "-b", "pr-1"]);
            commit(o, "feature.txt", "from the pr\n", "the PR's change");
            for i in 0..3 {
                commit(
                    o,
                    "fix.txt",
                    &format!("{i}\n"),
                    &format!("Fix\n\n{FIXED_BY}"),
                );
            }
            git(o, &["checkout", "-q", "main"]);
        },
        |s, o| add_pr(s, 1, o),
    )
    .await;
    let review = world.sent_back().await;
    world.fix_review(review).await;
    assert!(world.tasks().is_empty(), "the budget is its commits");
    assert!(!world.did(1, "review-fixing"));
}

#[tokio::test]
async fn a_job_that_fails_leaves_the_pr_to_the_lane() {
    // A check it cannot fix.
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.gh.with_mut(|s| checks_fail(s, 1));
    *world.agent.fails.lock().unwrap() = Some("the agent broke".into());
    let _drain = world.drain().await;
    world.tick_until(|w| w.did(1, "ejected")).await;
    assert!(world.did(1, "prep-left"), "{:?}", world.kinds(1));
    assert_eq!(world.gh.with(|s| s.prs[&1].labels.clone()), [FAILED]);
    assert_eq!(world.tasks().len(), 2, "off the lane, then on it");

    // A conflict it cannot resolve.
    let world = World::one_pr("shared.txt", "shared.txt").await;
    world.gh.with_mut(|s| {
        s.prs.get_mut(&1).unwrap().conflicting = true;
    });
    *world.agent.fails.lock().unwrap() = Some("the agent broke".into());
    let _drain = world.drain().await;
    world.tick_until(|w| w.did(1, "ejected")).await;
    assert!(world.did(1, "prep-left"), "{:?}", world.kinds(1));
    assert_eq!(world.tasks(), [Task::Rebase, Task::Rebase]);
}

#[tokio::test]
async fn a_pr_that_left_before_its_job_starts_gets_no_agent() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.gh.with_mut(|s| checks_fail(s, 1));
    let _drain = world.drain().await;
    world.tick().await;
    world
        .gh
        .with_mut(|s| s.prs.get_mut(&1).unwrap().labels.clear());
    let key = world.key();
    world.until(|w| w.ctx.prep.claim(&key, 1).is_some()).await;
    assert!(world.tasks().is_empty());
}

/// How a PR leaves, and what it does to GitHub.
type Departure = (&'static str, fn(&mut State));

/// What makes a PR leave the queue, or the trunk it was queued for.
fn departures() -> [Departure; 3] {
    [
        ("closed", |s| {
            s.prs.get_mut(&1).unwrap().state = "CLOSED".into();
        }),
        ("unlabeled", |s| s.prs.get_mut(&1).unwrap().labels.clear()),
        ("retargeted", |s| {
            s.add_branch("other");
            s.retarget(1, "other");
        }),
    ]
}

#[tokio::test]
async fn a_pr_that_leaves_while_its_review_fix_runs_gets_no_push() {
    for (how, leaves) in departures() {
        let world = World::one_pr("feature.txt", "other.txt").await;
        let before = world.pr(1).head_ref_oid;
        let gh = world.gh.clone();
        world.during(async move { gh.with_mut(leaves) });
        let review = world.sent_back().await;
        world.fix_review(review).await;
        assert_eq!(world.tasks().len(), 1, "{how}");
        assert_eq!(world.branch("pr-1"), before, "{how}");
        assert!(world.did(1, "review-unfixed"), "{how}");
        assert!(!world.did(1, "review-fixed"), "{how}");
    }
}

#[tokio::test]
async fn a_pr_that_leaves_while_its_review_fix_waits_for_the_cache_gets_no_push() {
    for (how, leaves) in departures() {
        let world = World::one_pr("feature.txt", "other.txt").await;
        let before = world.pr(1).head_ref_oid;
        // The lane has the cache as the agent ends, and the PR leaves before it lets go.
        let (ctx, gh) = (world.ctx.clone(), world.gh.clone());
        world.during(async move {
            let cache = ctx.caches.lock(&repo()).await;
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                gh.with_mut(leaves);
                drop(cache);
            });
        });
        let review = world.sent_back().await;
        world.fix_review(review).await;
        assert_eq!(world.tasks().len(), 1, "{how}");
        assert_eq!(world.branch("pr-1"), before, "{how}");
        assert!(!world.did(1, "review-fixed"), "{how}");
    }
}

#[tokio::test]
async fn a_pr_that_leaves_while_its_rebase_runs_gets_no_push() {
    for (how, leaves) in departures() {
        let world = World::one_pr("shared.txt", "shared.txt").await;
        world.gh.with_mut(|s| {
            s.prs.get_mut(&1).unwrap().conflicting = true;
        });
        let before = world.pr(1).head_ref_oid;
        let gh = world.gh.clone();
        world.during(async move { gh.with_mut(leaves) });
        let _drain = world.drain().await;
        world.tick().await;
        let key = world.key();
        world.until(|w| !w.tasks().is_empty()).await;
        world.until(|w| w.ctx.prep.claim(&key, 1).is_some()).await;
        assert_eq!(world.branch("pr-1"), before, "{how}");
        assert!(!world.did(1, "published"), "{how}");
        assert!(!world.did(1, "prep-left"), "{how}: nothing for the lane");
    }
}

#[tokio::test]
async fn a_job_waits_for_the_cache_but_the_agent_does_not_hold_it() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    let review = world.sent_back().await;

    // The fetch waits for whoever has the cache.
    let held = world.ctx.caches.lock(&repo()).await;
    let fixing = world.fix_review(review);
    let waited = tokio::time::timeout(Duration::from_millis(300), fixing).await;
    assert!(waited.is_err(), "it went on past the lock");
    assert!(world.tasks().is_empty(), "no agent before the fetch");
    drop(held);

    // While the agent works, the cache is free.
    let ctx = world.ctx.clone();
    world.during(async move {
        let free = tokio::time::timeout(Duration::from_secs(5), ctx.caches.lock(&repo())).await;
        free.expect("the agent's job holds the cache");
    });
    world.fix_review(review).await;
    assert!(world.did(1, "review-fixed"), "{:?}", world.kinds(1));
}

#[tokio::test]
async fn jobs_on_two_prs_run_at_once_on_one_cache() {
    let world = World::new(
        |o| {
            commit(o, "shared.txt", "base\n", "start");
            for n in [1, 2] {
                git(o, &["checkout", "-q", "-b", &format!("pr-{n}"), "main"]);
                commit(o, &format!("f{n}.txt"), "from the pr\n", "a change");
            }
            git(o, &["checkout", "-q", "main"]);
        },
        |s, o| {
            add_pr(s, 1, o);
            add_pr(s, 2, o);
            checks_fail(s, 1);
            checks_fail(s, 2);
        },
    )
    .await;
    // Each waits in its agent for the other.
    let both = Arc::new(Barrier::new(2));
    for _ in 0..2 {
        let both = both.clone();
        world.during(async move {
            both.wait().await;
        });
    }
    let heads = [world.pr(1).head_ref_oid, world.pr(2).head_ref_oid];
    let _drain = world.drain().await;
    world.tick().await;
    assert_eq!(world.ctx.prep.asked().len(), 2);
    world
        .until(|w| w.did(1, "published") && w.did(2, "published"))
        .await;
    assert_ne!(world.branch("pr-1"), heads[0]);
    assert_ne!(world.branch("pr-2"), heads[1]);
}

/// A stack of `pr-1` under `pr-2`, with an approval each. The trunk changes `trunk_file`.
async fn approved_stack(
    lower: &'static str,
    upper: &'static str,
    trunk_file: &'static str,
) -> World {
    let world = stack(lower, upper, trunk_file).await;
    for n in [1, 2] {
        let head = world.pr(n).head_ref_oid;
        let r = world
            .store
            .start_review(1, &repo(), n, &head)
            .await
            .unwrap();
        world.store.finish_review(r.number, ReviewStatus::Approved);
    }
    world
}

/// A stack of `pr-1` under `pr-2`, as stack 1. The trunk changes `trunk_file`.
async fn stack(lower: &'static str, upper: &'static str, trunk_file: &'static str) -> World {
    World::new(
        |o| {
            commit(o, "shared.txt", "base\n", "start");
            git(o, &["checkout", "-q", "-b", "pr-1"]);
            commit(o, lower, "from layer 1\n", "layer 1");
            git(o, &["checkout", "-q", "-b", "pr-2"]);
            commit(o, upper, "from layer 2\n", "layer 2");
            git(o, &["checkout", "-q", "main"]);
            commit(o, trunk_file, "from main\n", "the trunk's change");
        },
        |s, o| {
            s.add_stack(1, &[1, 2], &[1, 2]);
            point_at_branch(s, 1, o);
            point_at_branch(s, 2, o);
        },
    )
    .await
}

/// Whether `head` is a fix the agent committed.
fn a_fix(world: &World, head: &str) -> bool {
    let message = git(&world.origin, &["log", "-1", "--format=%B", head]);
    message.contains(FIXED_BY)
}

/// The commit the layer above sits on.
fn under_upper(world: &World) -> String {
    git(&world.origin, &["rev-parse", "pr-2~1"])
}

#[tokio::test]
async fn a_stack_s_lower_layer_is_fixed_off_the_lane_and_the_layer_above_goes_onto_it() {
    let world = stack("lower.txt", "upper.txt", "other.txt").await;
    world.gh.with_mut(|s| checks_fail(s, 1));
    let _drain = world.drain().await;
    world.tick().await;
    assert_eq!(world.lane(), [(1, "Held(PrepFixing)".into())]);

    let key = world.key();
    world.until(|w| w.ctx.prep.claim(&key, 1).is_some()).await;
    assert!(matches!(
        world.tasks()[..],
        [Task::FixChecks { .. }, Task::Rebase]
    ));
    let lower = world.branch("pr-1");
    assert!(a_fix(&world, &lower));
    assert_eq!(under_upper(&world), lower, "the layer above is on the fix");
    assert!(world.did(1, "published") && world.did(2, "published"));

    world.tick_until(|w| !w.merged().is_empty()).await;
    assert_eq!(world.merged(), [1, 2]);
}

#[tokio::test]
async fn a_review_s_fix_on_a_lower_layer_moves_the_layer_above_onto_it() {
    let world = stack("lower.txt", "upper.txt", "other.txt").await;
    let review = world.sent_back().await;
    world.fix_review(review).await;
    assert!(matches!(
        world.tasks()[..],
        [Task::FixReview { .. }, Task::Rebase]
    ));
    let lower = world.branch("pr-1");
    assert!(a_fix(&world, &lower));
    assert_eq!(under_upper(&world), lower);
    assert!(world.did(1, "review-fixed"), "{:?}", world.kinds(1));
    let first = world.store.review(review).await.unwrap().unwrap();
    assert!(first.agent_fixed);
    assert!(world.ctx.prep.claim(&world.key(), 1).is_some());
}

#[tokio::test]
async fn a_push_to_the_layer_above_while_a_fix_runs_stops_its_push() {
    let world = stack("lower.txt", "upper.txt", "other.txt").await;
    let lower = world.branch("pr-1");
    let gh = world.gh.clone();
    world.during(async move { gh.with_mut(|s| s.push(2, "pushed")) });
    let review = world.sent_back().await;
    world.fix_review(review).await;
    assert_eq!(world.tasks().len(), 1);
    assert_eq!(world.branch("pr-1"), lower);
    assert!(world.did(1, "review-unfixed"), "{:?}", world.kinds(1));
}

#[tokio::test]
async fn a_restack_that_fails_after_the_fix_leaves_the_stack_to_the_lane() {
    let world = stack("lower.txt", "upper.txt", "other.txt").await;
    let old = world.branch("pr-1");
    let agent = world.agent.clone();
    world.during(async {});
    world.during(async move { *agent.fails.lock().unwrap() = Some("the agent broke".into()) });
    let review = world.sent_back().await;
    world.fix_review(review).await;
    let lower = world.branch("pr-1");
    assert!(a_fix(&world, &lower), "the fix is published");
    assert_eq!(under_upper(&world), old, "the layer above is not");
    assert!(world.did(1, "review-unfixed"), "{:?}", world.kinds(1));
    assert!(world.ctx.prep.claim(&world.key(), 1).is_some());

    // The lane rebases the whole stack, the layer above onto the fix.
    *world.agent.fails.lock().unwrap() = None;
    world.tick_until(|w| !w.merged().is_empty()).await;
    assert_eq!(world.merged(), [1, 2]);
    assert_eq!(under_upper(&world), world.branch("pr-1"));
    assert!(git(&world.origin, &["log", "--format=%B", "main..pr-2"]).contains(FIXED_BY));
}

async fn rebase_stack(world: &World) -> Event {
    let unit = Unit {
        number: 1,
        base: "main".into(),
        layers: vec![world.pr(1), world.pr(2)],
        stacked: true,
    };
    let (key, settings) = (world.key(), &world.ctx.config.repo_defaults);
    let snap = GitHub::snapshot(&world.installation, &repo(), &settings.labels()).await;
    let snap = snap.unwrap();
    let sandbox = JobSandbox {
        ctx: &world.ctx,
        gh: &world.installation,
    };
    sandbox
        .rebase(&key, &snap, &unit, settings, true)
        .await
        .unwrap()
}

/// The status of the review on PR `number`'s head, and the head it goes on from.
fn review_of_head(world: &World, number: PrNumber) -> (ReviewStatus, Option<String>) {
    let head = world.pr(number).head_ref_oid;
    let reviews = world.store.reviews.lock().unwrap();
    let r = reviews
        .iter()
        .find(|r| r.number == number && r.head_sha == head);
    let r = r.expect("a review of the new head");
    (r.status, r.since_head.clone())
}

#[tokio::test]
async fn only_the_layer_whose_conflict_the_agent_resolved_loses_its_approval() {
    // The conflict is in the upper layer.
    let world = approved_stack("lower.txt", "shared.txt", "shared.txt").await;
    let upper = world.pr(2).head_ref_oid;
    let event = rebase_stack(&world).await;
    assert!(matches!(event, Event::Rebased { .. }), "{event:?}");
    assert_eq!(review_of_head(&world, 1), (ReviewStatus::Approved, None));
    assert_eq!(
        review_of_head(&world, 2),
        (ReviewStatus::Stale, Some(upper)),
        "the next review looks at what the agent wrote"
    );

    // The conflict is in the lower layer.
    let world = approved_stack("shared.txt", "upper.txt", "shared.txt").await;
    let lower = world.pr(1).head_ref_oid;
    let event = rebase_stack(&world).await;
    assert!(matches!(event, Event::Rebased { .. }), "{event:?}");
    assert_eq!(
        review_of_head(&world, 1),
        (ReviewStatus::Stale, Some(lower))
    );
    assert_eq!(review_of_head(&world, 2), (ReviewStatus::Approved, None));

    // The branches moved together, and the one the upper layer's job read is gone.
    assert_eq!(world.branch("pr-1"), world.pr(1).head_ref_oid);
    assert_eq!(world.branch("pr-2"), world.pr(2).head_ref_oid);
    let below = git(&world.origin, &["rev-parse", "pr-2~1"]);
    assert_eq!(below, world.branch("pr-1"));
    assert!(
        world
            .gh
            .with(|s| !s.branches.contains_key("mergepatrol/restack-1"))
    );
}

#[tokio::test]
async fn a_clean_rebase_keeps_every_layers_approval() {
    let world = approved_stack("lower.txt", "upper.txt", "other.txt").await;
    let event = rebase_stack(&world).await;
    assert!(matches!(event, Event::Rebased { .. }), "{event:?}");
    assert_eq!(review_of_head(&world, 1), (ReviewStatus::Approved, None));
    assert_eq!(review_of_head(&world, 2), (ReviewStatus::Approved, None));
}

#[tokio::test]
async fn a_finding_the_agent_fixes_is_never_posted() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    let before = world.pr(1).head_ref_oid;
    let review = world.conclude(100).await;
    assert_ne!(world.branch("pr-1"), before, "the agent pushed its fix");
    assert!(world.did(1, "review-fixed"), "{:?}", world.kinds(1));
    assert_eq!(world.posted(), Vec::<usize>::new());
    assert!(!world.fixing(review));
}

#[tokio::test]
async fn a_finding_the_fix_leaves_is_posted_after_it() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    *world.agent.fails.lock().unwrap() = Some("the agent gave up".into());
    let posted = world.gh.clone();
    world.during(async move {
        assert!(
            posted.with(|s| s.reviews.is_empty()),
            "not while the fix runs"
        );
    });
    let review = world.conclude(100).await;
    assert!(world.did(1, "review-unfixed"), "{:?}", world.kinds(1));
    assert_eq!(world.posted(), [1], "one thread, for the author");
    assert!(!world.fixing(review));
}

#[tokio::test]
async fn an_unsubmitted_author_review_does_not_reset_completed_review_rounds() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    world.sent_back().await;
    // Three completed reviews sent findings back. A fourth head waited for its
    // author's agent, but another push superseded it before anything was posted.
    {
        let mut reviews = world.store.reviews.lock().unwrap();
        let mut completed = reviews[0].clone();
        reviews.clear();
        for id in 1..=3 {
            completed.id = id;
            completed.head_sha = format!("completed-head-{id}");
            reviews.push(completed.clone());
        }
    }
    let abandoned = world
        .store
        .start_review(1, &repo(), 1, "superseded-author-head")
        .await
        .unwrap();
    world
        .store
        .await_author(abandoned.id, abandoned.claim, "Review this head")
        .await
        .unwrap();

    let mut settings = world.ctx.config.repo_defaults.clone();
    settings.agent.runs = mergepatrol_core::Runs::Local;
    let id = world.conclude_with(90, &settings).await;
    let finished = world.store.review(id).await.unwrap().unwrap();
    assert_eq!(
        finished.status,
        ReviewStatus::Asks,
        "an unsubmitted review is not a completed round: three completed send-backs still require a human"
    );
}

#[tokio::test]
async fn with_the_authors_agent_its_reviews_findings_go_to_the_author_unfixed() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    let mut settings = world.ctx.config.repo_defaults.clone();
    settings.agent.runs = mergepatrol_core::Runs::Local;
    let review = world.conclude_with(100, &settings).await;
    assert!(
        world.tasks().iter().all(|t| *t == Task::Rebase),
        "{:?}",
        world.tasks()
    );
    assert_eq!(world.posted(), [1], "one thread, for the author");
    assert!(!world.fixing(review));
}

#[tokio::test]
async fn a_finding_is_posted_at_once_while_a_job_holds_the_pr() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    let _job = world.ctx.prep.claim(&world.key(), 1).unwrap();
    world.conclude(100).await;
    assert!(world.tasks().is_empty(), "no fix");
    assert_eq!(world.posted(), [1]);
}

#[tokio::test]
async fn a_pr_pushed_to_during_the_fix_gets_no_out_of_date_review() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    *world.agent.fails.lock().unwrap() = Some("the agent gave up".into());
    let gh = world.gh.clone();
    world.during(async move {
        gh.with_mut(|s| s.prs.get_mut(&1).unwrap().head_ref_oid = "pushed".into());
    });
    let review = world.conclude(100).await;
    assert_eq!(world.posted(), Vec::<usize>::new());
    let r = world.store.review(review).await.unwrap().unwrap();
    assert_eq!(
        r.status,
        ReviewStatus::Stale,
        "the new head gets its own review"
    );
}

#[tokio::test]
async fn a_review_that_asks_a_human_posts_nothing_once_the_pr_was_pushed_to() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    // The author's agent reviews, so no fix runs: what it found goes straight to GitHub.
    let mut settings = world.ctx.config.repo_defaults.clone();
    settings.agent.runs = mergepatrol_core::Runs::Local;
    let head = world.pr(1).head_ref_oid;
    let r = world
        .store
        .start_review(1, &repo(), 1, &head)
        .await
        .unwrap();
    // The author pushes a fix while the review runs.
    world.gh.with_mut(|s| s.push(1, "fixed"));
    let review = world.conclude_review(r, 100, &settings).await;
    assert_eq!(
        world.posted(),
        Vec::<usize>::new(),
        "nothing about the old head"
    );
    let r = world.store.review(review).await.unwrap().unwrap();
    assert_eq!(
        r.status,
        ReviewStatus::Stale,
        "the new head gets its own review"
    );
}

#[tokio::test]
async fn a_review_that_clears_everything_posts_nothing_but_resolves_what_it_found_fixed() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    // A finding the author was left with, in a thread.
    let job = world.ctx.prep.claim(&world.key(), 1).unwrap();
    world.conclude(100).await;
    drop(job);
    assert_eq!(world.posted(), [1]);

    // The author pushes, and the next review clears everything.
    world.gh.with_mut(|s| s.push(1, "fixed"));
    world.conclude(10).await;
    assert!(world.did(1, "review-approved"), "{:?}", world.kinds(1));
    assert_eq!(world.posted(), [1], "no summary");
    let (_, replies, resolved) = world.gh.with(|s| s.threads[0].clone());
    assert!(resolved, "the thread it found fixed");
    assert!(
        replies[0].starts_with("Cleared by the review"),
        "{replies:?}"
    );
}

#[tokio::test]
async fn a_finding_is_posted_when_the_fix_errors() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    *world.store.review_reads_fail.lock().unwrap() = true;
    let review = world.conclude(100).await;
    assert!(world.tasks().is_empty(), "no fix");
    assert_eq!(world.posted(), [1]);
    assert!(!world.fixing(review));
}

#[tokio::test]
async fn a_review_whose_post_fails_after_the_fix_runs_again() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    *world.agent.fails.lock().unwrap() = Some("the agent gave up".into());
    // GitHub fails the read of the PR after the fix.
    let gh = world.gh.clone();
    world.during(async move { gh.with_mut(|s| s.outage = 1) });
    let review = world.conclude(100).await;
    assert!(world.did(1, "review-unfixed"), "{:?}", world.kinds(1));
    assert_eq!(world.posted(), Vec::<usize>::new());
    let r = world.store.review(review).await.unwrap().unwrap();
    assert_eq!(r.status, ReviewStatus::Pending, "the worker runs it again");
    assert!(!world.fixing(review));
}

#[tokio::test]
async fn a_fix_whose_branch_moves_under_it_gets_no_out_of_date_review() {
    let world = World::one_pr("feature.txt", "other.txt").await;
    world.review_on();
    world.tick().await;
    // The branch moves, though the PR GitHub shows still has the reviewed head.
    let gh = world.gh.clone();
    world.during(async move {
        gh.with_mut(|s| {
            let elsewhere = s.trunk.clone();
            s.branches.insert("pr-1".into(), elsewhere);
        });
    });
    let review = world.conclude(100).await;
    assert!(!world.did(1, "review-fixed"), "{:?}", world.kinds(1));
    assert_eq!(world.posted(), Vec::<usize>::new());
    let r = world.store.review(review).await.unwrap().unwrap();
    assert_eq!(r.status, ReviewStatus::Stale);
}
