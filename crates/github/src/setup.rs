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

/// The webhook events that wake a repo's queue: its PRs, reviews, checks, and trunk moves. And
/// what the You tab keeps of each PR: its comments, and who is on which team.
pub const WEBHOOK_EVENTS: [&str; 13] = [
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "issue_comment",
    "membership",
    "team",
    "push",
    "check_suite",
    "check_run",
    "status",
    // Whether a fork PR's workflow runs wait for a maintainer's approval.
    "workflow_run",
    // The checks a branch requires.
    "branch_protection_rule",
    "repository_ruleset",
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
            // The logs of failed GitHub Actions jobs, for the agent that fixes them.
            "actions": "read",
            "metadata": "read",
            // Who is on which team, for the reviews asked of a team.
            "members": "read",
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
        let challenge = Self::challenge(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    /// S256: base64url of the verifier's SHA-256, unpadded.
    pub fn challenge(verifier: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
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

/// A user's GitHub token. A session keeps it, encrypted, to check the user's installations
/// again. Apps that expire user tokens give an 8-hour token and a 6-month refresh token;
/// others give a token that lasts until it is revoked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserToken {
    pub access_token: String,
    /// Unix seconds. `None` for a token that does not expire.
    pub expires_at: Option<i64>,
    pub refresh_token: Option<String>,
}

impl UserToken {
    /// Expired, or about to: a minute's margin covers the calls made with it.
    pub fn expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|at| at - 60 <= now)
    }
}

/// GitHub no longer accepts the user's token: they revoked it, or the refresh token expired.
/// Their session is over.
#[derive(Debug)]
pub struct Revoked;

impl std::fmt::Display for Revoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitHub revoked the user's token")
    }
}

impl std::error::Error for Revoked {}

/// Trades the code from the OAuth callback for a user token.
pub async fn exchange_code(
    app: &AppCredentials,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<UserToken> {
    let res = token_request(
        app,
        &[
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ],
    )
    .await?;
    if let Some(err) = res["error"].as_str() {
        bail!(
            "GitHub sign-in failed: {err}: {}",
            res["error_description"].as_str().unwrap_or("")
        );
    }
    user_token(&res, now())
}

/// A new user token for an expired one.
pub async fn refresh_user_token(app: &AppCredentials, refresh_token: &str) -> Result<UserToken> {
    let res = token_request(
        app,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )
    .await?;
    match res["error"].as_str() {
        Some("bad_refresh_token") => Err(Revoked.into()),
        Some(err) => bail!(
            "GitHub refused to refresh the user's token: {err}: {}",
            res["error_description"].as_str().unwrap_or("")
        ),
        None => user_token(&res, now()),
    }
}

async fn token_request(app: &AppCredentials, params: &[(&str, &str)]) -> Result<Value> {
    let mut form = vec![
        ("client_id", app.client_id.as_str()),
        ("client_secret", app.client_secret.as_str()),
    ];
    form.extend_from_slice(params);
    Ok(client()?
        .post("https://github.com/login/oauth/access_token")
        .header("accept", "application/json")
        .form(&form)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

fn user_token(res: &Value, now: i64) -> Result<UserToken> {
    Ok(UserToken {
        access_token: res["access_token"]
            .as_str()
            .context("no access_token")?
            .to_owned(),
        expires_at: res["expires_in"].as_i64().map(|secs| now + secs),
        refresh_token: res["refresh_token"].as_str().map(str::to_owned),
    })
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
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

/// The login of the user a token acts as. `None` for an installation token, as a workflow's
/// `GITHUB_TOKEN` is: GitHub refuses it `/user`, as it names no user.
pub async fn login_of(token: &str) -> Result<Option<String>> {
    let res = user_get(token, "/user").await?;
    if res.status() == reqwest::StatusCode::FORBIDDEN {
        return Ok(None);
    }
    let user: GithubUser = res.error_for_status()?.json().await?;
    Ok(Some(user.login))
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

/// A 401 is `Revoked`: GitHub no longer takes the token.
async fn user_get(token: &str, path: &str) -> Result<reqwest::Response> {
    let res = client()?
        .get(format!("{API}{path}"))
        .bearer_auth(token)
        .header("accept", "application/vnd.github+json")
        .send()
        .await?;
    if res.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Revoked.into());
    }
    Ok(res)
}

/// GitHub refused the user's change: they lack the access it needs.
#[derive(Debug)]
pub struct Refused;

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitHub refused the user's change")
    }
}

impl std::error::Error for Refused {}

/// Adds labels to a PR as the user, so GitHub checks that they may: it takes triage access.
/// `Refused` when they lack it.
pub async fn add_labels_as(token: &str, repo: &str, number: u64, labels: &[&str]) -> Result<()> {
    let url = format!("{API}/repos/{repo}/issues/{number}/labels");
    let req = client()?.post(url).json(&json!({ "labels": labels }));
    user_write(token, req).await?.error_for_status()?;
    Ok(())
}

/// Removes a label from a PR as the user. A label the PR does not have is no error.
pub async fn remove_label_as(token: &str, repo: &str, number: u64, label: &str) -> Result<()> {
    let mut url = Url::parse(&format!("{API}/repos/{repo}/issues/{number}/labels"))?;
    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("not a base URL"))?
        .push(label);
    let res = user_write(token, client()?.delete(url)).await?;
    if res.status() != reqwest::StatusCode::NOT_FOUND {
        res.error_for_status()?;
    }
    Ok(())
}

/// Closes a PR as the user, so GitHub checks that they may: it takes triage access, or being its
/// author. `Refused` when they may not.
pub async fn close_pull_as(token: &str, repo: &str, number: u64) -> Result<()> {
    let url = format!("{API}/repos/{repo}/pulls/{number}");
    let req = client()?.patch(url).json(&json!({ "state": "closed" }));
    user_write(token, req).await?.error_for_status()?;
    Ok(())
}

/// Asks these users to review a PR, as the user. `Rejected` for a login GitHub will not ask, as
/// one without access.
pub async fn request_reviewers_as(
    token: &str,
    repo: &str,
    number: u64,
    logins: &[String],
) -> Result<()> {
    let url = format!("{API}/repos/{repo}/pulls/{number}/requested_reviewers");
    let req = client()?.post(url).json(&json!({ "reviewers": logins }));
    let res = user_write(token, req).await?;
    if res.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        return Err(Rejected(rejection(&res.json().await.unwrap_or_default())).into());
    }
    res.error_for_status()?;
    Ok(())
}

/// What a review says of a PR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
}

/// Comments on a PR as the user.
pub async fn comment_as(token: &str, repo: &str, number: u64, body: &str) -> Result<()> {
    let url = format!("{API}/repos/{repo}/issues/{number}/comments");
    let req = client()?.post(url).json(&json!({ "body": body }));
    user_write(token, req).await?.error_for_status()?;
    Ok(())
}

/// GitHub turned the user's change down, for the reason it gives: approving their own PR, say.
#[derive(Debug)]
pub struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

/// Reviews a PR as the user. `Rejected` for a review GitHub will not take: of their own PR, or
/// a change request without a comment.
pub async fn review_as(
    token: &str,
    repo: &str,
    number: u64,
    verdict: Verdict,
    body: &str,
) -> Result<()> {
    let url = format!("{API}/repos/{repo}/pulls/{number}/reviews");
    let event = match verdict {
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
        Verdict::Comment => "COMMENT",
    };
    let req = client()?
        .post(url)
        .json(&json!({ "event": event, "body": body }));
    let res = user_write(token, req).await?;
    if res.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        return Err(Rejected(rejection(&res.json().await.unwrap_or_default())).into());
    }
    res.error_for_status()?;
    Ok(())
}

/// A comment on a line of the PR, in a review.
pub struct LineComment<'a> {
    pub path: &'a str,
    pub line: u32,
    pub body: String,
}

/// Reviews a PR as the user at `head`, with comments on its lines. A comment on a line the PR
/// did not change goes in the body instead: GitHub takes line comments only on the diff.
pub async fn review_lines_as(
    token: &str,
    repo: &str,
    number: u64,
    head: &str,
    verdict: Verdict,
    body: &str,
    comments: &[LineComment<'_>],
) -> Result<()> {
    let url = format!("{API}/repos/{repo}/pulls/{number}/reviews");
    let event = match verdict {
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
        Verdict::Comment => "COMMENT",
    };
    let lines: Vec<Value> = comments
        .iter()
        .map(|c| json!({ "path": c.path, "line": c.line, "side": "RIGHT", "body": c.body }))
        .collect();
    let send = |lines: &[Value], body: &str| {
        Ok::<_, anyhow::Error>(client()?.post(&url).json(&json!({
            "commit_id": head, "event": event, "body": body, "comments": lines,
        })))
    };
    let res = user_write(token, send(&lines, body)?).await?;
    if res.status() != reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        res.error_for_status()?;
        return Ok(());
    }
    let why = rejection(&res.json().await.unwrap_or_default());
    if comments.is_empty() {
        return Err(Rejected(why).into());
    }
    // A line outside the diff: the comments go in the body.
    let mut all = body.to_owned();
    for c in comments {
        all.push_str(&format!("\n\n**`{}:{}`**\n{}", c.path, c.line, c.body));
    }
    let res = user_write(token, send(&[], all.trim())?).await?;
    if res.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        return Err(Rejected(rejection(&res.json().await.unwrap_or_default())).into());
    }
    res.error_for_status()?;
    Ok(())
}

/// GitHub's reason in a 422: its first error, which is a string for reviews, or its message.
fn rejection(body: &Value) -> String {
    let first = &body["errors"][0];
    first
        .as_str()
        .or_else(|| first["message"].as_str())
        .or_else(|| body["message"].as_str())
        .unwrap_or("GitHub turned the review down.")
        .to_owned()
}

/// Whether GitHub takes the token, of any kind: `/rate_limit` answers every token, and costs
/// none of its rate limit.
pub async fn token_works(token: &str) -> Result<bool> {
    match user_get(token, "/rate_limit").await {
        Ok(res) => {
            res.error_for_status()?;
            Ok(true)
        }
        Err(e) if e.is::<Revoked>() => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether the token can read `repo`. GitHub answers 404 for a private repo it cannot.
/// `Revoked` when GitHub does not take the token.
pub async fn can_read(token: &str, repo: &str) -> Result<bool> {
    let res = user_get(token, &format!("/repos/{repo}")).await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    res.error_for_status()?;
    Ok(true)
}

/// Whether the token's user may push to the repo. An App's installation token, as a workflow's
/// `GITHUB_TOKEN` is, names no user: no.
pub async fn can_write(token: &str, repo: &str) -> Result<bool> {
    let res = user_get(token, &format!("/repos/{repo}")).await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    let repo: Value = res.error_for_status()?.json().await?;
    Ok(repo["permissions"]["push"] == true)
}

/// A 401 is `Revoked`, a 403 `Refused`.
async fn user_write(token: &str, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
    let res = req
        .bearer_auth(token)
        .header("accept", "application/vnd.github+json")
        .send()
        .await?;
    match res.status() {
        reqwest::StatusCode::UNAUTHORIZED => Err(Revoked.into()),
        reqwest::StatusCode::FORBIDDEN => Err(Refused.into()),
        _ => Ok(res),
    }
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
    fn a_rejected_review_says_why() {
        let own = json!({ "message": "Unprocessable Entity",
            "errors": ["Review Can not approve your own pull request"] });
        assert_eq!(
            rejection(&own),
            "Review Can not approve your own pull request"
        );
        let field = json!({ "message": "Validation Failed", "errors": [{ "message": "body is required" }] });
        assert_eq!(rejection(&field), "body is required");
        assert_eq!(rejection(&json!({ "message": "No" })), "No");
        assert_eq!(rejection(&Value::Null), "GitHub turned the review down.");
    }

    #[test]
    fn reads_an_expiring_token_and_a_lasting_one() {
        let expiring = json!({
            "access_token": "ghu_a", "expires_in": 28800,
            "refresh_token": "ghr_b", "refresh_token_expires_in": 15811200,
        });
        assert_eq!(
            user_token(&expiring, 1000).unwrap(),
            UserToken {
                access_token: "ghu_a".into(),
                expires_at: Some(29800),
                refresh_token: Some("ghr_b".into()),
            }
        );
        let lasting = user_token(&json!({"access_token": "gho_c"}), 1000).unwrap();
        assert_eq!(lasting.expires_at, None);
        assert!(!lasting.expired(i64::MAX));
    }

    #[test]
    fn a_token_expires_a_minute_early() {
        let token = UserToken {
            access_token: String::new(),
            expires_at: Some(1000),
            refresh_token: None,
        };
        assert!(!token.expired(939));
        assert!(token.expired(940));
    }

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
