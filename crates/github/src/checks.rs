use std::collections::BTreeMap;

use anyhow::Result;
use reqwest::Method;
use serde_json::Value;

use mergepatrol_core::RepoName;

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
        // Classic protection needs the "administration: read" permission. Without it, rulesets are all we see.
        let res = self
            .request(
                Method::GET,
                &format!("/repos/{repo}/branches/{branch}/protection/required_status_checks"),
                None,
            )
            .await?;
        if res.status.is_success() {
            strict |= res.body["strict"] == true;
            for c in res.body["contexts"].as_array().into_iter().flatten() {
                if let Some(c) = c.as_str() {
                    names.push(c.to_owned());
                }
            }
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
    fn without_required_checks_all_count() {
        let c = checks(&[("test", CheckState::Passed), ("lint", CheckState::Failed)]);
        assert_eq!(
            c.verdict(&[]),
            (CheckState::Failed, vec!["lint".to_owned()])
        );
    }
}
