//! Failures that fall on different cases in each run hide different things: B must not win on
//! what its failures hid and A's failures cost A.

use crate::score::{Scored, Verdict, compare};

fn scored(id: &str, caught: &[bool], false_positives: usize, failed: bool) -> Scored {
    Scored {
        id: id.into(),
        caught: caught.to_vec(),
        false_positives: vec!["fp".into(); false_positives],
        unmatched: Vec::new(),
        failed,
        cost_usd: Some(1.0),
    }
}

#[test]
fn a_run_is_not_kept_for_what_failed_reviews_hid_on_either_side() {
    // A's review of the bug case failed; it sent one change back on the clean case.
    let a = [
        scored("bug", &[false], 0, true),
        scored("clean", &[], 1, false),
    ];
    // B caught the bug, and its review of the clean case failed. Same prompt quality shown:
    // none, since each run failed the case the other is scored on.
    let b = [
        scored("bug", &[true], 0, false),
        scored("clean", &[], 0, true),
    ];
    assert_eq!(compare(&a, &b).verdict, Verdict::Drop);
}
