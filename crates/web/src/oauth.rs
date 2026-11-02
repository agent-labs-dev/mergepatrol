//! OAuth for MCP clients, as the MCP spec's authorization asks: MergeLoop is the authorization
//! server, and signs the user in with GitHub. The access token is a MergeLoop session, as the
//! menu bar app's is, so the user's GitHub token stays on the server.
//!
//! Clients register themselves (RFC 7591), and must redirect to a loopback address (RFC 8252):
//! the code never leaves the user's machine, so no consent screen has to guard it.

use axum::extract::{Query, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Json};
use mergepatrol_github::setup::random_token;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::auth::{self, McpClient};

/// The MCP server's metadata (RFC 9728): who issues its tokens.
pub(crate) async fn resource(State(app): State<AppState>) -> Json<Value> {
    Json(json!({
        "resource": format!("{}/mcp", app.public_url),
        "authorization_servers": [&*app.public_url],
        "bearer_methods_supported": ["header"],
    }))
}

/// The authorization server's metadata (RFC 8414).
pub(crate) async fn server(State(app): State<AppState>) -> Json<Value> {
    let url = &app.public_url;
    Json(json!({
        "issuer": &**url,
        "authorization_endpoint": format!("{url}/oauth/authorize"),
        "token_endpoint": format!("{url}/oauth/token"),
        "registration_endpoint": format!("{url}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    }))
}

#[derive(Deserialize)]
pub(crate) struct Registration {
    #[serde(default)]
    redirect_uris: Vec<String>,
    client_name: Option<String>,
}

/// Registers a client (RFC 7591). Nothing is kept: a client is only its loopback redirect,
/// which `authorize` checks again.
pub(crate) async fn register(Json(r): Json<Registration>) -> Response {
    if r.redirect_uris.is_empty() || !r.redirect_uris.iter().all(|u| loopback(u)) {
        return oauth_error(
            "invalid_redirect_uri",
            "MergeLoop redirects only to a loopback address, such as http://127.0.0.1:<port>/callback.",
        );
    }
    let client = json!({
        "client_id": random_token(16),
        "client_name": r.client_name,
        "redirect_uris": r.redirect_uris,
        "token_endpoint_auth_method": "none",
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
    });
    (StatusCode::CREATED, Json(client)).into_response()
}

#[derive(Deserialize)]
pub(crate) struct AuthorizeQuery {
    response_type: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
}

pub(crate) async fn authorize(
    State(app): State<AppState>,
    Query(q): Query<AuthorizeQuery>,
) -> Response {
    // Without a redirect to trust, the error is the browser's to show.
    let Some(redirect_uri) = q.redirect_uri.filter(|u| loopback(u)) else {
        return (
            StatusCode::BAD_REQUEST,
            "This sign-in link has no loopback redirect_uri.",
        )
            .into_response();
    };
    let client = McpClient {
        redirect_uri,
        state: q.state,
        challenge: q.code_challenge.unwrap_or_default(),
    };
    let valid = q.response_type.as_deref() == Some("code")
        && q.code_challenge_method.as_deref() == Some("S256")
        && auth::valid_challenge(&client.challenge);
    if !valid {
        let mut url = format!(
            "{}{}error=invalid_request&error_description={}",
            client.redirect_uri,
            if client.redirect_uri.contains('?') {
                '&'
            } else {
                '?'
            },
            "response_type%20code%20with%20an%20S256%20code_challenge"
        );
        if let Some(state) = &client.state {
            url.push_str(&format!("&state={}", auth::percent_encode(state)));
        }
        return Redirect::to(&url).into_response();
    }
    auth::mcp_login(&app, &client).await
}

#[derive(Deserialize)]
pub(crate) struct TokenForm {
    grant_type: String,
    code: Option<String>,
    code_verifier: Option<String>,
}

/// Trades the one-time code and the PKCE verifier for the session token.
pub(crate) async fn token(State(app): State<AppState>, Form(f): Form<TokenForm>) -> Response {
    if f.grant_type != "authorization_code" {
        return oauth_error("unsupported_grant_type", "Only authorization_code.");
    }
    let (Some(code), Some(verifier)) = (f.code, f.code_verifier) else {
        return oauth_error("invalid_request", "code and code_verifier are required.");
    };
    match auth::redeem(&app, &code, &verifier).await {
        Ok(Some(token)) => (
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({ "access_token": token, "token_type": "Bearer" })),
        )
            .into_response(),
        Ok(None) => oauth_error("invalid_grant", "This sign-in expired, or was used."),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "MCP token error");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        }
    }
}

fn oauth_error(error: &str, description: &str) -> Response {
    let body = json!({ "error": error, "error_description": description });
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// `http` to this machine: what a native app listens on. No fragment, which would hide the
/// code from it.
pub(crate) fn loopback(uri: &str) -> bool {
    if uri.contains('#') {
        return false;
    }
    let Ok(uri) = uri.parse::<Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("http")
        && matches!(uri.host(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_only_to_this_machine() {
        assert!(loopback("http://127.0.0.1:33418/callback"));
        assert!(loopback("http://localhost:6274/oauth/callback?x=1"));
        assert!(loopback("http://[::1]:8080/cb"));
        assert!(!loopback("https://127.0.0.1/cb"));
        assert!(!loopback("http://127.0.0.1.evil.com/cb"));
        assert!(!loopback("http://localhost@evil.com/cb"));
        assert!(!loopback("https://claude.ai/api/mcp/auth_callback"));
        assert!(!loopback("/callback"));
        assert!(!loopback("http://127.0.0.1:9/cb#x"));
    }
}
