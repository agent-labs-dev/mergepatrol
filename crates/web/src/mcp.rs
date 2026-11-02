//! The MCP server at `/mcp`, for coding agents: a tool for each API route, as `routes` lists
//! them, so an agent does what the dashboard does. The bearer token is a GitHub token, or a
//! MergeLoop session from signing in through `oauth`, which holds one. Either way an agent acts
//! as the person or bot the GitHub token is for, and GitHub checks what they may do.
//!
//! Streamable HTTP without sessions: each POST carries one JSON-RPC message, answered as JSON.

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mergepatrol_core::RepoName;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::auth::{Caller, bearer_caller, percent_encode};
use crate::routes::{self, Route, Tool, Ty};
use crate::{ApiError, AppState};

/// The newest first: the protocol versions this server speaks.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "MergeLoop is the merge queue of the repos it is installed on. Queue a PR with `queue_pull` instead of merging it, then follow it with `pull_status`. The other tools do what the MergeLoop dashboard does. Calls act as the GitHub user or bot you connected as. A GitHub token reaches single repos: the tools for a whole workspace need a MergeLoop sign-in.";

pub(crate) async fn post(
    State(app): State<AppState>,
    headers: HeaderMap,
    Json(message): Json<Value>,
) -> Response {
    let Some(bearer) = bearer(&headers) else {
        return unauthorized(&app);
    };
    let caller = match bearer_caller(&app, &bearer).await {
        Ok(Some(caller)) => caller,
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
        "tools/list" => {
            let tools: Vec<Value> = routes::tools().map(|(r, t)| t.listing(r)).collect();
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => Ok(call(&app, &caller, &bearer, params).await),
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
        "serverInfo": { "name": "mergeloop", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

/// A tool's result: its route's answer. Its errors are the tool's, for the agent to read, not
/// the protocol's.
async fn call(app: &AppState, caller: &Caller, bearer: &str, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let result = match routes::tools().find(|(_, t)| t.name == name) {
        Some((route, tool)) => match request(app, caller, route, tool, bearer, args).await {
            Ok(req) => answer(app, req).await,
            Err(e) => Err(e),
        },
        None => Err(ApiError::bad_request(&format!("No tool {name}."))),
    };
    let (text, is_error) = match result {
        Ok(text) => (text, false),
        Err(ApiError::Status(_, m) | ApiError::GitHubLimited(_, m)) => (m, true),
        Err(ApiError::Internal(e)) => {
            tracing::error!(error = %format!("{e:#}"), tool = name, "MCP error");
            ("Internal error.".into(), true)
        }
    };
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// The tool's call to its route: the path parameters from their arguments, and the others as
/// the query of a GET, or the body of anything else.
async fn request(
    app: &AppState,
    caller: &Caller,
    route: &Route,
    tool: &Tool,
    bearer: &str,
    args: Value,
) -> Result<Request<Body>, ApiError> {
    let Value::Object(mut args) = args else {
        return Err(ApiError::bad_request("Bad arguments: not an object."));
    };
    let mut names: Vec<&str> = route.path_args().map(|(_, name, ..)| *name).collect();
    names.extend(tool.args.iter().filter(|a| a.required).map(|a| a.name));
    let workspace_from_repo = route.takes_repo(tool);
    if let Some(missing) = names
        .into_iter()
        .filter(|&n| !(n == "workspace" && workspace_from_repo))
        .find(|&n| !args.contains_key(n))
    {
        return Err(ApiError::bad_request(&format!(
            "Bad arguments: missing field `{missing}`."
        )));
    }
    let repo = match args.get("repo") {
        Some(repo) => Some(
            repo.as_str()
                .and_then(RepoName::parse)
                .ok_or_else(|| ApiError::bad_request("`repo` must be owner/name."))?,
        ),
        None => None,
    };
    let mut path = route.path.to_owned();
    for (param, name, ..) in route.path_args() {
        let value = match (*param, &repo) {
            ("id", Some(repo)) if !args.contains_key("workspace") => {
                caller.workspace_of(app, repo).await?.to_string()
            }
            ("owner", Some(repo)) => {
                args.remove("repo");
                path = path.replace("{owner}/{name}", &repo.to_string());
                continue;
            }
            _ => scalar(name, args.remove(*name))?,
        };
        path = path.replace(&format!("{{{param}}}"), &percent_encode(&value));
    }
    let req = Request::builder()
        .method(route.method.clone())
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    let req = if route.method == Method::GET {
        let query: Vec<String> = args
            .into_iter()
            .map(|(k, v)| Ok(format!("{k}={}", percent_encode(&scalar(&k, Some(v))?))))
            .collect::<Result<_, ApiError>>()?;
        if !query.is_empty() {
            path = format!("{path}?{}", query.join("&"));
        }
        req.uri(path).body(Body::empty())
    } else {
        let body = match tool.args.iter().find(|a| matches!(a.ty, Ty::Body)) {
            Some(arg) => args.remove(arg.name).unwrap_or_default(),
            None => Value::Object(args),
        };
        req.uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
    };
    req.map_err(|e| ApiError::bad_request(&format!("Bad arguments: {e}.")))
}

/// An argument that goes in the URL.
fn scalar(name: &str, value: Option<Value>) -> Result<String, ApiError> {
    match value {
        Some(Value::String(s)) => Ok(s),
        Some(v @ (Value::Number(_) | Value::Bool(_))) => Ok(v.to_string()),
        _ => Err(ApiError::bad_request(&format!(
            "Bad arguments: `{name}` must be a string, number or boolean."
        ))),
    }
}

/// The route's answer, as the agent reads it.
async fn answer(app: &AppState, req: Request<Body>) -> Result<String, ApiError> {
    let res = routes::router()
        .with_state(app.clone())
        .oneshot(req)
        .await
        .unwrap_or_else(|e| match e {});
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    match (status.is_success(), text.is_empty()) {
        (true, true) => Ok("Done.".into()),
        (true, false) => Ok(text),
        (false, _) => Err(ApiError::Status(status, text)),
    }
}
