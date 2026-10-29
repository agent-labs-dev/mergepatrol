//! Signs a workspace in to its ChatGPT plan, so Codex runs on the plan instead of an API key.
//!
//! OpenAI has no sign-in for third parties that a hosted app can use yet, so this is the Codex
//! CLI's own device-code login, with its OAuth client: the user enters a code at
//! auth.openai.com, and the tokens work against the backend Codex calls with a ChatGPT login.
//! OpenAI can change or close any of it; a workspace can always fall back to an API key. Mirrors
//! `codex-rs/login` in github.com/openai/codex.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The Codex CLI's OAuth client.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// How long a device code works, as the Codex CLI says.
const DEVICE_CODE_TTL: Duration = Duration::from_secs(15 * 60);
/// The claim OpenAI's tokens keep the ChatGPT account in.
const AUTH_CLAIM: &str = "https://api.openai.com/auth";

/// OpenAI's sign-in. Another issuer in tests.
#[derive(Clone)]
pub struct ChatGptAuth {
    issuer: String,
    http: reqwest::Client,
}

impl Default for ChatGptAuth {
    fn default() -> Self {
        Self::new("https://auth.openai.com")
    }
}

/// A sign-in the user has yet to finish: they enter `user_code` at `verification_url`.
#[derive(Serialize, Deserialize)]
pub struct DeviceLogin {
    device_auth_id: String,
    pub user_code: String,
    pub verification_url: String,
    /// How often to ask whether the user is done.
    pub interval_secs: u64,
    /// Unix seconds.
    pub expires_at: u64,
}

/// A workspace's ChatGPT login. Secret: kept in the vault, as JSON.
#[derive(Serialize, Deserialize)]
pub struct ChatGptLogin {
    pub access_token: String,
    refresh_token: String,
    /// The ChatGPT account, or workspace, whose plan the requests use.
    pub account_id: String,
    pub email: Option<String>,
    /// Unix seconds, when the access token expires.
    pub expires_at: u64,
}

impl ChatGptLogin {
    /// What the dashboard shows of it: the account's email.
    pub fn hint(&self) -> String {
        self.email
            .clone()
            .unwrap_or_else(|| self.account_id.clone())
    }

    /// Whether the access token stops working within `d`.
    pub fn expires_within(&self, d: Duration) -> bool {
        now() + d.as_secs() >= self.expires_at
    }

    /// From the token endpoint's answer. A refresh may leave out the ID token, and with it the
    /// account: `was` is the login it refreshes.
    fn from_tokens(tokens: Tokens, was: Option<&ChatGptLogin>) -> Result<Self> {
        let access = claims(&tokens.access_token)?;
        let id = tokens.id_token.as_deref().map(claims).transpose()?;
        let account_id = [id.as_ref(), Some(&access)]
            .into_iter()
            .flatten()
            .find_map(|c| c[AUTH_CLAIM]["chatgpt_account_id"].as_str())
            .map(str::to_owned)
            .or_else(|| was.map(|w| w.account_id.clone()))
            .context("the ChatGPT login has no account")?;
        Ok(Self {
            expires_at: access["exp"]
                .as_u64()
                .context("the ChatGPT access token has no expiry")?,
            access_token: tokens.access_token,
            refresh_token: tokens
                .refresh_token
                .or_else(|| was.map(|w| w.refresh_token.clone()))
                .context("the ChatGPT login has no refresh token")?,
            account_id,
            email: id
                .as_ref()
                .and_then(|c| c["email"].as_str())
                .map(str::to_owned)
                .or_else(|| was.and_then(|w| w.email.clone())),
        })
    }
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

impl ChatGptAuth {
    pub fn new(issuer: &str) -> Self {
        Self {
            issuer: issuer.trim_end_matches('/').to_owned(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                // Never send a code or a token on to somewhere else.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("a reqwest client"),
        }
    }

    /// Starts a sign-in.
    pub async fn start(&self) -> Result<DeviceLogin> {
        #[derive(Deserialize)]
        struct UserCode {
            device_auth_id: String,
            #[serde(alias = "usercode")]
            user_code: String,
            /// A number, as a string.
            interval: Option<String>,
        }
        let res = self
            .http
            .post(format!("{}/api/accounts/deviceauth/usercode", self.issuer))
            .json(&serde_json::json!({ "client_id": CLIENT_ID }))
            .send()
            .await
            .context("could not reach OpenAI's sign-in")?;
        let status = res.status();
        if !status.is_success() {
            bail!("OpenAI's sign-in answered {status}");
        }
        let code: UserCode = res.json().await.context("OpenAI's sign-in")?;
        Ok(DeviceLogin {
            device_auth_id: code.device_auth_id,
            user_code: code.user_code,
            verification_url: format!("{}/codex/device", self.issuer),
            interval_secs: code
                .interval
                .and_then(|i| i.trim().parse().ok())
                .unwrap_or(5),
            expires_at: now() + DEVICE_CODE_TTL.as_secs(),
        })
    }

    /// The login, once the user has entered the code. `None` while they have not.
    pub async fn poll(&self, device: &DeviceLogin) -> Result<Option<ChatGptLogin>> {
        #[derive(Deserialize)]
        struct Code {
            authorization_code: String,
            code_verifier: String,
        }
        if now() >= device.expires_at {
            bail!("the code expired: sign in again");
        }
        let res = self
            .http
            .post(format!("{}/api/accounts/deviceauth/token", self.issuer))
            .json(&serde_json::json!({
                "device_auth_id": device.device_auth_id,
                "user_code": device.user_code,
            }))
            .send()
            .await
            .context("could not reach OpenAI's sign-in")?;
        let status = res.status();
        // What it answers until the user has entered the code.
        if matches!(status.as_u16(), 403 | 404) {
            return Ok(None);
        }
        if !status.is_success() {
            bail!("OpenAI's sign-in answered {status}");
        }
        let code: Code = res.json().await.context("OpenAI's sign-in")?;
        let tokens = self
            .token(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", &code.authorization_code),
                (
                    "redirect_uri",
                    &format!("{}/deviceauth/callback", self.issuer),
                ),
                ("code_verifier", &code.code_verifier),
            ])
            .await?;
        ChatGptLogin::from_tokens(tokens, None).map(Some)
    }

    /// A new access token for `login`. Its refresh token works once: store the result before
    /// anything else refreshes.
    pub async fn refresh(&self, login: &ChatGptLogin) -> Result<ChatGptLogin> {
        let tokens = self
            .token(&[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_ID),
                ("refresh_token", &login.refresh_token),
            ])
            .await
            .context(
                "the ChatGPT login could not be renewed: sign in again in the dashboard settings",
            )?;
        ChatGptLogin::from_tokens(tokens, Some(login))
    }

    async fn token(&self, form: &[(&str, &str)]) -> Result<Tokens> {
        let res = self
            .http
            .post(format!("{}/oauth/token", self.issuer))
            .form(form)
            .send()
            .await
            .context("could not reach OpenAI's sign-in")?;
        let status = res.status();
        if status.is_success() {
            return res.json().await.context("OpenAI's sign-in");
        }
        // Only the error code: the body may echo the grant back.
        let body: Value = res.json().await.unwrap_or_default();
        let code = body["error"]["code"]
            .as_str()
            .or(body["error"].as_str())
            .unwrap_or("rejected");
        bail!("OpenAI's sign-in answered {status}: {code}")
    }
}

/// A JWT's claims, unchecked: it came straight from the issuer, over TLS.
fn claims(jwt: &str) -> Result<Value> {
    let payload = jwt.split('.').nth(1).context("not a JWT")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .context("not a JWT")?;
    serde_json::from_slice(&bytes).context("not a JWT")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A stand-in for OpenAI's sign-in, for tests here and in the crates that use it.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Form, Json};
    use axum::http::StatusCode;
    use axum::routing::post;
    use serde_json::{Value, json};

    use base64::Engine;

    use super::{AUTH_CLAIM, CLIENT_ID, now};

    fn jwt(claims: Value) -> String {
        let b64 = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        format!("{}.{}.sig", b64(b"{}"), b64(claims.to_string().as_bytes()))
    }

    fn access(exp: u64) -> String {
        jwt(json!({ "exp": exp, AUTH_CLAIM: { "chatgpt_account_id": "acct" } }))
    }

    /// A form the token endpoint got.
    pub type Grant = Vec<(String, String)>;

    /// OpenAI's sign-in, at `url`.
    pub struct Issuer {
        pub url: String,
        /// The forms the token endpoint got.
        pub grants: Arc<Mutex<Vec<Grant>>>,
    }

    /// OpenAI's sign-in: the user enters the code on the second poll. A login it issues expires
    /// in a minute, a renewed one in ten days.
    pub async fn issuer() -> Issuer {
        let grants = Arc::<Mutex<Vec<_>>>::default();
        let seen = Arc::clone(&grants);
        let polls = Arc::new(Mutex::new(0));
        let app = Router::new()
            .route(
                "/api/accounts/deviceauth/usercode",
                post(|Json(body): Json<Value>| async move {
                    assert_eq!(body["client_id"], CLIENT_ID);
                    Json(json!({ "device_auth_id": "dev", "user_code": "ABCD-1234", "interval": "3" }))
                }),
            )
            .route(
                "/api/accounts/deviceauth/token",
                post(move |Json(body): Json<Value>| async move {
                    assert_eq!(body["device_auth_id"], "dev");
                    let mut polls = polls.lock().unwrap();
                    *polls += 1;
                    match *polls {
                        1 => (StatusCode::FORBIDDEN, Json(json!({}))),
                        _ => (
                            StatusCode::OK,
                            Json(json!({ "authorization_code": "code", "code_challenge": "c", "code_verifier": "ver" })),
                        ),
                    }
                }),
            )
            .route(
                "/oauth/token",
                post(move |Form(form): Form<Vec<(String, String)>>| async move {
                    let refresh = form.iter().any(|(k, v)| k == "grant_type" && v == "refresh_token");
                    seen.lock().unwrap().push(form);
                    Json(match refresh {
                        // A refresh leaves out the ID token.
                        true => json!({ "access_token": access(now() + 10 * 24 * 60 * 60), "refresh_token": "r2" }),
                        false => json!({
                            "access_token": access(now() + 60),
                            "refresh_token": "r1",
                            "id_token": jwt(json!({ "email": "me@example.com", AUTH_CLAIM: { "chatgpt_account_id": "acct" } })),
                        }),
                    })
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(axum::serve(listener, app).into_future());
        Issuer { url, grants }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::issuer;
    use super::*;

    #[tokio::test]
    async fn signs_in_with_a_device_code_and_renews_the_login() {
        let issuer = issuer().await;
        let (url, grants) = (&issuer.url, &issuer.grants);
        let auth = ChatGptAuth::new(url);

        let device = auth.start().await.unwrap();
        assert_eq!(device.user_code, "ABCD-1234");
        assert_eq!(device.verification_url, format!("{url}/codex/device"));
        assert_eq!(device.interval_secs, 3);
        assert!(auth.poll(&device).await.unwrap().is_none());

        let login = auth.poll(&device).await.unwrap().unwrap();
        assert_eq!(login.account_id, "acct");
        assert_eq!(login.email.as_deref(), Some("me@example.com"));
        assert!(login.expires_within(Duration::from_secs(3600)));
        let exchange = grants.lock().unwrap()[0].clone();
        for pair in [
            ("grant_type", "authorization_code"),
            ("code", "code"),
            ("code_verifier", "ver"),
            ("client_id", CLIENT_ID),
        ] {
            assert!(
                exchange.contains(&(pair.0.into(), pair.1.into())),
                "{pair:?}"
            );
        }
        assert!(exchange.contains(&("redirect_uri".into(), format!("{url}/deviceauth/callback"))));

        let renewed = auth.refresh(&login).await.unwrap();
        assert!(!renewed.expires_within(Duration::from_secs(3600)));
        assert_eq!(renewed.refresh_token, "r2");
        assert_eq!(renewed.account_id, "acct");
        assert_eq!(renewed.email.as_deref(), Some("me@example.com"));
        assert!(grants.lock().unwrap()[1].contains(&("refresh_token".into(), "r1".into())));
    }

    #[tokio::test]
    async fn an_expired_code_fails() {
        let auth = ChatGptAuth::new(&issuer().await.url);
        let mut device = auth.start().await.unwrap();
        device.expires_at = now() - 1;
        assert!(auth.poll(&device).await.is_err());
    }
}
