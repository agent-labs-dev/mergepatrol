//! Jobs that run on another machine and reach the orchestrator over HTTP: in production, one
//! GKE sandbox per job (`crate::sandbox`). A job's only way in is its link, which holds a
//! single-use token: it fetches its spec once, uploads its output, and says it is done.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use crate::Executor;
use crate::protocol::{JobResult, JobSpec, MAX_BUNDLE_BYTES, OUTPUT_FILES};

/// The bundle goes up in parts of this size, so no one request holds all of it.
pub const CHUNK_BYTES: usize = 16 * 1024 * 1024;
/// The most a job may upload: its bundle, and room for its result. The job is untrusted, so
/// more is refused as it comes, not after it filled the disk.
const MAX_UPLOAD_BYTES: u64 = MAX_BUNDLE_BYTES + CHUNK_BYTES as u64;

/// The jobs waiting for their spec, or uploading their output, by token.
pub struct Mailbox {
    slots: Mutex<HashMap<String, Slot>>,
    /// The most each job may upload.
    limit: u64,
}

impl Default for Mailbox {
    fn default() -> Self {
        Self {
            slots: Mutex::default(),
            limit: MAX_UPLOAD_BYTES,
        }
    }
}

struct Slot {
    /// Taken by the first fetch.
    spec: Option<Vec<u8>>,
    out: PathBuf,
    /// The bytes uploaded so far.
    received: u64,
    done: Option<oneshot::Sender<()>>,
}

impl Mailbox {
    /// `GET /jobs/{token}/spec`, `POST /jobs/{token}/files/{name}` (appends), and
    /// `POST /jobs/{token}/done`.
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/jobs/{token}/spec", get(spec))
            .route("/jobs/{token}/files/{name}", post(append))
            .route("/jobs/{token}/done", post(done))
            .layer(DefaultBodyLimit::max(CHUNK_BYTES + 1024))
            .with_state(self)
    }

    fn open(
        self: &Arc<Self>,
        spec: &JobSpec,
        out: &Path,
    ) -> Result<(Closed, oneshot::Receiver<()>)> {
        let token = random_token();
        let (tx, rx) = oneshot::channel();
        self.slots.lock().unwrap().insert(
            token.clone(),
            Slot {
                spec: Some(serde_json::to_vec(spec)?),
                out: out.to_owned(),
                received: 0,
                done: Some(tx),
            },
        );
        Ok((
            Closed {
                mailbox: self.clone(),
                token,
            },
            rx,
        ))
    }

    /// Where the job's next `bytes` go, if they fit under the limit: they count as received from
    /// here on.
    fn receive(&self, token: &str, bytes: usize) -> Result<PathBuf, StatusCode> {
        let mut slots = self.slots.lock().unwrap();
        let slot = slots.get_mut(token).ok_or(StatusCode::NOT_FOUND)?;
        let received = slot.received + bytes as u64;
        if received > self.limit {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        slot.received = received;
        Ok(slot.out.clone())
    }
}

/// Closes the job's slot when dropped: its link stops working once the job is over.
struct Closed {
    mailbox: Arc<Mailbox>,
    token: String,
}

impl Drop for Closed {
    fn drop(&mut self) {
        self.mailbox.slots.lock().unwrap().remove(&self.token);
    }
}

async fn spec(
    State(mb): State<Arc<Mailbox>>,
    UrlPath(token): UrlPath<String>,
) -> Result<Vec<u8>, StatusCode> {
    let mut slots = mb.slots.lock().unwrap();
    slots
        .get_mut(&token)
        .and_then(|s| s.spec.take())
        .ok_or(StatusCode::NOT_FOUND)
}

async fn append(
    State(mb): State<Arc<Mailbox>>,
    UrlPath((token, name)): UrlPath<(String, String)>,
    body: Bytes,
) -> StatusCode {
    if !OUTPUT_FILES.contains(&name.as_str()) {
        return StatusCode::NOT_FOUND;
    }
    let out = match mb.receive(&token, body.len()) {
        Ok(out) => out,
        Err(status) => return status,
    };
    let written = async {
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(out.join(&name))
            .await?;
        f.write_all(&body).await?;
        f.flush().await
    };
    match written.await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(e) => {
            tracing::error!(error = %e, "could not write a job's output");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn done(State(mb): State<Arc<Mailbox>>, UrlPath(token): UrlPath<String>) -> StatusCode {
    let tx = mb
        .slots
        .lock()
        .unwrap()
        .get_mut(&token)
        .and_then(|s| s.done.take());
    match tx {
        Some(tx) => {
            let _ = tx.send(());
            StatusCode::NO_CONTENT
        }
        None => StatusCode::NOT_FOUND,
    }
}

pub(crate) fn random_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("the OS has no random source");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// The job's side: fetches the spec from its link.
pub async fn fetch_spec(link: &str) -> Result<JobSpec> {
    let res = reqwest::get(format!("{link}/spec")).await?;
    if !res.status().is_success() {
        bail!("fetch the job spec: {}", res.status());
    }
    res.json().await.context("bad job spec")
}

/// The job's side: uploads the output in `out`, then says it is done.
pub async fn upload(link: &str, out: &Path, chunk: usize) -> Result<()> {
    let http = reqwest::Client::new();
    for name in OUTPUT_FILES {
        let data = match tokio::fs::read(out.join(name)).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for part in data.chunks(chunk) {
            http.post(format!("{link}/files/{name}"))
                .body(part.to_vec())
                .send()
                .await?
                .error_for_status()
                .with_context(|| format!("upload {name}"))?;
        }
    }
    http.post(format!("{link}/done"))
        .send()
        .await?
        .error_for_status()
        .context("report the job done")?;
    Ok(())
}

/// Starts `mergepatrol job --remote <link>` somewhere.
#[async_trait]
pub trait Launch: Send + Sync {
    async fn launch(&self, job_id: &str, link: &str) -> Result<Box<dyn Launched>>;
}

/// Dropping it stops the job, if it still runs, and frees its machine: after the job, after an
/// error, and when the job is cut off because its queue stopped.
#[async_trait]
pub trait Launched: Send + Sync {
    /// Returns when the job's machine is done with it: `Err` with why, if it failed.
    async fn ended(&self) -> Result<()>;
}

/// Runs each job through `launch`, which starts it on a machine that reaches back to
/// `base_url` for its spec and with its output.
pub struct RemoteExecutor {
    pub mailbox: Arc<Mailbox>,
    /// The orchestrator's public URL.
    pub base_url: String,
    pub launch: Box<dyn Launch>,
    pub timeout: Duration,
}

#[async_trait]
impl Executor for RemoteExecutor {
    async fn run(&self, job_id: &str, spec: &JobSpec, out: &Path) -> Result<JobResult> {
        tokio::fs::create_dir_all(out).await?;
        let (closed, mut done) = self.mailbox.open(spec, out)?;
        let link = format!(
            "{}/jobs/{}",
            self.base_url.trim_end_matches('/'),
            closed.token
        );
        let job = self.launch.launch(job_id, &link).await?;
        let ended = tokio::select! {
            _ = &mut done => None,
            ended = job.ended() => Some(ended),
            _ = tokio::time::sleep(self.timeout) => bail!("the job did not finish in {:?}", self.timeout),
        };
        drop(job);
        if let Some(ended) = ended {
            ended?;
            // The job reports done before it exits.
            if done.try_recv().is_err() {
                bail!("the job ended without reporting its result");
            }
        }
        crate::read_result(out, "").await
    }
}

const EXECUTION_POLL: Duration = Duration::from_secs(10);
/// Failed polls in a row before the job counts as lost: a minute.
const MAX_FAILED_POLLS: u32 = 6;

/// Polls a job until it ends. One failed poll says nothing of the job, which runs on; after
/// `MAX_FAILED_POLLS` in a row it counts as lost, and its caller drops it, since it may still run.
pub(crate) async fn watch<F>(mut poll: impl FnMut() -> F) -> Result<()>
where
    F: Future<Output = Result<Option<Result<()>>>>,
{
    let mut failed = 0;
    loop {
        tokio::time::sleep(EXECUTION_POLL).await;
        match poll().await {
            Ok(None) => failed = 0,
            Ok(Some(ended)) => return ended,
            Err(e) => {
                failed += 1;
                if failed == MAX_FAILED_POLLS {
                    return Err(e.context("read the job's state"));
                }
                tracing::warn!(error = %format!("{e:#}"), "could not read the job's state");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{BUNDLE_FILE, JobOutcome, RESULT_FILE, REVIEW_FILE, ResolverSpec};

    fn spec() -> JobSpec {
        JobSpec {
            repo: "o/r".into(),
            clone_url: String::new(),
            token: "t".into(),
            head_ref: "h".into(),
            head_sha: "1".into(),
            trunk: "main".into(),
            trunk_sha: "2".into(),
            base_ref: None,
            upstream: None,
            pr_title: String::new(),
            pr_body: String::new(),
            verify: Vec::new(),
            resolver: ResolverSpec {
                provider: Default::default(),
                model: String::new(),
                effort: String::new(),
                timeout_secs: 1,
                max_turns: 1,
                api: None,
            },
            task: Default::default(),
        }
    }

    /// Runs the job in this process, as `mergepatrol job --remote` would: fetches the spec,
    /// tries a second fetch, and uploads a bundle in several chunks.
    #[derive(Clone, Copy)]
    enum Behave {
        Report,
        Exit,
        Hang,
    }

    struct InProcess {
        behave: Behave,
        freed: Arc<Mutex<bool>>,
    }

    struct Task(tokio::task::JoinHandle<Result<()>>, Arc<Mutex<bool>>);

    #[async_trait]
    impl Launch for InProcess {
        async fn launch(&self, _: &str, link: &str) -> Result<Box<dyn Launched>> {
            let (link, behave) = (link.to_owned(), self.behave);
            let task = tokio::spawn(async move {
                let spec = fetch_spec(&link).await?;
                assert_eq!(spec.head_sha, "1");
                assert!(fetch_spec(&link).await.is_err(), "the spec is single-use");
                match behave {
                    Behave::Report => {}
                    Behave::Exit => return Ok(()),
                    Behave::Hang => std::future::pending().await,
                }
                let dir = tempfile::tempdir()?;
                tokio::fs::write(dir.path().join(BUNDLE_FILE), b"0123456789").await?;
                tokio::fs::write(dir.path().join(REVIEW_FILE), b"{}").await?;
                let result = JobResult {
                    outcome: JobOutcome::Done,
                    events: vec![],
                };
                tokio::fs::write(dir.path().join(RESULT_FILE), serde_json::to_vec(&result)?)
                    .await?;
                upload(&link, dir.path(), 3).await
            });
            Ok(Box::new(Task(task, self.freed.clone())))
        }
    }

    #[async_trait]
    impl Launched for Task {
        async fn ended(&self) -> Result<()> {
            while !self.0.is_finished() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(())
        }
    }

    impl Drop for Task {
        fn drop(&mut self) {
            *self.1.lock().unwrap() = true;
        }
    }

    async fn executor(behave: Behave) -> (RemoteExecutor, Arc<Mutex<bool>>) {
        let mailbox = Arc::new(Mailbox::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(axum::serve(listener, mailbox.clone().router()).into_future());
        let freed = Arc::new(Mutex::new(false));
        (
            RemoteExecutor {
                mailbox,
                base_url,
                launch: Box::new(InProcess {
                    behave,
                    freed: freed.clone(),
                }),
                timeout: Duration::from_secs(10),
            },
            freed,
        )
    }

    #[tokio::test]
    async fn a_remote_job_fetches_its_spec_once_and_uploads_its_output() {
        let (ex, freed) = executor(Behave::Report).await;
        let out = tempfile::tempdir().unwrap();
        let result = ex.run("j", &spec(), out.path()).await.unwrap();
        assert_eq!(result.outcome, JobOutcome::Done);
        assert_eq!(
            std::fs::read(out.path().join(BUNDLE_FILE)).unwrap(),
            b"0123456789"
        );
        assert_eq!(std::fs::read(out.path().join(REVIEW_FILE)).unwrap(), b"{}");
        assert!(
            ex.mailbox.slots.lock().unwrap().is_empty(),
            "the link is closed"
        );
        assert!(*freed.lock().unwrap(), "the job's machine is freed");
    }

    #[tokio::test]
    async fn a_job_that_ends_without_its_result_fails() {
        let (ex, _) = executor(Behave::Exit).await;
        let out = tempfile::tempdir().unwrap();
        let err = ex.run("j", &spec(), out.path()).await.unwrap_err();
        assert!(err.to_string().contains("without reporting"), "{err}");
    }

    #[tokio::test]
    async fn an_upload_past_the_limit_is_refused_as_it_comes() {
        let mailbox = Arc::new(Mailbox {
            slots: Mutex::default(),
            limit: 12,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(axum::serve(listener, mailbox.clone().router()).into_future());
        let out = tempfile::tempdir().unwrap();
        let (closed, _done) = mailbox.open(&spec(), out.path()).unwrap();
        let url = format!("{base}/jobs/{}/files/{BUNDLE_FILE}", closed.token);
        let post = |body: &'static [u8]| reqwest::Client::new().post(&url).body(body).send();
        assert_eq!(post(b"0123456789").await.unwrap().status(), 204);
        assert_eq!(post(b"abcde").await.unwrap().status(), 413);
        assert_eq!(
            std::fs::read(out.path().join(BUNDLE_FILE)).unwrap(),
            b"0123456789"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn failed_polls_are_retried_until_the_job_counts_as_lost() {
        // Fails `failures` polls, then says the job succeeded.
        async fn watched(failures: u32) -> Result<()> {
            let polls = std::sync::atomic::AtomicU32::new(0);
            let poll = || async {
                match polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < failures {
                    true => bail!("503 Service Unavailable"),
                    false => Ok(Some(Ok(()))),
                }
            };
            watch(poll).await
        }
        let ended = watched(MAX_FAILED_POLLS - 1).await;
        assert!(ended.is_ok(), "{ended:?}");
        let ended = watched(MAX_FAILED_POLLS).await;
        assert!(ended.is_err(), "{ended:?}");
    }

    #[tokio::test]
    async fn a_job_cut_off_is_freed() {
        let (ex, freed) = executor(Behave::Hang).await;
        let out = tempfile::tempdir().unwrap();
        let spec = spec();
        // The queue stops: the job's future is dropped while it waits.
        let run = ex.run("j", &spec, out.path());
        assert!(
            tokio::time::timeout(Duration::from_millis(300), run)
                .await
                .is_err()
        );
        assert!(*freed.lock().unwrap());
    }
}
