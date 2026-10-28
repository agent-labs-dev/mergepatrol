mod config;
mod publish;
mod queue;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use mergepatrol_executor::LocalExecutor;
use mergepatrol_github::App;
use mergepatrol_resolver::ClaudeResolver;
use mergepatrol_store::SqliteStore;
use tokio::sync::Semaphore;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::queue::Ctx;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    if std::env::var_os("ANTHROPIC_API_KEY").is_none() {
        tracing::warn!("ANTHROPIC_API_KEY is not set: conflicts will fail to resolve");
    }
    tokio::fs::create_dir_all(&config.data_dir).await?;

    let store = Arc::new(SqliteStore::open(&config.data_dir.join("mergepatrol.db")).await?);
    let exec = Arc::new(LocalExecutor::new(config.data_dir.join("work")));
    let resolver = Arc::new(ClaudeResolver {
        exec: exec.clone(),
        model: config.model.clone(),
        effort: config.effort.clone(),
        timeout: Duration::from_secs(15 * 60),
        max_turns: 50,
    });

    let bind = config.bind;
    let ctx = Arc::new(Ctx {
        app: App::new(config.app_id, &config.private_key)?,
        store: store.clone(),
        exec,
        resolver,
        jobs: Semaphore::new(config.max_jobs),
        resolutions: Semaphore::new(config.max_resolutions),
        config,
    });

    for repo in ctx.config.repos.clone() {
        tracing::info!(%repo, "watching");
        tokio::spawn(queue::run_repo(ctx.clone(), repo));
    }

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "dashboard");
    axum::serve(listener, mergepatrol_web::router(store)).await?;
    Ok(())
}
