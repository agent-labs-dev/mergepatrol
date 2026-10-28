use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Settings from the environment. Everything else is set up in the dashboard.
pub struct Config {
    /// Encrypts the secrets stored in the database: the GitHub App and the Anthropic API key.
    pub secret_key: String,
    /// Where users reach the dashboard. GitHub redirects back here during setup and sign-in.
    pub public_url: String,
    pub labels: Labels,
    pub data_dir: PathBuf,
    pub poll: Duration,
    pub bind: SocketAddr,
    pub model: String,
    pub effort: String,
    pub max_jobs: usize,
    pub executor: ExecutorConfig,
}

/// Where rebase jobs run.
pub enum ExecutorConfig {
    /// A subprocess on this machine. No isolation: for development.
    Local,
    /// A container per job. Production uses the gVisor runtime (`runsc`).
    Docker {
        image: String,
        runtime: Option<String>,
        network: Option<String>,
        dns: Vec<String>,
        memory: String,
        cpus: String,
    },
}

pub struct Labels {
    pub trigger: String,
    pub priority: String,
    pub failed: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let secret_key = req("MERGEPATROL_SECRET_KEY")
            .context("generate one with `openssl rand -base64 32`, and keep it: it decrypts the stored secrets")?;
        let trigger = opt("MERGEPATROL_LABEL", "mergepatrol");
        Ok(Self {
            secret_key,
            public_url: opt("MERGEPATROL_PUBLIC_URL", "http://localhost:8080"),
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
            executor: match opt("MERGEPATROL_EXECUTOR", "local").as_str() {
                "local" => ExecutorConfig::Local,
                "docker" => ExecutorConfig::Docker {
                    image: opt("MERGEPATROL_JOB_IMAGE", "mergepatrol-job:latest"),
                    runtime: std::env::var("MERGEPATROL_JOB_RUNTIME").ok(),
                    network: std::env::var("MERGEPATROL_JOB_NETWORK").ok(),
                    dns: opt("MERGEPATROL_JOB_DNS", "")
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    memory: opt("MERGEPATROL_JOB_MEMORY", "4g"),
                    cpus: opt("MERGEPATROL_JOB_CPUS", "2"),
                },
                other => bail!("MERGEPATROL_EXECUTOR: {other:?} is not local or docker"),
            },
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
