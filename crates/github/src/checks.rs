use std::collections::BTreeMap;

use anyhow::{Result, bail};
use reqwest::{Method, StatusCode};
use serde_json::Value;

use mergepatrol_core::{CheckFailure, RepoName};

use crate::Installation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Pending,
    Passed,
    Failed,
}

/// The checks on one commit, by name. Check runs and commit statuses both count.
#[derive(Debug, Default)]
pub struct Checks {
    pub by_name: BTreeMap<String, CheckState>,
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
}

/// The status checks that a branch requires before a merge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequiredChecks {
    pub names: Vec<String>,
    /// The PR must also be up to date with the branch: GitHub merges only a PR that has every
    /// commit of the branch.
    pub strict: bool,
}

impl Installation {
    /// The status checks that the branch's rulesets and classic protection require.
    pub async fn required_checks(&self, repo: &RepoName, branch: &str) -> Result<RequiredChecks> {
        let mut names = Vec::new();
        let mut strict = false;
        let rules: Vec<Value> = self
            .rest(
                Method::GET,
                &format!("/repos/{repo}/rules/branches/{branch}"),
                None,
            )
            .await?;
        for rule in rules
            .iter()
            .filter(|r| r["type"] == "required_status_checks")
        {
            strict |= rule["parameters"]["strict_required_status_checks_policy"] == true;
            for check in rule["parameters"]["required_status_checks"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let Some(c) = check["context"].as_str() {
                    names.push(c.to_owned());
                }
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
                strict |= res.body["strict"] == true;
                for c in res.body["contexts"].as_array().into_iter().flatten() {
                    if let Some(c) = c.as_str() {
                        names.push(c.to_owned());
                    }
                }
            }
            // The branch has no classic protection, or none for status checks.
            StatusCode::NOT_FOUND => {}
            StatusCode::FORBIDDEN => strict = true,
            s => bail!("required status checks of {branch}: {s} {}", res.body),
        }
        names.sort();
        names.dedup();
        Ok(RequiredChecks { names, strict })
    }

    pub async fn checks(&self, repo: &RepoName, sha: &str) -> Result<Checks> {
        let mut checks = Checks::default();

        let runs = self
            .all_pages(
                &format!("/repos/{repo}/commits/{sha}/check-runs?filter=latest"),
                "check_runs",
            )
            .await?;
        for run in &runs {
            let state = match (run["status"].as_str(), run["conclusion"].as_str()) {
                (Some("completed"), Some("success" | "neutral" | "skipped")) => CheckState::Passed,
                (Some("completed"), _) => CheckState::Failed,
                _ => CheckState::Pending,
            };
            if let Some(name) = run["name"].as_str() {
                checks.by_name.insert(name.to_owned(), state);
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
        let runs = self
            .all_pages(
                &format!("/repos/{repo}/commits/{sha}/check-runs?filter=latest"),
                "check_runs",
            )
            .await?;
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
        }
    }

    #[test]
    fn missing_required_check_is_pending() {
        let c = checks(&[("lint", CheckState::Passed)]);
        assert_eq!(c.verdict(&["test".into()]).0, CheckState::Pending);
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
            requests.lock().unwrap()[1..3],
            [
                "GET /repos/o/r/actions/jobs/1/logs HTTP/1.1",
                "GET /repos/o/r/actions/jobs/3/logs HTTP/1.1",
            ]
        );
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
