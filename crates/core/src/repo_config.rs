//! `.mergepatrol/mergepatrol.yml`: a repo's queue settings, committed on its default branch so a
//! PR cannot change them for itself. A setting the file leaves out takes the instance default.

use serde::{Deserialize, Serialize};

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
    /// Run after an agent resolution, before the publish.
    pub verify: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolverSettings {
    pub model: String,
    pub effort: String,
}

pub struct Labels {
    pub trigger: String,
    pub priority: String,
    pub failed: String,
}

/// A label as Mergepatrol creates it on a repo that lacks it.
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
                description: "Queue this PR: Mergepatrol rebases, tests and merges it",
            },
            LabelSpec {
                name: &self.priority,
                color: "d93f0b",
                description: "Jump the Mergepatrol queue",
            },
            LabelSpec {
                name: &self.failed,
                color: "b60205",
                description: "Mergepatrol ejected this PR; see its comment",
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
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolverConfig {
    model: Option<String>,
    effort: Option<String>,
}

/// The file as Mergepatrol writes it: every setting spelled out.
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
        let settings = Self {
            label: config.label.unwrap_or_else(|| defaults.label.clone()),
            batch_size: config.batch_size.unwrap_or(defaults.batch_size),
            resolver: ResolverSettings {
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
        Ok(())
    }

    /// The file that sets exactly these settings.
    pub fn to_yaml(&self) -> String {
        let yaml = serde_yaml::to_string(&Written {
            version: 1,
            settings: self,
        })
        .expect("settings serialize");
        format!(
            "# Mergepatrol reads this file from the default branch. The dashboard opens a PR to change it.\n{yaml}"
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
                model: "claude-opus-5-5".into(),
                effort: "medium".into(),
            },
            verify: vec![],
        }
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
    }

    #[test]
    fn a_written_file_reads_back_the_same() {
        let mut s = defaults();
        s.label = "queue".into();
        s.verify = vec!["cargo check".into(), "pnpm test -- --run".into()];
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
            "version: 1\nlabel: ''\n",
            "{",
        ] {
            assert!(
                RepoSettings::from_file(Some(yaml), &defaults()).is_err(),
                "{yaml}"
            );
        }
    }
}
