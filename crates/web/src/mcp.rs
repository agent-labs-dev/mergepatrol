//! The MCP server at `/mcp`, for coding agents: queue a PR, take it out, and read where it is,
//! without handling the labels. The bearer token is a GitHub token, or a Mergepatrol session
//! from signing in through `oauth`, which holds one. Either way an agent acts as the person or
//! bot the GitHub token is for, and GitHub checks what they may do.
//!
//! Streamable HTTP without sessions: each POST carries one JSON-RPC message, answered as JSON.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mergepatrol_core::{EntryState, RepoName};
use mergepatrol_github::{Installation, setup};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::api::{dequeue, github_app, in_merge_order, queue};
use crate::auth::{hash, session_github_token};
use crate::{ApiError, AppState};

/// The newest first: the protocol versions this server speaks.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Mergepatrol is the merge queue of the repos it is installed on. Queue a PR with `queue_pull` instead of merging it, then follow it with `pull_status`. Calls act as the GitHub user or bot you connected as.";

/// How many of a PR's events `pull_status` returns.
const EVENTS: u32 = 10;

/// How long what GitHub said of a token holds. A token revoked meanwhile still reads the queue
/// until then, but changes nothing: the labels go on with the token itself.
const SEEN_FOR: Duration = Duration::from_secs(300);

/// What GitHub said lately of the GitHub tokens agents call with, by their hash: so a call costs
/// none of a token's rate limit before its own.
#[derive(Default)]
pub(crate) struct Seen {
    /// The tokens GitHub takes.
    tokens: Recent<String, ()>,
    /// The installation on each repo a token can read.
    repos: Recent<(String, RepoName), u64>,
}

struct Recent<K, V>(Mutex<HashMap<K, (Instant, V)>>);

impl<K, V> Default for Recent<K, V> {
    fn default() -> Self {
        Self(Mutex::default())
    }
}

impl<K: Eq + Hash, V: Clone> Recent<K, V> {
    fn get(&self, key: &K) -> Option<V> {
        let map = self.0.lock().unwrap();
        map.get(key)
            .filter(|(at, _)| at.elapsed() < SEEN_FOR)
            .map(|(_, v)| v.clone())
    }

    fn put(&self, key: K, value: V) {
        let mut map = self.0.lock().unwrap();
        map.retain(|_, (at, _)| at.elapsed() < SEEN_FOR);
        map.insert(key, (Instant::now(), value));
    }
}

pub(crate) async fn post(
    State(app): State<AppState>,
    headers: HeaderMap,
    Json(message): Json<Value>,
) -> Response {
    let token = match bearer(&headers) {
        Some(bearer) => caller(&app, &bearer).await,
        None => Ok(None),
    };
    let token = match token {
        Ok(Some(token)) => token,
        Ok(None) => return unauthorized(&app),
        Err(e) => return e.into_response(),
    };
    // A notification, or the client's answer to a request: nothing to answer.
    let (Some(id), Some(method)) = (message.get("id"), message["method"].as_str()) else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = &message["params"];
    let result = match method {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => Ok(call(&app, &token, params).await),
        _ => Err(json!({ "code": -32601, "message": format!("no method {method}") })),
    };
    let body = match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    };
    Json(body).into_response()
}

/// No server-sent events: everything is answered in the POST.
pub(crate) async fn get() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

/// Points an MCP client at the sign-in (RFC 9728), and anyone else at the header.
fn unauthorized(app: &AppState) -> Response {
    let metadata = format!(
        "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
        app.public_url
    );
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, metadata)],
        "Sign in, or pass a GitHub token: `Authorization: Bearer <token>`.",
    )
        .into_response()
}

/// The GitHub token to act with: the session's, or the bearer itself. `None` when it is
/// neither a session nor a token GitHub takes.
async fn caller(app: &AppState, bearer: &str) -> Result<Option<String>, ApiError> {
    match session_github_token(app, bearer).await {
        Ok(Some(token)) => return Ok(Some(token)),
        Ok(None) => {}
        // A session whose GitHub token is gone: the client signs in again.
        Err(ApiError::Status(StatusCode::UNAUTHORIZED, _)) => return Ok(None),
        Err(e) => return Err(e),
    }
    let key = hash(bearer);
    if app.seen.tokens.get(&key).is_some() {
        return Ok(Some(bearer.to_owned()));
    }
    if !setup::token_works(bearer).await? {
        return Ok(None);
    }
    app.seen.tokens.put(key, ());
    Ok(Some(bearer.to_owned()))
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str();
    let version = VERSIONS
        .into_iter()
        .find(|v| Some(*v) == asked)
        .unwrap_or(VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "mergepatrol", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn tools() -> Value {
    let repo = json!({ "type": "string", "description": "The repo, as `owner/name`." });
    let number = json!({ "type": "integer", "description": "The PR's number." });
    json!([
        {
            "name": "queue_pull",
            "description": "Queue a PR to merge: Mergepatrol rebases it, tests it, and merges it once its checks pass. On a queued PR, changes its priority. Queuing one PR of a stack queues the whole stack. Needs triage access to the repo.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo": repo,
                    "number": number,
                    "priority": { "type": "boolean", "description": "Put it at the front of the queue. Default false." },
                },
                "required": ["repo", "number"],
            },
        },
        {
            "name": "dequeue_pull",
            "description": "Take a PR, and its stack if it is in one, out of the queue. Cancels its run. Needs triage access to the repo.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo": repo, "number": number },
                "required": ["repo", "number"],
            },
        },
        {
            "name": "pull_status",
            "description": "Where a PR is: its place in the queue, or null when it is not queued, and its latest queue events, newest first. An event of kind `merged`, `ejected`, `dequeued` or `cancelled` is how it last left the queue.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo": repo, "number": number },
                "required": ["repo", "number"],
            },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "list_queue",
            "description": "A repo's queues, one per branch PRs merge into, each in the order it takes them.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo": repo },
                "required": ["repo"],
            },
            "annotations": { "readOnlyHint": true },
        },
    ])
}

#[derive(Deserialize)]
struct RepoArgs {
    repo: String,
}

#[derive(Deserialize)]
struct PullArgs {
    repo: String,
    number: u64,
}

#[derive(Deserialize)]
struct QueueArgs {
    repo: String,
    number: u64,
    #[serde(default)]
    priority: bool,
}

/// A tool's result. Its errors are the tool's, for the agent to read, not the protocol's.
async fn call(app: &AppState, token: &str, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let result = match name {
        "queue_pull" => queue_pull(app, token, args).await,
        "dequeue_pull" => dequeue_pull(app, token, args).await,
        "pull_status" => pull_status(app, token, args).await,
        "list_queue" => list_queue(app, token, args).await,
        _ => Err(ApiError::bad_request(&format!("No tool {name}."))),
    };
    let (text, is_error) = match result {
        Ok(value) => (value.to_string(), false),
        Err(ApiError::Status(_, m)) => (m, true),
        Err(ApiError::Internal(e)) => {
            tracing::error!(error = %format!("{e:#}"), tool = name, "MCP error");
            ("Internal error.".into(), true)
        }
    };
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

async fn queue_pull(app: &AppState, token: &str, args: Value) -> Result<Value, ApiError> {
    let QueueArgs {
        repo,
        number,
        priority,
    } = parse(args)?;
    let (repo, gh) = open(app, token, &repo).await?;
    queue(app, &gh, token, &repo, number, priority).await?;
    tracing::info!(installation = gh.id, %repo, number, priority, "queued over MCP");
    Ok(json!({ "queued": true }))
}

async fn dequeue_pull(app: &AppState, token: &str, args: Value) -> Result<Value, ApiError> {
    let PullArgs { repo, number } = parse(args)?;
    let (repo, gh) = open(app, token, &repo).await?;
    dequeue(app, &gh, token, &repo, number).await?;
    tracing::info!(installation = gh.id, %repo, number, "dequeued over MCP");
    Ok(json!({ "dequeued": true }))
}

async fn pull_status(app: &AppState, token: &str, args: Value) -> Result<Value, ApiError> {
    let PullArgs { repo, number } = parse(args)?;
    let (repo, gh) = open(app, token, &repo).await?;
    let queued = app
        .store
        .repo_lanes(gh.id, &repo)
        .await?
        .into_iter()
        .find_map(|(trunk, lane)| {
            let entries = in_merge_order(lane.entries);
            let place = entries.iter().position(|e| e.prs().contains(&number))?;
            let mut view = entry(&entries[place]);
            view["trunk"] = json!(trunk);
            view["position"] = json!(place + 1);
            view["batch_pr"] = json!(lane.batch_pr);
            Some(view)
        });
    let events: Vec<Value> = app
        .store
        .events(gh.id, &repo, Some(number), EVENTS)
        .await?
        .into_iter()
        .map(|e| json!({ "at": e.at, "kind": e.kind, "message": e.message }))
        .collect();
    Ok(json!({ "queued": queued, "events": events }))
}

async fn list_queue(app: &AppState, token: &str, args: Value) -> Result<Value, ApiError> {
    let RepoArgs { repo } = parse(args)?;
    let (repo, gh) = open(app, token, &repo).await?;
    let lanes: Vec<Value> = app
        .store
        .repo_lanes(gh.id, &repo)
        .await?
        .into_iter()
        .map(|(trunk, lane)| {
            let entries: Vec<Value> = in_merge_order(lane.entries).iter().map(entry).collect();
            json!({ "trunk": trunk, "batch_pr": lane.batch_pr, "entries": entries })
        })
        .collect();
    Ok(json!({ "queues": lanes }))
}

fn entry(e: &mergepatrol_core::Entry) -> Value {
    let (state, held_because) = match &e.state {
        EntryState::Waiting => ("waiting", None),
        EntryState::Active => ("running", None),
        EntryState::Held(why) => ("held", Some(why)),
    };
    json!({
        "number": e.number,
        "title": e.summary.title,
        "url": e.summary.url,
        "state": state,
        "held_because": held_because,
        "priority": e.priority,
        "stack": e.layers,
    })
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, ApiError> {
    serde_json::from_value(args).map_err(|e| ApiError::bad_request(&format!("Bad arguments: {e}.")))
}

/// The repo, and the App's installation on it, once the token shows its owner can read it.
async fn open(
    app: &AppState,
    token: &str,
    repo: &str,
) -> Result<(RepoName, Installation), ApiError> {
    let repo =
        RepoName::parse(repo).ok_or_else(|| ApiError::bad_request("`repo` must be owner/name."))?;
    let github = github_app(app).await?;
    let key = (hash(token), repo.clone());
    if let Some(id) = app.seen.repos.get(&key) {
        return Ok((repo, github.installation(id)));
    }
    let not_found = || ApiError::Status(StatusCode::NOT_FOUND, format!("No repo {repo}."));
    match setup::can_read(token, &repo.to_string()).await {
        Ok(true) => {}
        Ok(false) => return Err(not_found()),
        Err(e) if e.is::<setup::Revoked>() => {
            return Err(ApiError::Status(
                StatusCode::UNAUTHORIZED,
                "GitHub does not take this token.".into(),
            ));
        }
        Err(e) => return Err(e.into()),
    }
    let gh = github.installation_for(&repo).await?.ok_or_else(|| {
        ApiError::Status(
            StatusCode::NOT_FOUND,
            format!("Mergepatrol is not installed on {repo}."),
        )
    })?;
    app.seen.repos.put(key, gh.id);
    Ok((repo, gh))
}
