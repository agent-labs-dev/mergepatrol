//! `.mergepatrol/mergepatrol.yml`: a repo's queue settings, committed on its default branch so a
//! PR cannot change them for itself. A setting the file leaves out takes the instance default.

use serde::{Deserialize, Serialize};

use crate::{Provider, ReviewOverrides};

pub const REPO_CONFIG_PATH: &str = ".mergepatrol/mergepatrol.yml";

/// The resolver's `--effort` values.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// A repo's settings, complete: the file over the instance defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSettings {
    /// The trigger label. `<label>:priority` jumps the queue, and ejected PRs get `<label>:failed`.
    pub label: String,
    /// The most PRs tested together. 1 tests and merges one PR at a time.
    pub batch_size: usize,
    pub resolver: ResolverSettings,
    /// Run after an agent resolution or fix, before the publish.
    pub verify: Vec<String>,
    pub fix_ci: FixCiSettings,
    /// The branches with a queue besides the default branch, which always has one. A PR into
    /// any other branch is held.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,
    /// The agent's review, over the workspace's settings.
    #[serde(default, skip_serializing_if = "ReviewOverrides::is_empty")]
    pub review: ReviewOverrides,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolverSettings {
    /// Whose model the agent runs, with the workspace's key for it.
    #[serde(default)]
    pub provider: Provider,
    pub model: String,
    pub effort: String,
}

/// When a PR's required checks fail, the agent commits a fix and the checks run again, up to
/// `attempts` times, before the PR is ejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixCiSettings {
    pub enabled: bool,
    pub attempts: u32,
}

/// On, with two fixes: most failures a fix can mend take one, and a second covers a first that
/// went halfway.
impl Default for FixCiSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            attempts: 2,
        }
    }
}

impl FixCiSettings {
    /// The most fix attempts that "within reason" allows.
    pub const MAX_ATTEMPTS: u32 = 5;

    /// How many fixes a PR gets: none when off.
    pub fn allowed(&self) -> u32 {
        if self.enabled { self.attempts } else { 0 }
    }
}

pub struct Labels {
    pub trigger: String,
    pub priority: String,
    pub failed: String,
}

/// A label as MergeLoop creates it on a repo that lacks it.
pub struct LabelSpec<'a> {
    pub name: &'a str,
    pub color: &'static str,
    pub description: &'static str,
}

impl Labels {
    pub fn specs(&self) -> [LabelSpec<'_>; 3] {
        [
            LabelSpec {
                name: &self.trigger,
                color: "0e8a16",
                description: "Queue this PR: MergeLoop rebases, tests and merges it",
            },
            LabelSpec {
                name: &self.priority,
                color: "d93f0b",
                description: "Jump the MergeLoop queue",
            },
            LabelSpec {
                name: &self.failed,
                color: "b60205",
                description: "MergeLoop ejected this PR; see its comment",
            },
        ]
    }
}

/// The file as written: every setting optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoConfig {
    version: u32,
    label: Option<String>,
    batch_size: Option<usize>,
    #[serde(default)]
    resolver: ResolverConfig,
    verify: Option<Vec<String>>,
    #[serde(default)]
    fix_ci: FixCiConfig,
    branches: Option<Vec<String>>,
    review: Option<ReviewOverrides>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolverConfig {
    provider: Option<Provider>,
    model: Option<String>,
    effort: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixCiConfig {
    enabled: Option<bool>,
    attempts: Option<u32>,
}

/// The file as MergeLoop writes it: every setting spelled out.
#[derive(Serialize)]
struct Written<'a> {
    version: u32,
    #[serde(flatten)]
    settings: &'a RepoSettings,
}

impl RepoSettings {
    /// The settings of a repo whose default branch has `file`, or the defaults without one.
    pub fn from_file(file: Option<&str>, defaults: &RepoSettings) -> Result<Self, String> {
        let Some(yaml) = file else {
            return Ok(defaults.clone());
        };
        let config: RepoConfig =
            serde_yaml::from_str(yaml).map_err(|e| format!("{REPO_CONFIG_PATH}: {e}"))?;
        if config.version != 1 {
            return Err(format!(
                "{REPO_CONFIG_PATH}: version {} is not supported",
                config.version
            ));
        }
        // The default model is the default provider's: another provider needs its own.
        let provider = config
            .resolver
            .provider
            .unwrap_or(defaults.resolver.provider);
        if provider != defaults.resolver.provider && config.resolver.model.is_none() {
            return Err(format!(
                "{REPO_CONFIG_PATH}: resolver.model: set the {} model to use",
                provider.name()
            ));
        }
        let settings = Self {
            label: config.label.unwrap_or_else(|| defaults.label.clone()),
            batch_size: config.batch_size.unwrap_or(defaults.batch_size),
            resolver: ResolverSettings {
                provider,
                model: config
                    .resolver
                    .model
                    .unwrap_or_else(|| defaults.resolver.model.clone()),
                effort: config
                    .resolver
                    .effort
                    .unwrap_or_else(|| defaults.resolver.effort.clone()),
            },
            verify: config.verify.unwrap_or_else(|| defaults.verify.clone()),
            fix_ci: FixCiSettings {
                enabled: config.fix_ci.enabled.unwrap_or(defaults.fix_ci.enabled),
                attempts: config.fix_ci.attempts.unwrap_or(defaults.fix_ci.attempts),
            },
            branches: config.branches.unwrap_or_else(|| defaults.branches.clone()),
            review: config.review.unwrap_or_else(|| defaults.review.clone()),
        };
        settings
            .check()
            .map_err(|e| format!("{REPO_CONFIG_PATH}: {e}"))?;
        Ok(settings)
    }

    /// Why these settings cannot run a queue, if they cannot.
    pub fn check(&self) -> Result<(), String> {
        let label = self.label.trim();
        if label.is_empty() || label != self.label || label.contains(',') {
            return Err(
                "label must be a GitHub label: not empty, no commas, no outer spaces".into(),
            );
        }
        if self.batch_size == 0 {
            return Err("batch_size: 0 merges nothing; 1 is one PR at a time".into());
        }
        if !Provider::SUPPORTED.contains(&self.resolver.provider) {
            return Err(format!(
                "resolver.provider: {} is not supported yet",
                self.resolver.provider.as_str()
            ));
        }
        if self.resolver.model.trim().is_empty() {
            return Err("resolver.model is empty".into());
        }
        if !EFFORTS.contains(&self.resolver.effort.as_str()) {
            return Err(format!(
                "resolver.effort: {:?} is not one of {}",
                self.resolver.effort,
                EFFORTS.join(", ")
            ));
        }
        if self.verify.iter().any(|c| c.trim().is_empty()) {
            return Err("verify: a command is empty".into());
        }
        if !(1..=FixCiSettings::MAX_ATTEMPTS).contains(&self.fix_ci.attempts) {
            return Err(format!(
                "fix_ci.attempts: {} is not between 1 and {}; turn it off with enabled: false",
                self.fix_ci.attempts,
                FixCiSettings::MAX_ATTEMPTS
            ));
        }
        // Exact names: a pattern would read as one, and queue nothing.
        if let Some(b) = self
            .branches
            .iter()
            .find(|b| b.is_empty() || b.trim() != b.as_str() || b.contains(['*', '?', '[', ' ']))
        {
            return Err(format!("branches: {b:?} is not a branch name"));
        }
        Ok(())
    }

    /// Whether a PR into `branch` has a queue: the default branch, or a listed one.
    pub fn queues(&self, branch: &str, default_branch: &str) -> bool {
        branch == default_branch || self.branches.iter().any(|b| b == branch)
    }

    /// The file that sets exactly these settings.
    pub fn to_yaml(&self) -> String {
        let yaml = serde_yaml::to_string(&Written {
            version: 1,
            settings: self,
        })
        .expect("settings serialize");
        format!(
            "# MergeLoop reads this file from the default branch. The dashboard opens a PR to change it.\n{yaml}"
        )
    }

    pub fn labels(&self) -> Labels {
        Labels {
            trigger: self.label.clone(),
            priority: format!("{}:priority", self.label),
            failed: format!("{}:failed", self.label),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> RepoSettings {
        RepoSettings {
            label: "mergepatrol".into(),
            batch_size: 8,
            resolver: ResolverSettings {
                provider: Provider::Anthropic,
                model: "claude-opus-5-5".into(),
                effort: "medium".into(),
            },
            verify: vec![],
            fix_ci: FixCiSettings::default(),
            branches: vec![],
            review: ReviewOverrides::default(),
        }
    }

    #[test]
    fn the_default_branch_queues_and_the_listed_ones_do() {
        let s = RepoSettings::from_file(Some("version: 1\nbranches: [release]\n"), &defaults())
            .unwrap();
        assert!(s.queues("main", "main"));
        assert!(s.queues("release", "main"));
        assert!(!s.queues("feat/a", "main"));
        assert!(!defaults().queues("release", "main"));
    }

    #[test]
    fn another_provider_names_its_model() {
        let s = RepoSettings::from_file(
            Some(
                "version: 1
resolver: { provider: openai, model: gpt-6.1-sol }
",
            ),
            &defaults(),
        )
        .unwrap();
        assert_eq!(s.resolver.provider, Provider::OpenAi);
        assert_eq!(s.resolver.model, "gpt-6.1-sol");
        assert_eq!(s.resolver.effort, "medium");

        let err = RepoSettings::from_file(
            Some(
                "version: 1
resolver: { provider: openai }
",
            ),
            &defaults(),
        )
        .unwrap_err();
        assert!(err.contains("set the OpenAI model"), "{err}");
    }

    #[test]
    fn no_file_is_the_defaults() {
        assert_eq!(RepoSettings::from_file(None, &defaults()), Ok(defaults()));
    }

    #[test]
    fn the_file_overrides_what_it_sets() {
        let s = RepoSettings::from_file(
            Some("version: 1\nbatch_size: 2\nresolver:\n  effort: high\nverify: [cargo check]\n"),
            &defaults(),
        )
        .unwrap();
        assert_eq!(s.batch_size, 2);
        assert_eq!(s.resolver.effort, "high");
        assert_eq!(s.resolver.model, "claude-opus-5-5");
        assert_eq!(s.label, "mergepatrol");
        assert_eq!(s.verify, ["cargo check"]);
        assert_eq!(s.fix_ci, defaults().fix_ci);
    }

    #[test]
    fn fix_ci_is_on_unless_the_file_turns_it_off() {
        let s =
            RepoSettings::from_file(Some("version: 1\nfix_ci:\n  enabled: false\n"), &defaults())
                .unwrap();
        assert_eq!(s.fix_ci.allowed(), 0);
        assert_eq!(s.fix_ci.attempts, 2);
        let s = RepoSettings::from_file(Some("version: 1\nfix_ci: { attempts: 3 }\n"), &defaults())
            .unwrap();
        assert_eq!(s.fix_ci.allowed(), 3);
    }

    #[test]
    fn a_written_file_reads_back_the_same() {
        let mut s = defaults();
        s.label = "queue".into();
        s.verify = vec!["cargo check".into(), "pnpm test -- --run".into()];
        s.fix_ci.enabled = false;
        s.resolver.provider = Provider::OpenAi;
        s.resolver.model = "gpt-6.1-sol".into();
        s.branches = vec!["release".into(), "release/2026-10".into()];
        s.review = ReviewOverrides {
            enabled: Some(true),
            bars: [(crate::Kind::Security, crate::Bar::Always)].into(),
            require_resolved: Some(false),
        };
        assert_eq!(
            RepoSettings::from_file(Some(&s.to_yaml()), &defaults()),
            Ok(s)
        );
    }

    #[test]
    fn bad_files_are_errors() {
        for yaml in [
            "version: 2\n",
            "version: 1\nbatchsize: 2\n",
            "version: 1\nbatch_size: 0\n",
            "version: 1\nresolver: { effort: extreme }\n",
            "version: 1\nresolver: { provider: gemini, model: g }\n",
            "version: 1\nresolver: { provider: openrouter, model: qwen/qwen3-coder }\n",
            "version: 1\nlabel: ''\n",
            "version: 1\nfix_ci: { attempts: 0 }\n",
            "version: 1\nfix_ci: { attempts: 6 }\n",
            "version: 1\nfix_ci: { retries: 1 }\n",
            "version: 1\nbranches: ['release/*']\n",
            "version: 1\nbranches: ['']\n",
            "version: 1\nbranches: release\n",
            "version: 1\nreview: { bars: { other: 30 } }\n",
            "version: 1\nreview: { level: high }\n",
            "{",
        ] {
            assert!(
                RepoSettings::from_file(Some(yaml), &defaults()).is_err(),
                "{yaml}"
            );
        }
    }
}
