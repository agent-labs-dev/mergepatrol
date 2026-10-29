//! Signs in to a real ChatGPT plan, then runs the real Codex CLI on a small conflict through the
//! orchestrator's proxy, against the ChatGPT backend. Needs `codex` on PATH. Prints a code to
//! enter at auth.openai.com, then keeps the login in CHATGPT_LIVE_LOGIN (by default in the temp
//! dir) for the next run. CHATGPT_LIVE_MODEL picks the model, one the plan serves, and
//! CHATGPT_LIVE_RENEW=1 renews the login first.
//! `cargo test -p mergepatrol --test chatgpt_live -- --ignored --nocapture`

use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use mergepatrol_core::Provider;
use mergepatrol_executor::proxy::{Api, ApiProxy, Credential, Upstreams};
use mergepatrol_resolver::{
    ChatGptAuth, ChatGptLogin, CodexResolver, Outcome, ResolveRequest, Resolver,
};

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok || args[0] == "rebase", "git {args:?}");
}

/// The login from the last run, renewed if it must be, or a new one.
async fn login(auth: &ChatGptAuth, path: &Path) -> ChatGptLogin {
    let login = match std::fs::read_to_string(path) {
        Ok(json) => {
            let login: ChatGptLogin = serde_json::from_str(&json).unwrap();
            let renew = std::env::var("CHATGPT_LIVE_RENEW").is_ok_and(|v| v == "1");
            match renew || login.expires_within(Duration::from_secs(3600)) {
                true => auth.refresh(&login).await.unwrap(),
                false => return login,
            }
        }
        Err(_) => {
            let device = auth.start().await.unwrap();
            eprintln!(
                "\n>>> Open {} and enter the code {}\n",
                device.verification_url, device.user_code
            );
            loop {
                tokio::time::sleep(Duration::from_secs(device.interval_secs)).await;
                if let Some(login) = auth.poll(&device).await.unwrap() {
                    break login;
                }
            }
        }
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    serde_json::to_writer(&mut file, &login).unwrap();
    login
}

#[tokio::test]
#[ignore = "needs a ChatGPT plan and the codex CLI"]
async fn codex_resolves_a_conflict_on_a_chatgpt_plan() {
    let path = std::env::var("CHATGPT_LIVE_LOGIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("mergepatrol-chatgpt-live.json"));
    let login = login(&ChatGptAuth::default(), &path).await;
    eprintln!("signed in as {}", login.hint());

    let proxy = Arc::new(ApiProxy::new(Upstreams::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(axum::serve(listener, proxy.clone().router()).into_future());
    let credential = Credential::ChatGpt {
        access_token: login.access_token.clone().into(),
        account_id: login.account_id.clone(),
    };
    let granted = proxy.grant(Provider::OpenAi, credential, 100);

    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path();
    let write = |s: &str| std::fs::write(wt.join("greet.py"), s).unwrap();
    git(wt, &["init", "-q", "-b", "main"]);
    write("def greet(name):\n    return 'Hello ' + name\n");
    git(wt, &["add", "."]);
    git(wt, &["commit", "-q", "-m", "root"]);
    git(wt, &["checkout", "-q", "-b", "pr"]);
    write("def greet(name):\n    return 'Hello, ' + name + '!'\n");
    git(
        wt,
        &["commit", "-q", "-am", "Add punctuation to the greeting"],
    );
    git(wt, &["checkout", "-q", "main"]);
    write("def greet(person):\n    return 'Hello ' + person\n");
    git(wt, &["commit", "-q", "-am", "Rename name to person"]);
    git(wt, &["checkout", "-q", "pr"]);
    git(wt, &["rebase", "-q", "main"]);

    let resolver = CodexResolver {
        provider: Provider::OpenAi,
        api_key: Some(granted.token.clone()),
        base_url: Api::Responses.base_url(&proxy_url),
        model: std::env::var("CHATGPT_LIVE_MODEL").unwrap_or_else(|_| "gpt-6.1-sol".into()),
        effort: "low".into(),
        timeout: Duration::from_secs(300),
        max_turns: 20,
    };
    let files = vec!["greet.py".to_owned()];
    let transcript = wt.join("transcript.jsonl");
    let outcome = resolver
        .resolve(ResolveRequest {
            workdir: wt,
            conflicted_files: &files,
            pr_title: "Friendlier greeting",
            pr_body: "Adds a comma and an exclamation mark.",
            commit_message: "Add punctuation to the greeting",
            trunk: "main",
            verify_commands: &[],
            transcript: &transcript,
        })
        .await
        .unwrap();

    let result = std::fs::read_to_string(wt.join("greet.py")).unwrap();
    println!("{outcome:?}\n{result}");
    if outcome.outcome != Outcome::Resolved {
        println!(
            "{}",
            std::fs::read_to_string(&transcript).unwrap_or_default()
        );
    }
    assert_eq!(outcome.outcome, Outcome::Resolved);
    assert!(!result.contains("<<<<<<<"));
    assert!(result.contains("person") && result.contains("!"));
}
