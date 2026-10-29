//! The server's API, as far as the app reads it. The types mirror `crates/web/src/api.rs`
//! (and `ui/src/api`, which ts-rs generates from it), keeping only the fields the app shows.

use std::time::Duration;

use serde::Deserialize;

const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Deserialize)]
pub struct SessionView {
    pub user: Option<UserView>,
    pub workspaces: Vec<WorkspaceView>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserView {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceView {
    pub installation_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LaneView {
    pub repo: String,
    pub trunk: String,
    pub entries: Vec<EntryView>,
    /// The draft PR that tests the active PRs together.
    pub batch: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    Waiting,
    Held,
    Active,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntryView {
    pub number: u32,
    pub state: EntryState,
    /// Why the PR is held.
    pub detail: Option<String>,
    pub title: String,
    pub url: String,
    pub author: Option<PersonView>,
    /// A stack's PRs, bottom to top. Empty for a PR.
    pub layers: Vec<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PersonView {
    pub login: String,
}

/// Newest first.
#[derive(Debug, Clone, Deserialize)]
pub struct EventView {
    pub at: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExitsPage {
    /// Newest first.
    pub exits: Vec<ExitView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Merged,
    Ejected,
    Removed,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExitView {
    pub number: u32,
    pub outcome: Outcome,
    /// The merge commit, or why.
    pub detail: String,
    pub title: String,
    pub url: String,
    pub enqueued_at: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HistoryStatsView {
    pub steps: Vec<StepStatsView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepView {
    Waiting,
    Held,
    Rebasing,
    Checks,
    Fixing,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StepStatsView {
    pub step: StepView,
    pub p50_secs: f64,
}

#[derive(Debug)]
pub enum ApiError {
    /// The session expired, or was signed out elsewhere.
    SignedOut,
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::SignedOut => f.write_str("signed out"),
            ApiError::Other(e) => f.write_str(e),
        }
    }
}

/// Trades the sign-in's one-time code, with the PKCE verifier only this app has, for a
/// session token. Blocking.
pub fn trade_code(server: &str, code: &str, verifier: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Token {
        token: String,
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into();
    let mut res = agent
        .post(&format!("{}/auth/tray/token", server.trim_end_matches('/')))
        .send_json(serde_json::json!({ "code": code, "verifier": verifier }))
        .map_err(|e| format!("could not reach the server: {e}"))?;
    if res.status().is_success() {
        let token: Token = res.body_mut().read_json().map_err(|e| e.to_string())?;
        return Ok(token.token);
    }
    // The server says why in plain text.
    match res.body_mut().read_to_string() {
        Ok(why) if !why.trim().is_empty() => Err(why.trim().to_owned()),
        _ => Err(format!(
            "the server refused the sign-in: HTTP {}",
            res.status()
        )),
    }
}

/// Blocking: call it off the main thread.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    base: String,
    token: String,
}

impl Client {
    pub fn new(base: &str, token: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            base: base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
        }
    }

    pub fn session(&self) -> Result<SessionView, ApiError> {
        self.get("/api/session", &[])
    }

    pub fn lanes(&self, workspace: u64) -> Result<Vec<LaneView>, ApiError> {
        self.get(&format!("/api/w/{workspace}/lanes"), &[])
    }

    pub fn events(&self, workspace: u64, repo: &str, pr: u32) -> Result<Vec<EventView>, ApiError> {
        self.get(
            &format!("/api/w/{workspace}/events"),
            &[("repo", repo), ("pr", &pr.to_string())],
        )
    }

    pub fn exits(&self, workspace: u64, days: u32) -> Result<ExitsPage, ApiError> {
        self.get(
            &format!("/api/w/{workspace}/history"),
            &[("days", &days.to_string())],
        )
    }

    pub fn stats(&self, workspace: u64, days: u32) -> Result<HistoryStatsView, ApiError> {
        self.get(
            &format!("/api/w/{workspace}/history/stats"),
            &[("days", &days.to_string())],
        )
    }

    /// Ends the session on the server too. Best effort: the app forgets the token either way.
    pub fn sign_out(&self) {
        let _ = self
            .agent
            .post(&format!("{}/auth/logout", self.base))
            .header("authorization", &format!("Bearer {}", self.token))
            .send_empty();
    }

    fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, ApiError> {
        let mut res = self
            .agent
            .get(&format!("{}{path}", self.base))
            .query_pairs(query.iter().copied())
            .header("authorization", &format!("Bearer {}", self.token))
            .call()
            .map_err(|e| ApiError::Other(format!("{path}: {e}")))?;
        match res.status().as_u16() {
            200 => res
                .body_mut()
                .read_json()
                .map_err(|e| ApiError::Other(format!("{path}: {e}"))),
            401 => Err(ApiError::SignedOut),
            code => Err(ApiError::Other(format!("{path}: HTTP {code}"))),
        }
    }
}
