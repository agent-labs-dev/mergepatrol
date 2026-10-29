//! GitHub webhooks. A delivery only wakes the queue of the repo it names: the queue then reads
//! what changed from GitHub, as it does when it polls. A lost delivery costs time, not state.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use hmac::{Hmac, KeyInit, Mac};
use mergepatrol_core::RepoName;
use serde_json::Value;
use sha2::Sha256;

use crate::{ApiError, AppState};

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
    let installation = payload["installation"]["id"].as_u64();
    let repo = payload["repository"]["full_name"]
        .as_str()
        .and_then(RepoName::parse);
    if let (Some(installation), Some(repo)) = (installation, repo) {
        tracing::debug!(
            event = headers.get("x-github-event").and_then(|v| v.to_str().ok()),
            installation,
            %repo,
            "webhook"
        );
        state.wake.wake(installation, repo);
    }
    Ok(StatusCode::NO_CONTENT)
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
