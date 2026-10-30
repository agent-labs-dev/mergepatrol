//! The queue's GitHub, reading the config file and the snapshot from what it read before. It
//! reads GitHub again only for what changed since, as webhooks and its own writes tell, and for
//! all of it every `READ_ALL_EVERY`, in case a webhook is lost.

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use mergepatrol_core::{CheckFailure, LabelSpec, Labels, PrNumber, RepoName};
use mergepatrol_github::{
    BranchMerge, Checks, Installation, MergeOutcome, PullRequest, RepoSnapshot, RequiredChecks,
    SnapshotParts, queued,
};
use mergepatrol_store::{PrWrite, RepoReads, Store};
use serde_json::{Value, json};

use crate::github::GitHub;

pub const READ_ALL_EVERY: Duration = Duration::from_secs(15 * 60);

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
        self.store
            .save_config_read(self.installation, repo, reads.changes, config.as_deref())
            .await?;
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
            writes.push(PrWrite {
                number: pr.number,
                changes: pr.changes,
                node: keep.then(|| node.clone()),
            });
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
        let changes = |number: PrNumber| {
            reads
                .prs
                .iter()
                .find(|p| p.number == number)
                .map_or(0, |p| p.changes)
        };
        let mut writes: Vec<PrWrite> = Vec::new();
        for node in &parts.prs {
            let number = node["number"].as_u64().unwrap_or_default();
            writes.push(PrWrite {
                number,
                changes: changes(number),
                node: Some(node.clone()),
            });
        }
        for pr in &reads.prs {
            if !writes.iter().any(|w| w.number == pr.number) {
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
        self.store
            .save_repository_read(
                self.installation,
                repo,
                reads.changes,
                asked,
                &parts.repository,
            )
            .await?;
        RepoSnapshot::from_parts(&parts.repository, &parts.prs, labels)
    }

    /// The queue changed something on GitHub: the repo itself with `None`, or one of its PRs.
    pub async fn changed(&self, repo: &RepoName, pr: Option<PrNumber>) -> Result<()> {
        self.store.github_changed(self.installation, repo, pr).await
    }
}

/// An installation whose config file and snapshot come through a `Reader`. Its writes that
/// change those mark what they change, for the next read.
pub struct Cached<'a> {
    pub gh: &'a Installation,
    pub reader: Reader<'a>,
}

impl Cached<'_> {
    async fn marking<T>(
        &self,
        repo: &RepoName,
        changed: &[Option<PrNumber>],
        write: Result<T>,
    ) -> Result<T> {
        for pr in changed {
            self.reader.changed(repo, *pr).await?;
        }
        write
    }
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
        GitHub::pull_request(self.gh, repo, number).await
    }
    async fn stack_layers(&self, repo: &RepoName, stack: PrNumber) -> Result<Vec<PrNumber>> {
        GitHub::stack_layers(self.gh, repo, stack).await
    }
    async fn branch_sha(&self, repo: &RepoName, branch: &str) -> Result<String> {
        GitHub::branch_sha(self.gh, repo, branch).await
    }
    async fn pull_head(&self, repo: &RepoName, number: PrNumber) -> Result<String> {
        GitHub::pull_head(self.gh, repo, number).await
    }
    async fn behind(&self, repo: &RepoName, base: &str, head: &str) -> Result<bool> {
        GitHub::behind(self.gh, repo, base, head).await
    }
    async fn awaiting_approval(&self, repo: &RepoName, sha: &str) -> Result<bool> {
        GitHub::awaiting_approval(self.gh, repo, sha).await
    }
    async fn changes_under(&self, repo: &RepoName, number: PrNumber, dir: &str) -> Result<bool> {
        GitHub::changes_under(self.gh, repo, number, dir).await
    }
    async fn required_checks(&self, repo: &RepoName, branch: &str) -> Result<RequiredChecks> {
        GitHub::required_checks(self.gh, repo, branch).await
    }
    async fn checks(&self, repo: &RepoName, sha: &str) -> Result<Checks> {
        GitHub::checks(self.gh, repo, sha).await
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
        self.marking(repo, &[Some(number), None], merged).await
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
        GitHub::create_branch(self.gh, repo, branch, sha).await
    }
    async fn delete_branch(&self, repo: &RepoName, branch: &str) -> Result<()> {
        GitHub::delete_branch(self.gh, repo, branch).await
    }
    async fn merge_into_branch(
        &self,
        repo: &RepoName,
        branch: &str,
        head: &str,
        message: &str,
    ) -> Result<Option<BranchMerge>> {
        GitHub::merge_into_branch(self.gh, repo, branch, head, message).await
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
        self.marking(repo, &[None], created).await
    }
    async fn add_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()> {
        let added = GitHub::add_label(self.gh, repo, number, label).await;
        self.marking(repo, &[Some(number)], added).await
    }
    async fn remove_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()> {
        let removed = GitHub::remove_label(self.gh, repo, number, label).await;
        self.marking(repo, &[Some(number)], removed).await
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

    async fn numbers(reader: &Reader<'_>, trigger: &str) -> Vec<PrNumber> {
        let snap = reader.snapshot(&repo(), &labels(trigger)).await.unwrap();
        snap.labeled.iter().map(|pr| pr.number).collect()
    }

    #[sqlx::test(migrations = false)]
    async fn an_unchanged_repo_reads_nothing_from_github(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
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
        let store = PgStore::migrate(pool).await.unwrap();
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
        store.github_changed(1, &repo(), Some(2)).await.unwrap();
        store.github_changed(1, &repo(), Some(1)).await.unwrap();
        assert_eq!(numbers(&reader, "mp").await, [2]);
        assert_eq!(gh.calls(), ["pr 1", "pr 2"]);
        assert_eq!(numbers(&reader, "mp").await, [2]);
        assert!(gh.calls().is_empty());
    }

    #[sqlx::test(migrations = false)]
    async fn a_change_to_the_repo_reads_it_all_again(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
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
        store.github_changed(1, &repo(), None).await.unwrap();
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
        let store = PgStore::migrate(pool).await.unwrap();
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
        store.github_changed(1, &repo(), Some(1)).await.unwrap();
        assert!(numbers(&reader, "mp").await.is_empty());
        assert!(numbers(&reader, "mp").await.is_empty());
        assert_eq!(gh.calls(), ["pr 1", "snapshot"]);
    }

    #[sqlx::test(migrations = false)]
    async fn the_queues_own_writes_mark_what_they_change(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let (gh, _) = mergepatrol_github::fake::serve(vec![
            (200, "", "[]"),
            (200, "", "{}"),
            (201, "", "{}"),
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
}
