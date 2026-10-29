//! Runs the real agent CLIs against the proxy, with a stub for the model API. Needs `claude` and
//! `codex` on PATH: `cargo test -p mergepatrol-executor --test proxy_cli -- --ignored`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::routing::post;
use mergepatrol_core::Provider;
use mergepatrol_executor::proxy::{Api, ApiProxy, Upstreams};

/// A stub model API that records the path and the credential of each request, and refuses it,
/// so the CLI gives up.
async fn stub() -> (String, Arc<Mutex<Vec<(String, Option<String>)>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    let url = serve(
        Router::new().fallback(post(move |uri: Uri, headers: HeaderMap| async move {
            let credential = ["x-api-key", "authorization"]
                .iter()
                .find_map(|h| headers.get(*h))
                .map(|v| v.to_str().unwrap().to_owned());
            record
                .lock()
                .unwrap()
                .push((uri.path().to_owned(), credential));
            (
                StatusCode::BAD_REQUEST,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"stub"}}"#,
            )
        })),
    )
    .await;
    (url, seen)
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(axum::serve(listener, app).into_future());
    url
}

#[tokio::test]
#[ignore = "needs the claude CLI"]
async fn claude_reaches_the_api_with_the_key_through_the_proxy() {
    let (upstream, seen) = stub().await;
    let proxy = Arc::new(ApiProxy::new(Upstreams {
        anthropic: upstream,
        ..Upstreams::default()
    }));
    let base = format!(
        "{}{}",
        serve(proxy.clone().router()).await,
        Api::Messages.prefix()
    );
    let granted = proxy.grant(Provider::Anthropic, "sk-ant-real".into(), 10);

    let home = tempfile::tempdir().unwrap();
    let run = tokio::process::Command::new("claude")
        .current_dir(home.path())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("HOME", home.path())
        .env("ANTHROPIC_API_KEY", &granted.token)
        .env("ANTHROPIC_BASE_URL", &base)
        .args(["-p", "hi", "--bare", "--no-session-persistence"])
        .args(["--model", "claude-sonnet-5-5"])
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(Duration::from_secs(60), run)
        .await
        .unwrap()
        .unwrap();

    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "the CLI never reached the API");
    for (path, key) in seen.iter() {
        assert_eq!(path, "/v1/messages");
        assert_eq!(key.as_deref(), Some("sk-ant-real"));
    }
}

#[tokio::test]
#[ignore = "needs the codex CLI"]
async fn codex_reaches_the_api_with_the_key_through_the_proxy() {
    let (upstream, seen) = stub().await;
    let proxy = Arc::new(ApiProxy::new(Upstreams {
        openai: upstream,
        ..Upstreams::default()
    }));
    let base = format!(
        "{}{}/v1",
        serve(proxy.clone().router()).await,
        Api::Responses.prefix()
    );
    let granted = proxy.grant(Provider::OpenAi, "sk-openai-real".into(), 10);

    let home = tempfile::tempdir().unwrap();
    let provider = |k: &str, v: &str| ["-c".to_owned(), format!("model_providers.mp.{k}={v}")];
    let run = tokio::process::Command::new("codex")
        .current_dir(home.path())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("HOME", home.path())
        .env("CODEX_HOME", home.path())
        .env("MP_TOKEN", &granted.token)
        .args(["exec", "--json", "--ephemeral", "--ignore-user-config"])
        .arg("--skip-git-repo-check")
        .args(["-c", "model_provider=mp"])
        .args(provider("name", "\"mp\""))
        .args(provider("base_url", &format!("\"{base}\"")))
        .args(provider("env_key", "\"MP_TOKEN\""))
        .args(provider("wire_api", "\"responses\""))
        .args(provider("request_max_retries", "0"))
        .args(provider("stream_max_retries", "0"))
        .args(["-m", "gpt-6.1-sol", "hi"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(Duration::from_secs(60), run)
        .await
        .unwrap()
        .unwrap();

    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "the CLI never reached the API");
    for (path, auth) in seen.iter() {
        assert_eq!(path, "/v1/responses");
        assert_eq!(auth.as_deref(), Some("Bearer sk-openai-real"));
    }
}
