//! Launches each job as a sandbox on GKE, through Agent Sandbox (agent-sandbox.sigs.k8s.io): a
//! `SandboxClaim` takes a pod from a warm pool, which runs `mergepatrol job --serve` under gVisor.
//! A claim that sets env skips the warm pool, so the job's link goes to the pod over HTTP
//! instead. The template's network policy lets in only the orchestrator.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::remote::{Launch, Launched, watch};

const CLAIMS: &str = "apis/extensions.agents.x-k8s.io/v1beta1";
/// Where Agent Sandbox puts the claim's UID on the pod it gets.
const CLAIM_UID_LABEL: &str = "agents.x-k8s.io/claim-uid";
/// The port `mergepatrol job --serve` listens on.
pub const START_PORT: u16 = 8080;
/// The longest a claim may take to get a pod that takes its link: a new node takes 1 to 2.5
/// minutes when the warm pool is empty.
const START_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const START_POLL: Duration = Duration::from_secs(1);
/// Every call to the Kubernetes API and to the pod: a hung one would hold the job's queue.
pub const KUBE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The Kubernetes API, as this pod's service account.
#[derive(Clone)]
pub struct Kube {
    /// `https://<host>:<port>`, without a trailing slash.
    pub base: String,
    /// Read on each call: the kubelet rotates the token.
    pub token_file: PathBuf,
    pub http: reqwest::Client,
}

impl Kube {
    /// From inside a pod: the API server from the environment, and the service account's token
    /// and CA from where the kubelet mounts them.
    pub fn in_cluster() -> Result<Self> {
        const DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
        let host = std::env::var("KUBERNETES_SERVICE_HOST").context("not in a pod")?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").context("not in a pod")?;
        let ca = std::fs::read(format!("{DIR}/ca.crt")).context("read the cluster's CA")?;
        let http = reqwest::Client::builder()
            .tls_certs_only([reqwest::Certificate::from_pem(&ca)?])
            .timeout(KUBE_REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            base: format!("https://{host}:{port}"),
            token_file: format!("{DIR}/token").into(),
            http,
        })
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let token = tokio::fs::read_to_string(&self.token_file).await?;
        let res = req.bearer_auth(token.trim()).send().await?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("{status}: {}", body["message"].as_str().unwrap_or_default());
        }
        Ok(body)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.send(self.http.get(format!("{}/{path}", self.base)))
            .await
    }
}

/// Launches each job as a claim on `warm_pool`, in `namespace`.
pub struct SandboxLaunch {
    pub kube: Kube,
    pub namespace: String,
    pub warm_pool: String,
    /// How long a claim may live. Agent Sandbox deletes it then, should the orchestrator be gone.
    pub timeout: Duration,
    /// The pod's port for its link: `START_PORT`, but for tests.
    pub start_port: u16,
}

#[async_trait]
impl Launch for SandboxLaunch {
    async fn launch(&self, job_id: &str, link: &str) -> Result<Box<dyn Launched>> {
        let shutdown = time::OffsetDateTime::from(SystemTime::now() + self.timeout)
            .format(&time::format_description::well_known::Rfc3339)?;
        let claim = self
            .kube
            .send(
                self.kube
                    .http
                    .post(format!(
                        "{}/{CLAIMS}/namespaces/{}/sandboxclaims",
                        self.kube.base, self.namespace
                    ))
                    .json(&json!({
                        "apiVersion": "extensions.agents.x-k8s.io/v1beta1",
                        "kind": "SandboxClaim",
                        "metadata": {
                            "generateName": "job-",
                            "annotations": { "mergepatrol.ai/job": job_id },
                        },
                        "spec": {
                            "warmPoolRef": { "name": self.warm_pool },
                            "lifecycle": { "shutdownTime": shutdown, "shutdownPolicy": "Delete" },
                        },
                    })),
            )
            .await
            .with_context(|| format!("claim a sandbox for {job_id}"))?;
        let job = SandboxJob {
            kube: self.kube.clone(),
            namespace: self.namespace.clone(),
            name: claim["metadata"]["name"]
                .as_str()
                .context("the claim's name")?
                .to_owned(),
            uid: claim["metadata"]["uid"]
                .as_str()
                .context("the claim's uid")?
                .to_owned(),
        };
        // From here on, dropping `job` deletes the claim: on an error too.
        tokio::time::timeout(START_TIMEOUT, job.start(link, self.start_port))
            .await
            .with_context(|| format!("no sandbox took the job in {START_TIMEOUT:?}"))??;
        tracing::info!(job = job_id, claim = %job.name, "job started in a sandbox");
        Ok(Box::new(job))
    }
}

struct SandboxJob {
    kube: Kube,
    namespace: String,
    name: String,
    uid: String,
}

impl SandboxJob {
    fn claim_path(&self) -> String {
        format!(
            "{CLAIMS}/namespaces/{}/sandboxclaims/{}",
            self.namespace, self.name
        )
    }

    /// Waits for the claim's pod, then hands it the link. A new pod refuses it until it listens.
    async fn start(&self, link: &str, port: u16) -> Result<()> {
        loop {
            let claim = self.kube.get(&self.claim_path()).await?;
            if let Some(ip) = claim["status"]["sandbox"]["podIPs"][0].as_str() {
                let started = self
                    .kube
                    .http
                    .post(format!("http://{ip}:{port}/start"))
                    .body(link.to_owned())
                    .send()
                    .await;
                match started {
                    Ok(res) if res.status().is_success() => return Ok(()),
                    // The pod took another link: never, unless a claim got a used pod.
                    Ok(res) => bail!("the sandbox refused the job: {}", res.status()),
                    Err(e) if e.is_connect() => {}
                    Err(e) => return Err(e.into()),
                }
            }
            tokio::time::sleep(START_POLL).await;
        }
    }

    /// `None` while the pod runs; then whether the job exited cleanly.
    async fn poll(&self) -> Result<Option<Result<()>>> {
        let pods = self
            .kube
            .get(&format!(
                "api/v1/namespaces/{}/pods?labelSelector={CLAIM_UID_LABEL}%3D{}",
                self.namespace, self.uid
            ))
            .await?;
        let Some(pod) = pods["items"].get(0) else {
            bail!("the job's pod is gone");
        };
        match pod["status"]["phase"].as_str() {
            Some("Succeeded") => Ok(Some(Ok(()))),
            Some("Failed") => {
                let state = &pod["status"]["containerStatuses"][0]["state"]["terminated"];
                Ok(Some(Err(anyhow::anyhow!(
                    "the job's pod failed: {} (exit {}). Logs: kubectl -n {} logs {}",
                    state["reason"].as_str().unwrap_or("unknown"),
                    state["exitCode"],
                    self.namespace,
                    pod["metadata"]["name"].as_str().unwrap_or_default(),
                ))))
            }
            _ => Ok(None),
        }
    }
}

#[async_trait]
impl Launched for SandboxJob {
    async fn ended(&self) -> Result<()> {
        watch(|| self.poll()).await
    }
}

/// Deletes the claim, and with it the pod: after the job, after an error, and when the job is
/// cut off because its queue stopped.
impl Drop for SandboxJob {
    fn drop(&mut self) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (kube, path, name) = (self.kube.clone(), self.claim_path(), self.name.clone());
        rt.spawn(async move {
            let url = format!("{}/{path}?propagationPolicy=Background", kube.base);
            if let Err(e) = kube.send(kube.http.delete(url)).await {
                tracing::warn!(error = %format!("{e:#}"), claim = %name, "could not delete the job's sandbox claim");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path as UrlPath, Query, State};
    use axum::routing::{get, post};

    use super::*;

    /// A Kubernetes API with one claim, whose pod is this same server.
    #[derive(Default)]
    struct Api {
        /// The pod has an IP after this many claim reads.
        reads_before_ip: Mutex<u32>,
        link: Mutex<Option<String>>,
        phase: Mutex<&'static str>,
        deleted: Mutex<Vec<String>>,
        claim: Mutex<Value>,
    }

    async fn serve(api: Arc<Api>) -> (String, u16) {
        let app = Router::new()
            .route(
                "/apis/extensions.agents.x-k8s.io/v1beta1/namespaces/jobs/sandboxclaims",
                post(
                    |State(api): State<Arc<Api>>, axum::Json(body): axum::Json<Value>| async move {
                        *api.claim.lock().unwrap() = body;
                        axum::Json(json!({ "metadata": { "name": "job-x1", "uid": "u-1" } }))
                    },
                ),
            )
            .route(
                "/apis/extensions.agents.x-k8s.io/v1beta1/namespaces/jobs/sandboxclaims/{name}",
                get(|State(api): State<Arc<Api>>| async move {
                    let mut reads = api.reads_before_ip.lock().unwrap();
                    if *reads > 0 {
                        *reads -= 1;
                        return axum::Json(json!({ "status": {} }));
                    }
                    axum::Json(json!({ "status": { "sandbox": { "podIPs": ["127.0.0.1"] } } }))
                })
                .delete(
                    |State(api): State<Arc<Api>>, UrlPath(name): UrlPath<String>| async move {
                        api.deleted.lock().unwrap().push(name);
                        axum::Json(json!({}))
                    },
                ),
            )
            .route(
                "/api/v1/namespaces/jobs/pods",
                get(
                    |State(api): State<Arc<Api>>, Query(q): Query<HashMap<String, String>>| async move {
                        assert_eq!(q["labelSelector"], "agents.x-k8s.io/claim-uid=u-1");
                        let phase = *api.phase.lock().unwrap();
                        axum::Json(json!({ "items": [{
                            "metadata": { "name": "pod-1" },
                            "status": { "phase": phase, "containerStatuses": [{ "state": {
                                "terminated": { "reason": "Error", "exitCode": 1 } } }] },
                        }] }))
                    },
                ),
            )
            .route(
                "/start",
                post(|State(api): State<Arc<Api>>, link: String| async move {
                    *api.link.lock().unwrap() = Some(link);
                }),
            )
            .with_state(api);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, app).into_future());
        (format!("http://{addr}"), addr.port())
    }

    async fn launcher(api: &Arc<Api>) -> SandboxLaunch {
        let (base, port) = serve(api.clone()).await;
        let token = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(token.path(), "t\n").unwrap();
        SandboxLaunch {
            kube: Kube {
                base,
                token_file: token.into_temp_path().keep().unwrap(),
                http: reqwest::Client::new(),
            },
            namespace: "jobs".into(),
            warm_pool: "pool".into(),
            timeout: Duration::from_secs(60),
            start_port: port,
        }
    }

    async fn deleted(api: &Api) -> Vec<String> {
        for _ in 0..100 {
            let deleted = api.deleted.lock().unwrap().clone();
            if !deleted.is_empty() {
                return deleted;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Vec::new()
    }

    #[tokio::test]
    async fn a_job_claims_a_sandbox_hands_it_the_link_and_frees_it_after() {
        let api = Arc::new(Api {
            reads_before_ip: Mutex::new(2),
            phase: Mutex::new("Running"),
            ..Default::default()
        });
        let job = launcher(&api)
            .await
            .launch("o-r-1", "http://orch/jobs/t")
            .await
            .unwrap();
        assert_eq!(
            api.link.lock().unwrap().as_deref(),
            Some("http://orch/jobs/t")
        );
        let claim = api.claim.lock().unwrap().clone();
        assert_eq!(claim["spec"]["warmPoolRef"]["name"], "pool");
        assert_eq!(claim["spec"]["lifecycle"]["shutdownPolicy"], "Delete");
        assert_eq!(
            claim["metadata"]["annotations"]["mergepatrol.ai/job"],
            "o-r-1"
        );
        assert!(
            claim["spec"].get("env").is_none(),
            "env would skip the warm pool"
        );
        assert!(api.deleted.lock().unwrap().is_empty());
        drop(job);
        assert_eq!(deleted(&api).await, ["job-x1"]);
    }

    fn job(launch: &SandboxLaunch) -> SandboxJob {
        SandboxJob {
            kube: launch.kube.clone(),
            namespace: "jobs".into(),
            name: "job-x1".into(),
            uid: "u-1".into(),
        }
    }

    #[tokio::test]
    async fn the_job_runs_until_its_pod_ends() {
        let api = Arc::new(Api::default());
        let job = job(&launcher(&api).await);
        *api.phase.lock().unwrap() = "Running";
        assert!(job.poll().await.unwrap().is_none());
        *api.phase.lock().unwrap() = "Succeeded";
        assert!(matches!(job.poll().await.unwrap(), Some(Ok(()))));
        *api.phase.lock().unwrap() = "Failed";
        let err = job.poll().await.unwrap().unwrap().unwrap_err().to_string();
        assert!(err.contains("pod failed: Error (exit 1)"), "{err}");
        assert!(err.contains("kubectl -n jobs logs pod-1"), "{err}");
    }
}
