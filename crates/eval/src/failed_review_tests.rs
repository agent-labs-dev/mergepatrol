//! A review that failed shows nothing about the prompt: it must not win a comparison.

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
fn a_run_that_fails_on_clean_cases_is_not_kept_for_fewer_false_positives() {
    // A reviews every case, and sends one change back on the clean one.
    let a = [
        scored("bug", &[true], 0, false),
        scored("clean", &[], 1, false),
    ];
    // B catches the same bug, but its review of the clean case failed: it reviewed nothing.
    let b = [
        scored("bug", &[true], 0, false),
        scored("clean", &[], 0, true),
    ];
    assert_eq!(compare(&a, &b).verdict, Verdict::Drop);
}
