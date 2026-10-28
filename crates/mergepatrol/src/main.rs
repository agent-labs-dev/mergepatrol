mod config;
mod publish;
mod queue;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use mergepatrol_executor::LocalExecutor;
use mergepatrol_github::setup::random_token;
use mergepatrol_resolver::{ApiKeys, ClaudeResolver};
use mergepatrol_store::{ANTHROPIC_API_KEY, GITHUB_APP, SqliteStore, Store, Vault};
use mergepatrol_web::WebConfig;
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
    let exec = Arc::new(LocalExecutor::new(config.data_dir.join("work")));
    let resolver = Arc::new(ClaudeResolver {
        exec: exec.clone(),
        keys: Arc::new(VaultKeys(vault.clone())),
        model: config.model.clone(),
        effort: config.effort.clone(),
        timeout: Duration::from_secs(15 * 60),
        max_turns: 50,
    });

    let bind = config.bind;
    let ctx = Arc::new(Ctx {
        vault: vault.clone(),
        store: store.clone(),
        exec,
        resolver,
        jobs: Semaphore::new(config.max_jobs),
        resolutions: Semaphore::new(config.max_resolutions),
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

struct VaultKeys(Arc<Vault>);

#[async_trait::async_trait]
impl ApiKeys for VaultKeys {
    async fn anthropic(&self) -> Result<Option<String>> {
        self.0.get(ANTHROPIC_API_KEY).await
    }
}
