mod config;
mod github;
mod job;
mod publish;
mod queue;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use mergepatrol_executor::remote::{CloudRunLaunch, Mailbox, RemoteExecutor};
use mergepatrol_executor::{DockerExecutor, Executor, LocalExecutor};
use mergepatrol_github::App;
use mergepatrol_github::setup::random_token;
use mergepatrol_store::{GITHUB_APP, PgStore, Scope, Store, Vault};
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
        // `mergepatrol job --out <dir>` or `--remote <link>`: runs inside the sandbox, with no config.
        return match args.get(2..4) {
            Some([flag, dir]) if flag == "--out" => job::main(PathBuf::from(dir)).await,
            Some([flag, link]) if flag == "--remote" => job::main_remote(link).await,
            _ => anyhow::bail!("usage: mergepatrol job --out <dir> | --remote <link>"),
        };
    }
    if let [_, cmd, url] = args.as_slice()
        && cmd == "hosted-manifest"
    {
        // For deploy/gcp/hosted-app.sh, which creates the hosted App once.
        println!("{}", mergepatrol_github::setup::hosted_manifest(url));
        return Ok(());
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

    let store: Arc<dyn Store> = Arc::new(PgStore::connect(&config.database_url).await?);
    let vault = Arc::new(Vault::new(store.clone(), &config.secret_key)?);

    let hosted_app = match &config.hosted_app {
        Some(h) => {
            let app = App::new(h.id, h.private_key.as_bytes())?;
            let creds = app
                .credentials(
                    h.private_key.clone(),
                    h.client_id.clone(),
                    h.client_secret.clone(),
                    h.webhook_secret.clone(),
                )
                .await?;
            tracing::info!(app = %creds.slug, "hosted GitHub App");
            Some(creds)
        }
        None => None,
    };

    // Self-hosted first run: whoever reads this log can set Mergepatrol up from the dashboard.
    let setup_token = match (
        &hosted_app,
        vault.status(Scope::Instance, GITHUB_APP).await?,
    ) {
        (Some(_), _) | (None, Some(_)) => None,
        (None, None) => {
            let token = random_token(18);
            tracing::warn!(
                setup_token = %token,
                url = %config.public_url,
                "no GitHub App yet: open the dashboard and enter this setup token"
            );
            Some(token)
        }
    };

    let mailbox = Arc::new(Mailbox::default());
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
            resolv_conf,
            memory,
            cpus,
        } => {
            tracing::info!(%image, runtime = ?runtime, network = ?network, "jobs run in containers");
            Arc::new(DockerExecutor {
                image: image.clone(),
                runtime: runtime.clone(),
                network: network.clone(),
                resolv_conf: resolv_conf.clone(),
                memory: memory.clone(),
                cpus: cpus.clone(),
                timeout: JOB_TIMEOUT,
            })
        }
        ExecutorConfig::CloudRun { job } => {
            tracing::info!(%job, "jobs run as Cloud Run Job executions");
            Arc::new(RemoteExecutor {
                mailbox: mailbox.clone(),
                base_url: config.public_url.clone(),
                launch: Box::new(CloudRunLaunch {
                    job: job.clone(),
                    timeout: JOB_TIMEOUT,
                    http: reqwest::Client::new(),
                }),
                timeout: JOB_TIMEOUT,
            })
        }
    };

    let bind = config.bind;
    let ctx = Arc::new(Ctx {
        vault: vault.clone(),
        hosted_app: hosted_app.clone(),
        store: store.clone(),
        executor,
        jobs: Semaphore::new(config.max_jobs),
        wakers: Arc::default(),
        config,
    });

    let web = WebConfig {
        hosted_app,
        public_url: ctx.config.public_url.clone(),
        setup_token,
        wake: ctx.wakers.clone(),
    };
    tokio::spawn(queue::supervise(ctx.clone()));

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "dashboard");
    let app = mergepatrol_web::router(store, vault, web).merge(mailbox.router());
    axum::serve(listener, app).await?;
    Ok(())
}
