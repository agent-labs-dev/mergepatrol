//! Product analytics for the hosted Mergepatrol, in PostHog. Off without a project key, so a
//! self-hosted Mergepatrol sends nothing.
//!
//! A person is a GitHub login: a signed-in user, or the author of a queued PR. A group of type
//! `workspace` is an installation of the App, keyed by its installation ID: the organization or
//! account that uses Mergepatrol.

use std::sync::Arc;

use mergepatrol_core::StepTimes;
use serde_json::{Value, json};

pub const WORKSPACE: &str = "workspace";

#[derive(Clone, Default)]
pub struct Analytics(Option<Arc<Inner>>);

struct Inner {
    key: String,
    host: String,
    http: reqwest::Client,
}

/// A workspace the signed-in user can access.
pub struct WorkspaceInfo<'a> {
    pub installation_id: u64,
    pub account: &'a str,
    pub account_type: &'a str,
}

impl Analytics {
    /// `key` is the PostHog project key. `host` is the ingestion host, such as
    /// `https://us.i.posthog.com`.
    pub fn new(key: Option<String>, host: &str) -> Self {
        Self(key.map(|key| {
            Arc::new(Inner {
                key,
                host: host.trim_end_matches('/').to_owned(),
                http: reqwest::Client::new(),
            })
        }))
    }

    /// The project key and host, for the dashboard. The key is public: browsers send it.
    pub fn browser(&self) -> Option<(&str, &str)> {
        self.0.as_deref().map(|i| (i.key.as_str(), i.host.as_str()))
    }

    /// A user signed in: sets their person, and the workspaces they can access.
    pub fn signed_in(&self, login: &str, avatar_url: &str, workspaces: &[WorkspaceInfo]) {
        let mut batch = vec![json!({
            "event": "signed in",
            "distinct_id": login,
            "properties": {
                "workspaces": workspaces.len(),
                "$set": { "login": login, "name": login, "avatar_url": avatar_url },
            },
        })];
        batch.extend(workspaces.iter().map(|w| group_identify(login, w)));
        self.send(batch);
    }

    /// GitHub's `installation` webhook, for `sender`: `app installed`, `app uninstalled`,
    /// `app suspended` or `app unsuspended`. Other actions send nothing.
    pub fn installation(&self, action: &str, sender: &str, workspace: &WorkspaceInfo) {
        if let Some(batch) = installation(action, sender, workspace) {
            self.send(batch);
        }
    }

    /// A queue event, such as `merged` or `ejected`, as `pr <kind>`. It counts for the PR's
    /// author, by GitHub login, whether or not they ever signed in.
    pub fn queue_event(
        &self,
        installation_id: u64,
        repo: &str,
        trunk: &str,
        pr: u64,
        kind: &str,
        author: Option<&str>,
    ) {
        self.send(vec![queue_event(
            installation_id,
            repo,
            trunk,
            pr,
            kind,
            author,
        )]);
    }

    /// A PR left the queue, as `pr <outcome>`: a queue event, with the seconds it spent in the
    /// queue and in each step, and the kind of reason it was ejected.
    #[allow(clippy::too_many_arguments)]
    pub fn queue_exit(
        &self,
        installation_id: u64,
        repo: &str,
        trunk: &str,
        pr: u64,
        outcome: &str,
        author: Option<&str>,
        exit: &Exit,
    ) {
        let mut event = queue_event(installation_id, repo, trunk, pr, outcome, author);
        exit.set(&mut event["properties"]);
        self.send(vec![event]);
    }

    /// Fire and forget: analytics never hold up or fail the caller.
    fn send(&self, batch: Vec<Value>) {
        let Some(inner) = self.0.clone() else { return };
        tokio::spawn(async move {
            let body = json!({ "api_key": inner.key, "batch": batch });
            let sent = inner
                .http
                .post(format!("{}/batch/", inner.host))
                .json(&body)
                .send()
                .await
                .and_then(|r| r.error_for_status());
            if let Err(e) = sent {
                tracing::warn!(error = %e, "analytics not sent");
            }
        });
    }
}

fn group_identify(login: &str, w: &WorkspaceInfo) -> Value {
    json!({
        "event": "$groupidentify",
        "distinct_id": login,
        "properties": {
            "$group_type": WORKSPACE,
            "$group_key": w.installation_id.to_string(),
            "$group_set": { "name": w.account, "account_type": w.account_type },
        },
    })
}

fn installation(action: &str, sender: &str, workspace: &WorkspaceInfo) -> Option<Vec<Value>> {
    let event = match action {
        "created" => "app installed",
        "deleted" => "app uninstalled",
        "suspend" => "app suspended",
        "unsuspend" => "app unsuspended",
        _ => return None,
    };
    Some(vec![
        group_identify(sender, workspace),
        json!({
            "event": event,
            "distinct_id": sender,
            "properties": {
                "$groups": { WORKSPACE: workspace.installation_id.to_string() },
                "$set_once": { "login": sender, "name": sender },
                // The server's location, not the person's.
                "$geoip_disable": true,
            },
        }),
    ])
}

/// How a PR left the queue.
pub struct Exit<'a> {
    /// Where its time went: none for a PR that left before it joined.
    pub steps: Option<&'a StepTimes>,
    /// An ejected PR's [`EjectReason::kind`](mergepatrol_core::EjectReason::kind), or `fork`.
    pub reason: Option<&'a str>,
}

impl Exit<'_> {
    fn set(&self, properties: &mut Value) {
        if let Some(steps) = self.steps {
            properties["queue_seconds"] = json!(steps.values().sum::<f64>());
            for (step, seconds) in steps {
                properties[format!("{}_seconds", step.name())] = json!(seconds);
            }
        }
        if let Some(reason) = self.reason {
            properties["reason"] = json!(reason);
        }
    }
}

/// PostHog attaches `$groups` only to an event with a person, so an author-less one (a deleted
/// account) counts for no workspace.
fn queue_event(
    installation_id: u64,
    repo: &str,
    trunk: &str,
    pr: u64,
    kind: &str,
    author: Option<&str>,
) -> Value {
    let workspace = installation_id.to_string();
    let mut properties = json!({
        "repo": repo,
        "trunk": trunk,
        "pr": pr,
        "$groups": { WORKSPACE: &workspace },
        // The server's location, not the person's.
        "$geoip_disable": true,
    });
    let distinct_id = match author {
        Some(login) => {
            // `$set_once`: a signed-in user's own profile wins.
            properties["$set_once"] = json!({ "login": login, "name": login });
            login.to_owned()
        }
        None => {
            properties["$process_person_profile"] = json!(false);
            format!("workspace:{workspace}")
        }
    };
    json!({ "event": format!("pr {kind}"), "distinct_id": distinct_id, "properties": properties })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn without_a_key_it_is_off() {
        let analytics = Analytics::new(None, "https://us.i.posthog.com");
        assert!(analytics.browser().is_none());
        // Nothing to send to, and nothing spawned.
        analytics.queue_event(1, "o/r", "main", 2, "merged", Some("ann"));
    }

    #[test]
    fn a_queue_event_counts_for_its_author_and_workspace() {
        let event = queue_event(7, "o/r", "main", 2, "merged", Some("ann"));
        assert_eq!(event["event"], "pr merged");
        assert_eq!(event["distinct_id"], "ann");
        let props = &event["properties"];
        assert_eq!(props["$groups"]["workspace"], "7");
        assert_eq!(props["$set_once"]["login"], "ann");
        assert!(props.get("$process_person_profile").is_none());
    }

    #[test]
    fn without_an_author_a_queue_event_has_no_person() {
        let event = queue_event(7, "o/r", "main", 2, "merged", None);
        assert_eq!(event["distinct_id"], "workspace:7");
        assert_eq!(event["properties"]["$process_person_profile"], false);
    }

    #[test]
    fn an_exit_has_its_time_in_the_queue_and_its_reason() {
        use mergepatrol_core::Step;
        let steps = StepTimes::from([(Step::Waiting, 60.0), (Step::Checks, 30.0)]);
        let mut event = queue_event(7, "o/r", "main", 2, "ejected", Some("ann"));
        let exit = Exit {
            steps: Some(&steps),
            reason: Some("checks-failed"),
        };
        exit.set(&mut event["properties"]);
        let props = &event["properties"];
        assert_eq!(props["queue_seconds"], 90.0);
        assert_eq!(props["waiting_seconds"], 60.0);
        assert_eq!(props["checks_seconds"], 30.0);
        assert_eq!(props["reason"], "checks-failed");
    }

    #[test]
    fn an_exit_before_joining_has_no_time() {
        let mut event = queue_event(7, "o/r", "main", 2, "dequeued", Some("ann"));
        Exit {
            steps: None,
            reason: None,
        }
        .set(&mut event["properties"]);
        assert!(event["properties"].get("queue_seconds").is_none());
        assert!(event["properties"].get("reason").is_none());
    }

    #[test]
    fn an_install_counts_for_who_installed_it_and_its_workspace() {
        let workspace = WorkspaceInfo {
            installation_id: 7,
            account: "acme",
            account_type: "Organization",
        };
        let batch = installation("created", "ann", &workspace).unwrap();
        assert_eq!(batch[0]["properties"]["$group_set"]["name"], "acme");
        assert_eq!(batch[1]["event"], "app installed");
        assert_eq!(batch[1]["distinct_id"], "ann");
        assert_eq!(batch[1]["properties"]["$groups"]["workspace"], "7");
        assert_eq!(
            installation("deleted", "ann", &workspace).unwrap()[1]["event"],
            "app uninstalled"
        );
        assert!(installation("new_permissions_accepted", "ann", &workspace).is_none());
    }

    #[test]
    fn the_dashboard_gets_the_key_and_host() {
        let analytics = Analytics::new(Some("phc_x".into()), "https://us.i.posthog.com/");
        assert_eq!(
            analytics.browser(),
            Some(("phc_x", "https://us.i.posthog.com"))
        );
    }
}
