//! `.mergeloop/mergeloop.yml`: a repo's queue settings, committed on its default branch so a
//! PR cannot change them for itself. A setting the file leaves out follows the workspace, so an
//! empty file is a repo that inherits everything.

use serde::{Deserialize, Serialize};

use crate::{Provider, ReviewOverrides, SpendTask};

pub const REPO_CONFIG_PATH: &str = ".mergeloop/mergeloop.yml";
/// The trigger label a repo has unless its file or its workspace names another.
pub const DEFAULT_LABEL: &str = "mergeloop";

/// Where a repo's config file is. Repos set up before MergeLoop was renamed from Mergepatrol keep
/// theirs at the old path, which MergeLoop reads while the new one is missing, and keep the old
/// default label with it, so their labeled PRs stay queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigPath {
    Current,
    Legacy,
}

impl ConfigPath {
    /// In the order MergeLoop looks: the first that is there is the repo's.
    pub const ALL: [Self; 2] = [Self::Current, Self::Legacy];

    pub fn path(self) -> &'static str {
        match self {
            Self::Current => REPO_CONFIG_PATH,
            Self::Legacy => ".mergepatrol/mergepatrol.yml",
        }
    }

    /// The path this is, of [`ConfigPath::path`]'s.
    pub fn of(path: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|at| at.path() == path)
    }

    /// What a file here inherits from `defaults`.
    pub fn inherit(self, defaults: &RepoSettings) -> RepoSettings {
        let mut d = defaults.clone();
        if self == Self::Legacy && d.label == DEFAULT_LABEL {
            d.label = "mergepatrol".into();
        }
        d
    }
}

/// A repo's config file as read from its default branch: where it is, and its text, `None` for
/// none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigText {
    pub at: ConfigPath,
    pub text: Option<String>,
}

impl ConfigText {
    /// No file, so the repo would get one at the current path.
    pub fn none() -> Self {
        Self {
            at: ConfigPath::Current,
            text: None,
        }
    }

    /// The repo's settings, as [`RepoSettings::from_file_at`] reads them.
    pub fn settings(&self, defaults: &RepoSettings) -> Result<RepoSettings, String> {
        RepoSettings::from_file_at(self.at, self.text.as_deref(), defaults)
    }
}

/// The agent's `--effort` values.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// A repo's settings, complete: the file over the workspace's defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSettings {
    /// The trigger label. `<label>:priority` jumps the queue, and ejected PRs get `<label>:failed`.
    pub label: String,
    /// The most PRs tested together. 1 tests and merges one PR at a time.
    pub batch_size: usize,
    pub agent: AgentSettings,
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
pub struct AgentSettings {
    /// Whose model the agent runs, with the workspace's key for it.
    #[serde(default)]
    pub provider: Provider,
    pub model: String,
    pub effort: String,
    /// A model for one task over the default: each task without one runs the default.
    #[serde(default, skip_serializing_if = "ModelOverrides::is_empty")]
    pub overrides: ModelOverrides,
}

/// A model for each task the agent runs, over the default. A task without one runs the default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOverrides {
    /// Resolving a rebase's conflicts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebase: Option<TaskModel>,
    /// Fixing failed checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<TaskModel>,
    /// Reviewing a PR, and answering questions about the review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<TaskModel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskModel {
    pub provider: Provider,
    pub model: String,
}

impl ModelOverrides {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub fn get(&self, task: SpendTask) -> Option<&TaskModel> {
        match task {
            SpendTask::Rebase => self.rebase.as_ref(),
            SpendTask::Fix => self.fix.as_ref(),
            SpendTask::Review => self.review.as_ref(),
        }
    }

    fn all(&self) -> [(&'static str, Option<&TaskModel>); 3] {
        [
            ("rebase", self.rebase.as_ref()),
            ("fix", self.fix.as_ref()),
            ("review", self.review.as_ref()),
        ]
    }
}

impl AgentSettings {
    /// The provider and model that run `task`: its own, else the default.
    pub fn model(&self, task: SpendTask) -> (Provider, &str) {
        match self.overrides.get(task) {
            Some(t) => (t.provider, &t.model),
            None => (self.provider, &self.model),
        }
    }
}

/// When a PR's required checks fail, the agent commits a fix and the checks run again, up to
/// `attempts` times, before the PR is ejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixCiSettings {
    pub enabled: bool,
    pub attempts: u32,
}

/// On, with three fixes of a PR's checks: most need one, and the
/// rest cover a first that went halfway.
impl Default for FixCiSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            attempts: 3,
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

/// What a workspace sets for every repo that leaves it out of its config file. A repo's `verify`
/// and `branches` name its own commands and branches, so they have no workspace value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDefaults {
    pub label: String,
    pub batch_size: usize,
    pub agent: AgentSettings,
    pub fix_ci: FixCiSettings,
}

impl WorkspaceDefaults {
    /// The settings a repo inherits: these over the instance's.
    pub fn over(self, instance: &RepoSettings) -> RepoSettings {
        RepoSettings {
            label: self.label,
            batch_size: self.batch_size,
            agent: self.agent,
            fix_ci: self.fix_ci,
            ..instance.clone()
        }
    }
}

impl From<RepoSettings> for WorkspaceDefaults {
    fn from(s: RepoSettings) -> Self {
        Self {
            label: s.label,
            batch_size: s.batch_size,
            agent: s.agent,
            fix_ci: s.fix_ci,
        }
    }
}

/// The file as written: every setting optional, the empty file included.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoConfig {
    version: Option<u32>,
    label: Option<String>,
    batch_size: Option<usize>,
    /// `resolver` is its name from before a task could override its model.
    #[serde(default, alias = "resolver")]
    agent: AgentConfig,
    verify: Option<Vec<String>>,
    #[serde(default)]
    fix_ci: FixCiConfig,
    branches: Option<Vec<String>>,
    review: Option<ReviewOverrides>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentConfig {
    provider: Option<Provider>,
    model: Option<String>,
    effort: Option<String>,
    #[serde(default)]
    overrides: ModelOverridesConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelOverridesConfig {
    rebase: Option<TaskModelConfig>,
    fix: Option<TaskModelConfig>,
    review: Option<TaskModelConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskModelConfig {
    provider: Option<Provider>,
    model: Option<String>,
}

impl TaskModelConfig {
    /// The task's own model, if it differs from the default. The default model is its
    /// provider's: another provider needs its own.
    fn resolve(self, task: &str, provider: Provider) -> Result<Option<TaskModel>, String> {
        let task_provider = self.provider.unwrap_or(provider);
        match self.model {
            Some(model) => Ok(Some(TaskModel {
                provider: task_provider,
                model,
            })),
            None if task_provider == provider => Ok(None),
            None => Err(format!(
                "agent.overrides.{task}.model: set the {} model to use",
                task_provider.name()
            )),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixCiConfig {
    enabled: Option<bool>,
    attempts: Option<u32>,
}

/// How the header `to_yaml` writes ends, under every name MergeLoop has gone by.
const HEADER_END: &str = "The dashboard opens a PR to change it.";

/// The file as MergeLoop writes it: only what differs from the workspace, so the rest follows it.
#[derive(Serialize)]
struct Written<'a> {
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    batch_size: Option<usize>,
    #[serde(skip_serializing_if = "WrittenAgent::is_empty")]
    agent: WrittenAgent<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verify: Option<&'a [String]>,
    #[serde(skip_serializing_if = "WrittenFixCi::is_empty")]
    fix_ci: WrittenFixCi,
    #[serde(skip_serializing_if = "Option::is_none")]
    branches: Option<&'a [String]>,
    #[serde(skip_serializing_if = "ReviewOverrides::is_empty")]
    review: &'a ReviewOverrides,
}

#[derive(Serialize)]
struct WrittenAgent<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<Provider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<&'a str>,
    #[serde(skip_serializing_if = "WrittenOverrides::is_empty")]
    overrides: WrittenOverrides<'a>,
}

impl WrittenAgent<'_> {
    fn is_empty(&self) -> bool {
        self.provider.is_none()
            && self.model.is_none()
            && self.effort.is_none()
            && self.overrides.is_empty()
    }
}

#[derive(Serialize)]
struct WrittenOverrides<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    rebase: Option<WrittenTask<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<WrittenTask<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    review: Option<WrittenTask<'a>>,
}

impl WrittenOverrides<'_> {
    fn is_empty(&self) -> bool {
        self.rebase.is_none() && self.fix.is_none() && self.review.is_none()
    }
}

/// A task's model, or an empty mapping: the task runs the repo's default, not the workspace's
/// override, whatever provider that is by then.
#[derive(Serialize)]
struct WrittenTask<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<Provider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
}

impl<'a> WrittenTask<'a> {
    /// `None` when the task follows the workspace.
    fn over(own: &'a Option<TaskModel>, inherited: &Option<TaskModel>) -> Option<Self> {
        (own != inherited).then(|| Self {
            provider: own.as_ref().map(|t| t.provider),
            model: own.as_ref().map(|t| t.model.as_str()),
        })
    }
}

#[derive(Serialize)]
struct WrittenFixCi {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempts: Option<u32>,
}

impl WrittenFixCi {
    fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.attempts.is_none()
    }
}

impl RepoSettings {
    /// The settings of a repo whose default branch has `file`, or the defaults without one. An
    /// empty file is the defaults too.
    pub fn from_file(file: Option<&str>, defaults: &RepoSettings) -> Result<Self, String> {
        Self::from_file_at(ConfigPath::Current, file, defaults)
    }

    /// The settings of a repo whose file at `at` is `file`, over what a file there inherits.
    pub fn from_file_at(
        at: ConfigPath,
        file: Option<&str>,
        defaults: &RepoSettings,
    ) -> Result<Self, String> {
        let path = at.path();
        let defaults = &at.inherit(defaults);
        let Some(yaml) = file else {
            return Ok(defaults.clone());
        };
        let config: Option<RepoConfig> =
            serde_yaml::from_str(yaml).map_err(|e| format!("{path}: {e}"))?;
        let config = config.unwrap_or_default();
        if let Some(version) = config.version.filter(|v| *v != 1) {
            return Err(format!("{path}: version {version} is not supported"));
        }
        // The default model is the default provider's: another provider needs its own.
        let provider = config.agent.provider.unwrap_or(defaults.agent.provider);
        if provider != defaults.agent.provider && config.agent.model.is_none() {
            return Err(format!(
                "{path}: agent.model: set the {} model to use",
                provider.name()
            ));
        }
        let model = config
            .agent
            .model
            .unwrap_or_else(|| defaults.agent.model.clone());
        // A task the file leaves out follows the workspace. One it names is its own, and may say
        // the repo's default.
        let task = |name: &str, t: Option<TaskModelConfig>, inherited: &Option<TaskModel>| {
            t.map_or(Ok(inherited.clone()), |t| t.resolve(name, provider))
                .map_err(|e| format!("{path}: {e}"))
        };
        let overrides = config.agent.overrides;
        let inherited = &defaults.agent.overrides;
        let overrides = ModelOverrides {
            rebase: task("rebase", overrides.rebase, &inherited.rebase)?,
            fix: task("fix", overrides.fix, &inherited.fix)?,
            review: task("review", overrides.review, &inherited.review)?,
        };
        let settings = Self {
            label: config.label.unwrap_or_else(|| defaults.label.clone()),
            batch_size: config.batch_size.unwrap_or(defaults.batch_size),
            agent: AgentSettings {
                provider,
                model,
                effort: config
                    .agent
                    .effort
                    .unwrap_or_else(|| defaults.agent.effort.clone()),
                overrides,
            },
            verify: config.verify.unwrap_or_else(|| defaults.verify.clone()),
            fix_ci: FixCiSettings {
                enabled: config.fix_ci.enabled.unwrap_or(defaults.fix_ci.enabled),
                attempts: config.fix_ci.attempts.unwrap_or(defaults.fix_ci.attempts),
            },
            branches: config.branches.unwrap_or_else(|| defaults.branches.clone()),
            review: config.review.unwrap_or_else(|| defaults.review.clone()),
        };
        settings.check().map_err(|e| format!("{path}: {e}"))?;
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
        if !Provider::SUPPORTED.contains(&self.agent.provider) {
            return Err(format!(
                "agent.provider: {} is not supported yet",
                self.agent.provider.as_str()
            ));
        }
        if self.agent.model.trim().is_empty() {
            return Err("agent.model is empty".into());
        }
        for (task, t) in self.agent.overrides.all() {
            let Some(t) = t else { continue };
            if !Provider::SUPPORTED.contains(&t.provider) {
                return Err(format!(
                    "agent.overrides.{task}.provider: {} is not supported yet",
                    t.provider.as_str()
                ));
            }
            if t.model.trim().is_empty() {
                return Err(format!("agent.overrides.{task}.model is empty"));
            }
        }
        if !EFFORTS.contains(&self.agent.effort.as_str()) {
            return Err(format!(
                "agent.effort: {:?} is not one of {}",
                self.agent.effort,
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

    /// The file that reads back as these settings over `defaults`, and sets nothing else.
    pub fn to_yaml(&self, defaults: &RepoSettings) -> String {
        let (s, d) = (&self.agent, &defaults.agent);
        // A model belongs to its provider, so the two are written together: the workspace's
        // provider can change under a model the file names.
        let model = s.provider != d.provider || s.model != d.model;
        let task = WrittenTask::over;
        let yaml = serde_yaml::to_string(&Written {
            version: 1,
            label: (self.label != defaults.label).then_some(&self.label),
            batch_size: (self.batch_size != defaults.batch_size).then_some(self.batch_size),
            agent: WrittenAgent {
                provider: model.then_some(s.provider),
                model: model.then_some(&s.model),
                effort: (s.effort != d.effort).then_some(&s.effort),
                overrides: WrittenOverrides {
                    rebase: task(&s.overrides.rebase, &d.overrides.rebase),
                    fix: task(&s.overrides.fix, &d.overrides.fix),
                    review: task(&s.overrides.review, &d.overrides.review),
                },
            },
            verify: (self.verify != defaults.verify).then_some(&self.verify),
            fix_ci: WrittenFixCi {
                enabled: (self.fix_ci.enabled != defaults.fix_ci.enabled)
                    .then_some(self.fix_ci.enabled),
                attempts: (self.fix_ci.attempts != defaults.fix_ci.attempts)
                    .then_some(self.fix_ci.attempts),
            },
            branches: (self.branches != defaults.branches).then_some(&self.branches),
            review: &self.review,
        })
        .expect("settings serialize");
        format!("# MergeLoop reads this file from the default branch. {HEADER_END}\n{yaml}")
    }

    /// Whether `file` is these settings as `to_yaml` writes them, whatever its comments say.
    pub fn written_in(&self, file: &str, defaults: &RepoSettings) -> bool {
        let written = self.to_yaml(defaults);
        settings_lines(file).eq(settings_lines(&written))
    }

    /// Whether `to_yaml` would lose a comment of `file`, other than the header it writes itself.
    pub fn loses_comments_of(&self, file: &str, defaults: &RepoSettings) -> bool {
        let written = self.to_yaml(defaults);
        file.lines()
            .filter(|l| l.contains('#') && !l.ends_with(HEADER_END))
            .any(|l| !written.lines().any(|w| w == l))
    }

    pub fn labels(&self) -> Labels {
        Labels {
            trigger: self.label.clone(),
            priority: format!("{}:priority", self.label),
            failed: format!("{}:failed", self.label),
        }
    }
}

/// The lines of a file that set something: neither blank, a comment, nor the version, which only
/// an empty file leaves out.
fn settings_lines(file: &str) -> impl Iterator<Item = &str> {
    file.lines().filter(|l| {
        let l = l.trim();
        !l.is_empty() && !l.starts_with('#') && l != "version: 1"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> RepoSettings {
        RepoSettings {
            label: DEFAULT_LABEL.into(),
            batch_size: 8,
            agent: AgentSettings {
                provider: Provider::Anthropic,
                model: "claude-opus-5-5".into(),
                effort: "medium".into(),
                overrides: ModelOverrides::default(),
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
agent: { provider: openai, model: gpt-6.1-sol }
",
            ),
            &defaults(),
        )
        .unwrap();
        assert_eq!(s.agent.provider, Provider::OpenAi);
        assert_eq!(s.agent.model, "gpt-6.1-sol");
        assert_eq!(s.agent.effort, "medium");

        let err = RepoSettings::from_file(
            Some(
                "version: 1
agent: { provider: openai }
",
            ),
            &defaults(),
        )
        .unwrap_err();
        assert!(err.contains("set the OpenAI model"), "{err}");
    }

    #[test]
    fn each_task_runs_the_default_model_unless_it_overrides_it() {
        let s = RepoSettings::from_file(
            Some(
                "version: 1
agent:
  model: claude-sonnet-5-5
  overrides:
    rebase: { provider: openai, model: gpt-6.1-sol }
    review: { model: claude-fable-5-1 }
",
            ),
            &defaults(),
        )
        .unwrap();
        let r = &s.agent;
        assert_eq!(
            r.model(SpendTask::Rebase),
            (Provider::OpenAi, "gpt-6.1-sol")
        );
        assert_eq!(
            r.model(SpendTask::Fix),
            (Provider::Anthropic, "claude-sonnet-5-5")
        );
        assert_eq!(
            r.model(SpendTask::Review),
            (Provider::Anthropic, "claude-fable-5-1")
        );

        // A file from before overrides, under the old name, runs every task on its model.
        let s = RepoSettings::from_file(
            Some("version: 1\nresolver: { provider: openai, model: gpt-6.1-sol }\n"),
            &defaults(),
        )
        .unwrap();
        assert!(s.agent.overrides.is_empty());
        for task in [SpendTask::Rebase, SpendTask::Fix, SpendTask::Review] {
            assert_eq!(s.agent.model(task), (Provider::OpenAi, "gpt-6.1-sol"));
        }

        // The default provider, without a model, is no override.
        let s = RepoSettings::from_file(
            Some("version: 1\nagent: { overrides: { fix: { provider: anthropic } } }\n"),
            &defaults(),
        )
        .unwrap();
        assert!(s.agent.overrides.is_empty());

        let err = RepoSettings::from_file(
            Some("version: 1\nagent: { overrides: { fix: { provider: openai } } }\n"),
            &defaults(),
        )
        .unwrap_err();
        assert!(
            err.contains("agent.overrides.fix.model: set the OpenAI model"),
            "{err}"
        );
    }

    #[test]
    fn no_file_is_the_defaults() {
        assert_eq!(RepoSettings::from_file(None, &defaults()), Ok(defaults()));
    }

    #[test]
    fn the_file_overrides_what_it_sets() {
        let s = RepoSettings::from_file(
            Some("version: 1\nbatch_size: 2\nagent:\n  effort: high\nverify: [cargo check]\n"),
            &defaults(),
        )
        .unwrap();
        assert_eq!(s.batch_size, 2);
        assert_eq!(s.agent.effort, "high");
        assert_eq!(s.agent.model, "claude-opus-5-5");
        assert_eq!(s.label, DEFAULT_LABEL);
        assert_eq!(s.verify, ["cargo check"]);
        assert_eq!(s.fix_ci, defaults().fix_ci);
    }

    #[test]
    fn fix_ci_is_on_unless_the_file_turns_it_off() {
        let s =
            RepoSettings::from_file(Some("version: 1\nfix_ci:\n  enabled: false\n"), &defaults())
                .unwrap();
        assert_eq!(s.fix_ci.allowed(), 0);
        assert_eq!(s.fix_ci.attempts, 3);
        let s = RepoSettings::from_file(Some("version: 1\nfix_ci: { attempts: 2 }\n"), &defaults())
            .unwrap();
        assert_eq!(s.fix_ci.allowed(), 2);
    }

    #[test]
    fn a_written_file_reads_back_the_same() {
        let mut s = defaults();
        s.label = "queue".into();
        s.verify = vec!["cargo check".into(), "pnpm test -- --run".into()];
        s.fix_ci.enabled = false;
        s.agent.provider = Provider::OpenAi;
        s.agent.model = "gpt-6.1-sol".into();
        s.agent.overrides = ModelOverrides {
            rebase: None,
            fix: Some(TaskModel {
                provider: Provider::OpenAi,
                model: "gpt-6-luna".into(),
            }),
            review: Some(TaskModel {
                provider: Provider::Anthropic,
                model: "claude-fable-5-1".into(),
            }),
        };
        s.branches = vec!["release".into(), "release/2026-10".into()];
        s.review = ReviewOverrides {
            enabled: Some(true),
            bars: [(crate::Kind::Security, crate::Bar::Always)].into(),
            require_resolved: Some(false),
            depth: Some(crate::Depth::Deep),
        };
        assert_eq!(
            RepoSettings::from_file(Some(&s.to_yaml(&defaults())), &defaults()),
            Ok(s)
        );
    }

    #[test]
    fn an_empty_file_inherits_everything() {
        for yaml in [
            "",
            "\n",
            "# Follows the workspace.\n",
            "---\n",
            "version: 1\n",
        ] {
            assert_eq!(
                RepoSettings::from_file(Some(yaml), &defaults()),
                Ok(defaults()),
                "{yaml:?}"
            );
        }
        assert!(defaults().written_in("", &defaults()));
        assert!(defaults().written_in("# Why.\n", &defaults()));
    }

    /// Defaults a workspace chose: a label, a batch, a model and a model for reviews of its own.
    fn workspace() -> RepoSettings {
        WorkspaceDefaults {
            label: "queue".into(),
            batch_size: 3,
            agent: AgentSettings {
                provider: Provider::OpenAi,
                model: "gpt-6.1-sol".into(),
                effort: "high".into(),
                overrides: ModelOverrides {
                    review: Some(TaskModel {
                        provider: Provider::Anthropic,
                        model: "claude-fable-5-1".into(),
                    }),
                    ..Default::default()
                },
            },
            fix_ci: FixCiSettings {
                enabled: false,
                attempts: 4,
            },
        }
        .over(&defaults())
    }

    #[test]
    fn a_repo_follows_the_workspace_in_what_its_file_leaves_out() {
        let s = RepoSettings::from_file(Some("batch_size: 2\n"), &workspace()).unwrap();
        assert_eq!(s.batch_size, 2);
        assert_eq!(s.label, "queue");
        assert_eq!(s.agent, workspace().agent);
        assert_eq!(s.fix_ci.allowed(), 0);
    }

    #[test]
    fn a_setting_spelled_out_stays_put_when_the_workspace_moves() {
        let d = workspace();
        let file = format!("batch_size: {}\n", d.batch_size);
        let s = RepoSettings::from_file(Some(&file), &d).unwrap();
        assert!(!s.written_in(&file, &d), "equal, but not following");
        assert!(s.written_in(&s.to_yaml(&d), &d));

        let mut moved = d.clone();
        moved.batch_size += 1;
        let s = RepoSettings::from_file(Some(&file), &moved).unwrap();
        assert_eq!(s.batch_size, d.batch_size);
    }

    #[test]
    fn a_task_the_file_names_is_its_own_and_may_be_the_default() {
        for task in ["{}", "{ provider: openai }"] {
            let yaml = format!("agent: {{ overrides: {{ review: {task} }} }}\n");
            let s = RepoSettings::from_file(Some(&yaml), &workspace()).unwrap();
            assert!(s.agent.overrides.is_empty(), "{yaml}");
        }
        let s =
            RepoSettings::from_file(Some("agent: { overrides: { review: {} } }\n"), &workspace())
                .unwrap();
        assert_eq!(
            s.agent.model(SpendTask::Review),
            (Provider::OpenAi, "gpt-6.1-sol")
        );
    }

    #[test]
    fn a_file_says_only_what_differs_from_the_workspace() {
        let d = workspace();
        let yaml = d.to_yaml(&d);
        assert!(yaml.lines().skip(1).eq(["version: 1"]), "{yaml}");

        let mut s = workspace();
        s.batch_size = 9;
        s.fix_ci.enabled = true;
        s.agent.model = "gpt-6-luna".into();
        s.agent.overrides.review = None;
        s.agent.overrides.fix = Some(TaskModel {
            provider: Provider::Anthropic,
            model: "claude-haiku-5-5".into(),
        });
        let yaml = s.to_yaml(&d);
        assert_eq!(RepoSettings::from_file(Some(&yaml), &d), Ok(s), "{yaml}");
        for inherited in ["label", "effort", "attempts", "verify"] {
            assert!(!yaml.contains(inherited), "{yaml}");
        }

        // A repo that names a model keeps it, and its opt-out, when the workspace's provider
        // changes.
        let mut s = d.clone();
        s.agent.model = "gpt-6-luna".into();
        s.agent.overrides.review = None;
        let yaml = s.to_yaml(&d);
        let mut moved = d.clone();
        moved.agent.provider = Provider::Anthropic;
        moved.agent.model = "claude-opus-5-5".into();
        let read = RepoSettings::from_file(Some(&yaml), &moved).unwrap();
        assert_eq!(
            read.agent.model(SpendTask::Review),
            (Provider::OpenAi, "gpt-6-luna"),
            "{yaml}"
        );

        // An opt-out alone follows the workspace's provider wherever it goes.
        let mut s = d.clone();
        s.agent.overrides.review = None;
        let read = RepoSettings::from_file(Some(&s.to_yaml(&d)), &moved).unwrap();
        assert_eq!(
            read.agent.model(SpendTask::Review),
            (Provider::Anthropic, "claude-opus-5-5")
        );

        // Another provider than the workspace's says its model, even a model of the same name.
        let mut s = d.clone();
        s.agent.provider = Provider::Anthropic;
        assert_eq!(
            RepoSettings::from_file(Some(&s.to_yaml(&d)), &d),
            Ok(s.clone())
        );
    }

    #[test]
    fn a_file_from_before_the_rename_is_not_written_as_mergeloop_writes_it_now() {
        let old = "# Mergepatrol reads this file from the default branch. The dashboard opens a PR to change it.\nversion: 1\nlabel: mergepatrol\nbatch_size: 8\nresolver:\n  model: claude-opus-5-5\n  effort: medium\nverify: []\n";
        let at = ConfigPath::Legacy;
        let d = at.inherit(&defaults());
        let settings = RepoSettings::from_file_at(at, Some(old), &defaults()).unwrap();
        assert!(!settings.written_in(old, &d));
        // Rewriting it keeps what it means, and there is nothing left to do after.
        let new = settings.to_yaml(&d);
        assert_eq!(
            RepoSettings::from_file_at(at, Some(&new), &defaults()).unwrap(),
            settings
        );
        assert!(settings.written_in(&new, &d));
        assert!(!new.contains("label"), "{new}");
    }

    #[test]
    fn a_file_at_the_old_path_keeps_the_old_default_label() {
        let empty = Some("version: 1\n");
        let label = |at, d: &RepoSettings| RepoSettings::from_file_at(at, empty, d).unwrap().label;
        assert_eq!(label(ConfigPath::Current, &defaults()), "mergeloop");
        assert_eq!(label(ConfigPath::Legacy, &defaults()), "mergepatrol");
        // A label the workspace names is every repo's.
        let mut named = defaults();
        named.label = "ship".into();
        assert_eq!(label(ConfigPath::Legacy, &named), "ship");
        let err = RepoSettings::from_file_at(ConfigPath::Legacy, Some("version: 2\n"), &named);
        assert!(
            err.unwrap_err()
                .starts_with(".mergepatrol/mergepatrol.yml: ")
        );
    }

    #[test]
    fn comments_make_no_file_out_of_date() {
        let d = defaults();
        let mut settings = defaults();
        settings.verify = vec!["cargo check".into()];
        let written = settings.to_yaml(&d);
        let old_header = written.replacen("MergeLoop", "Mergepatrol", 1);
        assert!(settings.written_in(&old_header, &d));
        let commented = written.replace("verify:", "# Why these run.\n\nverify:");
        assert!(settings.written_in(&commented, &d));
    }

    #[test]
    fn only_a_comment_someone_wrote_counts_as_lost() {
        let d = defaults();
        let mut settings = defaults();
        settings.batch_size = 2;
        settings.verify = vec!["cargo check".into()];
        let written = settings.to_yaml(&d);
        assert!(!settings.loses_comments_of(&written, &d));
        assert!(!settings.loses_comments_of(&written.replacen("MergeLoop", "Mergepatrol", 1), &d));
        assert!(settings.loses_comments_of(&written.replace("verify:", "# Why.\nverify:"), &d));
        assert!(settings.loses_comments_of(
            &written.replace("batch_size: 2", "batch_size: 2 # Why."),
            &d
        ));
        // A `#` the rewrite keeps is no comment lost.
        let mut hash = defaults();
        hash.verify = vec!["echo '#'".into()];
        assert!(!hash.loses_comments_of(&hash.to_yaml(&d), &d));
    }

    #[test]
    fn bad_files_are_errors() {
        for yaml in [
            "version: 2\n",
            "- a list\n",
            "version: 1\nbatchsize: 2\n",
            "version: 1\nbatch_size: 0\n",
            "version: 1\nagent: { effort: extreme }\n",
            "version: 1\nagent: { provider: gemini, model: g }\n",
            "version: 1\nagent: { provider: openrouter, model: qwen/qwen3-coder }\n",
            "version: 1\nlabel: ''\n",
            "version: 1\nagent: { overrides: { resolve: { model: m } } }\n",
            "version: 1\nagent: { overrides: { fix: { provider: openrouter, model: m } } }\n",
            "version: 1\nagent: { overrides: { fix: { model: '' } } }\n",
            "version: 1\nagent: { overrides: { fix: { effort: high } } }\n",
            "version: 1\nresolver: { model: m }\nagent: { model: m }\n",
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
