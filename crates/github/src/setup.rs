//! Creating the GitHub App from the dashboard (the manifest flow), and signing users in with it.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const API: &str = "https://api.github.com";

/// What GitHub returns when it creates the App. Stored encrypted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppCredentials {
    pub id: u64,
    pub slug: String,
    pub client_id: String,
    pub client_secret: String,
    pub webhook_secret: Option<String>,
    pub pem: String,
    /// The account that owns the App. Its installations decide who can sign in.
    pub owner: String,
    pub html_url: String,
}

impl AppCredentials {
    pub fn install_url(&self) -> String {
        format!("{}/installations/new", self.html_url)
    }
}

/// The webhook events that wake a repo's queue: its PRs, reviews, checks, and trunk moves.
pub const WEBHOOK_EVENTS: [&str; 6] = [
    "pull_request",
    "pull_request_review",
    "push",
    "check_suite",
    "check_run",
    "status",
];

/// The App that Mergepatrol needs. GitHub only delivers webhooks to a public URL, so on
/// http (localhost) the webhook is off and the queue only polls.
pub fn manifest(public_url: &str, owner: &str) -> Value {
    // App names are unique across GitHub, and at most 34 characters.
    let mut name = format!("mergepatrol-{owner}");
    name.truncate(34);
    json!({
        "name": name,
        "url": public_url,
        "description": "Merge queue that rebases PRs and resolves conflicts with an agent.",
        "redirect_url": format!("{public_url}/setup/callback"),
        "callback_urls": [format!("{public_url}/auth/callback")],
        "setup_url": format!("{public_url}/setup/installed"),
        "hook_attributes": {
            "url": format!("{public_url}/webhooks/github"),
            "active": public_url.starts_with("https://"),
        },
        "default_events": WEBHOOK_EVENTS,
        "public": false,
        "default_permissions": {
            "contents": "write",
            // Classic branch protection: its required checks, and whether the trunk is strict.
            "administration": "read",
            "pull_requests": "write",
            "issues": "write",
            "checks": "read",
            "statuses": "read",
            "metadata": "read",
        },
    })
}

/// The hosted App: the same App, public, so any account can install it.
pub fn hosted_manifest(public_url: &str) -> Value {
    let mut m = manifest(public_url, "");
    m["name"] = json!("Mergepatrol");
    m["public"] = json!(true);
    m
}

/// Where the browser posts the manifest form.
pub fn manifest_form_url(owner: &str, organization: bool, state: &str) -> Result<String> {
    let base = if organization {
        format!("https://github.com/organizations/{owner}/settings/apps/new")
    } else {
        "https://github.com/settings/apps/new".to_owned()
    };
    Ok(Url::parse_with_params(&base, [("state", state)])?.into())
}

pub async fn convert_manifest(code: &str) -> Result<AppCredentials> {
    let res: Value = client()?
        .post(format!("{API}/app-manifests/{code}/conversions"))
        .header("accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()
        .context("GitHub did not convert the manifest code")?
        .json()
        .await?;
    let s = |k: &str| {
        res[k]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("manifest conversion: no {k}"))
    };
    Ok(AppCredentials {
        id: res["id"].as_u64().context("manifest conversion: no id")?,
        slug: s("slug")?,
        client_id: s("client_id")?,
        client_secret: s("client_secret")?,
        webhook_secret: res["webhook_secret"].as_str().map(str::to_owned),
        pem: s("pem")?,
        owner: res["owner"]["login"]
            .as_str()
            .context("manifest conversion: no owner")?
            .to_owned(),
        html_url: s("html_url")?,
    })
}

/// A random URL-safe string with `bytes` bytes of entropy.
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the OS has no random source");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        let verifier = random_token(32);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

pub fn authorize_url(
    app: &AppCredentials,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> Result<String> {
    Ok(Url::parse_with_params(
        "https://github.com/login/oauth/authorize",
        [
            ("client_id", app.client_id.as_str()),
            ("redirect_uri", redirect_uri),
            ("state", state),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ],
    )?
    .into())
}

/// Trades the code from the OAuth callback for a user access token.
pub async fn exchange_code(
    app: &AppCredentials,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<String> {
    let res: Value = client()?
        .post("https://github.com/login/oauth/access_token")
        .header("accept", "application/json")
        .form(&[
            ("client_id", app.client_id.as_str()),
            ("client_secret", app.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if let Some(err) = res["error"].as_str() {
        bail!(
            "GitHub sign-in failed: {err}: {}",
            res["error_description"].as_str().unwrap_or("")
        );
    }
    Ok(res["access_token"]
        .as_str()
        .context("no access_token")?
        .to_owned())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GithubUser {
    pub login: String,
    pub avatar_url: String,
}

pub async fn user(token: &str) -> Result<GithubUser> {
    Ok(user_get(token, "/user")
        .await?
        .error_for_status()?
        .json()
        .await?)
}

/// An installation of the App that a signed-in user can access.
#[derive(Clone, Debug)]
pub struct UserInstallation {
    pub id: u64,
    pub account: String,
    pub account_type: String,
    pub avatar_url: String,
}

/// The installations of this App that the user can access: their workspaces.
pub async fn user_installations(token: &str) -> Result<Vec<UserInstallation>> {
    let mut all = Vec::new();
    for page in 1.. {
        let res: Value = user_get(
            token,
            &format!("/user/installations?per_page=100&page={page}"),
        )
        .await?
        .error_for_status()?
        .json()
        .await?;
        let batch = res["installations"].as_array().cloned().unwrap_or_default();
        all.extend(batch.iter().filter_map(|i| {
            Some(UserInstallation {
                id: i["id"].as_u64()?,
                account: i["account"]["login"].as_str()?.to_owned(),
                account_type: i["account"]["type"].as_str().unwrap_or("User").to_owned(),
                avatar_url: i["account"]["avatar_url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            })
        }));
        if batch.len() < 100 {
            break;
        }
    }
    Ok(all)
}

async fn user_get(token: &str, path: &str) -> Result<reqwest::Response> {
    Ok(client()?
        .get(format!("{API}{path}"))
        .bearer_auth(token)
        .header("accept", "application/vnd.github+json")
        .send()
        .await?)
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("mergepatrol/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_points_back_at_us() {
        let m = manifest("http://localhost:8080", "agent-labs-dev");
        assert_eq!(m["redirect_url"], "http://localhost:8080/setup/callback");
        assert_eq!(m["callback_urls"][0], "http://localhost:8080/auth/callback");
        assert!(m["name"].as_str().unwrap().len() <= 34);
    }

    #[test]
    fn the_hosted_app_is_public() {
        let m = hosted_manifest("https://mergepatrol.ai");
        assert_eq!(m["public"], true);
        assert_eq!(
            m["hook_attributes"]["url"],
            "https://mergepatrol.ai/webhooks/github"
        );
    }

    #[test]
    fn org_and_user_form_urls() {
        assert_eq!(
            manifest_form_url("acme", true, "s1").unwrap(),
            "https://github.com/organizations/acme/settings/apps/new?state=s1"
        );
        assert_eq!(
            manifest_form_url("me", false, "s1").unwrap(),
            "https://github.com/settings/apps/new?state=s1"
        );
    }

    #[test]
    fn pkce_challenge_is_43_chars() {
        assert_eq!(Pkce::new().challenge.len(), 43);
    }
}
