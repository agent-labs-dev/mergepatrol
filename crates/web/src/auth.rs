//! Who may use the dashboard.
//!
//! Anyone with a GitHub account can sign in. A user sees the workspaces (installations of the
//! App) that GitHub lets them access, as of their sign-in.
//!
//! Self-hosted, before the GitHub App exists: the server logs a setup token, and whoever has it
//! creates the App (the manifest flow). The hosted App comes from the environment instead.
//!
//! A session lasts 30 days from its last use. It keeps the user's GitHub token, encrypted, and
//! reads their installations from GitHub again once an hour, so a user who loses access to an
//! installation loses its workspace within the hour.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{AppendHeaders, IntoResponse, Redirect, Response};
use mergepatrol_analytics::WorkspaceInfo;
use mergepatrol_github::setup::{self, Pkce, random_token};
use mergepatrol_github::setup::{Revoked, UserInstallation, UserToken};
use mergepatrol_store::{GITHUB_APP, LoginCode, Scope, Session, SessionKind, Workspace};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::{ApiError, AppState};

const SESSION_COOKIE: &str = "mp_session";
/// A sign-in in progress, in the browser that started it: `<state>.<PKCE verifier>`.
const OAUTH_COOKIE: &str = "mp_oauth";
/// The App's creation in progress: its `state`.
const MANIFEST_COOKIE: &str = "mp_manifest";
/// A sign-in for the menu bar app, in the browser that started it:
/// `<port>.<the app's state>.<the app's PKCE challenge>`.
const TRAY_COOKIE: &str = "mp_tray";
/// From the session's last check: an active session does not end.
const USER_SESSION: Duration = Duration::from_secs(30 * 24 * 3600);
/// The session cookie outlives any session: the server's expiry is the one that counts.
const SESSION_COOKIE_AGE: Duration = Duration::from_secs(400 * 24 * 3600);
/// How often a session reads the user's installations from GitHub again.
const RECHECK: i64 = 3600;
/// While GitHub does not answer, a session keeps the installations it read for this long:
/// as long as a session lasted before sessions were checked.
const MAX_STALE: i64 = 8 * 3600;
/// How long one request holds a session it checks. A check that dies lets go after it.
const CHECK_HOLD: i64 = 60;
/// How long the menu bar app has to trade its one-time code.
const LOGIN_CODE: i64 = 120;
const SETUP_SESSION: Duration = Duration::from_secs(3600);
/// GitHub gives the manifest flow one hour.
const MANIFEST_FLOW: Duration = Duration::from_secs(3600);
const LOGIN_FLOW: Duration = Duration::from_secs(600);

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
    /// The installations the user can access, as of their sign-in.
    pub workspaces: Vec<WorkspaceView>,
    /// Product analytics in PostHog. `None` when off, as when self-hosted.
    pub analytics: Option<AnalyticsView>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct AnalyticsView {
    /// The PostHog project key.
    pub key: String,
    pub host: String,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct WorkspaceView {
    pub installation_id: u32,
    pub account: String,
    pub account_type: String,
    pub avatar_url: String,
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
    pub html_url: String,
    /// Install the App on another account: a new workspace.
    pub install_url: String,
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

/// A signed-in user, and the installations they could access when they signed in.
pub(crate) struct SignedIn {
    installations: Vec<u64>,
}

impl SignedIn {
    /// Refuses workspaces the user cannot access.
    pub(crate) fn workspace(&self, installation_id: u64) -> Result<u64, ApiError> {
        if self.installations.contains(&installation_id) {
            Ok(installation_id)
        } else {
            Err(ApiError::Status(
                StatusCode::NOT_FOUND,
                "no such workspace".into(),
            ))
        }
    }
}

impl FromRequestParts<AppState> for SignedIn {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        match current_session(&parts.headers, state).await? {
            Some(Session {
                kind: SessionKind::User { installations, .. },
                ..
            }) => Ok(SignedIn { installations }),
            _ => Err(ApiError::Status(
                StatusCode::UNAUTHORIZED,
                "sign in first".into(),
            )),
        }
    }
}

pub(crate) async fn session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SessionView>, ApiError> {
    let session = current_session(&headers, &state).await?;
    let creds = state.app_credentials().await?;
    let (user, installations) = match &session {
        Some(Session {
            kind:
                SessionKind::User {
                    login,
                    avatar_url,
                    installations,
                },
            ..
        }) => (
            Some(UserView {
                login: login.clone(),
                avatar_url: avatar_url.clone(),
            }),
            installations.clone(),
        ),
        _ => (None, Vec::new()),
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
    let workspaces = state
        .store
        .workspaces(&installations)
        .await?
        .into_iter()
        .map(|w| WorkspaceView {
            installation_id: w.installation_id as u32,
            account: w.account,
            account_type: w.account_type,
            avatar_url: w.avatar_url,
        })
        .collect();
    Ok(Json(SessionView {
        status,
        setup_unlocked: setup_unlocked && creds.is_none(),
        user,
        app: creds.map(|c| AppView {
            install_url: c.install_url(),
            slug: c.slug,
            html_url: c.html_url,
        }),
        workspaces,
        analytics: state.analytics.browser().map(|(key, host)| AnalyticsView {
            key: key.to_owned(),
            host: host.to_owned(),
        }),
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
    let token = create_session(&state, SessionKind::Setup, None, SETUP_SESSION).await?;
    let cookie = cookie(&state, SESSION_COOKIE, &token, SETUP_SESSION);
    Ok((
        AppendHeaders([(header::SET_COOKIE, cookie)]),
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

pub(crate) async fn start_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ManifestRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if !matches!(
        current_session(&headers, &state).await?,
        Some(Session {
            kind: SessionKind::Setup,
            ..
        })
    ) {
        return Err(ApiError::Status(
            StatusCode::UNAUTHORIZED,
            "enter the setup token first".into(),
        ));
    }
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
    // In the browser's cookie, not in this process: a restart, or another instance, takes the
    // callback too.
    let token = random_token(24);
    let cookie = cookie(&state, MANIFEST_COOKIE, &token, MANIFEST_FLOW);
    Ok((
        AppendHeaders([(header::SET_COOKIE, cookie)]),
        Json(ManifestForm {
            action: setup::manifest_form_url(owner, body.organization, &token)?,
            manifest: setup::manifest(&state.public_url, owner).to_string(),
        }),
    ))
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
    headers: HeaderMap,
    Query(q): Query<CodeQuery>,
) -> Response {
    let result = async {
        let (Some(code), Some(token)) = (q.code, q.state) else {
            anyhow::bail!("GitHub did not create the App");
        };
        // The cookie only ties the callback to the browser: whoever calls can set it. The setup
        // session, which the setup token unlocked, is what lets the App be created.
        let setup = current_session(&headers, &state).await?;
        if !matches!(
            setup,
            Some(Session {
                kind: SessionKind::Setup,
                ..
            })
        ) {
            anyhow::bail!("enter the setup token first");
        }
        let started = read_cookie(&headers, MANIFEST_COOKIE);
        if !started.is_some_and(|s| constant_time_eq(s.as_bytes(), token.as_bytes())) {
            anyhow::bail!("this setup link expired, or started in another browser: start again");
        }
        if state.hosted_app.is_some() {
            anyhow::bail!("this Mergepatrol uses the hosted GitHub App");
        }
        if state.app_credentials().await?.is_some() {
            anyhow::bail!("the GitHub App already exists");
        }
        let creds = setup::convert_manifest(&code).await?;
        state
            .vault
            .set(Scope::Instance, GITHUB_APP, &serde_json::to_string(&creds)?)
            .await?;
        *state.setup_token.lock().unwrap() = None;
        tracing::info!(app = %creds.slug, "GitHub App created");
        Ok(creds.install_url())
    }
    .await;
    match result {
        Ok(install) => (
            AppendHeaders([(
                header::SET_COOKIE,
                cookie(&state, MANIFEST_COOKIE, "", Duration::ZERO),
            )]),
            Redirect::to(&install),
        )
            .into_response(),
        Err(e) => error_redirect(&format!("{e:#}")),
    }
}

/// GitHub sends the user here after they install the App. Signing in again adds the new
/// workspace to the session.
pub(crate) async fn installed() -> Redirect {
    Redirect::to("/auth/login")
}

/// The menu bar app signs in through the browser (RFC 8252): it listens on a loopback port and
/// passes it with a `state` and a PKCE challenge of its own. The browser comes back to the app
/// with a one-time code, which only the app, with the verifier, can trade for the session.
#[derive(Deserialize)]
pub(crate) struct LoginQuery {
    tray_port: Option<u16>,
    tray_state: Option<String>,
    tray_challenge: Option<String>,
}

pub(crate) async fn login(State(state): State<AppState>, Query(q): Query<LoginQuery>) -> Response {
    let tray = match (q.tray_port, q.tray_state, q.tray_challenge) {
        (None, None, None) => None,
        (Some(port), Some(app_state), Some(challenge))
            if valid_tray_state(&app_state) && valid_challenge(&challenge) =>
        {
            Some(format!("{port}.{app_state}.{challenge}"))
        }
        _ => return error_redirect("the menu bar app's sign-in link is malformed"),
    };
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
    // The cookie ties the callback to this browser, so nobody can sign you in as them. It holds
    // the verifier too, so the sign-in does not depend on this process: a restart, or another
    // instance, takes the callback.
    let value = format!("{token}.{}", pkce.verifier);
    let oauth = cookie(&state, OAUTH_COOKIE, &value, LOGIN_FLOW);
    // A browser sign-in clears a tray sign-in left over from before.
    let tray = match tray {
        Some(tray) => cookie(&state, TRAY_COOKIE, &tray, LOGIN_FLOW),
        None => cookie(&state, TRAY_COOKIE, "", Duration::ZERO),
    };
    (
        AppendHeaders([(header::SET_COOKIE, oauth), (header::SET_COOKIE, tray)]),
        Redirect::to(&url),
    )
        .into_response()
}

/// The app's own random `state`, which it checks on the way back.
fn valid_tray_state(s: &str) -> bool {
    (16..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// An S256 challenge: 32 bytes in unpadded base64url.
fn valid_challenge(s: &str) -> bool {
    s.len() == 43
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(crate) async fn login_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CodeQuery>,
) -> Response {
    let tray = read_cookie(&headers, TRAY_COOKIE).and_then(|v| {
        let mut parts = v.splitn(3, '.');
        let port = parts.next()?.parse::<u16>().ok()?;
        Some((port, parts.next()?.to_owned(), parts.next()?.to_owned()))
    });
    let result = async {
        if let Some(err) = q.error {
            anyhow::bail!("GitHub sign-in failed: {err}");
        }
        let (Some(code), Some(token)) = (q.code, q.state) else {
            anyhow::bail!("GitHub sign-in failed");
        };
        let started = read_cookie(&headers, OAUTH_COOKIE);
        let Some((started, verifier)) = started.as_deref().and_then(|v| v.split_once('.')) else {
            anyhow::bail!("this sign-in link expired, or started in another browser: try again");
        };
        if !constant_time_eq(started.as_bytes(), token.as_bytes()) {
            anyhow::bail!("this sign-in started in another browser: try again");
        }
        let creds = state
            .app_credentials()
            .await?
            .ok_or_else(|| anyhow::anyhow!("no GitHub App yet"))?;
        let user_token =
            setup::exchange_code(&creds, &code, &callback_url(&state), verifier).await?;
        let user = setup::user(&user_token.access_token).await?;
        let installations = setup::user_installations(&user_token.access_token).await?;
        save_workspaces(&state, &installations).await?;
        tracing::info!(login = %user.login, workspaces = installations.len(), "signed in");
        state.analytics.signed_in(
            &user.login,
            &user.avatar_url,
            &installations
                .iter()
                .map(|i| WorkspaceInfo {
                    installation_id: i.id,
                    account: &i.account,
                    account_type: &i.account_type,
                })
                .collect::<Vec<_>>(),
        );
        let token = create_session(
            &state,
            SessionKind::User {
                login: user.login,
                avatar_url: user.avatar_url,
                installations: installations.iter().map(|i| i.id).collect(),
            },
            Some(&user_token),
            USER_SESSION,
        )
        .await?;
        match &tray {
            Some((_, _, challenge)) => login_code(&state, &token, challenge).await,
            None => Ok(token),
        }
    }
    .await;
    let clear_oauth = cookie(&state, OAUTH_COOKIE, "", Duration::ZERO);
    match (result, tray) {
        // The session goes to the menu bar app, not to this browser: the browser carries only a
        // one-time code to it.
        (result, Some((port, app_state, _))) => {
            let outcome = match result {
                Ok(code) => format!("code={code}"),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "menu bar sign-in failed");
                    format!("error={}", percent_encode(&format!("{e:#}")))
                }
            };
            (
                AppendHeaders([
                    (header::SET_COOKIE, clear_oauth),
                    (
                        header::SET_COOKIE,
                        cookie(&state, TRAY_COOKIE, "", Duration::ZERO),
                    ),
                ]),
                Redirect::to(&format!(
                    "http://127.0.0.1:{port}/callback?state={app_state}&{outcome}"
                )),
            )
                .into_response()
        }
        (Ok(token), None) => (
            AppendHeaders([
                (
                    header::SET_COOKIE,
                    cookie(&state, SESSION_COOKIE, &token, SESSION_COOKIE_AGE),
                ),
                (header::SET_COOKIE, clear_oauth),
            ]),
            Redirect::to("/"),
        )
            .into_response(),
        (Err(e), None) => error_redirect(&format!("{e:#}")),
    }
}

/// Seals the new session's token under a one-time code for the menu bar app.
async fn login_code(state: &AppState, token: &str, challenge: &str) -> anyhow::Result<String> {
    let code = random_token(32);
    let code_hash = hash(&code);
    let login = LoginCode {
        token_hash: hash(token),
        challenge: challenge.to_owned(),
        token: state.vault.seal(&code_secret(&code_hash), token)?,
        expires_at: unix_now() + LOGIN_CODE,
    };
    state.store.create_login_code(&code_hash, &login).await?;
    Ok(code)
}

#[derive(Deserialize)]
pub(crate) struct TokenRequest {
    code: String,
    verifier: String,
}

#[derive(Serialize)]
pub(crate) struct TokenResponse {
    token: String,
}

/// The menu bar app trades its one-time code and PKCE verifier for the session token.
pub(crate) async fn tray_token(
    State(state): State<AppState>,
    Json(body): Json<TokenRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    let code_hash = hash(&body.code);
    let refused = || {
        ApiError::Status(
            StatusCode::BAD_REQUEST,
            "This sign-in expired, or was used.".into(),
        )
    };
    let Some(code) = state.store.take_login_code(&code_hash, unix_now()).await? else {
        return Err(refused());
    };
    if !constant_time_eq(
        Pkce::challenge(&body.verifier).as_bytes(),
        code.challenge.as_bytes(),
    ) {
        // Whoever has the code is not the app that asked: the session it hands over goes.
        state.store.delete_session(&code.token_hash).await?;
        return Err(refused());
    }
    let token = state.vault.open(&code_secret(&code_hash), &code.token)?;
    Ok(Json(TokenResponse {
        token: token.expose_secret().to_owned(),
    }))
}

pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = session_token(&headers) {
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
    let Some(token) = session_token(headers) else {
        return Ok(None);
    };
    let token_hash = hash(&token);
    let Some(session) = state.store.session(&token_hash).await? else {
        return Ok(None);
    };
    let now = unix_now();
    if !check_due(&session, now) {
        return Ok(Some(session));
    }
    // Another request is checking it: this one goes on with what the session has.
    if !state
        .store
        .claim_session_check(&token_hash, now, now + CHECK_HOLD)
        .await?
    {
        return still_valid(session, now);
    }
    // A check that finished since this request read the session spent its refresh token: start
    // from what that check saved.
    let Some(session) = state.store.session(&token_hash).await? else {
        return Ok(None);
    };
    if !check_due(&session, now) {
        return Ok(Some(session));
    }
    match recheck(state, &token_hash, &session, now).await {
        Ok(checked) => Ok(Some(checked)),
        Err(e) if e.downcast_ref::<Revoked>().is_some() => {
            tracing::info!("a session ended: GitHub no longer takes its token");
            state.store.delete_session(&token_hash).await?;
            Ok(None)
        }
        Err(e) => {
            // The hold runs out, and a later request tries again.
            tracing::warn!(error = %format!("{e:#}"), "could not check a session with GitHub");
            still_valid(session, now)
        }
    }
}

/// A user session that keeps its GitHub token, an hour after its last check. A session from
/// before sessions kept it lasts out its 8 hours as it is.
fn check_due(session: &Session, now: i64) -> bool {
    matches!(session.kind, SessionKind::User { .. })
        && session.github.is_some()
        && now - session.checked_at >= RECHECK
}

/// A session not checked lately still counts, unless its installations are too old to trust.
fn still_valid(session: Session, now: i64) -> anyhow::Result<Option<Session>> {
    if now - session.checked_at < MAX_STALE {
        Ok(Some(session))
    } else {
        anyhow::bail!("could not check the session with GitHub for 8 hours")
    }
}

/// Reads the user's installations from GitHub again, refreshing their token first if it
/// expired, and saves the session with them for another 30 days. `Revoked` when GitHub no
/// longer takes the token.
async fn recheck(
    state: &AppState,
    token_hash: &str,
    session: &Session,
    now: i64,
) -> anyhow::Result<Session> {
    let (
        SessionKind::User {
            login, avatar_url, ..
        },
        Some(sealed),
    ) = (&session.kind, &session.github)
    else {
        anyhow::bail!("only a user session with a GitHub token is checked");
    };
    let name = session_secret(token_hash);
    // A key rotation that dropped the key it was sealed with ends the session.
    let Ok(json) = state.vault.open(&name, sealed) else {
        return Err(Revoked.into());
    };
    let mut token: UserToken = serde_json::from_str(json.expose_secret())?;
    let mut checked = session.clone();
    if token.expired(now) {
        let Some(refresh) = &token.refresh_token else {
            return Err(Revoked.into());
        };
        let creds = state
            .app_credentials()
            .await?
            .ok_or_else(|| anyhow::anyhow!("no GitHub App yet"))?;
        token = setup::refresh_user_token(&creds, refresh).await?;
        // GitHub spent the old refresh token: keep the new one before anything else can fail.
        // The hold stays, so no other request checks with the old one meanwhile.
        checked.github = Some(state.vault.seal(&name, &serde_json::to_string(&token)?)?);
        state.store.update_session(token_hash, &checked).await?;
    }
    let installations = setup::user_installations(&token.access_token).await?;
    save_workspaces(state, &installations).await?;
    checked.kind = SessionKind::User {
        login: login.clone(),
        avatar_url: avatar_url.clone(),
        installations: installations.iter().map(|i| i.id).collect(),
    };
    checked.checked_at = now;
    checked.expires_at = now + USER_SESSION.as_secs() as i64;
    // Sealed again each time, so a key rotation reaches every session in use.
    checked.github = Some(state.vault.seal(&name, &serde_json::to_string(&token)?)?);
    state.store.update_session(token_hash, &checked).await?;
    Ok(checked)
}

async fn save_workspaces(
    state: &AppState,
    installations: &[UserInstallation],
) -> anyhow::Result<()> {
    state
        .store
        .save_workspaces(
            &installations
                .iter()
                .map(|i| Workspace {
                    installation_id: i.id,
                    account: i.account.clone(),
                    account_type: i.account_type.clone(),
                    avatar_url: i.avatar_url.clone(),
                })
                .collect::<Vec<_>>(),
        )
        .await
}

/// Where a session's GitHub token is sealed: it opens for this session only.
fn session_secret(token_hash: &str) -> String {
    format!("session/{token_hash}")
}

fn code_secret(code_hash: &str) -> String {
    format!("login-code/{code_hash}")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The browser's cookie, or the menu bar app's `Authorization: Bearer` header.
fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .or_else(|| read_cookie(headers, SESSION_COOKIE))
}

/// Stores a new session and returns its token. A user session keeps their GitHub token.
async fn create_session(
    state: &AppState,
    kind: SessionKind,
    github: Option<&UserToken>,
    ttl: Duration,
) -> anyhow::Result<String> {
    let token = random_token(32);
    let token_hash = hash(&token);
    let now = unix_now();
    let github = github
        .map(|g| {
            state
                .vault
                .seal(&session_secret(&token_hash), &serde_json::to_string(g)?)
        })
        .transpose()?;
    let session = Session {
        kind,
        expires_at: now + ttl.as_secs() as i64,
        checked_at: now,
        github,
    };
    state.store.create_session(&token_hash, &session).await?;
    Ok(token)
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

pub(crate) fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
    fn trusts_unchecked_installations_for_8_hours() {
        let session = |checked_at| Session {
            kind: SessionKind::User {
                login: "ada".into(),
                avatar_url: String::new(),
                installations: vec![1],
            },
            expires_at: i64::MAX,
            checked_at,
            github: Some(mergepatrol_store::Sealed {
                nonce: Vec::new(),
                ciphertext: Vec::new(),
            }),
        };
        let now = 100_000;
        assert!(!check_due(&session(now - RECHECK + 1), now));
        assert!(check_due(&session(now - RECHECK), now));
        assert!(
            still_valid(session(now - MAX_STALE + 1), now)
                .unwrap()
                .is_some()
        );
        assert!(still_valid(session(now - MAX_STALE), now).is_err());
    }

    #[test]
    fn prefers_the_bearer_header() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "mp_session=cookie".parse().unwrap());
        assert_eq!(session_token(&h).as_deref(), Some("cookie"));
        h.insert(header::AUTHORIZATION, "Bearer tray".parse().unwrap());
        assert_eq!(session_token(&h).as_deref(), Some("tray"));
    }

    #[test]
    fn takes_only_a_long_alphanumeric_tray_state() {
        assert!(valid_tray_state(&"a1".repeat(16)));
        assert!(!valid_tray_state("short"));
        assert!(!valid_tray_state(&format!("{}&x=y", "a".repeat(16))));
    }

    #[test]
    fn encodes_errors_for_the_query() {
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
    }
}
