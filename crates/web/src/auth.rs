//! Who may use the dashboard.
//!
//! First run: the server logs a setup token. Whoever has it creates the GitHub App (the manifest
//! flow) and installs it. From then on, users sign in with GitHub, and anyone who can access an
//! installation of the App on its owner account gets in.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{AppendHeaders, IntoResponse, Redirect, Response};
use mergepatrol_github::App;
use mergepatrol_github::setup::{self, Pkce, random_token};
use mergepatrol_store::{GITHUB_APP, Session, SessionKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::{ApiError, AppState};

const SESSION_COOKIE: &str = "mp_session";
const OAUTH_COOKIE: &str = "mp_oauth";
const USER_SESSION: Duration = Duration::from_secs(7 * 24 * 3600);
const SETUP_SESSION: Duration = Duration::from_secs(3600);
/// GitHub gives the manifest flow one hour.
const PENDING: Duration = Duration::from_secs(3600);

pub(crate) enum Pending {
    Manifest { expires: Instant },
    Login { verifier: String, expires: Instant },
}

impl Pending {
    fn expired(&self) -> bool {
        match self {
            Self::Manifest { expires } | Self::Login { expires, .. } => *expires < Instant::now(),
        }
    }
}

#[derive(Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SessionStatus {
    /// No GitHub App yet. The setup token unlocks the setup.
    NeedsSetup,
    SignedOut,
    SignedIn,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct SessionView {
    pub status: SessionStatus,
    /// The setup token was entered: the GitHub App can be created now.
    pub setup_unlocked: bool,
    pub user: Option<UserView>,
    pub app: Option<AppView>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct UserView {
    pub login: String,
    pub avatar_url: String,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct AppView {
    pub slug: String,
    pub owner: String,
    pub html_url: String,
    pub install_url: String,
    /// The App has at least one installation. Nobody can sign in before that.
    pub installed: bool,
}

#[derive(Deserialize, TS)]
#[ts(export)]
pub struct Unlock {
    pub token: String,
}

#[derive(Deserialize, TS)]
#[ts(export)]
pub struct ManifestRequest {
    /// The GitHub account that will own the App.
    pub owner: String,
    pub organization: bool,
}

/// The browser posts `manifest` to `action` as a form: GitHub's manifest flow needs a real
/// form post from the user's browser.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct ManifestForm {
    pub action: String,
    pub manifest: String,
}

/// A request from someone who may use the dashboard: a signed-in user, or, before the GitHub
/// App exists, the holder of the setup token.
pub(crate) struct Admin;

impl FromRequestParts<AppState> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let unauthorized = || ApiError::Status(StatusCode::UNAUTHORIZED, "sign in first".into());
        let session = current_session(&parts.headers, state)
            .await?
            .ok_or_else(unauthorized)?;
        match session.kind {
            SessionKind::User { .. } => Ok(Admin),
            SessionKind::Setup if state.app_credentials().await?.is_none() => Ok(Admin),
            SessionKind::Setup => Err(unauthorized()),
        }
    }
}

pub(crate) async fn session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SessionView>, ApiError> {
    let session = current_session(&headers, &state).await?;
    let creds = state.app_credentials().await?;
    let user = match &session {
        Some(Session {
            kind: SessionKind::User { login, avatar_url },
            ..
        }) => Some(UserView {
            login: login.clone(),
            avatar_url: avatar_url.clone(),
        }),
        _ => None,
    };
    let setup_unlocked = matches!(
        &session,
        Some(Session {
            kind: SessionKind::Setup,
            ..
        })
    );
    let status = match (&creds, &user) {
        (None, _) => SessionStatus::NeedsSetup,
        (Some(_), Some(_)) => SessionStatus::SignedIn,
        (Some(_), None) => SessionStatus::SignedOut,
    };
    let app = match creds {
        None => None,
        Some(c) => {
            let installed = match App::from_credentials(&c)?.installations().await {
                Ok(list) => !list.is_empty(),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "could not list installations");
                    false
                }
            };
            Some(AppView {
                install_url: c.install_url(),
                slug: c.slug,
                owner: c.owner,
                html_url: c.html_url,
                installed,
            })
        }
    };
    Ok(Json(SessionView {
        status,
        setup_unlocked: setup_unlocked && app.is_none(),
        user,
        app,
    }))
}

pub(crate) async fn unlock(
    State(state): State<AppState>,
    Json(body): Json<Unlock>,
) -> Result<Response, ApiError> {
    let expected = state.setup_token.lock().unwrap().clone();
    let Some(expected) = expected else {
        return Err(ApiError::Status(
            StatusCode::CONFLICT,
            "Setup is done. Sign in with GitHub.".into(),
        ));
    };
    if !constant_time_eq(body.token.trim().as_bytes(), expected.as_bytes()) {
        return Err(ApiError::Status(
            StatusCode::FORBIDDEN,
            "That is not the setup token.".into(),
        ));
    }
    let cookie = create_session(&state, SessionKind::Setup, SETUP_SESSION).await?;
    Ok((
        AppendHeaders([(header::SET_COOKIE, cookie)]),
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

pub(crate) async fn start_manifest(
    State(state): State<AppState>,
    _admin: Admin,
    Json(body): Json<ManifestRequest>,
) -> Result<Json<ManifestForm>, ApiError> {
    if state.app_credentials().await?.is_some() {
        return Err(ApiError::Status(
            StatusCode::CONFLICT,
            "The GitHub App already exists.".into(),
        ));
    }
    let owner = body.owner.trim();
    if owner.is_empty() || !owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(ApiError::bad_request(
            "the owner must be a GitHub account name",
        ));
    }
    let token = random_token(24);
    remember(
        &state,
        token.clone(),
        Pending::Manifest {
            expires: Instant::now() + PENDING,
        },
    );
    Ok(Json(ManifestForm {
        action: setup::manifest_form_url(owner, body.organization, &token)?,
        manifest: setup::manifest(&state.public_url, owner).to_string(),
    }))
}

#[derive(Deserialize)]
pub(crate) struct CodeQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// GitHub created the App. Store its credentials, then send the user to install it.
pub(crate) async fn manifest_callback(
    State(state): State<AppState>,
    Query(q): Query<CodeQuery>,
) -> Response {
    let result = async {
        let (Some(code), Some(token)) = (q.code, q.state) else {
            anyhow::bail!("GitHub did not create the App");
        };
        if !matches!(take(&state, &token), Some(Pending::Manifest { .. })) {
            anyhow::bail!("this setup link expired: start again");
        }
        let creds = setup::convert_manifest(&code).await?;
        state
            .vault
            .set(GITHUB_APP, &serde_json::to_string(&creds)?)
            .await?;
        *state.setup_token.lock().unwrap() = None;
        tracing::info!(app = %creds.slug, owner = %creds.owner, "GitHub App created");
        Ok(creds.install_url())
    }
    .await;
    match result {
        Ok(install) => Redirect::to(&install).into_response(),
        Err(e) => error_redirect(&format!("{e:#}")),
    }
}

/// GitHub sends the user here after they install the App.
pub(crate) async fn installed() -> Redirect {
    Redirect::to("/auth/login")
}

pub(crate) async fn login(State(state): State<AppState>) -> Response {
    let creds = match state.app_credentials().await {
        Ok(Some(c)) => c,
        Ok(None) => return Redirect::to("/").into_response(),
        Err(e) => return error_redirect(&format!("{e:#}")),
    };
    let token = random_token(24);
    let pkce = Pkce::new();
    let url = match setup::authorize_url(&creds, &callback_url(&state), &token, &pkce.challenge) {
        Ok(u) => u,
        Err(e) => return error_redirect(&format!("{e:#}")),
    };
    remember(
        &state,
        token.clone(),
        Pending::Login {
            verifier: pkce.verifier,
            expires: Instant::now() + PENDING,
        },
    );
    // The cookie ties the callback to this browser, so nobody can sign you in as them.
    let cookie = cookie(&state, OAUTH_COOKIE, &token, Duration::from_secs(600));
    (
        AppendHeaders([(header::SET_COOKIE, cookie)]),
        Redirect::to(&url),
    )
        .into_response()
}

pub(crate) async fn login_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CodeQuery>,
) -> Response {
    let result = async {
        if let Some(err) = q.error {
            anyhow::bail!("GitHub sign-in failed: {err}");
        }
        let (Some(code), Some(token)) = (q.code, q.state) else {
            anyhow::bail!("GitHub sign-in failed");
        };
        if read_cookie(&headers, OAUTH_COOKIE).as_deref() != Some(token.as_str()) {
            anyhow::bail!("this sign-in started in another browser: try again");
        }
        let Some(Pending::Login { verifier, .. }) = take(&state, &token) else {
            anyhow::bail!("this sign-in link expired: try again");
        };
        let creds = state
            .app_credentials()
            .await?
            .ok_or_else(|| anyhow::anyhow!("no GitHub App yet"))?;
        let user_token =
            setup::exchange_code(&creds, &code, &callback_url(&state), &verifier).await?;
        let user = setup::user(&user_token).await?;
        if !setup::user_can_access(&user_token, &creds.owner).await? {
            anyhow::bail!(
                "{} cannot access the {} installation of {}",
                user.login,
                creds.owner,
                creds.slug
            );
        }
        tracing::info!(login = %user.login, "signed in");
        create_session(
            &state,
            SessionKind::User {
                login: user.login,
                avatar_url: user.avatar_url,
            },
            USER_SESSION,
        )
        .await
    }
    .await;
    match result {
        Ok(session_cookie) => (
            AppendHeaders([
                (header::SET_COOKIE, session_cookie),
                (
                    header::SET_COOKIE,
                    cookie(&state, OAUTH_COOKIE, "", Duration::ZERO),
                ),
            ]),
            Redirect::to("/"),
        )
            .into_response(),
        Err(e) => error_redirect(&format!("{e:#}")),
    }
}

pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = read_cookie(&headers, SESSION_COOKIE) {
        state.store.delete_session(&hash(&token)).await?;
    }
    let clear = cookie(&state, SESSION_COOKIE, "", Duration::ZERO);
    Ok((
        AppendHeaders([(header::SET_COOKIE, clear)]),
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

async fn current_session(headers: &HeaderMap, state: &AppState) -> anyhow::Result<Option<Session>> {
    match read_cookie(headers, SESSION_COOKIE) {
        Some(token) => state.store.session(&hash(&token)).await,
        None => Ok(None),
    }
}

/// Stores a new session and returns its `Set-Cookie` value.
async fn create_session(
    state: &AppState,
    kind: SessionKind,
    ttl: Duration,
) -> anyhow::Result<String> {
    let token = random_token(32);
    let expires_at = (SystemTime::now() + ttl)
        .duration_since(UNIX_EPOCH)?
        .as_secs() as i64;
    state
        .store
        .create_session(&hash(&token), &Session { kind, expires_at })
        .await?;
    Ok(cookie(state, SESSION_COOKIE, &token, ttl))
}

fn cookie(state: &AppState, name: &str, value: &str, max_age: Duration) -> String {
    let secure = if state.secure_cookies() {
        "; Secure"
    } else {
        ""
    };
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        max_age.as_secs()
    )
}

fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
        .filter(|v| !v.is_empty())
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn remember(state: &AppState, token: String, pending: Pending) {
    let mut map = state.pending.lock().unwrap();
    map.retain(|_, p| !p.expired());
    map.insert(token, pending);
}

fn take(state: &AppState, token: &str) -> Option<Pending> {
    state
        .pending
        .lock()
        .unwrap()
        .remove(token)
        .filter(|p| !p.expired())
}

fn callback_url(state: &AppState) -> String {
    format!("{}/auth/callback", state.public_url)
}

/// Back to the dashboard, which shows `error`.
fn error_redirect(error: &str) -> Response {
    tracing::warn!(%error, "sign-in or setup failed");
    let url = percent_encode(error);
    Redirect::to(&format!("/?error={url}")).into_response()
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_right_cookie() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "a=1; mp_session=tok; b=2".parse().unwrap());
        assert_eq!(read_cookie(&h, SESSION_COOKIE).as_deref(), Some("tok"));
        assert_eq!(read_cookie(&h, OAUTH_COOKIE), None);
    }

    #[test]
    fn encodes_errors_for_the_query() {
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
    }
}
