use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::header::HeaderMap;
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use mergepatrol_core::RepoName;

const API: &str = "https://api.github.com";

/// One delivery of the App's webhook: GitHub keeps one per attempt, a redelivery under the same
/// `guid`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HookDelivery {
    pub id: u64,
    pub guid: String,
    /// The response's. 0 for none, as when the request timed out.
    pub status_code: u16,
    pub event: String,
}

impl HookDelivery {
    pub fn delivered(&self) -> bool {
        (200..300).contains(&self.status_code)
    }
}

/// The `rel="next"` URL of a `Link` header.
fn next_link(link: &str) -> Option<String> {
    let next = link.split(',').find(|l| l.contains(r#"rel="next""#))?;
    let url = next.split(';').next()?.trim();
    Some(url.strip_prefix('<')?.strip_suffix('>')?.to_owned())
}

#[derive(Clone)]
pub struct App {
    app_id: u64,
    key: Arc<EncodingKey>,
    http: reqwest::Client,
    /// GitHub's REST API. Another in tests.
    api: String,
    /// RS256 with the App's key. Another in tests.
    alg: Algorithm,
}

impl App {
    pub fn new(app_id: u64, private_key_pem: &[u8]) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("mergepatrol/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            app_id,
            key: Arc::new(
                EncodingKey::from_rsa_pem(private_key_pem).context("GitHub App private key")?,
            ),
            http,
            api: API.into(),
            alg: Algorithm::RS256,
        })
    }

    fn jwt(&self) -> Result<String> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let claims = json!({ "iat": now - 60, "exp": now + 540, "iss": self.app_id.to_string() });
        Ok(jsonwebtoken::encode(
            &Header::new(self.alg),
            &claims,
            &self.key,
        )?)
    }

    pub fn from_credentials(c: &crate::AppCredentials) -> Result<Self> {
        Self::new(c.id, c.pem.as_bytes())
    }

    /// Fills in the parts of the credentials that GitHub knows: slug, owner, and page.
    pub async fn credentials(
        &self,
        pem: String,
        client_id: String,
        client_secret: String,
        webhook_secret: Option<String>,
    ) -> Result<crate::AppCredentials> {
        let info: Value = self
            .http
            .get(format!("{}/app", self.api))
            .bearer_auth(self.jwt()?)
            .headers(api_headers())
            .send()
            .await?
            .error_for_status()
            .context("GET /app: is the App ID or private key wrong?")?
            .json()
            .await?;
        let s = |v: &Value| {
            v.as_str()
                .map(str::to_owned)
                .context("GET /app: missing field")
        };
        Ok(crate::AppCredentials {
            id: self.app_id,
            slug: s(&info["slug"])?,
            client_id,
            client_secret,
            webhook_secret,
            pem,
            owner: s(&info["owner"]["login"])?,
            html_url: s(&info["html_url"])?,
        })
    }

    pub async fn installations(&self) -> Result<Vec<InstallationInfo>> {
        let mut all = Vec::new();
        for page in 1.. {
            let list: Vec<Value> = self
                .http
                .get(format!(
                    "{}/app/installations?per_page=100&page={page}",
                    self.api
                ))
                .bearer_auth(self.jwt()?)
                .headers(api_headers())
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            all.extend(list.iter().filter_map(|i| {
                Some(InstallationInfo {
                    id: i["id"].as_u64()?,
                    account: i["account"]["login"].as_str()?.to_owned(),
                })
            }));
            if list.len() < 100 {
                break;
            }
        }
        Ok(all)
    }

    /// The webhook's deliveries newer than `after`, newest first. Without `after`, the newest
    /// page of them.
    pub async fn hook_deliveries(&self, after: Option<u64>) -> Result<Vec<HookDelivery>> {
        let mut all = Vec::new();
        let mut url = format!("{}/app/hook/deliveries?per_page=100", self.api);
        // GitHub keeps three days of them: a hiccup's worth is a few pages.
        for _ in 0..10 {
            let res = self
                .http
                .get(&url)
                .bearer_auth(self.jwt()?)
                .headers(api_headers())
                .send()
                .await?
                .error_for_status()
                .context("GET /app/hook/deliveries")?;
            let next = res
                .headers()
                .get("link")
                .and_then(|l| l.to_str().ok())
                .and_then(next_link);
            let page: Vec<HookDelivery> = res.json().await?;
            let reached = page.is_empty() || after.is_none_or(|a| page.iter().any(|d| d.id <= a));
            all.extend(page.into_iter().filter(|d| after.is_none_or(|a| d.id > a)));
            match next {
                Some(next) if !reached => url = next,
                _ => break,
            }
        }
        Ok(all)
    }

    /// Has GitHub send a delivery of the webhook again.
    pub async fn redeliver(&self, delivery: u64) -> Result<()> {
        self.http
            .post(format!(
                "{}/app/hook/deliveries/{delivery}/attempts",
                self.api
            ))
            .bearer_auth(self.jwt()?)
            .headers(api_headers())
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("redeliver webhook delivery {delivery}"))?;
        Ok(())
    }

    pub fn installation(&self, id: u64) -> Installation {
        Installation {
            id,
            app: self.clone(),
            token: Arc::new(Mutex::new(None)),
        }
    }

    /// The installation of this App on `repo`.
    pub async fn installation_for(&self, repo: &RepoName) -> Result<Installation> {
        #[derive(Deserialize)]
        struct Inst {
            id: u64,
        }
        let inst: Inst = self
            .http
            .get(format!("{}/repos/{repo}/installation", self.api))
            .bearer_auth(self.jwt()?)
            .headers(api_headers())
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("the App is not installed on {repo}"))?
            .json()
            .await?;
        Ok(Installation {
            id: inst.id,
            app: self.clone(),
            token: Arc::new(Mutex::new(None)),
        })
    }
}

#[derive(Clone, Debug)]
pub struct InstallationInfo {
    pub id: u64,
    pub account: String,
}

#[derive(Clone, Debug)]
pub struct RepoInfo {
    pub repo: RepoName,
    pub private: bool,
}

/// A client that acts as one installation of the App. Tokens refresh before they expire.
#[derive(Clone)]
pub struct Installation {
    pub id: u64,
    app: App,
    token: Arc<Mutex<Option<(String, SystemTime)>>>,
}

pub(crate) struct Response {
    pub status: StatusCode,
    pub body: Value,
}

impl Installation {
    pub async fn token(&self) -> Result<String> {
        let mut cached = self.token.lock().await;
        if let Some((token, expires)) = cached.as_ref()
            && *expires > SystemTime::now() + Duration::from_secs(300)
        {
            return Ok(token.clone());
        }
        #[derive(Deserialize)]
        struct Tok {
            token: String,
        }
        let mint = || async {
            Ok(self
                .app
                .http
                .post(format!(
                    "{}/app/installations/{}/access_tokens",
                    self.app.api, self.id
                ))
                .bearer_auth(self.app.jwt()?)
                .headers(api_headers()))
        };
        let tok: Tok = json_ok(send(mint, "create an access token").await?)?;
        // Tokens live one hour.
        *cached = Some((
            tok.token.clone(),
            SystemTime::now() + Duration::from_secs(3600),
        ));
        Ok(tok.token)
    }

    /// A token that can only read `repo`. It goes into the job sandbox.
    pub async fn read_token(&self, repo: &RepoName) -> Result<String> {
        #[derive(Deserialize)]
        struct Tok {
            token: String,
        }
        let permissions =
            json!({ "repositories": [repo.name], "permissions": { "contents": "read" } });
        let mint = || async {
            Ok(self
                .app
                .http
                .post(format!(
                    "{}/app/installations/{}/access_tokens",
                    self.app.api, self.id
                ))
                .bearer_auth(self.app.jwt()?)
                .headers(api_headers())
                .json(&permissions))
        };
        let tok: Tok = json_ok(send(mint, "create a read-only token").await?)?;
        Ok(tok.token)
    }

    /// The repos this installation can access.
    pub async fn repositories(&self) -> Result<Vec<RepoInfo>> {
        let mut repos = Vec::new();
        for page in 1.. {
            let res: Value = self
                .rest(
                    Method::GET,
                    &format!("/installation/repositories?per_page=100&page={page}"),
                    None,
                )
                .await?;
            let batch = res["repositories"].as_array().cloned().unwrap_or_default();
            for r in &batch {
                if let Some(repo) = r["full_name"].as_str().and_then(RepoName::parse) {
                    repos.push(RepoInfo {
                        repo,
                        private: r["private"].as_bool().unwrap_or(true),
                    });
                }
            }
            if batch.len() < 100 {
                break;
            }
        }
        Ok(repos)
    }

    /// A GET for a body that is not JSON, in the media type `accept` names: its status and text.
    pub(crate) async fn get_text(&self, path: &str, accept: &str) -> Result<(StatusCode, String)> {
        let build = || async move {
            let mut headers = api_headers();
            headers.insert("accept", accept.parse()?);
            Ok(self
                .app
                .http
                .get(format!("{}{path}", self.app.api))
                .bearer_auth(self.token().await?)
                .headers(headers))
        };
        send(build, &format!("GET {path}")).await
    }

    /// A REST call that returns the status and the body, for callers that branch on the status.
    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Response> {
        let (method, body) = (&method, body.as_ref());
        let build = || async move {
            let mut req = self
                .app
                .http
                .request(method.clone(), format!("{}{path}", self.app.api))
                .bearer_auth(self.token().await?)
                .headers(api_headers());
            if let Some(body) = body {
                req = req.json(body);
            }
            Ok(req)
        };
        let (status, text) = send(build, &format!("{method} {path}")).await?;
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok(Response { status, body })
    }

    pub(crate) async fn rest<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T> {
        let what = format!("{method} {path}");
        let res = self.request(method, path, body).await?;
        if !res.status.is_success() {
            bail!("{what}: {} {}", res.status, res.body);
        }
        serde_json::from_value(res.body).with_context(|| what)
    }

    pub(crate) async fn graphql<T: DeserializeOwned>(
        &self,
        name: &str,
        query: &str,
        variables: Value,
    ) -> Result<T> {
        let body = graphql_body(query, variables);
        let build = || async {
            Ok(self
                .app
                .http
                .post(format!("{}/graphql", self.app.api))
                .bearer_auth(self.token().await?)
                .headers(api_headers())
                .json(&body))
        };
        graphql_data(
            name,
            Some(self.id),
            false,
            send(build, "POST /graphql").await?,
        )
    }

    /// A GraphQL query whose answer may lack what does not exist: GitHub reports a missing file
    /// or repo as a `NOT_FOUND` error, beside the rest of the answer. Any other error fails it.
    pub(crate) async fn graphql_missing_ok(
        &self,
        name: &str,
        query: &str,
        variables: Value,
    ) -> Result<Value> {
        let body = graphql_body(query, variables);
        let build = || async {
            Ok(self
                .app
                .http
                .post(format!("{}/graphql", self.app.api))
                .bearer_auth(self.token().await?)
                .headers(api_headers())
                .json(&body))
        };
        graphql_data(
            name,
            Some(self.id),
            true,
            send(build, "POST /graphql").await?,
        )
    }
}

/// A client that acts as a signed-in user, on the user's own rate limit, not the installation's:
/// for what the dashboard reads on the user's behalf.
pub struct AsUser {
    http: reqwest::Client,
    api: String,
    token: String,
}

impl AsUser {
    pub fn new(token: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("mergepatrol/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            api: API.into(),
            token,
        })
    }

    pub(crate) async fn graphql<T: DeserializeOwned>(
        &self,
        name: &str,
        query: &str,
        variables: Value,
    ) -> Result<T> {
        let body = graphql_body(query, variables);
        let build = || async {
            Ok(self
                .http
                .post(format!("{}/graphql", self.api))
                .bearer_auth(&self.token)
                .headers(api_headers())
                .json(&body))
        };
        graphql_data(name, None, false, send(build, "POST /graphql").await?)
    }

    /// As [`Installation::graphql_missing_ok`].
    pub(crate) async fn graphql_missing_ok(
        &self,
        name: &str,
        query: &str,
        variables: Value,
    ) -> Result<Value> {
        let body = graphql_body(query, variables);
        let build = || async {
            Ok(self
                .http
                .post(format!("{}/graphql", self.api))
                .bearer_auth(&self.token)
                .headers(api_headers())
                .json(&body))
        };
        graphql_data(name, None, true, send(build, "POST /graphql").await?)
    }
}

/// The request body of a query. A query asks for its own cost too, so the log shows what spends
/// the rate limit. A mutation cannot ask.
fn graphql_body(query: &str, variables: Value) -> Value {
    let query = match query.find('{') {
        Some(i) if !query.trim_start().starts_with("mutation") => format!(
            "{} rateLimit {{ cost remaining resetAt }} {}",
            &query[..=i],
            &query[i + 1..]
        ),
        _ => query.to_owned(),
    };
    json!({ "query": query, "variables": variables })
}

/// The `data` of a GraphQL response, with its cost logged. `installation` is `None` for a user.
/// `missing_ok` lets `NOT_FOUND` errors through, beside the data.
fn graphql_data<T: DeserializeOwned>(
    name: &str,
    installation: Option<u64>,
    missing_ok: bool,
    (status, body): (StatusCode, String),
) -> Result<T> {
    if installation.is_none() && status == StatusCode::UNAUTHORIZED {
        return Err(crate::setup::Revoked.into());
    }
    if !status.is_success() {
        bail!("GraphQL {name}: {status} {body}");
    }
    let mut res: Value = serde_json::from_str(&body)?;
    let missing = |errors: &Value| {
        !res["data"].is_null()
            && errors
                .as_array()
                .is_some_and(|e| e.iter().all(|e| e["type"] == "NOT_FOUND"))
    };
    if missing_ok && res["data"].is_null() {
        bail!("GraphQL: {}", res["errors"]);
    }
    if let Some(errors) = res.get("errors")
        && !(missing_ok && missing(errors))
    {
        bail!("GraphQL: {errors}");
    }
    let mut data = res["data"].take();
    if let Some(limit) = data.as_object_mut().and_then(|d| d.remove("rateLimit")) {
        tracing::info!(
            query = name,
            installation,
            cost = limit["cost"].as_u64(),
            remaining = limit["remaining"].as_u64(),
            reset_at = limit["resetAt"].as_str(),
            "GraphQL cost"
        );
    }
    Ok(serde_json::from_value(data)?)
}

/// GitHub's rate limit is used up, past the longest wait for it. The caller should stop calling
/// until `until`.
#[derive(Debug)]
pub struct RateLimited {
    pub until: SystemTime,
}

impl RateLimited {
    /// How long until the limit lifts.
    pub fn remaining(&self) -> Duration {
        self.until
            .duration_since(SystemTime::now())
            .unwrap_or_default()
    }
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mins = self.remaining().as_secs().div_ceil(60).max(1);
        write!(
            f,
            "GitHub's API rate limit is used up for another {mins} min"
        )
    }
}

impl std::error::Error for RateLimited {}

/// How many times a request goes out before its transient failure counts.
const ATTEMPTS: u32 = 5;
/// The longest wait for a rate limit to lift. Beyond it, the request fails.
const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(10 * 60);

/// Sends the request `build` makes, and again after a transient failure: a network error, a 5xx,
/// or a rate limit. Each attempt builds it anew, so its token is not one that expired while it
/// waited. The status and body of the last response, or `RateLimited` once waiting is no use.
async fn send<F: Future<Output = Result<RequestBuilder>>>(
    build: impl Fn() -> F,
    what: &str,
) -> Result<(StatusCode, String)> {
    let mut backoff = Duration::from_secs(1);
    let mut attempt = 1;
    let (status, _, body) = loop {
        let sent = send_once(build().await?).await;
        let retry = match &sent {
            Ok((status, headers, body)) => retry_after(*status, headers, body, backoff, unix_now()),
            Err(_) => Some(Retry::Backoff(backoff)),
        };
        match retry {
            Some(retry) if attempt < ATTEMPTS && retry.wait() <= MAX_RATE_LIMIT_WAIT => {
                let failure = match &sent {
                    Ok((status, ..)) => status.to_string(),
                    Err(e) => e.to_string(),
                };
                let wait = retry.wait();
                tracing::warn!(what, attempt, %failure, wait_secs = wait.as_secs(), "GitHub request failed, retrying");
                tokio::time::sleep(wait).await;
                attempt += 1;
                backoff *= 2;
            }
            Some(Retry::RateLimit(wait)) => {
                return Err(RateLimited {
                    until: SystemTime::now() + wait,
                }
                .into());
            }
            _ => break sent?,
        }
    };
    Ok((status, body))
}

async fn send_once(req: RequestBuilder) -> reqwest::Result<(StatusCode, HeaderMap, String)> {
    let res = req.send().await?;
    let (status, headers) = (res.status(), res.headers().clone());
    Ok((status, headers, res.text().await?))
}

#[derive(Debug, PartialEq)]
enum Retry {
    /// A network error or a 5xx.
    Backoff(Duration),
    RateLimit(Duration),
}

impl Retry {
    fn wait(&self) -> Duration {
        match self {
            Self::Backoff(d) | Self::RateLimit(d) => *d,
        }
    }
}

/// How long to wait before sending the request again, when its response is a transient failure.
/// A rate limit takes as long as GitHub says, or a minute when it does not say.
fn retry_after(
    status: StatusCode,
    headers: &HeaderMap,
    body: &str,
    backoff: Duration,
    now: u64,
) -> Option<Retry> {
    let header = |name: &str| headers.get(name)?.to_str().ok()?.trim().parse::<u64>().ok();
    let exhausted = header("x-ratelimit-remaining") == Some(0);
    let limited = status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN
            && (exhausted
                || header("retry-after").is_some()
                || body.contains("secondary rate limit")))
        || (status.is_success() && graphql_rate_limited(body));
    if !limited {
        return status.is_server_error().then_some(Retry::Backoff(backoff));
    }
    let secs = match (header("retry-after"), header("x-ratelimit-reset")) {
        (Some(secs), _) => secs,
        (None, Some(reset)) if exhausted => reset.saturating_sub(now).max(1),
        _ => 60,
    };
    Some(Retry::RateLimit(Duration::from_secs(secs)))
}

/// GraphQL answers its rate limit with a 200 whose errors say so: `RATE_LIMITED`, or
/// `RATE_LIMIT` once the limit was already exceeded.
fn graphql_rate_limited(body: &str) -> bool {
    if !body.contains("RATE_LIMIT") {
        return false;
    }
    let Ok(res) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    res["errors"].as_array().is_some_and(|errors| {
        errors
            .iter()
            .any(|e| matches!(e["type"].as_str(), Some("RATE_LIMITED" | "RATE_LIMIT")))
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The body of a successful response, as JSON.
fn json_ok<T: DeserializeOwned>((status, body): (StatusCode, String)) -> Result<T> {
    if !status.is_success() {
        bail!("{status} {body}");
    }
    Ok(serde_json::from_str(&body)?)
}

/// Revokes an installation token: the token itself is the credential.
pub async fn revoke_token(token: &str) -> Result<()> {
    revoke_token_at(API, token).await
}

async fn revoke_token_at(api: &str, token: &str) -> Result<()> {
    let http = reqwest::Client::builder()
        .user_agent(concat!("mergepatrol/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .build()?;
    let build = || async {
        Ok(http
            .delete(format!("{api}/installation/token"))
            .bearer_auth(token)
            .headers(api_headers()))
    };
    let (status, body) = send(build, "revoke the read-only token").await?;
    if !status.is_success() {
        bail!("revoke the read-only token: {status} {body}");
    }
    Ok(())
}

fn api_headers() -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("accept", "application/vnd.github+json".parse().unwrap());
    h.insert("x-github-api-version", "2022-11-28".parse().unwrap());
    h
}

/// A GitHub on localhost that answers each request with the next scripted response.
#[cfg(any(test, feature = "testing"))]
pub mod fake {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{App, Installation};

    /// A response: status, extra header lines, body.
    pub type Scripted = (u16, &'static str, &'static str);

    /// An installation, with a token already, that talks to the fake. The request lines it got.
    pub async fn serve(responses: Vec<Scripted>) -> (Installation, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        tokio::spawn(async move {
            for (status, headers, body) in responses {
                let (mut conn, _) = listener.accept().await.unwrap();
                let request = read_request(&mut conn).await;
                seen.lock()
                    .unwrap()
                    .push(request.lines().next().unwrap_or_default().to_owned());
                let response = format!(
                    "HTTP/1.1 {status} Scripted\r\ncontent-length: {}\r\nconnection: close\r\n{headers}\r\n{body}",
                    body.len()
                );
                conn.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let app = App {
            app_id: 1,
            key: Arc::new(jsonwebtoken::EncodingKey::from_secret(b"unused")),
            http: reqwest::Client::new(),
            api,
            alg: jsonwebtoken::Algorithm::HS256,
        };
        let token = Some(("t".into(), SystemTime::now() + Duration::from_secs(3600)));
        let inst = Installation {
            id: 1,
            app,
            token: Arc::new(tokio::sync::Mutex::new(token)),
        };
        (inst, requests)
    }

    /// The request's head and body, read in full.
    async fn read_request(conn: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let n = conn.read(&mut chunk).await.unwrap();
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).into_owned();
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if n == 0 || buf.len() >= end + 4 + length {
                    return text;
                }
            }
            if n == 0 {
                return text;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn transient_failures_are_retried_and_others_are_not() {
        let backoff = Duration::from_secs(2);
        let none = HeaderMap::new();
        let wait = |status: u16, h: &HeaderMap, body: &str| {
            retry_after(
                StatusCode::from_u16(status).unwrap(),
                h,
                body,
                backoff,
                1000,
            )
            .map(|r| r.wait())
        };
        assert_eq!(
            retry_after(StatusCode::BAD_GATEWAY, &none, "", backoff, 1000),
            Some(Retry::Backoff(backoff))
        );
        assert_eq!(wait(502, &none, ""), Some(backoff));
        assert_eq!(wait(404, &none, ""), None);
        assert_eq!(wait(422, &none, ""), None);
        // A permission error is not a rate limit.
        assert_eq!(
            wait(403, &none, "Resource not accessible by integration"),
            None
        );
        let exhausted = headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1030"),
        ]);
        assert_eq!(wait(403, &exhausted, ""), Some(Duration::from_secs(30)));
        let told = headers(&[("retry-after", "5")]);
        assert_eq!(wait(429, &told, ""), Some(Duration::from_secs(5)));
        assert_eq!(wait(403, &told, ""), Some(Duration::from_secs(5)));
        let secondary = "You have exceeded a secondary rate limit.";
        assert_eq!(wait(403, &none, secondary), Some(Duration::from_secs(60)));
        let graphql =
            r#"{"errors":[{"type":"RATE_LIMIT","message":"API rate limit already exceeded"}]}"#;
        assert_eq!(
            wait(200, &exhausted, graphql),
            Some(Duration::from_secs(30))
        );
        let limited = r#"{"errors":[{"type":"RATE_LIMITED"}]}"#;
        assert_eq!(wait(200, &none, limited), Some(Duration::from_secs(60)));
        // Any other GraphQL error is the query's own.
        let other = r#"{"errors":[{"type":"NOT_FOUND","message":"RATE_LIMIT"}]}"#;
        assert_eq!(wait(200, &none, other), None);
    }

    #[tokio::test]
    async fn a_graphql_rate_limit_waits_then_sends_again() {
        let (gh, requests) = fake::serve(vec![
            (
                200,
                "x-ratelimit-remaining: 0\r\nx-ratelimit-reset: 0\r\n",
                r#"{"errors":[{"type":"RATE_LIMIT"}]}"#,
            ),
            (200, "", r#"{"data":{"ok":true}}"#),
        ])
        .await;
        let v: Value = gh.graphql("ok", "{ ok }", json!({})).await.unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_rate_limit_past_the_longest_wait_fails_as_rate_limited() {
        let (gh, requests) = fake::serve(vec![(
            200,
            "x-ratelimit-remaining: 0\r\nx-ratelimit-reset: 99999999999\r\n",
            r#"{"errors":[{"type":"RATE_LIMIT"}]}"#,
        )])
        .await;
        let e = gh
            .graphql::<Value>("ok", "{ ok }", json!({}))
            .await
            .unwrap_err();
        assert!(e.downcast_ref::<RateLimited>().unwrap().remaining() > MAX_RATE_LIMIT_WAIT);
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_query_asks_for_its_cost_and_a_mutation_does_not() {
        let q = graphql_body("query($a: Int!) { repository { id } }", json!({}));
        assert_eq!(
            q["query"],
            "query($a: Int!) { rateLimit { cost remaining resetAt }  repository { id } }"
        );
        let m = graphql_body("mutation { updateRefs { id } }", json!({}));
        assert_eq!(m["query"], "mutation { updateRefs { id } }");
    }

    #[tokio::test]
    async fn a_missing_ok_query_lets_only_not_found_through() {
        let (gh, _) = fake::serve(vec![
            (
                200,
                "",
                r#"{"data":{"repository":null},"errors":[{"type":"NOT_FOUND"}]}"#,
            ),
            (
                200,
                "",
                r#"{"data":{"repository":null},"errors":[{"type":"FORBIDDEN"}]}"#,
            ),
        ])
        .await;
        let v = gh
            .graphql_missing_ok("q", "{ ok }", json!({}))
            .await
            .unwrap();
        assert_eq!(v, json!({ "repository": null }));
        assert!(
            gh.graphql_missing_ok("q", "{ ok }", json!({}))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_cost_is_taken_out_of_the_data() {
        let (gh, _) = fake::serve(vec![(
            200,
            "",
            r#"{"data":{"rateLimit":{"cost":1,"remaining":4999,"resetAt":"x"},"ok":true}}"#,
        )])
        .await;
        let v: Value = gh.graphql("ok", "{ ok }", json!({})).await.unwrap();
        assert_eq!(v, json!({ "ok": true }));
    }

    #[tokio::test]
    async fn a_5xx_is_sent_again() {
        let (gh, requests) =
            fake::serve(vec![(502, "", "bad gateway"), (200, "", r#"{"ok":true}"#)]).await;
        let v: Value = gh.rest(Method::GET, "/x", None).await.unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(*requests.lock().unwrap(), ["GET /x HTTP/1.1"; 2]);
    }

    #[tokio::test]
    async fn each_attempt_builds_the_request_anew() {
        let (gh, _) = fake::serve(vec![(429, "retry-after: 0\r\n", ""), (200, "", "{}")]).await;
        let built = std::sync::atomic::AtomicU32::new(0);
        let build = || async {
            built.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(gh.app.http.get(format!("{}/w", gh.app.api)))
        };
        let (status, _) = send(build, "GET /w").await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(built.into_inner(), 2);
    }

    #[tokio::test]
    async fn a_rate_limit_waits_as_told_then_sends_again() {
        let (gh, requests) =
            fake::serve(vec![(429, "retry-after: 0\r\n", ""), (201, "", "{}")]).await;
        let _: Value = gh
            .rest(Method::POST, "/y", Some(json!({ "a": 1 })))
            .await
            .unwrap();
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_client_error_is_not_sent_again() {
        let (gh, requests) = fake::serve(vec![(404, "", r#"{"message":"Not Found"}"#)]).await;
        let res = gh.request(Method::GET, "/z", None).await.unwrap();
        assert_eq!(res.status, StatusCode::NOT_FOUND);
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_deliveries_stop_at_the_last_one_seen() {
        let page = r#"[
            {"id":12,"guid":"c","status_code":502,"event":"push"},
            {"id":11,"guid":"b","status_code":204,"event":"pull_request"},
            {"id":10,"guid":"a","status_code":204,"event":"push"}
        ]"#;
        let more = "link: <http://unused/app/hook/deliveries?cursor=v1_10>; rel=\"next\"\r\n";
        let (gh, requests) = fake::serve(vec![(200, more, page), (202, "", "{}")]).await;
        let seen = gh.app.hook_deliveries(Some(10)).await.unwrap();
        assert_eq!(seen.iter().map(|d| d.id).collect::<Vec<_>>(), [12, 11]);
        assert!(!seen[0].delivered() && seen[1].delivered());
        gh.app.redeliver(12).await.unwrap();
        assert_eq!(
            *requests.lock().unwrap(),
            [
                "GET /app/hook/deliveries?per_page=100 HTTP/1.1",
                "POST /app/hook/deliveries/12/attempts HTTP/1.1",
            ]
        );
    }

    #[test]
    fn the_next_page_is_in_the_link_header() {
        let link = r#"<https://api.github.com/x?cursor=b>; rel="next", <https://api.github.com/x?cursor=a>; rel="prev""#;
        assert_eq!(
            next_link(link).as_deref(),
            Some("https://api.github.com/x?cursor=b")
        );
        assert_eq!(next_link(r#"<https://api.github.com/x>; rel="prev""#), None);
    }

    #[tokio::test]
    async fn a_token_revokes_itself() {
        let (gh, requests) = fake::serve(vec![(204, "", "")]).await;
        revoke_token_at(&gh.app.api, "t").await.unwrap();
        assert_eq!(
            *requests.lock().unwrap(),
            ["DELETE /installation/token HTTP/1.1"]
        );
    }

    #[tokio::test]
    async fn a_refused_revoke_fails() {
        let (gh, _) = fake::serve(vec![(401, "", r#"{"message":"Bad credentials"}"#)]).await;
        assert!(revoke_token_at(&gh.app.api, "t").await.is_err());
    }
}
