use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{Method, StatusCode};
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
            .get(format!("{API}/app"))
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
        let list: Vec<Value> = self
            .http
            .get(format!("{API}/app/installations?per_page=100"))
            .bearer_auth(self.jwt()?)
            .headers(api_headers())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(list
            .iter()
            .filter_map(|i| {
                Some(InstallationInfo {
                    id: i["id"].as_u64()?,
                    account: i["account"]["login"].as_str()?.to_owned(),
                })
            })
            .collect())
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
            .get(format!("{API}/repos/{repo}/installation"))
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
        let tok: Tok = self
            .app
            .http
            .post(format!("{API}/app/installations/{}/access_tokens", self.id))
            .bearer_auth(self.app.jwt()?)
            .headers(api_headers())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
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
        let tok: Tok = self
            .app
            .http
            .post(format!("{API}/app/installations/{}/access_tokens", self.id))
            .bearer_auth(self.app.jwt()?)
            .headers(api_headers())
            .json(&json!({ "repositories": [repo.name], "permissions": { "contents": "read" } }))
            .send()
            .await?
            .error_for_status()
            .context("create a read-only token")?
            .json()
            .await?;
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
        let mut req = self
            .app
            .http
            .request(method, format!("{API}{path}"))
            .bearer_auth(self.token().await?)
            .headers(api_headers());
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await?;
        let status = res.status();
        let text = res.text().await?;
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

fn api_headers() -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("accept", "application/vnd.github+json".parse().unwrap());
    h.insert("x-github-api-version", "2022-11-28".parse().unwrap());
    h
}
