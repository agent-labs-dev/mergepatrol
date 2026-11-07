use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use mergepatrol_core::{CheckFailure, RepoName};

use crate::Installation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckState {
    Pending,
    Passed,
    Failed,
}

/// The checks on one commit, by name. Check runs and commit statuses both count.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Checks {
    pub by_name: BTreeMap<String, CheckState>,
    /// The checks that passed without running: skipped, or neutral.
    #[serde(default)]
    pub skipped: BTreeSet<String>,
    /// Whether a workflow run on the commit has yet to finish: queued behind its `concurrency`
    /// group, waiting for an approval, or running. Its jobs may not have check runs yet. `None`
    /// when no GitHub Actions check run is on the commit, so the workflow runs were not read:
    /// `Installation::workflows_running` reads them.
    #[serde(default)]
    pub workflows_running: Option<bool>,
    /// The failed checks that GitHub Actions can run again, each with its workflow run, which is
    /// on its first attempt. What failed again on a later attempt, or is no Actions job, stays
    /// failed.
    #[serde(default)]
    pub reruns: BTreeMap<String, u64>,
}

impl Checks {
    /// The verdict for `required`, with the checks that failed, or else those still pending. With
    /// no required checks, every check on the commit counts.
    pub fn verdict(&self, required: &[String]) -> (CheckState, Vec<String>) {
        let relevant: Vec<(&String, CheckState)> = if required.is_empty() {
            self.by_name.iter().map(|(n, s)| (n, *s)).collect()
        } else {
            required
                .iter()
                .map(|n| {
                    (
                        n,
                        self.by_name.get(n).copied().unwrap_or(CheckState::Pending),
                    )
                })
                .collect()
        };
        let named = |state: CheckState| -> Vec<String> {
            relevant
                .iter()
                .filter(|(_, s)| *s == state)
                .map(|(n, _)| (*n).clone())
                .collect()
        };
        let (failed, pending) = (named(CheckState::Failed), named(CheckState::Pending));
        if !failed.is_empty() {
            (CheckState::Failed, failed)
        } else if !pending.is_empty() {
            (CheckState::Pending, pending)
        } else {
            (CheckState::Passed, vec![])
        }
    }

    /// The `required` checks that never reported on the commit, once no check that did is still
    /// running: CI is done, and does not run them, as when a PR renames one. Empty while any
    /// check runs, as a job that waits for another creates its check only when it starts, and
    /// while a workflow run is known to have yet to finish, as one queued creates no check runs
    /// yet. Empty too while no check has reported at all: CI may not have started, as in an
    /// outage.
    pub fn unreported(&self, required: &[String]) -> Vec<String> {
        if self.by_name.is_empty()
            || self.workflows_running == Some(true)
            || self.by_name.values().any(|s| *s == CheckState::Pending)
        {
            return Vec::new();
        }
        required
            .iter()
            .filter(|n| !self.by_name.contains_key(*n))
            .cloned()
            .collect()
    }
}

/// What a branch's rules ask of a merge into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchRules {
    /// The status checks that must pass.
    pub checks: Vec<String>,
    /// The PR must also be up to date with the branch: GitHub merges only a PR that has every
    /// commit of the branch.
    pub strict: bool,
    /// Of `squash`, `rebase` and `merge`, those the rulesets allow.
    pub merge_methods: Vec<String>,
}

/// The merge methods, in the order to fall back on when the repo's default is not allowed.
const MERGE_METHODS: [&str; 3] = ["squash", "rebase", "merge"];

impl Default for BranchRules {
    fn default() -> Self {
        Self {
            checks: Vec::new(),
            strict: false,
            merge_methods: MERGE_METHODS.map(String::from).to_vec(),
        }
    }
}

impl BranchRules {
    /// The method to merge with: `default` if the rules allow it, or else the first they allow.
    pub fn merge_method(&self, default: &str) -> String {
        let allowed = |m: &str| self.merge_methods.iter().any(|a| a == m);
        if allowed(default) {
            return default.to_owned();
        }
        MERGE_METHODS
            .into_iter()
            .find(|m| allowed(m))
            .unwrap_or(default)
            .to_owned()
    }
}

impl Installation {
    /// What the branch's rulesets and classic protection ask of a merge into it.
    pub async fn branch_rules(&self, repo: &RepoName, branch: &str) -> Result<BranchRules> {
        let mut rules = BranchRules::default();
        let res = self
            .request(
                Method::GET,
                &format!("/repos/{repo}/rules/branches/{branch}"),
                None,
            )
            .await?;
        let active = match res.status {
            s if s.is_success() => res.body.as_array().cloned().unwrap_or_default(),
            // GitHub Free has no rulesets on a private repo, and answers 403 for them.
            StatusCode::NOT_FOUND | StatusCode::FORBIDDEN => Vec::new(),
            s => bail!("rules of {branch}: {s} {}", res.body),
        };
        for rule in &active {
            let params = &rule["parameters"];
            match rule["type"].as_str() {
                Some("required_status_checks") => {
                    rules.strict |= params["strict_required_status_checks_policy"] == true;
                    for check in params["required_status_checks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        if let Some(c) = check["context"].as_str() {
                            rules.checks.push(c.to_owned());
                        }
                    }
                }
                Some("pull_request") => {
                    if let Some(allowed) = params["allowed_merge_methods"].as_array() {
                        rules
                            .merge_methods
                            .retain(|m| allowed.iter().any(|a| a == m.as_str()));
                    }
                }
                Some("required_linear_history") => rules.merge_methods.retain(|m| m != "merge"),
                _ => {}
            }
        }
        // Classic protection needs the "administration: read" permission. Without it, rulesets are
        // all we see, and the trunk counts as strict: a batch would be rejected on a strict one.
        let res = self
            .request(
                Method::GET,
                &format!("/repos/{repo}/branches/{branch}/protection/required_status_checks"),
                None,
            )
            .await?;
        match res.status {
            s if s.is_success() => {
                rules.strict |= res.body["strict"] == true;
                for c in res.body["contexts"].as_array().into_iter().flatten() {
                    if let Some(c) = c.as_str() {
                        rules.checks.push(c.to_owned());
                    }
                }
            }
            // The branch has no classic protection, or none for status checks.
            StatusCode::NOT_FOUND => {}
            StatusCode::FORBIDDEN => rules.strict = true,
            s => bail!("required status checks of {branch}: {s} {}", res.body),
        }
        rules.checks.sort();
        rules.checks.dedup();
        Ok(rules)
    }

    pub async fn checks(&self, repo: &RepoName, sha: &str) -> Result<Checks> {
        let mut checks = Checks::default();

        let (runs, workflow_runs) = self.check_runs(repo, sha).await?;
        checks.workflows_running = workflow_runs.as_ref().map(|w| w.iter().any(unfinished));
        for run in &runs {
            let workflow_run = workflow_runs.iter().flatten().find(|w| {
                !w["check_suite_id"].is_null() && w["check_suite_id"] == run["check_suite"]["id"]
            });
            let state = match (run["status"].as_str(), run["conclusion"].as_str()) {
                (Some("completed"), Some("success" | "neutral" | "skipped")) => CheckState::Passed,
                // A failed job of a workflow run that still runs may yet run again, and a job
                // run again shows its first attempt's failure until it starts.
                (Some("completed"), _) if workflow_run.is_some_and(unfinished) => {
                    CheckState::Pending
                }
                (Some("completed"), _) => CheckState::Failed,
                _ => CheckState::Pending,
            };
            if let Some(name) = run["name"].as_str() {
                checks.by_name.insert(name.to_owned(), state);
                let first = workflow_run.filter(|w| w["run_attempt"] == 1);
                if let (CheckState::Failed, Some(id)) =
                    (state, first.and_then(|w| w["id"].as_u64()))
                {
                    checks.reruns.insert(name.to_owned(), id);
                }
                if matches!(run["conclusion"].as_str(), Some("neutral" | "skipped")) {
                    checks.skipped.insert(name.to_owned());
                }
            }
        }

        let statuses = self
            .all_pages(&format!("/repos/{repo}/commits/{sha}/status?"), "statuses")
            .await?;
        for s in &statuses {
            let state = match s["state"].as_str() {
                Some("success") => CheckState::Passed,
                Some("failure" | "error") => CheckState::Failed,
                _ => CheckState::Pending,
            };
            if let Some(name) = s["context"].as_str() {
                checks.by_name.insert(name.to_owned(), state);
            }
        }

        Ok(checks)
    }
}

impl Installation {
    /// The check runs on `sha`, without those of a workflow run that a later run of the same
    /// workflow replaced: a push that triggers a workflow twice cancels the first run, and its
    /// gate jobs still fail. And the workflow runs on `sha`, read only when a GitHub Actions
    /// check run is on it.
    async fn check_runs(
        &self,
        repo: &RepoName,
        sha: &str,
    ) -> Result<(Vec<Value>, Option<Vec<Value>>)> {
        let runs = self
            .all_pages(
                &format!("/repos/{repo}/commits/{sha}/check-runs?filter=latest"),
                "check_runs",
            )
            .await?;
        if !runs.iter().any(|r| r["app"]["slug"] == "github-actions") {
            return Ok((runs, None));
        }
        let workflow_runs = self
            .all_pages(
                &format!("/repos/{repo}/actions/runs?head_sha={sha}"),
                "workflow_runs",
            )
            .await?;
        let replaced = replaced_suites(&workflow_runs);
        let runs = runs
            .into_iter()
            .filter(|r| {
                r["check_suite"]["id"]
                    .as_u64()
                    .is_none_or(|id| !replaced.contains(&id))
            })
            .collect();
        Ok((runs, Some(workflow_runs)))
    }

    /// Runs the failed jobs of the workflow run `run_id` again. GitHub needs the App's
    /// `actions: write` permission for it.
    pub async fn rerun_failed_jobs(&self, repo: &RepoName, run_id: u64) -> Result<()> {
        let path = format!("/repos/{repo}/actions/runs/{run_id}/rerun-failed-jobs");
        let res = self.request(Method::POST, &path, None).await?;
        match res.status {
            s if s.is_success() => Ok(()),
            StatusCode::FORBIDDEN => bail!(
                "GitHub refused to run the failed jobs again: the GitHub App needs the `actions: write` permission, which the account's owner accepts on GitHub"
            ),
            s => bail!("POST {path}: {s} {}", res.body),
        }
    }

    /// Whether a workflow run on `sha` has yet to finish: queued, waiting for an approval, or
    /// running. For a commit whose checks did not lead to read its workflow runs.
    pub async fn workflows_running(&self, repo: &RepoName, sha: &str) -> Result<bool> {
        let workflow_runs = self
            .all_pages(
                &format!("/repos/{repo}/actions/runs?head_sha={sha}"),
                "workflow_runs",
            )
            .await?;
        Ok(workflow_runs.iter().any(unfinished))
    }
}

/// Whether a workflow run has yet to finish: queued, waiting for an approval or its
/// environment, or running.
fn unfinished(workflow_run: &Value) -> bool {
    matches!(
        workflow_run["status"].as_str(),
        Some("queued" | "pending" | "waiting" | "requested" | "in_progress")
    )
}

/// The check suites of the workflow runs that a later run of the same workflow replaced.
fn replaced_suites(workflow_runs: &[Value]) -> Vec<u64> {
    let mut latest: BTreeMap<u64, u64> = BTreeMap::new();
    for w in workflow_runs {
        if let (Some(workflow), Some(id)) = (w["workflow_id"].as_u64(), w["id"].as_u64()) {
            let newest = latest.entry(workflow).or_insert(id);
            *newest = (*newest).max(id);
        }
    }
    workflow_runs
        .iter()
        .filter(|w| {
            w["workflow_id"]
                .as_u64()
                .and_then(|workflow| latest.get(&workflow))
                .is_some_and(|newest| w["id"].as_u64() != Some(*newest))
        })
        .filter_map(|w| w["check_suite_id"].as_u64())
        .collect()
}

/// The most a page of a list holds.
const PER_PAGE: usize = 100;

impl Installation {
    /// Every item of the list under `key`, page by page. `path` has its query started.
    async fn all_pages(&self, path: &str, key: &str) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        for page in 1.. {
            let sep = if path.ends_with('?') { "" } else { "&" };
            let res: Value = self
                .rest(
                    Method::GET,
                    &format!("{path}{sep}per_page={PER_PAGE}&page={page}"),
                    None,
                )
                .await?;
            let items = res[key].as_array().cloned().unwrap_or_default();
            let last = items.len() < PER_PAGE;
            all.extend(items);
            let total = res["total_count"].as_u64().unwrap_or(0) as usize;
            if last || all.len() >= total {
                break;
            }
        }
        Ok(all)
    }
}

/// How much of a failed job's log the fix agent gets: its end, where the error is.
const LOG_TAIL_BYTES: usize = 32 * 1024;
const ANNOTATIONS: usize = 50;
/// How many failed jobs beside the required ones the fix agent gets the logs of.
const UPSTREAM_JOBS: usize = 5;

impl Installation {
    /// What GitHub shows of why the checks `names` failed on `sha`.
    pub async fn check_failures(
        &self,
        repo: &RepoName,
        sha: &str,
        names: &[String],
    ) -> Result<Vec<CheckFailure>> {
        let mut found: BTreeMap<String, String> = BTreeMap::new();
        let (runs, _) = self.check_runs(repo, sha).await?;
        for run in &runs {
            let Some(name) = run["name"]
                .as_str()
                .filter(|n| names.iter().any(|m| m == n))
            else {
                continue;
            };
            found.insert(name.to_owned(), self.check_run_details(repo, run).await?);
        }
        // A required check is often a gate job that only reports that the jobs it needs failed.
        // Those jobs ran in the same workflow run, which is one check suite.
        let suites: Vec<&Value> = runs
            .iter()
            .filter(|r| {
                r["app"]["slug"] == "github-actions"
                    && failed(r)
                    && r["name"].as_str().is_some_and(|n| found.contains_key(n))
            })
            .map(|r| &r["check_suite"]["id"])
            .filter(|id| !id.is_null())
            .collect();
        let mut upstream = Vec::new();
        for run in runs
            .iter()
            .filter(|r| {
                failed(r)
                    && suites.contains(&&r["check_suite"]["id"])
                    && r["name"]
                        .as_str()
                        .is_some_and(|n| !names.iter().any(|m| m == n))
            })
            .take(UPSTREAM_JOBS)
        {
            upstream.push(CheckFailure {
                name: run["name"].as_str().unwrap_or_default().to_owned(),
                details: self.check_run_details(repo, run).await?,
            });
        }
        let statuses = self
            .all_pages(&format!("/repos/{repo}/commits/{sha}/status?"), "statuses")
            .await?;
        for s in &statuses {
            let Some(name) = s["context"]
                .as_str()
                .filter(|n| names.iter().any(|m| m == n))
            else {
                continue;
            };
            let mut details = format!("Commit status: {}", s["state"].as_str().unwrap_or("?"));
            for (label, field) in [("Description", "description"), ("Details", "target_url")] {
                if let Some(v) = s[field].as_str().filter(|v| !v.is_empty()) {
                    details.push_str(&format!("\n{label}: {v}"));
                }
            }
            found.entry(name.to_owned()).or_insert(details);
        }
        Ok(names
            .iter()
            .map(|name| CheckFailure {
                name: name.clone(),
                details: found
                    .remove(name)
                    .unwrap_or_else(|| "GitHub shows nothing about this check.".into()),
            })
            .chain(upstream)
            .collect())
    }

    /// A check run's conclusion, output and annotations, and for a GitHub Actions job, the
    /// end of its log.
    async fn check_run_details(&self, repo: &RepoName, run: &Value) -> Result<String> {
        let mut details = format!(
            "Check run: {}",
            run["conclusion"].as_str().unwrap_or("failed")
        );
        let output = &run["output"];
        for field in ["title", "summary", "text"] {
            if let Some(v) = output[field].as_str().filter(|v| !v.trim().is_empty()) {
                details.push_str(&format!("\n\n{}", v.trim()));
            }
        }
        let id = run["id"].as_u64().unwrap_or_default();
        if output["annotations_count"].as_u64().unwrap_or(0) > 0 {
            let annotations: Value = self
                .rest(
                    Method::GET,
                    &format!("/repos/{repo}/check-runs/{id}/annotations?per_page={ANNOTATIONS}"),
                    None,
                )
                .await?;
            let lines: Vec<String> = annotations
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| {
                    format!(
                        "- {}:{} [{}] {}",
                        a["path"].as_str().unwrap_or("?"),
                        a["start_line"].as_u64().unwrap_or(0),
                        a["annotation_level"].as_str().unwrap_or("?"),
                        a["message"].as_str().unwrap_or("").trim()
                    )
                })
                .collect();
            if !lines.is_empty() {
                details.push_str(&format!("\n\nAnnotations:\n{}", lines.join("\n")));
            }
        }
        // A GitHub Actions check run is a workflow job, with the same ID. Its log needs the
        // App's `actions: read` permission, which an App created before it lacks.
        if run["app"]["slug"] == "github-actions" {
            let res = self
                .request(
                    Method::GET,
                    &format!("/repos/{repo}/actions/jobs/{id}/logs"),
                    None,
                )
                .await?;
            match (res.status, res.body) {
                (s, Value::String(log)) if s.is_success() => {
                    details.push_str(&format!(
                        "\n\nThe end of the log:\n```\n{}\n```",
                        log_tail(&log)
                    ));
                }
                (StatusCode::FORBIDDEN, _) => details
                    .push_str("\n\n(No log: the GitHub App lacks the `actions: read` permission.)"),
                _ => {}
            }
        }
        if let Some(url) = run["details_url"].as_str() {
            details.push_str(&format!("\n\nDetails: {url}"));
        }
        Ok(details)
    }
}

fn failed(run: &Value) -> bool {
    matches!(run["conclusion"].as_str(), Some("failure" | "timed_out"))
}

/// The last `LOG_TAIL_BYTES` of a job log up to its last error, from a line start, without the
/// timestamp that GitHub Actions puts before each line. Post-job steps after the error can log
/// more than the whole tail.
fn log_tail(log: &str) -> String {
    let end = log.rfind("##[error]").map_or(log.len(), |i| {
        log[i..].find('\n').map_or(log.len(), |j| i + j)
    });
    let log = &log[..end];
    let mut start = log.len().saturating_sub(LOG_TAIL_BYTES);
    while !log.is_char_boundary(start) {
        start += 1;
    }
    let tail = &log[start..];
    let tail = match (start, tail.find('\n')) {
        (0, _) | (_, None) => tail,
        (_, Some(i)) => &tail[i + 1..],
    };
    tail.lines()
        .map(|l| match l.split_once(' ') {
            Some((stamp, rest))
                if stamp.len() >= 20 && stamp.ends_with('Z') && stamp.as_bytes()[4] == b'-' =>
            {
                rest
            }
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checks(list: &[(&str, CheckState)]) -> Checks {
        Checks {
            by_name: list.iter().map(|(n, s)| ((*n).to_owned(), *s)).collect(),
            skipped: Default::default(),
            workflows_running: None,
            reruns: Default::default(),
        }
    }

    #[test]
    fn missing_required_check_is_pending() {
        let c = checks(&[("lint", CheckState::Passed)]);
        assert_eq!(c.verdict(&["test".into()]).0, CheckState::Pending);
    }

    #[test]
    fn a_required_check_is_unreported_only_once_every_reported_check_is_done() {
        let required = ["test".into(), "sift".into()];
        let running = checks(&[("test", CheckState::Passed), ("build", CheckState::Pending)]);
        assert!(running.unreported(&required).is_empty());
        let done = checks(&[("test", CheckState::Passed), ("build", CheckState::Failed)]);
        assert_eq!(done.unreported(&required), ["sift"]);
        assert!(done.unreported(&[]).is_empty());
    }

    #[tokio::test]
    async fn a_workflow_run_queued_behind_its_concurrency_group_keeps_its_checks_unreported() {
        // `lint` finished; the workflow that runs `sift` waits in its concurrency group, so it
        // has no check run yet.
        let runs = r#"{"total_count":1,"check_runs":[
            {"name":"lint","status":"completed","conclusion":"success","check_suite":{"id":10},"app":{"slug":"github-actions"}}]}"#;
        let queued = r#"{"total_count":2,"workflow_runs":[
            {"id":1,"workflow_id":7,"check_suite_id":10,"status":"completed"},
            {"id":2,"workflow_id":8,"check_suite_id":20,"status":"queued"}]}"#;
        let done = r#"{"total_count":2,"workflow_runs":[
            {"id":1,"workflow_id":7,"check_suite_id":10,"status":"completed"},
            {"id":2,"workflow_id":8,"check_suite_id":20,"status":"completed"}]}"#;
        let statuses = r#"{"total_count":0,"statuses":[]}"#;
        let (gh, _) = crate::app::fake::serve(vec![
            (200, "", runs),
            (200, "", queued),
            (200, "", statuses),
            (200, "", runs),
            (200, "", done),
            (200, "", statuses),
        ])
        .await;
        let repo = RepoName::parse("o/r").unwrap();
        let required = ["lint".into(), "sift".into()];
        let checks = gh.checks(&repo, "abc").await.unwrap();
        assert_eq!(checks.workflows_running, Some(true));
        assert!(checks.unreported(&required).is_empty());
        let checks = gh.checks(&repo, "abc").await.unwrap();
        assert_eq!(checks.workflows_running, Some(false));
        assert_eq!(checks.unreported(&required), ["sift"]);
    }

    #[tokio::test]
    async fn without_an_actions_check_run_the_workflow_runs_are_read_only_when_asked() {
        // `lint` is a commit status; the workflow that runs `sift` is queued.
        let runs = r#"{"total_count":0,"check_runs":[]}"#;
        let statuses = r#"{"total_count":1,"statuses":[{"context":"lint","state":"success"}]}"#;
        let queued = r#"{"total_count":1,"workflow_runs":[
            {"id":2,"workflow_id":8,"check_suite_id":20,"status":"queued"}]}"#;
        let (gh, requests) = crate::app::fake::serve(vec![
            (200, "", runs),
            (200, "", statuses),
            (200, "", queued),
        ])
        .await;
        let repo = RepoName::parse("o/r").unwrap();
        let checks = gh.checks(&repo, "abc").await.unwrap();
        assert_eq!(checks.workflows_running, None);
        assert_eq!(requests.lock().unwrap().len(), 2);
        assert!(gh.workflows_running(&repo, "abc").await.unwrap());
        assert_eq!(
            requests.lock().unwrap()[2],
            "GET /repos/o/r/actions/runs?head_sha=abc&per_page=100&page=1 HTTP/1.1"
        );
    }

    #[test]
    fn with_no_check_reported_at_all_none_is_unreported_yet() {
        assert!(checks(&[]).unreported(&["sift".into()]).is_empty());
    }

    #[test]
    fn only_required_checks_count() {
        let c = checks(&[
            ("test", CheckState::Passed),
            ("flaky-optional", CheckState::Failed),
        ]);
        assert_eq!(c.verdict(&["test".into()]).0, CheckState::Passed);
    }

    #[test]
    fn a_log_tail_starts_at_a_line_and_drops_the_timestamps() {
        let log = format!(
            "{}\n2026-10-29T10:00:00.1234567Z error[E0425]: cannot find value `x`\nplain line",
            "x".repeat(LOG_TAIL_BYTES)
        );
        assert_eq!(
            log_tail(&log),
            "error[E0425]: cannot find value `x`\nplain line"
        );
    }

    #[test]
    fn a_log_tail_ends_at_the_last_error() {
        let log = format!(
            "FAILED test_x\n2026-10-29T10:00:00.1234567Z ##[error]Process completed with exit code 1.\n{}",
            "post-job cleanup\n".repeat(LOG_TAIL_BYTES)
        );
        assert_eq!(
            log_tail(&log),
            "FAILED test_x\n##[error]Process completed with exit code 1."
        );
    }

    #[test]
    fn a_pending_verdict_names_the_checks_it_waits_for() {
        let c = checks(&[("lint", CheckState::Passed), ("test", CheckState::Pending)]);
        let required = ["lint".into(), "test".into(), "e2e".into()];
        assert_eq!(
            c.verdict(&required),
            (
                CheckState::Pending,
                vec!["test".to_owned(), "e2e".to_owned()]
            )
        );
    }

    /// A page of `n` items under `key`, named `<prefix><i>`, of `total` in all.
    fn page(
        key: &str,
        field: &str,
        prefix: &str,
        n: usize,
        total: usize,
        extra: &str,
    ) -> &'static str {
        let items: Vec<String> = (0..n)
            .map(|i| format!(r#"{{"{field}":"{prefix}{i}",{extra}}}"#))
            .collect();
        let body = format!(r#"{{"total_count":{total},"{key}":[{}]}}"#, items.join(","));
        Box::leak(body.into_boxed_str())
    }

    #[tokio::test]
    async fn checks_past_the_first_page_count() {
        let passed = r#""status":"completed","conclusion":"success""#;
        let (gh, requests) = crate::app::fake::serve(vec![
            (200, "", page("check_runs", "name", "run", 100, 101, passed)),
            (
                200,
                "",
                page("check_runs", "name", "late", 1, 101, r#""status":"queued""#),
            ),
            (
                200,
                "",
                page(
                    "statuses",
                    "context",
                    "ctx",
                    100,
                    101,
                    r#""state":"success""#,
                ),
            ),
            (
                200,
                "",
                page(
                    "statuses",
                    "context",
                    "slow",
                    1,
                    101,
                    r#""state":"pending""#,
                ),
            ),
        ])
        .await;
        let repo = RepoName::parse("o/r").unwrap();
        let checks = gh.checks(&repo, "abc").await.unwrap();
        assert_eq!(checks.by_name.len(), 202);
        assert_eq!(
            checks.verdict(&["late0".into(), "slow0".into(), "run99".into()]),
            (
                CheckState::Pending,
                vec!["late0".to_owned(), "slow0".to_owned()]
            )
        );
        assert_eq!(
            *requests.lock().unwrap(),
            [
                "GET /repos/o/r/commits/abc/check-runs?filter=latest&per_page=100&page=1 HTTP/1.1",
                "GET /repos/o/r/commits/abc/check-runs?filter=latest&per_page=100&page=2 HTTP/1.1",
                "GET /repos/o/r/commits/abc/status?per_page=100&page=1 HTTP/1.1",
                "GET /repos/o/r/commits/abc/status?per_page=100&page=2 HTTP/1.1",
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_gate_job_brings_the_logs_of_the_failed_jobs_it_gates() {
        let run = |id: u64, name: &str, conclusion: &str, suite: u64| {
            format!(
                r#"{{"id":{id},"name":"{name}","conclusion":"{conclusion}","check_suite":{{"id":{suite}}},"app":{{"slug":"github-actions"}},"output":{{}}}}"#
            )
        };
        let runs = format!(
            r#"{{"total_count":4,"check_runs":[{},{},{},{}]}}"#,
            run(1, "Integration Tests", "failure", 7),
            run(2, "Integration Tests / shard-1", "success", 7),
            run(3, "Integration Tests / shard-2", "failure", 7),
            run(4, "Lint", "failure", 8),
        );
        let (gh, requests) = crate::app::fake::serve(vec![
            (200, "", Box::leak(runs.into_boxed_str())),
            (200, "", r#"{"total_count":0,"workflow_runs":[]}"#),
            (200, "", "Integration suite failed"),
            (200, "", "FAIL auth.test.ts > reads the user once"),
            (200, "", r#"{"total_count":0,"statuses":[]}"#),
        ])
        .await;
        let repo = RepoName::parse("o/r").unwrap();
        let failures = gh
            .check_failures(&repo, "abc", &["Integration Tests".into()])
            .await
            .unwrap();
        let names: Vec<&str> = failures.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Integration Tests", "Integration Tests / shard-2"]);
        assert!(failures[1].details.contains("FAIL auth.test.ts"));
        assert_eq!(
            requests.lock().unwrap()[2..4],
            [
                "GET /repos/o/r/actions/jobs/1/logs HTTP/1.1",
                "GET /repos/o/r/actions/jobs/3/logs HTTP/1.1",
            ]
        );
    }

    #[tokio::test]
    async fn a_cancelled_run_that_a_later_run_replaced_does_not_count() {
        // Two pushes in a second ran the workflow twice. The first run was cancelled, and its
        // gate failed; the second still runs, and has no gate yet.
        let runs = r#"{"total_count":3,"check_runs":[
            {"name":"Integration Tests / shard-1","status":"in_progress","check_suite":{"id":20},"app":{"slug":"github-actions"}},
            {"name":"Integration Tests","status":"completed","conclusion":"failure","check_suite":{"id":10},"app":{"slug":"github-actions"}},
            {"name":"Lint","status":"completed","conclusion":"failure","check_suite":{"id":30},"app":{"slug":"github-actions"}}]}"#;
        let workflow_runs = r#"{"total_count":3,"workflow_runs":[
            {"id":2,"workflow_id":7,"check_suite_id":20},
            {"id":1,"workflow_id":7,"check_suite_id":10},
            {"id":3,"workflow_id":8,"check_suite_id":30}]}"#;
        let (gh, _) = crate::app::fake::serve(vec![
            (200, "", runs),
            (200, "", workflow_runs),
            (200, "", r#"{"total_count":0,"statuses":[]}"#),
        ])
        .await;
        let repo = RepoName::parse("o/r").unwrap();
        let checks = gh.checks(&repo, "abc").await.unwrap();
        assert_eq!(
            checks.verdict(&["Integration Tests".into()]),
            (CheckState::Pending, vec!["Integration Tests".to_owned()])
        );
        assert_eq!(
            checks.verdict(&["Lint".into()]),
            (CheckState::Failed, vec!["Lint".to_owned()])
        );
    }

    #[tokio::test]
    async fn a_job_failed_on_its_first_attempt_may_run_again_once() {
        // `test` failed on the first attempt of run 1, and `lint` on the second of run 2. Run 3
        // runs `e2e` again: its first attempt's failure is not its verdict.
        let runs = r#"{"total_count":4,"check_runs":[
            {"name":"test","status":"completed","conclusion":"failure","check_suite":{"id":10},"app":{"slug":"github-actions"}},
            {"name":"lint","status":"completed","conclusion":"failure","check_suite":{"id":20},"app":{"slug":"github-actions"}},
            {"name":"e2e","status":"completed","conclusion":"failure","check_suite":{"id":30},"app":{"slug":"github-actions"}},
            {"name":"build","status":"completed","conclusion":"success","check_suite":{"id":30},"app":{"slug":"github-actions"}}]}"#;
        let workflow_runs = r#"{"total_count":3,"workflow_runs":[
            {"id":1,"workflow_id":7,"check_suite_id":10,"run_attempt":1,"status":"completed"},
            {"id":2,"workflow_id":8,"check_suite_id":20,"run_attempt":2,"status":"completed"},
            {"id":3,"workflow_id":9,"check_suite_id":30,"run_attempt":2,"status":"queued"}]}"#;
        let (gh, requests) = crate::app::fake::serve(vec![
            (200, "", runs),
            (200, "", workflow_runs),
            (200, "", r#"{"total_count":0,"statuses":[]}"#),
            (201, "", "{}"),
            (
                403,
                "",
                r#"{"message":"Resource not accessible by integration"}"#,
            ),
        ])
        .await;
        let repo = RepoName::parse("o/r").unwrap();
        let checks = gh.checks(&repo, "abc").await.unwrap();
        assert_eq!(checks.reruns, BTreeMap::from([("test".to_owned(), 1)]));
        assert_eq!(checks.by_name["lint"], CheckState::Failed);
        assert_eq!(checks.by_name["e2e"], CheckState::Pending);
        assert_eq!(checks.by_name["build"], CheckState::Passed);

        gh.rerun_failed_jobs(&repo, 1).await.unwrap();
        assert_eq!(
            requests.lock().unwrap()[3],
            "POST /repos/o/r/actions/runs/1/rerun-failed-jobs HTTP/1.1"
        );
        let refused = gh.rerun_failed_jobs(&repo, 1).await.unwrap_err();
        assert!(
            refused.to_string().contains("`actions: write`"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn rules_out_of_reach_on_github_free_count_as_none() {
        let upgrade = r#"{"message":"Upgrade to GitHub Pro or make this repository public to enable this feature."}"#;
        let (gh, _) = crate::app::fake::serve(vec![(403, "", upgrade), (403, "", upgrade)]).await;
        let repo = RepoName::parse("o/r").unwrap();
        let rules = gh.branch_rules(&repo, "main").await.unwrap();
        assert_eq!(rules.checks, Vec::<String>::new());
        assert_eq!(rules.merge_method("merge"), "merge");
    }

    #[tokio::test]
    async fn a_ruleset_narrows_the_merge_methods() {
        let rules = r#"[
            {"type":"pull_request","parameters":{"allowed_merge_methods":["squash","rebase"]}},
            {"type":"required_linear_history"},
            {"type":"pull_request","parameters":{"allowed_merge_methods":["merge","rebase"]}},
            {"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci"}]}}]"#;
        let (gh, _) = crate::app::fake::serve(vec![(200, "", rules), (404, "", "{}")]).await;
        let repo = RepoName::parse("o/r").unwrap();
        let rules = gh.branch_rules(&repo, "main").await.unwrap();
        assert_eq!(rules.checks, ["ci"]);
        assert_eq!(rules.merge_methods, ["rebase"]);
        assert_eq!(rules.merge_method("merge"), "rebase");
    }

    #[test]
    fn the_default_merge_method_stays_while_allowed() {
        let squash_only = BranchRules {
            merge_methods: vec!["squash".into()],
            ..Default::default()
        };
        assert_eq!(squash_only.merge_method("merge"), "squash");
        assert_eq!(squash_only.merge_method("squash"), "squash");
        let no_merge = BranchRules {
            merge_methods: vec!["squash".into(), "rebase".into()],
            ..Default::default()
        };
        assert_eq!(no_merge.merge_method("rebase"), "rebase");
        assert_eq!(no_merge.merge_method("merge"), "squash");
    }

    #[test]
    fn without_required_checks_all_count() {
        let c = checks(&[("test", CheckState::Passed), ("lint", CheckState::Failed)]);
        assert_eq!(
            c.verdict(&[]),
            (CheckState::Failed, vec!["lint".to_owned()])
        );
    }
}
