use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mergepatrol_billing::StripeConfig;
use mergepatrol_core::{Provider, RepoSettings, ResolverSettings};

/// Settings from the environment. Everything else is set up in the dashboard, or in each repo's
/// config file.
pub struct Config {
    /// Encrypts the secrets stored in the database: the GitHub App and the Anthropic API key.
    /// The current key, then any it replaces.
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
    /// Where jobs reach the orchestrator: its model API proxy, and a remote job's link.
    /// `public_url` by default.
    pub job_proxy_url: String,
    /// A second listener that serves only what jobs use, for jobs that cannot reach `bind`: the
    /// Docker sandbox network reaches the host on one port only.
    pub job_proxy_bind: Option<SocketAddr>,
    /// The PostHog project key. Product analytics are off without it, as when self-hosted.
    pub posthog_key: Option<String>,
    pub posthog_host: String,
    /// Billing, hosted. Off without it, as when self-hosted: no plan limits.
    pub stripe: Option<StripeConfig>,
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
    /// A GKE sandbox per job, claimed from an Agent Sandbox warm pool, which reaches back to
    /// `job_proxy_url`. The orchestrator runs in the same cluster.
    AgentSandbox {
        namespace: String,
        warm_pool: String,
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
        let secret_key = secret_key()?;
        let provider = match var("MERGEPATROL_PROVIDER") {
            Some(p) => Provider::parse(&p).with_context(|| {
                format!("MERGEPATROL_PROVIDER: {p:?} is not anthropic or openai")
            })?,
            None => Provider::Anthropic,
        };
        let model = match (provider, var("MERGEPATROL_MODEL")) {
            (_, Some(model)) => model,
            (Provider::Anthropic, None) => "claude-opus-5-5".into(),
            (_, None) => bail!(
                "MERGEPATROL_MODEL: set the {} model to use",
                provider.name()
            ),
        };
        let repo_defaults = RepoSettings {
            label: opt("MERGEPATROL_LABEL", "mergepatrol"),
            batch_size: opt("MERGEPATROL_BATCH_SIZE", "8").parse()?,
            resolver: ResolverSettings {
                provider,
                model,
                effort: opt("MERGEPATROL_EFFORT", "medium"),
            },
            verify: Vec::new(),
            fix_ci: Default::default(),
            branches: Vec::new(),
            review: Default::default(),
        };
        repo_defaults.check().map_err(|e| {
            anyhow::anyhow!("MERGEPATROL_LABEL, _BATCH_SIZE, _PROVIDER, _MODEL or _EFFORT: {e}")
        })?;
        let public_url = opt("MERGEPATROL_PUBLIC_URL", "http://localhost:8080");
        Ok(Self {
            secret_key,
            database_url: req("MERGEPATROL_DATABASE_URL")?,
            hosted_app: hosted_app()?,
            job_proxy_url: opt("MERGEPATROL_JOB_PROXY_URL", &public_url),
            job_proxy_bind: var("MERGEPATROL_JOB_PROXY_BIND")
                .map(|b| b.parse())
                .transpose()
                .context("MERGEPATROL_JOB_PROXY_BIND")?,
            public_url,
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
                "agent-sandbox" => ExecutorConfig::AgentSandbox {
                    namespace: opt("MERGEPATROL_SANDBOX_NAMESPACE", "mergepatrol-jobs"),
                    warm_pool: opt("MERGEPATROL_SANDBOX_WARM_POOL", "mergepatrol-job"),
                },
                other => {
                    bail!("MERGEPATROL_EXECUTOR: {other:?} is not local, docker or agent-sandbox")
                }
            },
            posthog_key: var("MERGEPATROL_POSTHOG_KEY"),
            posthog_host: opt("MERGEPATROL_POSTHOG_HOST", "https://us.i.posthog.com"),
            stripe: stripe()?,
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

/// The three variables, or none.
fn stripe() -> Result<Option<StripeConfig>> {
    let names = [
        "MERGEPATROL_STRIPE_SECRET_KEY",
        "MERGEPATROL_STRIPE_WEBHOOK_SECRET",
        "MERGEPATROL_STRIPE_PRICE",
    ];
    let values: Vec<Option<String>> = names.iter().map(|n| var(n)).collect();
    match values.as_slice() {
        [None, None, None] => Ok(None),
        [Some(secret_key), Some(webhook_secret), Some(price)] => Ok(Some(StripeConfig {
            secret_key: secret_key.clone(),
            webhook_secret: webhook_secret.clone(),
            price: price.clone(),
        })),
        _ => bail!("set all of {} for billing, or none", names.join(", ")),
    }
}

/// From `MERGEPATROL_SECRET_KEY`, or the file at `MERGEPATROL_SECRET_KEY_FILE`.
fn secret_key() -> Result<String> {
    let key = match (
        var("MERGEPATROL_SECRET_KEY"),
        var("MERGEPATROL_SECRET_KEY_FILE"),
    ) {
        (Some(_), Some(_)) => {
            bail!("set MERGEPATROL_SECRET_KEY or MERGEPATROL_SECRET_KEY_FILE, not both")
        }
        (Some(key), None) => key,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .with_context(|| format!("MERGEPATROL_SECRET_KEY_FILE: read {path}"))?,
        (None, None) => String::new(),
    };
    let key = key.trim();
    if key.is_empty() {
        bail!(
            "MERGEPATROL_SECRET_KEY is not set: generate one with `openssl rand -base64 32`, and keep it: it decrypts the stored secrets"
        );
    }
    Ok(key.to_owned())
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
