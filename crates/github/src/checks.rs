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
    /// The verdict for `required`. With no required checks, every check on the commit counts.
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
        let failed: Vec<String> = relevant
            .iter()
            .filter(|(_, s)| *s == CheckState::Failed)
            .map(|(n, _)| (*n).clone())
            .collect();
        if !failed.is_empty() {
            (CheckState::Failed, failed)
        } else if relevant.iter().any(|(_, s)| *s == CheckState::Pending) {
            (CheckState::Pending, vec![])
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

        let runs: Value = self
            .rest(
                Method::GET,
                &format!("/repos/{repo}/commits/{sha}/check-runs?per_page=100&filter=latest"),
                None,
            )
            .await?;
        for run in runs["check_runs"].as_array().into_iter().flatten() {
            let state = match (run["status"].as_str(), run["conclusion"].as_str()) {
                (Some("completed"), Some("success" | "neutral" | "skipped")) => CheckState::Passed,
                (Some("completed"), _) => CheckState::Failed,
                _ => CheckState::Pending,
            };
            if let Some(name) = run["name"].as_str() {
                checks.by_name.insert(name.to_owned(), state);
            }
        }

        let status: Value = self
            .rest(
                Method::GET,
                &format!("/repos/{repo}/commits/{sha}/status"),
                None,
            )
            .await?;
        for s in status["statuses"].as_array().into_iter().flatten() {
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

/// How much of a failed job's log the fix agent gets: its end, where the error is.
const LOG_TAIL_BYTES: usize = 32 * 1024;
const ANNOTATIONS: usize = 50;

impl Installation {
    /// What GitHub shows of why the checks `names` failed on `sha`.
    pub async fn check_failures(
        &self,
        repo: &RepoName,
        sha: &str,
        names: &[String],
    ) -> Result<Vec<CheckFailure>> {
        let mut found: BTreeMap<String, String> = BTreeMap::new();
        let runs: Value = self
            .rest(
                Method::GET,
                &format!("/repos/{repo}/commits/{sha}/check-runs?per_page=100&filter=latest"),
                None,
            )
            .await?;
        for run in runs["check_runs"].as_array().into_iter().flatten() {
            let Some(name) = run["name"]
                .as_str()
                .filter(|n| names.iter().any(|m| m == n))
            else {
                continue;
            };
            found.insert(name.to_owned(), self.check_run_details(repo, run).await?);
        }
        let status: Value = self
            .rest(
                Method::GET,
                &format!("/repos/{repo}/commits/{sha}/status"),
                None,
            )
            .await?;
        for s in status["statuses"].as_array().into_iter().flatten() {
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

/// The last `LOG_TAIL_BYTES` of a job log, from a line start, without the timestamp that
/// GitHub Actions puts before each line.
fn log_tail(log: &str) -> String {
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
    fn without_required_checks_all_count() {
        let c = checks(&[("test", CheckState::Passed), ("lint", CheckState::Failed)]);
        assert_eq!(
            c.verdict(&[]),
            (CheckState::Failed, vec!["lint".to_owned()])
        );
    }
}
