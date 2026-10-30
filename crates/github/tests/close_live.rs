//! Closes a real PR as a user, the way the dashboard closes one. Needs a user token, and an open
//! PR to close that nothing needs: `LIVE_TOKEN=$(gh auth token) LIVE_REPO=owner/name LIVE_PR=1 \
//! cargo test -p mergepatrol-github --test close_live -- --ignored`

use mergepatrol_github::setup::close_pull_as;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

#[tokio::test]
#[ignore]
async fn a_user_closes_a_pr() {
    let (token, repo) = (env("LIVE_TOKEN"), env("LIVE_REPO"));
    let pr: u64 = env("LIVE_PR").parse().unwrap();

    close_pull_as(&token, &repo, pr).await.unwrap();
    let res: serde_json::Value = reqwest::Client::new()
        .get(format!("https://api.github.com/repos/{repo}/pulls/{pr}"))
        .bearer_auth(&token)
        .header("user-agent", "mergepatrol-test")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["state"], "closed");
    // Closing a closed PR again is no error.
    close_pull_as(&token, &repo, pr).await.unwrap();
}
