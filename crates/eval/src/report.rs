//! A run's scores, and a comparison of two runs, as text.

use std::fmt::Write;

use crate::score::{Comparison, Scored, Totals, Verdict};

pub fn run(label: &str, scores: &[Scored]) -> String {
    let mut out = format!("{label}\n");
    for s in scores {
        let what = if s.failed {
            "failed".to_owned()
        } else if s.caught.is_empty() {
            match s.false_positives.len() {
                0 => "clean, nothing sent back".to_owned(),
                n => format!("clean, {n} sent back: {}", s.false_positives.join("; ")),
            }
        } else {
            let caught = s.caught.iter().filter(|c| **c).count();
            format!("caught {caught}/{}", s.caught.len())
        };
        let _ = writeln!(out, "  {:<40} {what}", s.id);
    }
    out.push_str(&totals(&Totals::of(scores)));
    out
}

fn totals(t: &Totals) -> String {
    let recall = t
        .recall()
        .map_or("-".to_owned(), |r| format!("{:.0}%", r * 100.0));
    format!(
        "recall {recall} ({}/{} bugs), false positives {} on {}/{} clean PRs, {} other changes \
sent back, {} failed, ${:.2} over {} cases\n",
        t.caught,
        t.bugs,
        t.false_positives,
        t.clean_flagged,
        t.clean,
        t.unmatched,
        t.failed,
        t.cost_usd,
        t.cases,
    )
}

pub fn comparison(a: &str, b: &str, c: &Comparison) -> String {
    let mut out = format!("A {a}: {}B {b}: {}", totals(&c.a), totals(&c.b));
    for (id, i) in &c.gained {
        let _ = writeln!(out, "  + {id} bug {}", i + 1);
    }
    for (id, i) in &c.lost {
        let _ = writeln!(out, "  - {id} bug {}", i + 1);
    }
    if !c.left_out.is_empty() {
        let _ = writeln!(out, "left out, in one run only: {}", c.left_out.join(", "));
    }
    out.push_str(match c.verdict {
        Verdict::Keep => "verdict: keep B\n",
        Verdict::Drop => "verdict: drop B\n",
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::compare;

    fn scored(id: &str, caught: &[bool], fps: &[&str], failed: bool) -> Scored {
        Scored {
            id: id.into(),
            caught: caught.to_vec(),
            false_positives: fps.iter().map(|s| s.to_string()).collect(),
            unmatched: Vec::new(),
            failed,
            cost_usd: Some(0.25),
        }
    }

    #[test]
    fn a_run_reads_case_by_case_then_in_total() {
        let text = run(
            "main",
            &[
                scored("bug-a", &[true, false], &[], false),
                scored("clean-b", &[], &["Off by one"], false),
                scored("clean-c", &[], &[], false),
                scored("bug-d", &[false], &[], true),
            ],
        );
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        assert_eq!(lines[0], "main");
        assert!(lines[1].starts_with("  bug-a") && lines[1].ends_with("caught 1/2"));
        assert!(lines[2].ends_with("clean, 1 sent back: Off by one"));
        assert!(lines[3].ends_with("clean, nothing sent back"));
        assert!(lines[4].ends_with("failed"));
        assert_eq!(
            lines[5],
            "recall 33% (1/3 bugs), false positives 1 on 1/2 clean PRs, 0 other changes sent \
back, 1 failed, $1.00 over 4 cases"
        );
    }

    #[test]
    fn a_comparison_names_each_bug_won_or_lost_and_the_verdict() {
        let a = [
            scored("x", &[true, false], &[], false),
            scored("y", &[], &[], false),
        ];
        let b = [
            scored("x", &[false, true], &[], false),
            scored("z", &[], &[], false),
        ];
        let text = comparison("main", "try", &compare(&a, &b));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("A main: recall 50% (1/2 bugs)"));
        assert!(lines[1].starts_with("B try: recall 50% (1/2 bugs)"));
        assert_eq!(
            lines[2..],
            [
                "  + x bug 2",
                "  - x bug 1",
                "left out, in one run only: y, z",
                "verdict: drop B"
            ]
        );
        let keep = [scored("x", &[true, true], &[], false)];
        assert!(comparison("a", "b", &compare(&a[..1], &keep)).ends_with("verdict: keep B\n"));
    }
}
