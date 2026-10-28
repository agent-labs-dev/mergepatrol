mod config;
mod job;
mod publish;
mod queue;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use mergepatrol_executor::{DockerExecutor, Executor, LocalExecutor};
use mergepatrol_github::setup::random_token;
use mergepatrol_store::{ANTHROPIC_API_KEY, GITHUB_APP, SqliteStore, Store, Vault};
use mergepatrol_web::WebConfig;
use tokio::sync::Semaphore;
use tracing_subscriber::EnvFilter;

use crate::config::{Config, ExecutorConfig};
use crate::queue::Ctx;

/// Longer than the resolver's and the verify commands' own limits.
const JOB_TIMEOUT: Duration = Duration::from_secs(40 * 60);

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("job") {
        // `mergepatrol job --out <dir>`: runs inside the sandbox, with no config.
        let out = match args.get(2..4) {
            Some([flag, dir]) if flag == "--out" => PathBuf::from(dir),
            _ => anyhow::bail!("usage: mergepatrol job --out <dir>"),
        };
        return job::main(out).await;
    }
    orchestrator().await
}

async fn orchestrator() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    tokio::fs::create_dir_all(&config.data_dir).await?;

    let store: Arc<dyn Store> =
        Arc::new(SqliteStore::open(&config.data_dir.join("mergepatrol.db")).await?);
    let vault = Arc::new(Vault::new(store.clone(), &config.secret_key)?);

    // First run: whoever reads this log can set Mergepatrol up from the dashboard.
    let setup_token = match vault.status(GITHUB_APP).await? {
        Some(_) => None,
        None => {
            let token = random_token(18);
            tracing::warn!(
                setup_token = %token,
                url = %config.public_url,
                "no GitHub App yet: open the dashboard and enter this setup token"
            );
            Some(token)
        }
    };
    if vault.status(ANTHROPIC_API_KEY).await?.is_none() {
        tracing::warn!("no Anthropic API key yet: add one in the dashboard settings");
    }

    let executor: Arc<dyn Executor> = match &config.executor {
        ExecutorConfig::Local => {
            tracing::warn!("jobs run as local processes, without a sandbox");
            Arc::new(LocalExecutor {
                program: std::env::current_exe().context("find the mergepatrol binary")?,
                timeout: JOB_TIMEOUT,
            })
        }
        ExecutorConfig::Docker {
            image,
            runtime,
            network,
            dns,
            memory,
            cpus,
        } => {
            tracing::info!(%image, runtime = ?runtime, network = ?network, "jobs run in containers");
            Arc::new(DockerExecutor {
                image: image.clone(),
                runtime: runtime.clone(),
                network: network.clone(),
                dns: dns.clone(),
                memory: memory.clone(),
                cpus: cpus.clone(),
                timeout: JOB_TIMEOUT,
            })
        }
    };

    let bind = config.bind;
    let ctx = Arc::new(Ctx {
        vault: vault.clone(),
        store: store.clone(),
        executor,
        jobs: Semaphore::new(config.max_jobs),
        config,
    });

    let web = WebConfig {
        public_url: ctx.config.public_url.clone(),
        setup_token,
    };
    tokio::spawn(queue::supervise(ctx.clone()));

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "dashboard");
    axum::serve(listener, mergepatrol_web::router(store, vault, web)).await?;
    Ok(())
}
