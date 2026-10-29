use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mergepatrol_core::{RepoSettings, ResolverSettings};

/// Settings from the environment. Everything else is set up in the dashboard, or in each repo's
/// config file.
pub struct Config {
    /// Encrypts the secrets stored in the database: the GitHub App and the Anthropic API key.
    pub secret_key: String,
    pub database_url: String,
    /// The hosted GitHub App. Without it, the App is created in the dashboard (self-hosting).
    pub hosted_app: Option<HostedApp>,
    /// Where users reach the dashboard. GitHub redirects back here during setup and sign-in.
    pub public_url: String,
    /// The settings of a repo whose config file leaves them out.
    pub repo_defaults: RepoSettings,
    pub data_dir: PathBuf,
    pub poll: Duration,
    pub bind: SocketAddr,
    pub max_jobs: usize,
    pub executor: ExecutorConfig,
    /// The PostHog project key. Product analytics are off without it, as when self-hosted.
    pub posthog_key: Option<String>,
    pub posthog_host: String,
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
        resolv_conf: Option<String>,
        memory: String,
        cpus: String,
    },
    /// An execution of a Cloud Run Job per job, which reaches back to `public_url`.
    CloudRun {
        /// `projects/<p>/locations/<r>/jobs/<name>`.
        job: String,
    },
}

pub struct HostedApp {
    pub id: u64,
    pub private_key: String,
    pub client_id: String,
    pub client_secret: String,
    /// Off without it: the queue only polls.
    pub webhook_secret: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let secret_key = req("MERGEPATROL_SECRET_KEY")
            .context("generate one with `openssl rand -base64 32`, and keep it: it decrypts the stored secrets")?;
        let repo_defaults = RepoSettings {
            label: opt("MERGEPATROL_LABEL", "mergepatrol"),
            batch_size: opt("MERGEPATROL_BATCH_SIZE", "8").parse()?,
            resolver: ResolverSettings {
                model: opt("MERGEPATROL_MODEL", "claude-opus-5-5"),
                effort: opt("MERGEPATROL_EFFORT", "medium"),
            },
            verify: Vec::new(),
        };
        repo_defaults.check().map_err(|e| {
            anyhow::anyhow!("MERGEPATROL_LABEL, _BATCH_SIZE, _MODEL or _EFFORT: {e}")
        })?;
        Ok(Self {
            secret_key,
            database_url: req("MERGEPATROL_DATABASE_URL")?,
            hosted_app: hosted_app()?,
            public_url: opt("MERGEPATROL_PUBLIC_URL", "http://localhost:8080"),
            repo_defaults,
            data_dir: opt("MERGEPATROL_DATA_DIR", "./data").into(),
            poll: Duration::from_secs(opt("MERGEPATROL_POLL_SECS", "60").parse()?),
            bind: opt("MERGEPATROL_BIND", "127.0.0.1:8080").parse()?,
            max_jobs: opt("MERGEPATROL_MAX_JOBS", "4").parse()?,
            executor: match opt("MERGEPATROL_EXECUTOR", "local").as_str() {
                "local" => ExecutorConfig::Local,
                "docker" => ExecutorConfig::Docker {
                    image: opt("MERGEPATROL_JOB_IMAGE", "mergepatrol-job:latest"),
                    runtime: var("MERGEPATROL_JOB_RUNTIME"),
                    network: var("MERGEPATROL_JOB_NETWORK"),
                    resolv_conf: var("MERGEPATROL_JOB_RESOLV_CONF"),
                    memory: opt("MERGEPATROL_JOB_MEMORY", "4g"),
                    cpus: opt("MERGEPATROL_JOB_CPUS", "2"),
                },
                "cloud-run" => ExecutorConfig::CloudRun {
                    job: req("MERGEPATROL_CLOUD_RUN_JOB")?,
                },
                other => bail!("MERGEPATROL_EXECUTOR: {other:?} is not local, docker or cloud-run"),
            },
            posthog_key: var("MERGEPATROL_POSTHOG_KEY"),
            posthog_host: opt("MERGEPATROL_POSTHOG_HOST", "https://us.i.posthog.com"),
        })
    }
}

/// All four variables, or none.
fn hosted_app() -> Result<Option<HostedApp>> {
    let names = [
        "MERGEPATROL_GITHUB_APP_ID",
        "MERGEPATROL_GITHUB_APP_PRIVATE_KEY",
        "MERGEPATROL_GITHUB_CLIENT_ID",
        "MERGEPATROL_GITHUB_CLIENT_SECRET",
    ];
    let values: Vec<Option<String>> = names.iter().map(|n| var(n)).collect();
    match values.as_slice() {
        [None, None, None, None] => Ok(None),
        [Some(id), Some(key), Some(client_id), Some(secret)] => Ok(Some(HostedApp {
            id: id.parse().context("MERGEPATROL_GITHUB_APP_ID")?,
            private_key: key.clone(),
            client_id: client_id.clone(),
            client_secret: secret.clone(),
            webhook_secret: var("MERGEPATROL_GITHUB_WEBHOOK_SECRET"),
        })),
        _ => bail!(
            "set all of {} for the hosted GitHub App, or none",
            names.join(", ")
        ),
    }
}

/// Empty is unset: compose's `${X:-}` passes an empty variable.
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn req(name: &str) -> Result<String> {
    var(name).with_context(|| format!("{name} is not set"))
}

fn opt(name: &str, default: &str) -> String {
    var(name).unwrap_or_else(|| default.to_owned())
}
