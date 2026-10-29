//! GitHub webhooks. A delivery only wakes the queue of the repo it names: the queue then reads
//! what changed from GitHub, as it does when it polls. A lost delivery costs time, not state.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use hmac::{Hmac, KeyInit, Mac};
use mergepatrol_analytics::{Analytics, WorkspaceInfo};
use mergepatrol_core::{REPO_CONFIG_PATH, RepoName};
use serde_json::Value;
use sha2::Sha256;

use crate::{ApiError, AppState};

/// GitHub's own cap on a delivery. A big push comes close; axum's default is 2 MB.
pub(crate) const MAX_BODY_BYTES: usize = 25 * 1024 * 1024;

/// Implemented by the queue.
pub trait Wake: Send + Sync {
    fn wake(&self, installation: u64, repo: RepoName);
}

pub(crate) async fn receive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let Some(secret) = state
        .app_credentials()
        .await?
        .and_then(|app| app.webhook_secret)
    else {
        return Err(ApiError::Status(
            StatusCode::NOT_FOUND,
            "webhooks are not set up".into(),
        ));
    };
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !signed(&secret, &body, signature) {
        return Err(ApiError::Status(
            StatusCode::UNAUTHORIZED,
            "bad signature".into(),
        ));
    }

    let payload: Value =
        serde_json::from_slice(&body).map_err(|_| ApiError::bad_request("not JSON"))?;
    let event = headers.get("x-github-event").and_then(|v| v.to_str().ok());
    if event == Some("installation") {
        installation(&state.analytics, &payload);
    }
    let installation = payload["installation"]["id"].as_u64();
    let repo = payload["repository"]["full_name"]
        .as_str()
        .and_then(RepoName::parse);
    if let (Some(installation), Some(repo)) = (installation, repo) {
        tracing::debug!(event, installation, %repo, "webhook");
        let mut stored = Ok(());
        if event == Some("push")
            && let Some(has) = config_after_push(&payload)
        {
            stored = state.store.set_has_config(installation, &repo, has).await;
        }
        if event == Some("pull_request")
            && let Some((pr, label, head)) = fork_labeled(&payload)
        {
            stored = state
                .store
                .set_label_head(installation, &repo, pr, label, head)
                .await;
        }
        // The queue reads the rest from GitHub, so it wakes even when the write failed. The
        // error still goes back, so the delivery shows as failed on GitHub, to send again.
        state.wake.wake(installation, repo);
        stored?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The App was installed or uninstalled, for analytics.
fn installation(analytics: &Analytics, payload: &Value) {
    let i = &payload["installation"];
    let (Some(installation_id), Some(action), Some(sender)) = (
        i["id"].as_u64(),
        payload["action"].as_str(),
        payload["sender"]["login"].as_str(),
    ) else {
        return;
    };
    tracing::info!(
        installation = installation_id,
        action,
        sender,
        "installation"
    );
    analytics.installation(
        action,
        sender,
        &WorkspaceInfo {
            installation_id,
            account: i["account"]["login"].as_str().unwrap_or_default(),
            account_type: i["account"]["type"].as_str().unwrap_or_default(),
        },
    );
}

/// Whether a push to the default branch leaves it with the config file, if the push touched the
/// file. The periodic look at every repo catches what this misses.
fn config_after_push(payload: &Value) -> Option<bool> {
    let default = payload["repository"]["default_branch"].as_str()?;
    if payload["ref"].as_str()? != format!("refs/heads/{default}") {
        return None;
    }
    let has = |commit: &Value, list: &str| {
        commit[list]
            .as_array()
            .is_some_and(|files| files.iter().any(|f| f == REPO_CONFIG_PATH))
    };
    payload["commits"].as_array()?.iter().rev().find_map(|c| {
        if has(c, "removed") {
            Some(false)
        } else if has(c, "added") || has(c, "modified") {
            Some(true)
        } else {
            None
        }
    })
}

/// A label went on a fork PR: the PR, the label, and the head it had then. The queue first sees
/// the PR at its next poll, and a fork's author may push before that.
fn fork_labeled(payload: &Value) -> Option<(u64, &str, &str)> {
    if payload["action"].as_str()? != "labeled" {
        return None;
    }
    let pr = &payload["pull_request"];
    if pr["head"]["repo"]["full_name"] == payload["repository"]["full_name"] {
        return None;
    }
    Some((
        pr["number"].as_u64()?,
        payload["label"]["name"].as_str()?,
        pr["head"]["sha"].as_str()?,
    ))
}

/// `signature` is GitHub's `X-Hub-Signature-256`: `sha256=` and the hex HMAC of the body.
fn signed(secret: &str, body: &[u8], signature: &str) -> bool {
    let Some(hex) = signature.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = decode_hex(hex) else {
        return false;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

fn decode_hex(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).ok_or(())?, 16).map_err(|_| ()))
        .collect()
}

#[cfg(test)]
pub(crate) fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256={hex}")
}
