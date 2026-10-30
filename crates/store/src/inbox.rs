//! The You tab's PRs, kept from webhooks and reads of GitHub, so the dashboard never asks GitHub
//! for them. Logins are lowercase here.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use mergepatrol_core::{PrNumber, RepoName};
use serde_json::Value;
use sqlx::Row;

use crate::PgStore;

/// What a read of a PR found, and what the inbox looks it up by.
#[derive(Clone, Debug, PartialEq)]
pub struct InboxRead {
    /// `open`, `merged` or `closed`.
    pub state: String,
    pub author: Option<String>,
    pub requested_users: Vec<String>,
    pub requested_teams: Vec<String>,
    pub reviewers: Vec<String>,
    /// RFC 3339.
    pub closed_at: Option<String>,
    pub node: Value,
}

/// A read of a PR, begun at `changes`. `None`: the PR is gone. Only a `settled` read makes the
/// PR fresh: GitHub's reads lag its webhooks.
pub struct InboxWrite {
    pub number: PrNumber,
    pub changes: i64,
    pub read: Option<InboxRead>,
    pub settled: bool,
}

/// A PR to read again, and whether its change is old enough for a read to show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboxStale {
    pub installation_id: u64,
    pub repo: RepoName,
    pub number: PrNumber,
    pub changes: i64,
    pub settled: bool,
}

/// A PR of a user's inbox.
#[derive(Clone, Debug, PartialEq)]
pub struct InboxRow {
    pub installation_id: u64,
    pub node: Value,
    /// When the user last opened it, RFC 3339.
    pub seen_at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UserInbox {
    /// Theirs and open.
    pub authored: Vec<InboxRow>,
    /// Open, and asked of them or of a team of theirs, which they have not reviewed since.
    pub requested: Vec<InboxRow>,
    /// Theirs, merged within the window asked for.
    pub merged: Vec<InboxRow>,
}

#[async_trait]
pub trait InboxStore: Send + Sync {
    /// A webhook said the PR changed: read it again. Its repo joins the inbox if it was not in.
    async fn inbox_changed(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
    ) -> Result<()>;
    /// `login` reviewed the PR from the dashboard: out of their requests now, before GitHub's
    /// reads show it.
    async fn inbox_reviewed(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        login: &str,
    ) -> Result<()>;
    /// PRs that changed since they were read, oldest change first. A PR read since its last
    /// change waits for the change to settle.
    async fn inbox_stale(&self, settle: Duration, limit: u32) -> Result<Vec<InboxStale>>;
    /// Each PR of the repo the inbox has, by number, with its `changes`.
    async fn inbox_changes(
        &self,
        installation_id: u64,
        repo: &RepoName,
    ) -> Result<HashMap<PrNumber, i64>>;
    async fn save_inbox_reads(
        &self,
        installation_id: u64,
        repo: &RepoName,
        writes: &[InboxWrite],
    ) -> Result<()>;
    /// Saves a read of every open PR of the repo. One the inbox has open but the read lacks
    /// closed meanwhile: it reads again.
    async fn save_inbox_index(
        &self,
        installation_id: u64,
        repo: &RepoName,
        open: &[InboxWrite],
    ) -> Result<()>;
    /// Makes the installation's repos these: adds the new ones, drops the rest with their PRs.
    async fn set_inbox_repos(&self, installation_id: u64, repos: &[RepoName]) -> Result<()>;
    /// Repos whose open PRs were never read, or not for `age`.
    async fn inbox_repos_due(&self, age: Duration) -> Result<Vec<(u64, RepoName)>>;
    /// The installation's teams, by slug, with their members' logins.
    async fn set_inbox_teams(
        &self,
        installation_id: u64,
        teams: &[(String, Vec<String>)],
    ) -> Result<()>;
    async fn set_inbox_member(
        &self,
        installation_id: u64,
        team: &str,
        login: &str,
        member: bool,
    ) -> Result<()>;
    async fn delete_inbox_team(&self, installation_id: u64, team: &str) -> Result<()>;
    /// `login`'s inbox, in these installations. Merged PRs from the last `merged_for`.
    async fn inbox(
        &self,
        login: &str,
        installation_ids: &[u64],
        merged_for: Duration,
    ) -> Result<UserInbox>;
    async fn inbox_seen(
        &self,
        login: &str,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
    ) -> Result<()>;
    /// Deletes PRs closed or merged `age` ago, and what users saw of them: how many.
    async fn prune_inbox(&self, age: Duration) -> Result<u64>;
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

#[async_trait]
impl InboxStore for PgStore {
    async fn inbox_changed(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO inbox_repos (installation_id, repo) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO inbox_pulls (installation_id, repo, number) VALUES ($1, $2, $3)
             ON CONFLICT (installation_id, repo, number)
             DO UPDATE SET changes = inbox_pulls.changes + 1, changed_at = now()",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(pr as i64)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn inbox_reviewed(
        &self,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
        login: &str,
    ) -> Result<()> {
        let login = login.to_lowercase();
        sqlx::query(
            "UPDATE inbox_pulls SET requested_users = array_remove(requested_users, $4),
                 reviewers = array_append(array_remove(reviewers, $4), $4),
                 changes = changes + 1, changed_at = now()
             WHERE installation_id = $1 AND repo = $2 AND number = $3",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(pr as i64)
        .bind(login)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn inbox_stale(&self, settle: Duration, limit: u32) -> Result<Vec<InboxStale>> {
        let rows = sqlx::query(
            "SELECT installation_id, repo, number, changes,
                    changed_at < now() - make_interval(secs => $1) AS settled
             FROM inbox_pulls
             WHERE changes > read
               AND (read_at IS NULL OR read_at < changed_at
                    OR changed_at < now() - make_interval(secs => $1))
             ORDER BY changed_at LIMIT $2",
        )
        .bind(secs(settle))
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .filter_map(|r| {
                let repo = RepoName::parse(r.get::<&str, _>("repo"))?;
                Some(Ok(InboxStale {
                    installation_id: r.get::<i64, _>("installation_id") as u64,
                    repo,
                    number: r.get::<i64, _>("number") as u64,
                    changes: r.get("changes"),
                    settled: r.get("settled"),
                }))
            })
            .collect()
    }

    async fn inbox_changes(
        &self,
        installation_id: u64,
        repo: &RepoName,
    ) -> Result<HashMap<PrNumber, i64>> {
        let rows = sqlx::query(
            "SELECT number, changes FROM inbox_pulls WHERE installation_id = $1 AND repo = $2",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<i64, _>("number") as u64, r.get("changes")))
            .collect())
    }

    async fn save_inbox_reads(
        &self,
        installation_id: u64,
        repo: &RepoName,
        writes: &[InboxWrite],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        save_writes(&mut tx, installation_id, repo, writes).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn save_inbox_index(
        &self,
        installation_id: u64,
        repo: &RepoName,
        open: &[InboxWrite],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO inbox_repos (installation_id, repo, indexed_at) VALUES ($1, $2, now())
             ON CONFLICT (installation_id, repo) DO UPDATE SET indexed_at = now()",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .execute(&mut *tx)
        .await?;
        save_writes(&mut tx, installation_id, repo, open).await?;
        let numbers: Vec<i64> = open.iter().map(|w| w.number as i64).collect();
        sqlx::query(
            "UPDATE inbox_pulls SET changes = changes + 1, changed_at = now()
             WHERE installation_id = $1 AND repo = $2 AND state = 'open'
               AND NOT number = ANY($3)",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(&numbers)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn set_inbox_repos(&self, installation_id: u64, repos: &[RepoName]) -> Result<()> {
        let names: Vec<String> = repos.iter().map(ToString::to_string).collect();
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM inbox_repos WHERE installation_id = $1 AND NOT repo = ANY($2)")
            .bind(installation_id as i64)
            .bind(&names)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO inbox_repos (installation_id, repo) SELECT $1, unnest($2::text[])
             ON CONFLICT DO NOTHING",
        )
        .bind(installation_id as i64)
        .bind(&names)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn inbox_repos_due(&self, age: Duration) -> Result<Vec<(u64, RepoName)>> {
        let rows = sqlx::query(
            "SELECT installation_id, repo FROM inbox_repos
             WHERE indexed_at IS NULL OR indexed_at < now() - make_interval(secs => $1)
             ORDER BY indexed_at NULLS FIRST",
        )
        .bind(secs(age))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some((
                    r.get::<i64, _>("installation_id") as u64,
                    RepoName::parse(r.get::<&str, _>("repo"))?,
                ))
            })
            .collect())
    }

    async fn set_inbox_teams(
        &self,
        installation_id: u64,
        teams: &[(String, Vec<String>)],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM inbox_teams WHERE installation_id = $1")
            .bind(installation_id as i64)
            .execute(&mut *tx)
            .await?;
        for (team, members) in teams {
            let members: Vec<String> = members.iter().map(|m| m.to_lowercase()).collect();
            sqlx::query(
                "INSERT INTO inbox_teams (installation_id, team, login)
                 SELECT $1, $2, unnest($3::text[]) ON CONFLICT DO NOTHING",
            )
            .bind(installation_id as i64)
            .bind(team)
            .bind(&members)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn set_inbox_member(
        &self,
        installation_id: u64,
        team: &str,
        login: &str,
        member: bool,
    ) -> Result<()> {
        let query = if member {
            "INSERT INTO inbox_teams (installation_id, team, login) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING"
        } else {
            "DELETE FROM inbox_teams WHERE installation_id = $1 AND team = $2 AND login = $3"
        };
        sqlx::query(query)
            .bind(installation_id as i64)
            .bind(team)
            .bind(login.to_lowercase())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_inbox_team(&self, installation_id: u64, team: &str) -> Result<()> {
        sqlx::query("DELETE FROM inbox_teams WHERE installation_id = $1 AND team = $2")
            .bind(installation_id as i64)
            .bind(team)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn inbox(
        &self,
        login: &str,
        installation_ids: &[u64],
        merged_for: Duration,
    ) -> Result<UserInbox> {
        let login = login.to_lowercase();
        let ids: Vec<i64> = installation_ids.iter().map(|&i| i as i64).collect();
        // A team's request stays after one member reviews: theirs is answered then.
        let rows = sqlx::query(
            "SELECT * FROM (
                 SELECT p.installation_id, p.node, p.node->>'updatedAt' AS updated,
                     to_char(s.at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS seen_at,
                     CASE
                         WHEN p.state = 'open' AND p.author = $1 THEN 'authored'
                         WHEN p.state = 'merged' AND p.author = $1
                             AND p.closed_at > now() - make_interval(secs => $3) THEN 'merged'
                         WHEN p.state = 'open' AND p.author IS DISTINCT FROM $1
                             AND ($1 = ANY(p.requested_users)
                                 OR (NOT $1 = ANY(p.reviewers) AND EXISTS (
                                     SELECT 1 FROM inbox_teams t
                                     WHERE t.installation_id = p.installation_id AND t.login = $1
                                       AND t.team = ANY(p.requested_teams))))
                             THEN 'requested'
                     END AS kind
                 FROM inbox_pulls p
                 LEFT JOIN inbox_seen s ON s.login = $1 AND s.installation_id = p.installation_id
                     AND s.repo = p.repo AND s.number = p.number
                 WHERE p.installation_id = ANY($2) AND p.node IS NOT NULL
             ) rows WHERE kind IS NOT NULL ORDER BY updated DESC",
        )
        .bind(&login)
        .bind(&ids)
        .bind(secs(merged_for))
        .fetch_all(&self.pool)
        .await?;
        let mut inbox = UserInbox::default();
        for r in rows {
            let row = InboxRow {
                installation_id: r.get::<i64, _>("installation_id") as u64,
                node: r.get("node"),
                seen_at: r.get("seen_at"),
            };
            match r.get::<&str, _>("kind") {
                "authored" => inbox.authored.push(row),
                "merged" => inbox.merged.push(row),
                _ => inbox.requested.push(row),
            }
        }
        Ok(inbox)
    }

    async fn inbox_seen(
        &self,
        login: &str,
        installation_id: u64,
        repo: &RepoName,
        pr: PrNumber,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO inbox_seen (login, installation_id, repo, number, at)
             VALUES ($1, $2, $3, $4, now())
             ON CONFLICT (login, installation_id, repo, number) DO UPDATE SET at = now()",
        )
        .bind(login.to_lowercase())
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(pr as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn prune_inbox(&self, age: Duration) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let gone = sqlx::query(
            "DELETE FROM inbox_pulls
             WHERE state IN ('merged', 'closed') AND closed_at < now() - make_interval(secs => $1)
             RETURNING installation_id, repo, number",
        )
        .bind(secs(age))
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM inbox_seen s WHERE NOT EXISTS (SELECT 1 FROM inbox_pulls p
                 WHERE p.installation_id = s.installation_id AND p.repo = s.repo
                   AND p.number = s.number)",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(gone.len() as u64)
    }
}

async fn save_writes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    installation_id: u64,
    repo: &RepoName,
    writes: &[InboxWrite],
) -> Result<()> {
    for w in writes {
        let Some(read) = &w.read else {
            sqlx::query(
                "DELETE FROM inbox_pulls
                 WHERE installation_id = $1 AND repo = $2 AND number = $3 AND changes = $4",
            )
            .bind(installation_id as i64)
            .bind(repo.to_string())
            .bind(w.number as i64)
            .bind(w.changes)
            .execute(&mut **tx)
            .await?;
            continue;
        };
        let lower = |v: &[String]| v.iter().map(|s| s.to_lowercase()).collect::<Vec<_>>();
        // A PR new to the inbox starts at the `changes` the read began at: 0.
        sqlx::query(
            "INSERT INTO inbox_pulls (installation_id, repo, number, changes, read, read_at,
                 state, author, requested_users, requested_teams, reviewers, closed_at, node)
             VALUES ($1, $2, $3, $4, $4, now(), $5, $6, $7, $8, $9, $10::timestamptz, $11)
             ON CONFLICT (installation_id, repo, number) DO UPDATE SET
                 read = CASE WHEN $12 THEN GREATEST(inbox_pulls.read, $4) ELSE inbox_pulls.read END,
                 read_at = now(), state = $5, author = $6, requested_users = $7,
                 requested_teams = $8, reviewers = $9, closed_at = $10::timestamptz, node = $11",
        )
        .bind(installation_id as i64)
        .bind(repo.to_string())
        .bind(w.number as i64)
        .bind(w.changes)
        .bind(&read.state)
        .bind(read.author.as_ref().map(|a| a.to_lowercase()))
        .bind(lower(&read.requested_users))
        .bind(&read.requested_teams)
        .bind(lower(&read.reviewers))
        .bind(&read.closed_at)
        .bind(&read.node)
        .bind(w.settled)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sqlx::PgPool;

    fn repo() -> RepoName {
        RepoName::parse("o/r").unwrap()
    }

    fn read(state: &str, author: &str, requested: &[&str], teams: &[&str]) -> InboxRead {
        InboxRead {
            state: state.into(),
            author: Some(author.into()),
            requested_users: requested.iter().map(|s| s.to_string()).collect(),
            requested_teams: teams.iter().map(|s| s.to_string()).collect(),
            reviewers: Vec::new(),
            closed_at: (state != "open").then(|| "2099-01-01T00:00:00Z".into()),
            node: json!({ "author": author, "updatedAt": "2026-10-30T00:00:00Z" }),
        }
    }

    fn write(number: PrNumber, changes: i64, read: InboxRead) -> InboxWrite {
        InboxWrite {
            number,
            changes,
            read: Some(read),
            settled: true,
        }
    }

    fn numbers(rows: &[InboxRow]) -> Vec<String> {
        rows.iter().map(|r| r.node["author"].to_string()).collect()
    }

    #[sqlx::test(migrations = false)]
    async fn an_inbox_has_my_prs_those_asked_of_me_or_my_team_and_my_merged_ones(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        store
            .save_inbox_index(
                1,
                &repo(),
                &[
                    write(1, 0, read("open", "Me", &[], &[])),
                    write(2, 0, read("open", "ann", &["ME"], &[])),
                    write(3, 0, read("open", "bo", &[], &["core"])),
                    write(4, 0, read("open", "cy", &["dee"], &["web"])),
                    write(5, 0, read("merged", "me", &[], &[])),
                ],
            )
            .await
            .unwrap();
        store
            .set_inbox_teams(1, &[("core".into(), vec!["Me".into()])])
            .await
            .unwrap();
        let inbox = store
            .inbox("me", &[1], Duration::from_secs(3600))
            .await
            .unwrap();
        assert_eq!(numbers(&inbox.authored), ["\"Me\""]);
        assert_eq!(numbers(&inbox.requested), ["\"ann\"", "\"bo\""]);
        assert_eq!(numbers(&inbox.merged), ["\"me\""]);
        // Another workspace's PRs are not mine to see.
        assert_eq!(
            store
                .inbox("me", &[2], Duration::from_secs(3600))
                .await
                .unwrap(),
            UserInbox::default()
        );
        // Reviewed: out, the team's request too.
        store.inbox_reviewed(1, &repo(), 2, "Me").await.unwrap();
        store.inbox_reviewed(1, &repo(), 3, "me").await.unwrap();
        let inbox = store
            .inbox("me", &[1], Duration::from_secs(3600))
            .await
            .unwrap();
        assert!(inbox.requested.is_empty());
        // Asked again: back in.
        store
            .save_inbox_reads(
                1,
                &repo(),
                &[write(2, 1, read("open", "ann", &["me"], &[]))],
            )
            .await
            .unwrap();
        let inbox = store
            .inbox("me", &[1], Duration::from_secs(3600))
            .await
            .unwrap();
        assert_eq!(numbers(&inbox.requested), ["\"ann\""]);
    }

    #[sqlx::test(migrations = false)]
    async fn a_change_reads_again_and_an_unsettled_read_stays_stale(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        store.inbox_changed(1, &repo(), 7).await.unwrap();
        let stale = store
            .inbox_stale(Duration::from_secs(60), 10)
            .await
            .unwrap();
        assert_eq!(stale.len(), 1);
        assert!(!stale[0].settled);
        let mut w = write(7, stale[0].changes, read("open", "ann", &[], &[]));
        w.settled = false;
        store.save_inbox_reads(1, &repo(), &[w]).await.unwrap();
        // Read since the change: it waits for the change to settle.
        assert!(
            store
                .inbox_stale(Duration::from_secs(60), 10)
                .await
                .unwrap()
                .is_empty()
        );
        let settled = store.inbox_stale(Duration::ZERO, 10).await.unwrap();
        assert_eq!(settled.len(), 1);
        assert!(settled[0].settled);
        store
            .save_inbox_reads(
                1,
                &repo(),
                &[write(7, settled[0].changes, read("open", "ann", &[], &[]))],
            )
            .await
            .unwrap();
        assert!(
            store
                .inbox_stale(Duration::ZERO, 10)
                .await
                .unwrap()
                .is_empty()
        );
        // A read that began before a change leaves it stale.
        store.inbox_changed(1, &repo(), 7).await.unwrap();
        store
            .save_inbox_reads(
                1,
                &repo(),
                &[write(7, settled[0].changes, read("open", "ann", &[], &[]))],
            )
            .await
            .unwrap();
        assert_eq!(
            store.inbox_stale(Duration::ZERO, 10).await.unwrap().len(),
            1
        );
    }

    #[sqlx::test(migrations = false)]
    async fn an_index_reads_again_a_pr_it_no_longer_finds_open(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        store
            .save_inbox_index(1, &repo(), &[write(1, 0, read("open", "a", &[], &[]))])
            .await
            .unwrap();
        assert!(
            store
                .inbox_repos_due(Duration::from_secs(60))
                .await
                .unwrap()
                .is_empty()
        );
        store.save_inbox_index(1, &repo(), &[]).await.unwrap();
        let stale = store.inbox_stale(Duration::ZERO, 10).await.unwrap();
        assert_eq!(stale.iter().map(|s| s.number).collect::<Vec<_>>(), [1]);
        // Gone from GitHub: gone.
        store
            .save_inbox_reads(
                1,
                &repo(),
                &[InboxWrite {
                    number: 1,
                    changes: stale[0].changes,
                    read: None,
                    settled: true,
                }],
            )
            .await
            .unwrap();
        assert!(store.inbox_changes(1, &repo()).await.unwrap().is_empty());
    }

    async fn requested(store: &PgStore) -> usize {
        let inbox = store
            .inbox("me", &[1], Duration::from_secs(60))
            .await
            .unwrap();
        inbox.requested.len()
    }

    #[sqlx::test(migrations = false)]
    async fn repos_and_teams_follow_the_installation(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let other = RepoName::parse("o/other").unwrap();
        store
            .set_inbox_repos(1, &[repo(), other.clone()])
            .await
            .unwrap();
        assert_eq!(
            store
                .inbox_repos_due(Duration::from_secs(60))
                .await
                .unwrap()
                .len(),
            2
        );
        store
            .save_inbox_index(1, &other, &[write(1, 0, read("open", "me", &[], &[]))])
            .await
            .unwrap();
        store.set_inbox_repos(1, &[repo()]).await.unwrap();
        let inbox = store
            .inbox("me", &[1], Duration::from_secs(60))
            .await
            .unwrap();
        assert!(inbox.authored.is_empty());
        store
            .save_inbox_index(
                1,
                &repo(),
                &[write(2, 0, read("open", "ann", &[], &["core"]))],
            )
            .await
            .unwrap();
        store.set_inbox_member(1, "core", "Me", true).await.unwrap();
        assert_eq!(requested(&store).await, 1);
        store
            .set_inbox_member(1, "core", "me", false)
            .await
            .unwrap();
        assert_eq!(requested(&store).await, 0);
        store.set_inbox_member(1, "core", "me", true).await.unwrap();
        store.delete_inbox_team(1, "core").await.unwrap();
        assert_eq!(requested(&store).await, 0);
    }

    #[sqlx::test(migrations = false)]
    async fn seen_and_closed_long_ago_are_pruned(pool: PgPool) {
        let store = PgStore::migrate(pool).await.unwrap();
        let mut old = read("closed", "me", &[], &[]);
        old.closed_at = Some("2000-01-01T00:00:00Z".into());
        store
            .save_inbox_index(
                1,
                &repo(),
                &[write(1, 0, old), write(2, 0, read("open", "me", &[], &[]))],
            )
            .await
            .unwrap();
        store.inbox_seen("me", 1, &repo(), 1).await.unwrap();
        store.inbox_seen("Me", 1, &repo(), 2).await.unwrap();
        assert_eq!(
            store.prune_inbox(Duration::from_secs(86400)).await.unwrap(),
            1
        );
        let inbox = store
            .inbox("me", &[1], Duration::from_secs(60))
            .await
            .unwrap();
        assert!(inbox.authored[0].seen_at.is_some());
    }
}
