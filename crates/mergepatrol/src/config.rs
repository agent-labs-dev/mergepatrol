use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mergepatrol_core::RepoName;
use serde::Deserialize;

/// Settings from the environment. The dashboard and the App manifest flow replace most of these later.
pub struct Config {
    pub app_id: u64,
    pub private_key: Vec<u8>,
    /// Encrypts the secrets entered in the dashboard. 32 bytes, base64.
    pub secret_key: Option<String>,
    pub repos: Vec<RepoName>,
    pub labels: Labels,
    pub data_dir: PathBuf,
    pub poll: Duration,
    pub bind: SocketAddr,
    pub model: String,
    pub effort: String,
    pub max_jobs: usize,
    pub max_resolutions: usize,
}

pub struct Labels {
    pub trigger: String,
    pub priority: String,
    pub failed: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let key_path = req("MERGEPATROL_GITHUB_PRIVATE_KEY_PATH")?;
        let repos = req("MERGEPATROL_REPOS")?
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                RepoName::parse(s)
                    .with_context(|| format!("MERGEPATROL_REPOS: {s:?} is not owner/name"))
            })
            .collect::<Result<Vec<_>>>()?;
        if repos.is_empty() {
            bail!("MERGEPATROL_REPOS is empty");
        }
        let trigger = opt("MERGEPATROL_LABEL", "mergepatrol");
        Ok(Self {
            app_id: req("MERGEPATROL_GITHUB_APP_ID")?
                .parse()
                .context("MERGEPATROL_GITHUB_APP_ID")?,
            private_key: std::fs::read(&key_path).with_context(|| format!("read {key_path}"))?,
            secret_key: std::env::var("MERGEPATROL_SECRET_KEY").ok(),
            repos,
            labels: Labels {
                priority: format!("{trigger}:priority"),
                failed: format!("{trigger}:failed"),
                trigger,
            },
            data_dir: opt("MERGEPATROL_DATA_DIR", "./data").into(),
            poll: Duration::from_secs(opt("MERGEPATROL_POLL_SECS", "60").parse()?),
            bind: opt("MERGEPATROL_BIND", "127.0.0.1:8080").parse()?,
            model: opt("MERGEPATROL_MODEL", "claude-opus-5-5"),
            effort: opt("MERGEPATROL_EFFORT", "medium"),
            max_jobs: opt("MERGEPATROL_MAX_JOBS", "4").parse()?,
            max_resolutions: opt("MERGEPATROL_MAX_RESOLUTIONS", "2").parse()?,
        })
    }
}

fn req(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} is not set"))
}

fn opt(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

/// `.mergepatrol/mergepatrol.yml` in the repo, read from the trunk so a PR cannot change it for itself.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    #[allow(dead_code)]
    pub version: u32,
    #[serde(default)]
    pub verify: Vec<String>,
}

pub const REPO_CONFIG_PATH: &str = ".mergepatrol/mergepatrol.yml";

impl RepoConfig {
    pub fn parse(yaml: &str) -> Result<Self> {
        let config: Self = serde_yaml::from_str(yaml).context(REPO_CONFIG_PATH)?;
        if config.version != 1 {
            bail!(
                "{REPO_CONFIG_PATH}: version {} is not supported",
                config.version
            );
        }
        Ok(config)
    }
}
