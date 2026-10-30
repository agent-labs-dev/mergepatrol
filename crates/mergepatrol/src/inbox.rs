//! Keeps the You tab's PRs from GitHub: reads each PR a webhook said changed, every repo's open
//! PRs now and then for what webhooks missed, and who is on each organization's teams. The
//! dashboard reads only what this keeps.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use mergepatrol_core::RepoName;
use mergepatrol_github::{App, InboxPull, Installation};
use mergepatrol_store::{InboxRead, InboxStale, InboxWrite};
use serde_json::Value;

use crate::queue::{Ctx, app_credentials};

/// How often to look for PRs that changed.
const READ_EVERY: Duration = Duration::from_secs(2);
/// How long after a change GitHub's reads may still show what was before.
const SETTLE: Duration = Duration::from_secs(10);
/// The changed PRs read in one go.
const READ_BATCH: u32 = 100;
/// A repo whose PRs cannot be read waits this long to try again.
const FAILED_WAIT: Duration = Duration::from_secs(5 * 60);
/// How often to look at the installations, their repos and their teams.
const INSTALLATIONS_EVERY: Duration = Duration::from_secs(10 * 60);
/// How often every open PR of a repo is read again: daily with webhooks, which tell of each
/// change, and as often as the installations without, as on a self-hosted http URL.
const INDEX_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// The repos indexed in one go, so the first index after a deploy spreads out.
const INDEX_BATCH: usize = 20;
const INDEX_GAP: Duration = Duration::from_secs(30);

/// The installations' clients, kept so their tokens are too.
#[derive(Default)]
struct Clients {
    app: Option<(u64, App)>,
    installations: HashMap<u64, Installation>,
}

impl Clients {
    /// The installation's client. `None` before the GitHub App exists.
    async fn get(&mut self, ctx: &Ctx, id: u64) -> Result<Option<Installation>> {
        let Some(app) = self.app(ctx).await? else {
            return Ok(None);
        };
        Ok(Some(
            self.installations
                .entry(id)
                .or_insert_with(|| app.installation(id))
                .clone(),
        ))
    }

    async fn app(&mut self, ctx: &Ctx) -> Result<Option<App>> {
        let Some(creds) = app_credentials(ctx).await? else {
            return Ok(None);
        };
        // A new App (set up again) means new credentials.
        if self.app.as_ref().is_none_or(|(id, _)| *id != creds.id) {
            self.app = Some((creds.id, App::from_credentials(&creds)?));
            self.installations.clear();
        }
        Ok(self.app.as_ref().map(|(_, app)| app.clone()))
    }
}

/// Reads again each PR that changed, as webhooks and the dashboard tell.
pub async fn read_changed(ctx: Arc<Ctx>) {
    let mut clients = Clients::default();
    let mut failed: HashMap<(u64, RepoName), Instant> = HashMap::new();
    loop {
        if let Err(e) = read_changed_once(&ctx, &mut clients, &mut failed).await {
            tracing::warn!(error = %format!("{e:#}"), "could not read the inbox's changed PRs");
        }
        tokio::time::sleep(READ_EVERY).await;
    }
}

async fn read_changed_once(
    ctx: &Ctx,
    clients: &mut Clients,
    failed: &mut HashMap<(u64, RepoName), Instant>,
) -> Result<()> {
    failed.retain(|_, at| at.elapsed() < FAILED_WAIT);
    let mut by_repo: HashMap<(u64, RepoName), Vec<InboxStale>> = HashMap::new();
    for pr in ctx.store.inbox_stale(SETTLE, READ_BATCH).await? {
        by_repo
            .entry((pr.installation_id, pr.repo.clone()))
            .or_default()
            .push(pr);
    }
    for ((id, repo), prs) in by_repo {
        if failed.contains_key(&(id, repo.clone())) {
            continue;
        }
        let Some(gh) = clients.get(ctx, id).await? else {
            return Ok(());
        };
        let numbers: Vec<u64> = prs.iter().map(|p| p.number).collect();
        let nodes = match gh.inbox_pulls(&repo, &numbers).await {
            Ok(nodes) => nodes,
            Err(e) => {
                tracing::warn!(installation = id, %repo, error = %format!("{e:#}"), "could not read the inbox's PRs");
                failed.insert((id, repo), Instant::now());
                continue;
            }
        };
        let writes: Vec<InboxWrite> = prs
            .iter()
            .zip(nodes)
            .filter_map(|(pr, (_, node))| {
                Some(InboxWrite {
                    number: pr.number,
                    changes: pr.changes,
                    read: match node {
                        Some(node) => Some(inbox_read(node, id, &repo)?),
                        None => None,
                    },
                    settled: pr.settled,
                })
            })
            .collect();
        ctx.store.save_inbox_reads(id, &repo, &writes).await?;
    }
    Ok(())
}

/// What the inbox keeps of a PR node. `None` for one it cannot read, which waits for the next
/// index.
fn inbox_read(node: Value, id: u64, repo: &RepoName) -> Option<InboxRead> {
    let p = InboxPull::from_node(&node)
        .inspect_err(
            |e| tracing::warn!(installation = id, %repo, error = %format!("{e:#}"), "an inbox PR"),
        )
        .ok()?;
    let s = p.pr.summary();
    Some(InboxRead {
        state: p.state,
        author: s.author.map(|a| a.login),
        requested_users: s.requested.into_iter().map(|r| r.login).collect(),
        requested_teams: p.requested_teams,
        reviewers: s.reviews.into_iter().map(|r| r.reviewer.login).collect(),
        closed_at: p.closed_at,
        node,
    })
}

/// Follows the installations, their repos and teams, and reads every open PR of each repo now
/// and then.
pub async fn index(ctx: Arc<Ctx>) {
    let mut clients = Clients::default();
    let mut installations_at: Option<Instant> = None;
    // Without webhooks, every change waits for the index.
    let every = if ctx.config.public_url.starts_with("https://") {
        INDEX_EVERY
    } else {
        INSTALLATIONS_EVERY
    };
    loop {
        if installations_at.is_none_or(|at| at.elapsed() >= INSTALLATIONS_EVERY) {
            match follow_installations(&ctx, &mut clients).await {
                Ok(()) => installations_at = Some(Instant::now()),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "could not follow the installations")
                }
            }
        }
        if let Err(e) = index_due(&ctx, &mut clients, every).await {
            tracing::warn!(error = %format!("{e:#}"), "could not index the inbox's repos");
        }
        tokio::time::sleep(INDEX_GAP).await;
    }
}

async fn follow_installations(ctx: &Ctx, clients: &mut Clients) -> Result<()> {
    let Some(app) = clients.app(ctx).await? else {
        return Ok(());
    };
    for inst in app.installations().await? {
        let Some(gh) = clients.get(ctx, inst.id).await? else {
            return Ok(());
        };
        // One installation failing (suspended, say) does not stop the others.
        match gh.repositories().await {
            Ok(repos) => {
                let repos: Vec<RepoName> = repos.into_iter().map(|r| r.repo).collect();
                ctx.store.set_inbox_repos(inst.id, &repos).await?;
            }
            Err(e) => {
                tracing::warn!(installation = inst.id, error = %format!("{e:#}"), "could not list the repos");
                continue;
            }
        }
        if !inst.organization {
            continue;
        }
        // Without `members: read`, which the account's owner accepts, only reviews asked of
        // the user show.
        match gh.org_teams(&inst.account).await {
            Ok(teams) => ctx.store.set_inbox_teams(inst.id, &teams).await?,
            Err(e) => {
                tracing::info!(installation = inst.id, account = %inst.account, error = %format!("{e:#}"), "could not read the teams")
            }
        }
    }
    Ok(())
}

async fn index_due(ctx: &Ctx, clients: &mut Clients, every: Duration) -> Result<()> {
    for (id, repo) in ctx
        .store
        .inbox_repos_due(every)
        .await?
        .into_iter()
        .take(INDEX_BATCH)
    {
        let Some(gh) = clients.get(ctx, id).await? else {
            return Ok(());
        };
        // The `changes` each PR had before the read: one that changes during it stays stale.
        let changes = ctx.store.inbox_changes(id, &repo).await?;
        let nodes = match gh.open_inbox_pulls(&repo).await {
            Ok(nodes) => nodes,
            Err(e) => {
                tracing::warn!(installation = id, %repo, error = %format!("{e:#}"), "could not index the repo");
                continue;
            }
        };
        let writes: Vec<InboxWrite> = nodes
            .into_iter()
            .filter_map(|node| {
                let read = inbox_read(node, id, &repo)?;
                let number = read.node["number"].as_u64()?;
                Some(InboxWrite {
                    number,
                    changes: changes.get(&number).copied().unwrap_or(0),
                    read: Some(read),
                    settled: true,
                })
            })
            .collect();
        ctx.store.save_inbox_index(id, &repo, &writes).await?;
    }
    Ok(())
}
