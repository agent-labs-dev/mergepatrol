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

#[derive(Clone)]
pub struct App {
    app_id: u64,
    key: Arc<EncodingKey>,
    http: reqwest::Client,
    /// GitHub's REST API. Another in tests.
    api: String,
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
        })
    }

    fn jwt(&self) -> Result<String> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let claims = json!({ "iat": now - 60, "exp": now + 540, "iss": self.app_id.to_string() });
        Ok(jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
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
        query: &str,
        variables: Value,
    ) -> Result<T> {
        let res: Value = self
            .rest(
                Method::POST,
                "/graphql",
                Some(json!({ "query": query, "variables": variables })),
            )
            .await?;
        if let Some(errors) = res.get("errors") {
            bail!("GraphQL: {errors}");
        }
        Ok(serde_json::from_value(res["data"].clone())?)
    }
}

/// How many times a request goes out before its transient failure counts.
const ATTEMPTS: u32 = 5;
/// The longest wait for a rate limit to lift. Beyond it, the request fails.
const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(10 * 60);

/// Sends the request `build` makes, and again after a transient failure: a network error, a 5xx,
/// or a rate limit. Each attempt builds it anew, so its token is not one that expired while it
/// waited. The status and body of the last response.
async fn send<F: Future<Output = Result<RequestBuilder>>>(
    build: impl Fn() -> F,
    what: &str,
) -> Result<(StatusCode, String)> {
    let mut backoff = Duration::from_secs(1);
    let mut attempt = 1;
    let (status, _, body) = loop {
        let sent = send_once(build().await?).await;
        let wait = match &sent {
            Ok((status, headers, body)) => retry_after(*status, headers, body, backoff, unix_now()),
            Err(_) => Some(backoff),
        };
        match wait {
            Some(wait) if attempt < ATTEMPTS && wait <= MAX_RATE_LIMIT_WAIT => {
                let failure = match &sent {
                    Ok((status, ..)) => status.to_string(),
                    Err(e) => e.to_string(),
                };
                tracing::warn!(what, attempt, %failure, wait_secs = wait.as_secs(), "GitHub request failed, retrying");
                tokio::time::sleep(wait).await;
                attempt += 1;
                backoff *= 2;
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

/// How long to wait before sending the request again, when its response is a transient failure.
/// A rate limit takes as long as GitHub says, or a minute when it does not say.
fn retry_after(
    status: StatusCode,
    headers: &HeaderMap,
    body: &str,
    backoff: Duration,
    now: u64,
) -> Option<Duration> {
    let header = |name: &str| headers.get(name)?.to_str().ok()?.trim().parse::<u64>().ok();
    let exhausted = header("x-ratelimit-remaining") == Some(0);
    let limited = status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN
            && (exhausted
                || header("retry-after").is_some()
                || body.contains("secondary rate limit")));
    if !limited {
        return status.is_server_error().then_some(backoff);
    }
    let secs = match (header("retry-after"), header("x-ratelimit-reset")) {
        (Some(secs), _) => secs,
        (None, Some(reset)) if exhausted => reset.saturating_sub(now).max(1),
        _ => 60,
    };
    Some(Duration::from_secs(secs))
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

fn api_headers() -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("accept", "application/vnd.github+json".parse().unwrap());
    h.insert("x-github-api-version", "2022-11-28".parse().unwrap());
    h
}

/// A GitHub on localhost that answers each request with the next scripted response.
#[cfg(test)]
pub(crate) mod fake {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{App, Installation};

    /// A response: status, extra header lines, body.
    pub(crate) type Scripted = (u16, &'static str, &'static str);

    /// An installation, with a token already, that talks to the fake. The request lines it got.
    pub(crate) async fn serve(responses: Vec<Scripted>) -> (Installation, Arc<Mutex<Vec<String>>>) {
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
        };
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
}
