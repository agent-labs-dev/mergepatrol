//! A push can fix a change referred to a human before that person answers.

use std::collections::BTreeMap;

use mergepatrol_core::{Finding, Judged, Outcome, carry_back, settled_by};
use serde_json::json;

fn asked_change() -> Judged {
    serde_json::from_value(json!({
        "id": "f1",
        "kind": "other",
        "title": "Retry never stops",
        "confidence": 100,
        "lean": "change",
        "finding": "The retry loop has no bound.",
        "sent_back": false,
        "asked": true,
        "bar": 80
    }))
    .unwrap()
}

#[test]
fn the_next_review_settles_a_fix_before_the_human_answers() {
    let fixed: Finding = serde_json::from_value(json!({
        "id": "f2",
        "kind": "other",
        "title": "Retry never stops",
        "confidence": 100,
        "lean": "fine",
        "finding": "The new head bounds the retry loop.",
        "settled": "fixed"
    }))
    .unwrap();

    assert_eq!(
        settled_by(&[asked_change()], &BTreeMap::new(), &[fixed]),
        [("f1".to_owned(), Outcome::Fixed)],
        "the change counted in review stats must stop counting as open after an explicit fix"
    );
}

#[test]
fn a_later_outcome_also_settles_an_unanswered_earlier_request() {
    let mut earlier = [asked_change()];
    carry_back(
        &mut earlier,
        &BTreeMap::new(),
        &[("Retry never stops".to_owned(), Outcome::Fixed)],
    );
    assert_eq!(earlier[0].outcome, Some(Outcome::Fixed));
}
