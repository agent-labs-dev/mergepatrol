//! The queue's GitHub, reading from what it read before. It reads GitHub again only for what
//! changed since, as webhooks and its own writes tell, and for all of it every
//! `READ_ALL_EVERY`, in case a webhook is lost.

use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use mergepatrol_core::{CheckFailure, LabelSpec, Labels, PrNumber, RepoName};
use mergepatrol_github::{
    BranchMerge, Checks, Installation, MergeOutcome, PullRequest, RepoSnapshot, RequiredChecks,
    SnapshotParts, queued,
};
use mergepatrol_store::{Change, PrWrite, RepoReads, Store};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::github::GitHub;

pub const READ_ALL_EVERY: Duration = Duration::from_secs(15 * 60);
/// How long after a change GitHub's reads may still show what was before: a read sooner is not
/// kept.
const SETTLE: Duration = Duration::from_secs(10);
/// A fork PR's pull ref follows a push to the fork moments later, the PR's webhook sooner.
const PULL_REF_SETTLE: Duration = Duration::from_secs(30);

/// What the cache reads from GitHub. `Installation` in production; a fake in the tests.
#[async_trait]
pub trait Reads: Send + Sync {
    async fn config_file(&self, repo: &RepoName) -> Result<Option<String>>;
    async fn snapshot_parts(&self, repo: &RepoName, labels: &Labels) -> Result<SnapshotParts>;
    async fn pull_request_node(&self, repo: &RepoName, number: PrNumber) -> Result<Value>;
}

#[async_trait]
impl Reads for Installation {
    async fn config_file(&self, repo: &RepoName) -> Result<Option<String>> {
        GitHub::config_file(self, repo).await
    }
    async fn snapshot_parts(&self, repo: &RepoName, labels: &Labels) -> Result<SnapshotParts> {
        Installation::snapshot_parts(self, repo, labels).await
    }
    async fn pull_request_node(&self, repo: &RepoName, number: PrNumber) -> Result<Value> {
        Installation::pull_request_node(self, repo, number).await
    }
}

/// Whether a change `changed` ago is old enough for a read now to show it.
fn settled(changed: Option<Duration>, settle: Duration) -> bool {
    changed.is_none_or(|c| c >= settle)
}

/// Reads one installation's repos through the store.
pub struct Reader<'a> {
    pub installation: u64,
    pub reads: &'a dyn Reads,
    pub store: &'a dyn Store,
}

impl Reader<'_> {
    pub async fn config_file(&self, repo: &RepoName) -> Result<Option<String>> {
        let reads = self.store.github_reads(self.installation, repo).await?;
        if let Some(read) = reads.config
            && read.fresh(reads.changes, READ_ALL_EVERY)
        {
            return Ok(read.value);
        }
        let config = self.reads.config_file(repo).await?;
        if settled(reads.changed, SETTLE) {
            self.store
                .save_config_read(self.installation, repo, reads.changes, config.as_deref())
                .await?;
        }
        Ok(config)
    }

    pub async fn snapshot(&self, repo: &RepoName, labels: &Labels) -> Result<RepoSnapshot> {
        let reads = self.store.github_reads(self.installation, repo).await?;
        let asked = json!([labels.trigger, labels.priority, labels.failed]);
        let Some(read) = reads
            .repository
            .as_ref()
            .filter(|r| r.fresh(reads.changes, READ_ALL_EVERY) && r.value.0 == asked)
        else {
            return self.read_all(repo, labels, &reads, &asked).await;
        };
        let mut nodes = Vec::new();
        let mut writes = Vec::new();
        for pr in &reads.prs {
            if let (true, Some(node)) = (pr.fresh(), &pr.node) {
                nodes.push(node.clone());
                continue;
            }
            let node = match self.reads.pull_request_node(repo, pr.number).await {
                Ok(node) => node,
                // A PR that cannot be read, as one deleted, would fail every snapshot.
                Err(e) => {
                    tracing::warn!(installation = self.installation, %repo, pr = pr.number, error = %format!("{e:#}"), "reading the whole repo");
                    return self.read_all(repo, labels, &reads, &asked).await;
                }
            };
            let keep = queued(&node, &labels.trigger);
            if settled(pr.changed, SETTLE) {
                writes.push(PrWrite {
                    number: pr.number,
                    changes: pr.changes,
                    node: keep.then(|| node.clone()),
                });
            }
            if keep {
                nodes.push(node);
            }
        }
        if !writes.is_empty() {
            self.store
                .save_pr_reads(self.installation, repo, &writes)
                .await?;
        }
        RepoSnapshot::from_parts(&read.value.1, &nodes, labels)
    }

    async fn read_all(
        &self,
        repo: &RepoName,
        labels: &Labels,
        reads: &RepoReads,
        asked: &Value,
    ) -> Result<RepoSnapshot> {
        let parts = self.reads.snapshot_parts(repo, labels).await?;
        let known = |number: PrNumber| reads.prs.iter().find(|p| p.number == number);
        let mut writes: Vec<PrWrite> = Vec::new();
        for node in &parts.prs {
            let number = node["number"].as_u64().unwrap_or_default();
            let pr = known(number);
            if pr.is_none_or(|p| settled(p.changed, SETTLE)) {
                writes.push(PrWrite {
                    number,
                    changes: pr.map_or(0, |p| p.changes),
                    node: Some(node.clone()),
                });
            }
        }
        for pr in &reads.prs {
            let read = parts.prs.iter().any(|n| n["number"] == pr.number);
            if !read && settled(pr.changed, SETTLE) {
                writes.push(PrWrite {
                    number: pr.number,
                    changes: pr.changes,
                    node: None,
                });
            }
        }
        self.store
            .save_pr_reads(self.installation, repo, &writes)
            .await?;
        if settled(reads.changed, SETTLE) {
            self.store
                .save_repository_read(
                    self.installation,
                    repo,
                    reads.changes,
                    asked,
                    &parts.repository,
                )
                .await?;
        }
        RepoSnapshot::from_parts(&parts.repository, &parts.prs, labels)
    }

    /// A PR, whether queued or not, with the code owners of the repository as the snapshot read
    /// it. Without a fresh snapshot, `direct` reads both.
    pub async fn pull_request(
        &self,
        repo: &RepoName,
        number: PrNumber,
        direct: impl Future<Output = Result<PullRequest>> + Send,
    ) -> Result<PullRequest> {
        let reads = self.store.github_reads(self.installation, repo).await?;
        let Some(repository) = reads
            .repository
            .as_ref()
            .filter(|r| r.fresh(reads.changes, READ_ALL_EVERY))
            .map(|r| &r.value.1)
        else {
            return direct.await;
        };
        let pr = reads.prs.iter().find(|p| p.number == number);
        if let Some(node) = pr.filter(|p| p.fresh()).and_then(|p| p.node.as_ref()) {
            return PullRequest::from_node(node, repository);
        }
        let node = self.reads.pull_request_node(repo, number).await?;
        if pr.is_none_or(|p| settled(p.changed, SETTLE)) {
            let write = PrWrite {
                number,
                changes: pr.map_or(0, |p| p.changes),
                node: Some(node.clone()),
            };
            self.store
                .save_pr_reads(self.installation, repo, &[write])
                .await?;
        }
        PullRequest::from_node(&node, repository)
    }

    /// Another read, by `key`: what `read` returns, kept until the key changes, or for
    /// `max_age`.
    async fn value<T: Serialize + DeserializeOwned>(
        &self,
        repo: &RepoName,
        key: String,
        max_age: Option<Duration>,
        settle: Duration,
        read: impl Future<Output = Result<T>> + Send,
    ) -> Result<T> {
        let v = self
            .store
            .github_value(self.installation, repo, &key)
            .await?;
        if let Some(r) = &v.read
            && r.changes == v.changes
            && max_age.is_none_or(|m| r.age < m)
            && let Ok(value) = serde_json::from_value(r.value.clone())
        {
            return Ok(value);
        }
        let value = read.await?;
        if settled(v.changed, settle) {
            let json = serde_json::to_value(&value)?;
            self.store
                .save_github_value(self.installation, repo, &key, v.changes, &json)
                .await?;
        }
        Ok(value)
    }

    /// The queue changed something on GitHub.
    pub async fn changed(&self, repo: &RepoName, change: Change) -> Result<()> {
        self.store
            .github_changed(self.installation, repo, &change)
            .await
    }
}

/// An installation that reads through a `Reader`. Its writes mark what they change, for the
/// next read.
pub struct Cached<'a> {
    pub gh: &'a Installation,
    pub reader: Reader<'a>,
}

impl Cached<'_> {
    async fn marking<T>(
        &self,
        repo: &RepoName,
        changed: Vec<Change>,
        write: Result<T>,
    ) -> Result<T> {
        for change in changed {
            self.reader.changed(repo, change).await?;
        }
        write
    }
}

fn under(prefix: &str) -> Change {
    Change::ValuesUnder(prefix.into())
}

fn key(key: String) -> Change {
    Change::Value(key)
}

#[async_trait]
impl GitHub for Cached<'_> {
    fn installation_id(&self) -> u64 {
        self.gh.id
    }
    async fn config_file(&self, repo: &RepoName) -> Result<Option<String>> {
        self.reader.config_file(repo).await
    }
    async fn snapshot(&self, repo: &RepoName, labels: &Labels) -> Result<RepoSnapshot> {
        self.reader.snapshot(repo, labels).await
    }
    async fn pull_request(&self, repo: &RepoName, number: PrNumber) -> Result<PullRequest> {
        let direct = GitHub::pull_request(self.gh, repo, number);
        self.reader.pull_request(repo, number, direct).await
    }
    async fn stack_layers(&self, repo: &RepoName, stack: PrNumber) -> Result<Vec<PrNumber>> {
        let read = GitHub::stack_layers(self.gh, repo, stack);
        let key = format!("stack:{stack}");
        self.reader
            .value(repo, key, Some(READ_ALL_EVERY), SETTLE, read)
            .await
    }
    async fn branch_sha(&self, repo: &RepoName, branch: &str) -> Result<String> {
        let read = GitHub::branch_sha(self.gh, repo, branch);
        let key = format!("ref:{branch}");
        self.reader
            .value(repo, key, Some(READ_ALL_EVERY), SETTLE, read)
            .await
    }
    async fn pull_head(&self, repo: &RepoName, number: PrNumber) -> Result<String> {
        let read = GitHub::pull_head(self.gh, repo, number);
        let key = format!("pull:{number}");
        self.reader
            .value(repo, key, Some(READ_ALL_EVERY), PULL_REF_SETTLE, read)
            .await
    }
    async fn behind(&self, repo: &RepoName, base: &str, head: &str) -> Result<bool> {
        // Two commits: what one lacks of the other never changes.
        let read = GitHub::behind(self.gh, repo, base, head);
        let key = format!("behind:{base}...{head}");
        self.reader
            .value(repo, key, None, Duration::ZERO, read)
            .await
    }
    async fn awaiting_approval(&self, repo: &RepoName, sha: &str) -> Result<bool> {
        let read = GitHub::awaiting_approval(self.gh, repo, sha);
        let key = format!("approval:{sha}");
        self.reader
            .value(repo, key, Some(READ_ALL_EVERY), SETTLE, read)
            .await
    }
    async fn changes_under(&self, repo: &RepoName, number: PrNumber, dir: &str) -> Result<bool> {
        GitHub::changes_under(self.gh, repo, number, dir).await
    }
    async fn required_checks(&self, repo: &RepoName, branch: &str) -> Result<RequiredChecks> {
        let read = GitHub::required_checks(self.gh, repo, branch);
        let key = format!("required:{branch}");
        self.reader
            .value(repo, key, Some(READ_ALL_EVERY), SETTLE, read)
            .await
    }
    async fn checks(&self, repo: &RepoName, sha: &str) -> Result<Checks> {
        let read = GitHub::checks(self.gh, repo, sha);
        let key = format!("checks:{sha}");
        self.reader
            .value(repo, key, Some(READ_ALL_EVERY), SETTLE, read)
            .await
    }
    async fn check_failures(
        &self,
        repo: &RepoName,
        sha: &str,
        names: &[String],
    ) -> Result<Vec<CheckFailure>> {
        GitHub::check_failures(self.gh, repo, sha, names).await
    }
    async fn merge(
        &self,
        repo: &RepoName,
        number: PrNumber,
        sha: &str,
        method: &str,
    ) -> Result<MergeOutcome> {
        let merged = GitHub::merge(self.gh, repo, number, sha, method).await;
        let changed = vec![
            Change::Pr(number),
            Change::Repo,
            under("ref:"),
            key(format!("pull:{number}")),
        ];
        self.marking(repo, changed, merged).await
    }
    async fn merged_as(
        &self,
        repo: &RepoName,
        number: PrNumber,
        head: &str,
    ) -> Result<Option<String>> {
        GitHub::merged_as(self.gh, repo, number, head).await
    }
    async fn create_branch(&self, repo: &RepoName, branch: &str, sha: &str) -> Result<()> {
        let created = GitHub::create_branch(self.gh, repo, branch, sha).await;
        self.marking(repo, vec![key(format!("ref:{branch}"))], created)
            .await
    }
    async fn delete_branch(&self, repo: &RepoName, branch: &str) -> Result<()> {
        let deleted = GitHub::delete_branch(self.gh, repo, branch).await;
        self.marking(repo, vec![key(format!("ref:{branch}"))], deleted)
            .await
    }
    async fn merge_into_branch(
        &self,
        repo: &RepoName,
        branch: &str,
        head: &str,
        message: &str,
    ) -> Result<Option<BranchMerge>> {
        let merged = GitHub::merge_into_branch(self.gh, repo, branch, head, message).await;
        self.marking(repo, vec![key(format!("ref:{branch}"))], merged)
            .await
    }
    async fn open_draft_pr(
        &self,
        repo: &RepoName,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrNumber> {
        GitHub::open_draft_pr(self.gh, repo, head, base, title, body).await
    }
    async fn commit_tree(&self, repo: &RepoName, sha: &str) -> Result<String> {
        GitHub::commit_tree(self.gh, repo, sha).await
    }
    async fn create_label(&self, repo: &RepoName, label: &LabelSpec<'_>) -> Result<()> {
        let created = GitHub::create_label(self.gh, repo, label).await;
        self.marking(repo, vec![Change::Repo], created).await
    }
    async fn add_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()> {
        let added = GitHub::add_label(self.gh, repo, number, label).await;
        self.marking(repo, vec![Change::Pr(number)], added).await
    }
    async fn remove_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()> {
        let removed = GitHub::remove_label(self.gh, repo, number, label).await;
        self.marking(repo, vec![Change::Pr(number)], removed).await
    }
    async fn sticky_comment(&self, repo: &RepoName, number: PrNumber, body: &str) -> Result<()> {
        GitHub::sticky_comment(self.gh, repo, number, body).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use mergepatrol_store::{PgStore, ReadCache};
    use sqlx::PgPool;

    use super::*;

    fn repo() -> RepoName {
        RepoName::parse("o/r").unwrap()
    }

    fn labels(trigger: &str) -> Labels {
        Labels {
            trigger: trigger.into(),
            priority: "mp:priority".into(),
            failed: "mp:failed".into(),
        }
    }

    fn node(number: PrNumber, state: &str, label: &str) -> Value {
        json!({
            "number": number, "state": state, "title": "t", "body": "", "isDraft": false,
            "isCrossRepository": false, "reviewDecision": null, "baseRefName": "main",
            "headRefName": format!("b{number}"), "headRefOid": format!("h{number}"),
            "labels": { "nodes": [{ "name": label }] }, "files": { "nodes": [] }, "stack": null,
            "url": "u", "createdAt": format!("2026-10-0{number}T00:00:00Z"), "additions": 0,
            "deletions": 0, "changedFiles": 0, "author": null,
            "latestOpinionatedReviews": { "nodes": [] }, "reviewRequests": { "nodes": [] },
            "commits": { "nodes": [] },
        })
    }

    /// GitHub's PRs, by number, and every read the cache made of it.
    #[derive(Default)]
    struct Fake {
        trunk: Mutex<String>,
        prs: Mutex<BTreeMap<PrNumber, Value>>,
        fail_pr: Mutex<Option<PrNumber>>,
        calls: Mutex<Vec<String>>,
    }

    impl Fake {
        fn set_pr(&self, number: PrNumber, state: &str, label: &str) {
            let pr = node(number, state, label);
            self.prs.lock().unwrap().insert(number, pr);
        }
        fn calls(&self) -> Vec<String> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }
    }

    #[async_trait]
    impl Reads for Fake {
        async fn config_file(&self, _: &RepoName) -> Result<Option<String>> {
            self.calls.lock().unwrap().push("config".into());
            Ok(Some("label: mp".into()))
        }
        async fn snapshot_parts(&self, _: &RepoName, labels: &Labels) -> Result<SnapshotParts> {
            self.calls.lock().unwrap().push("snapshot".into());
            let prs = self.prs.lock().unwrap();
            Ok(SnapshotParts {
                repository: json!({
                    "id": "R", "isPrivate": false, "trigger": { "name": labels.trigger },
                    "priority": null, "failed": null, "viewerDefaultMergeMethod": "SQUASH",
                    "defaultBranchRef": { "name": "main", "target": { "oid": *self.trunk.lock().unwrap() } },
                }),
                prs: prs
                    .values()
                    .filter(|pr| queued(pr, &labels.trigger))
                    .cloned()
                    .collect(),
            })
        }
        async fn pull_request_node(&self, _: &RepoName, number: PrNumber) -> Result<Value> {
            self.calls.lock().unwrap().push(format!("pr {number}"));
            if *self.fail_pr.lock().unwrap() == Some(number) {
                anyhow::bail!("no PR #{number}");
            }
            Ok(self.prs.lock().unwrap()[&number].clone())
        }
    }

    /// As a webhook would, long enough ago for GitHub's reads to show it.
    async fn changed_a_while_ago(store: &PgStore, pool: &PgPool, change: Change) {
        store.github_changed(1, &repo(), &change).await.unwrap();
        for aged in [
            "UPDATE github_repos SET changed_at = changed_at - interval '1 minute'",
            "UPDATE github_prs SET changed_at = changed_at - interval '1 minute'",
            "UPDATE github_values SET changed_at = changed_at - interval '1 minute'",
        ] {
            sqlx::query(aged).execute(pool).await.unwrap();
        }
    }

    async fn numbers(reader: &Reader<'_>, trigger: &str) -> Vec<PrNumber> {
        let snap = reader.snapshot(&repo(), &labels(trigger)).await.unwrap();
        snap.labeled.iter().map(|pr| pr.number).collect()
    }

    #[sqlx::test(migrations = false)]
    async fn an_unchanged_repo_reads_nothing_from_github(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let gh = Fake::default();
        gh.set_pr(1, "OPEN", "mp");
        gh.set_pr(2, "OPEN", "other");
        let reader = Reader {
            installation: 1,
            reads: &gh,
            store: &store,
        };
        for _ in 0..3 {
            assert_eq!(
                reader.config_file(&repo()).await.unwrap().as_deref(),
                Some("label: mp")
            );
            assert_eq!(numbers(&reader, "mp").await, [1]);
        }
        assert_eq!(gh.calls(), ["config", "snapshot"]);
    }

    #[sqlx::test(migrations = false)]
    async fn a_changed_pr_is_all_that_reads_again(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let gh = Fake::default();
        gh.set_pr(1, "OPEN", "mp");
        gh.set_pr(2, "OPEN", "other");
        let reader = Reader {
            installation: 1,
            reads: &gh,
            store: &store,
        };
        assert_eq!(numbers(&reader, "mp").await, [1]);
        gh.calls();

        // #2 is labeled, and #1 closes: their webhooks come.
        gh.set_pr(2, "OPEN", "mp");
        gh.set_pr(1, "CLOSED", "mp");
        changed_a_while_ago(&store, &pool, Change::Pr(2)).await;
        changed_a_while_ago(&store, &pool, Change::Pr(1)).await;
        assert_eq!(numbers(&reader, "mp").await, [2]);
        assert_eq!(gh.calls(), ["pr 1", "pr 2"]);
        assert_eq!(numbers(&reader, "mp").await, [2]);
        assert!(gh.calls().is_empty());
    }

    #[sqlx::test(migrations = false)]
    async fn a_change_to_the_repo_reads_it_all_again(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let gh = Fake::default();
        *gh.trunk.lock().unwrap() = "t1".into();
        gh.set_pr(1, "OPEN", "mp");
        let reader = Reader {
            installation: 1,
            reads: &gh,
            store: &store,
        };
        reader.config_file(&repo()).await.unwrap();
        numbers(&reader, "mp").await;
        gh.calls();

        // A push to the trunk.
        *gh.trunk.lock().unwrap() = "t2".into();
        changed_a_while_ago(&store, &pool, Change::Repo).await;
        reader.config_file(&repo()).await.unwrap();
        let snap = reader.snapshot(&repo(), &labels("mp")).await.unwrap();
        assert_eq!(snap.trunk_sha, "t2");
        assert_eq!(gh.calls(), ["config", "snapshot"]);

        // Other labels in the config file.
        gh.set_pr(2, "OPEN", "queue");
        assert_eq!(numbers(&reader, "queue").await, [2]);
        assert_eq!(gh.calls(), ["snapshot"]);
    }

    #[sqlx::test(migrations = false)]
    async fn a_pr_that_cannot_be_read_reads_the_repo_again(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let gh = Fake::default();
        gh.set_pr(1, "OPEN", "mp");
        let reader = Reader {
            installation: 1,
            reads: &gh,
            store: &store,
        };
        numbers(&reader, "mp").await;
        gh.calls();

        // Gone from GitHub.
        gh.prs.lock().unwrap().remove(&1);
        *gh.fail_pr.lock().unwrap() = Some(1);
        changed_a_while_ago(&store, &pool, Change::Pr(1)).await;
        assert!(numbers(&reader, "mp").await.is_empty());
        assert!(numbers(&reader, "mp").await.is_empty());
        assert_eq!(gh.calls(), ["pr 1", "snapshot"]);
    }

    #[sqlx::test(migrations = false)]
    async fn the_queues_own_writes_mark_what_they_change(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let (gh, _) = mergepatrol_github::fake::serve(vec![
            (200, "", "[]"),
            (200, "", "{}"),
            (201, "", "{}"),
            (204, "", ""),
        ])
        .await;
        let cached = Cached {
            gh: &gh,
            reader: Reader {
                installation: 1,
                reads: &gh,
                store: &store,
            },
        };
        store.github_reads(1, &repo()).await.unwrap();
        cached.add_label(&repo(), 7, "mp").await.unwrap();
        cached.remove_label(&repo(), 8, "mp").await.unwrap();
        let spec = LabelSpec {
            name: "mp",
            color: "ffffff",
            description: "",
        };
        cached.create_label(&repo(), &spec).await.unwrap();
        for key in ["ref:batch", "ref:main"] {
            store.github_value(1, &repo(), key).await.unwrap();
        }
        cached.delete_branch(&repo(), "batch").await.unwrap();
        let moved = async |key| store.github_value(1, &repo(), key).await.unwrap().changes;
        assert_eq!((moved("ref:batch").await, moved("ref:main").await), (1, 0));
        let reads = store.github_reads(1, &repo()).await.unwrap();
        assert_eq!(reads.changes, 1);
        assert_eq!(
            reads
                .prs
                .iter()
                .map(|p| (p.number, p.changes))
                .collect::<Vec<_>>(),
            [(7, 1), (8, 1)]
        );
    }

    #[sqlx::test(migrations = false)]
    async fn a_read_soon_after_a_change_is_not_kept(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let gh = Fake::default();
        gh.set_pr(1, "OPEN", "mp");
        let reader = Reader {
            installation: 1,
            reads: &gh,
            store: &store,
        };
        numbers(&reader, "mp").await;
        gh.calls();

        // The webhook just came: GitHub may not show the change yet.
        store
            .github_changed(1, &repo(), &Change::Pr(1))
            .await
            .unwrap();
        numbers(&reader, "mp").await;
        numbers(&reader, "mp").await;
        assert_eq!(gh.calls(), ["pr 1", "pr 1"]);

        changed_a_while_ago(&store, &pool, Change::ValuesUnder("none".into())).await;
        numbers(&reader, "mp").await;
        numbers(&reader, "mp").await;
        assert_eq!(gh.calls(), ["pr 1"]);
    }

    #[sqlx::test(migrations = false)]
    async fn a_value_reads_again_once_its_key_changes(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let gh = Fake::default();
        let reader = Reader {
            installation: 1,
            reads: &gh,
            store: &store,
        };
        let reads = Mutex::new(0);
        let read = |key: &'static str, max_age| {
            let reads = &reads;
            let reader = &reader;
            async move {
                let read = async {
                    *reads.lock().unwrap() += 1;
                    Ok(format!("v{}", reads.lock().unwrap()))
                };
                let value: String = reader
                    .value(&repo(), key.into(), max_age, SETTLE, read)
                    .await
                    .unwrap();
                value
            }
        };
        assert_eq!(read("checks:a", Some(READ_ALL_EVERY)).await, "v1");
        assert_eq!(read("checks:a", Some(READ_ALL_EVERY)).await, "v1");
        changed_a_while_ago(&store, &pool, Change::Value("checks:a".into())).await;
        assert_eq!(read("checks:a", Some(READ_ALL_EVERY)).await, "v2");
        assert_eq!(read("checks:a", Some(READ_ALL_EVERY)).await, "v2");

        // Too old to keep.
        assert_eq!(read("checks:a", Some(Duration::ZERO)).await, "v3");
        // One that never changes keeps.
        assert_eq!(read("behind:a...b", None).await, "v4");
        assert_eq!(read("behind:a...b", None).await, "v4");
    }

    #[sqlx::test(migrations = false)]
    async fn a_pr_reads_from_the_snapshot_and_keeps(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let gh = Fake::default();
        gh.set_pr(1, "OPEN", "mp");
        gh.set_pr(2, "OPEN", "other");
        let reader = Reader {
            installation: 1,
            reads: &gh,
            store: &store,
        };
        let direct = || async { anyhow::bail!("read directly") };
        // No snapshot yet, to read its code owners from.
        assert!(reader.pull_request(&repo(), 1, direct()).await.is_err());

        numbers(&reader, "mp").await;
        gh.calls();
        let pr = reader.pull_request(&repo(), 1, direct()).await.unwrap();
        assert_eq!(pr.head_ref_oid, "h1");
        assert!(gh.calls().is_empty());

        // Not queued: read once, then kept.
        for _ in 0..2 {
            let pr = reader.pull_request(&repo(), 2, direct()).await.unwrap();
            assert_eq!(pr.number, 2);
        }
        assert_eq!(gh.calls(), ["pr 2"]);
        assert_eq!(numbers(&reader, "mp").await, [1]);
    }

    #[sqlx::test(migrations = false)]
    async fn a_first_read_soon_after_its_change_is_not_kept(pool: PgPool) {
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let gh = Fake::default();
        let reader = Reader {
            installation: 1,
            reads: &gh,
            store: &store,
        };
        numbers(&reader, "mp").await;
        let reads = Mutex::new(0);
        let read = async |key: &str| {
            let read = async {
                *reads.lock().unwrap() += 1;
                Ok(())
            };
            let max_age = Some(READ_ALL_EVERY);
            reader
                .value(&repo(), key.into(), max_age, SETTLE, read)
                .await
                .unwrap();
        };
        // A new head's check run, and a publish, before the queue first reads either.
        for change in [
            Change::Value("checks:new".into()),
            Change::ValuesUnder("ref:".into()),
        ] {
            store.github_changed(1, &repo(), &change).await.unwrap();
        }
        for key in ["checks:new", "checks:new", "ref:topic", "ref:topic"] {
            read(key).await;
        }
        assert_eq!(*reads.lock().unwrap(), 4);

        sqlx::query("UPDATE github_values SET changed_at = changed_at - interval '1 minute'")
            .execute(&pool)
            .await
            .unwrap();
        for key in ["checks:new", "checks:new", "ref:topic", "ref:topic"] {
            read(key).await;
        }
        assert_eq!(*reads.lock().unwrap(), 6);
    }
}
