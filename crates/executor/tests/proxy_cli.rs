//! Runs the real Claude CLI against the proxy, with a stub for the Anthropic API. Needs `claude`
//! on PATH: `cargo test -p mergepatrol-executor --test proxy_cli -- --ignored`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::routing::post;
use mergepatrol_executor::proxy::{AnthropicProxy, PREFIX};

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(axum::serve(listener, app).into_future());
    url
}

#[tokio::test]
#[ignore = "needs the claude CLI"]
async fn the_cli_reaches_the_api_with_the_key_through_the_proxy() {
    // The stub records what reached it, and refuses it, so the CLI gives up.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    let upstream = serve(Router::new().fallback(post(
        move |uri: Uri, headers: HeaderMap| async move {
            let key = headers
                .get("x-api-key")
                .map(|v| v.to_str().unwrap().to_owned());
            record.lock().unwrap().push((uri.path().to_owned(), key));
            (
                StatusCode::BAD_REQUEST,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"stub"}}"#,
            )
        },
    )))
    .await;
    let proxy = Arc::new(AnthropicProxy::new(&upstream));
    let base = format!("{}{PREFIX}", serve(proxy.clone().router()).await);
    let granted = proxy.grant("sk-ant-real".into(), 10);

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
