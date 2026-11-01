//! Publishes a real draft PR as a user, the way the dashboard does. Needs a user token, and a
//! draft PR nothing needs: `LIVE_TOKEN=$(gh auth token) LIVE_REPO=owner/name LIVE_PR=1 \
//! cargo test -p mergepatrol-github --test ready_live -- --ignored`

use mergepatrol_github::setup::ready_for_review_as;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

#[tokio::test]
#[ignore]
async fn a_user_publishes_a_draft() {
    let (token, repo) = (env("LIVE_TOKEN"), env("LIVE_REPO"));
    let pr: u64 = env("LIVE_PR").parse().unwrap();

    ready_for_review_as(&token, &repo, pr).await.unwrap();
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
    assert_eq!(res["draft"], false);
    // Publishing a ready PR again is no error.
    ready_for_review_as(&token, &repo, pr).await.unwrap();
}
