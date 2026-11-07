//! A local review's security panel needs every place the recurring claim is about, just as
//! MergeLoop's hosted panel does.

use std::path::Path;

use mergepatrol_resolver::{ReviewRequest, Scope, Verify, author_review_brief};

#[test]
fn the_authors_panel_is_told_to_include_the_other_places_in_its_claim() {
    let req = ReviewRequest {
        workdir: Path::new("."),
        base: "base",
        pr_title: "t",
        pr_body: "b",
        trunk: "main",
        answered: &[],
        sent_back: &[],
        scope: Scope::Whole,
        verify: &Verify::None,
        transcript: Path::new("t"),
    };
    let brief = author_review_brief(&req, "o/r", 5, "head");
    let panel = brief
        .split_once("Before you post, have fresh agents check")
        .expect("the brief has a panel step")
        .1
        .split_once("Once you have the JSON")
        .unwrap()
        .0;
    // The review prompt describes also_at, but each fresh verifier gets only the claim that
    // this separate panel step tells the author to construct.
    assert!(
        panel.contains("also_at"),
        "the panel's claim instructions omit also_at, so the fresh verifiers receive only the first place:\n{panel}"
    );
}
