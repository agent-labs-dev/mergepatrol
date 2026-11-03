//! Reads a real PR as a user, the way the dashboard's detail pane does: its timeline, checks and
//! diff. Needs a user token and any PR the user can read, with checks and a few commits:
//! `LIVE_TOKEN=$(gh auth token) LIVE_REPO=cli/cli LIVE_PR=14858 \
//! cargo test -p mergepatrol-github --test pull_detail_live -- --ignored a_user_reads`
//!
//! The images test needs a PR in a private repo with images of the repo's own files:
//! `LIVE_TOKEN=$(gh auth token) LIVE_REPO=acme/private LIVE_PR=12 \
//! cargo test -p mergepatrol-github --test pull_detail_live -- --ignored a_private_repos`

use mergepatrol_core::RepoName;
use mergepatrol_github::{AsUser, TimelineEvent};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

#[tokio::test]
#[ignore]
async fn a_user_reads_a_pr_and_its_diffs() {
    let gh = AsUser::new(env("LIVE_TOKEN")).unwrap();
    let repo = RepoName::parse(&env("LIVE_REPO")).unwrap();
    let pr: u64 = env("LIVE_PR").parse().unwrap();

    let detail = gh.pull_detail(&repo, pr).await.unwrap();
    assert!(!detail.checks.is_empty(), "no checks");
    let commits: Vec<&str> = detail
        .timeline
        .iter()
        .filter_map(|t| match &t.event {
            TimelineEvent::Commit { sha, .. } => Some(sha.as_str()),
            _ => None,
        })
        .collect();
    assert!(!commits.is_empty(), "no commits in {:?}", detail.timeline);

    let whole = gh.pull_diff(&repo, pr, None).await.unwrap().unwrap();
    assert!(whole.starts_with("diff --git "), "{whole:.200}");
    let one = gh
        .pull_diff(&repo, pr, Some(commits[0]))
        .await
        .unwrap()
        .unwrap();
    assert!(one.starts_with("diff --git "), "{one:.200}");
}

#[tokio::test]
#[ignore]
async fn a_private_repos_images_load_without_a_session() {
    let gh = AsUser::new(env("LIVE_TOKEN")).unwrap();
    let repo = RepoName::parse(&env("LIVE_REPO")).unwrap();
    let pr: u64 = env("LIVE_PR").parse().unwrap();

    let detail = gh.pull_detail(&repo, pr).await.unwrap();
    let html = detail.body_html;
    let unsigned = format!(" src=\"https://github.com/{repo}/");
    assert!(!html.contains(&unsigned), "an image was not signed");
    let signed = format!(" src=\"https://raw.githubusercontent.com/{repo}/");
    let images: Vec<_> = html.match_indices(&signed).collect();
    assert!(!images.is_empty(), "no images");
    for (i, _) in images {
        let src = &html[i + " src=\"".len()..];
        let src = src[..src.find('"').unwrap()].replace("&amp;", "&");
        let res = reqwest::get(&src).await.unwrap();
        assert!(res.status().is_success(), "{}", res.status());
        let kind = res.headers()["content-type"].to_str().unwrap();
        assert!(kind.starts_with("image/"), "{kind}");
    }
}
