//! The panel the author's agent runs checks the PR's head, as MergeLoop's own panel does: its
//! subagents start wherever the agent runs, not in the review's checkout.

use std::path::Path;

use mergepatrol_resolver::{ReviewRequest, Verify, author_review_brief};

#[test]
fn the_authors_panel_checks_a_checkout_of_the_prs_head() {
    let req = ReviewRequest {
        workdir: Path::new("."),
        base: "$(git merge-base origin/main HEAD)",
        pr_title: "t",
        pr_body: "b",
        trunk: "main",
        answered: &[],
        sent_back: &[],
        queue_commits: &[],
        verify: &Verify::None,
        transcript: Path::new("t"),
    };
    let head = "4176990841769908c8b27a81f8de13f270104026";
    let brief = author_review_brief(&req, "o/r", 5, head);
    let panel = brief
        .split_once("Before you post, have fresh agents check")
        .expect("the brief has a panel step")
        .1;
    let panel = panel.split_once("Once you have the JSON").unwrap().0;
    // The review's own checkout is the one the brief says to remove once done, and the panel's
    // `git diff <base>..HEAD` reads whatever HEAD is where each subagent runs.
    assert!(
        panel.contains(head),
        "the panel's subagents are never told to check out the PR's head:\n{panel}"
    );
}
