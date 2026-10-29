//! Labels a real PR as a user, the way the dashboard queues one. Needs a user token, and an open
//! PR to label in a repo the user can triage: not one Mergepatrol runs on, as it uses its own
//! labels only. `LIVE_TOKEN=$(gh auth token) LIVE_REPO=owner/name LIVE_PR=1 \
//! cargo test -p mergepatrol-github --test labels_live -- --ignored`

use mergepatrol_github::setup::{add_labels_as, remove_label_as};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

/// The PR's labels, as GitHub has them now.
async fn labels(token: &str, repo: &str, pr: u64) -> Vec<String> {
    let res: serde_json::Value = reqwest::Client::new()
        .get(format!(
            "https://api.github.com/repos/{repo}/issues/{pr}/labels"
        ))
        .bearer_auth(token)
        .header("user-agent", "mergepatrol-test")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names = res.as_array().unwrap().iter();
    names
        .map(|l| l["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
#[ignore]
async fn a_user_labels_a_pr_and_takes_the_labels_off() {
    let (token, repo) = (env("LIVE_TOKEN"), env("LIVE_REPO"));
    let pr: u64 = env("LIVE_PR").parse().unwrap();
    // A space and a colon, as a repo's own trigger label may have: the path must encode them.
    let [trigger, priority] = ["live test", "live test:priority"];

    add_labels_as(&token, &repo, pr, &[trigger, priority])
        .await
        .unwrap();
    let on = labels(&token, &repo, pr).await;
    assert!(
        on.iter().any(|l| l == trigger) && on.iter().any(|l| l == priority),
        "{on:?}"
    );

    for label in [trigger, priority] {
        remove_label_as(&token, &repo, pr, label).await.unwrap();
    }
    let off = labels(&token, &repo, pr).await;
    assert!(!off.iter().any(|l| l.starts_with(trigger)), "{off:?}");
    // Gone already: no error.
    remove_label_as(&token, &repo, pr, trigger).await.unwrap();
}
