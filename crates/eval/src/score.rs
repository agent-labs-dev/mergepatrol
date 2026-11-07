//! How a review of a case scores: which expected bugs a change MergeLoop sends back caught, and
//! which changes it sent back on a clean PR. Pure: the run is done before this reads it.

use mergepatrol_core::{Finding, ReviewSettings, Route};
use serde::{Deserialize, Serialize};

use crate::case::{Bug, Case, Expected, Sha};

/// How far, in lines, a finding may sit from a bug's lines and still be about it.
pub const NEAR: u32 = 5;

/// One case's review in a run, as the run file keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaseRun {
    pub id: String,
    /// The head it reviewed: a case that has moved on since is not scored on this run.
    pub head: Sha,
    pub reviewed: Reviewed,
    /// `None` when the agent's CLI did not say.
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reviewed {
    Done {
        summary: String,
        findings: Vec<Finding>,
    },
    /// The job or the agent failed: it caught nothing, and cleared nothing.
    Failed { reason: String },
}

/// What one case's review scored.
#[derive(Clone, Debug, PartialEq)]
pub struct Scored {
    pub id: String,
    /// One per expected bug, in the case's order: whether a change sent back was about it.
    /// Empty for a clean case.
    pub caught: Vec<bool>,
    /// The titles of the changes sent back on a clean case.
    pub false_positives: Vec<String>,
    /// The titles of the changes sent back on a case with bugs that were about none of them.
    pub unmatched: Vec<String>,
    pub failed: bool,
    pub cost_usd: Option<f64>,
}

/// Whether a finding is about a bug: in a file it lives in, within `NEAR` lines of it there.
pub fn about(bug: &Bug, finding: &Finding) -> bool {
    let (Some(file), Some(line)) = (&finding.file, finding.line) else {
        return false;
    };
    let file = file.trim_start_matches("./");
    bug.at.iter().any(|p| {
        let near = p.lines.start().saturating_sub(NEAR)..=p.lines.end() + NEAR;
        file == p.file && near.contains(&line)
    })
}

/// Scores a case's review, as MergeLoop routes its findings under `settings`.
pub fn score(case: &Case, run: &CaseRun, settings: &ReviewSettings) -> Scored {
    let sent: Vec<&Finding> = match &run.reviewed {
        Reviewed::Done { findings, .. } => findings
            .iter()
            .filter(|f| settings.route(f) == Route::Author)
            .collect(),
        Reviewed::Failed { .. } => Vec::new(),
    };
    let titles = |fs: Vec<&Finding>| fs.into_iter().map(|f| f.title.clone()).collect();
    let (caught, false_positives, unmatched) = match &case.expected {
        Expected::Clean => (Vec::new(), titles(sent), Vec::new()),
        Expected::Bugs(bugs) => {
            let caught = bugs
                .iter()
                .map(|b| sent.iter().any(|f| about(b, f)))
                .collect();
            let other = sent
                .into_iter()
                .filter(|f| !bugs.iter().any(|b| about(b, f)))
                .collect();
            (caught, Vec::new(), titles(other))
        }
    };
    Scored {
        id: case.id.clone(),
        caught,
        false_positives,
        unmatched,
        failed: matches!(run.reviewed, Reviewed::Failed { .. }),
        cost_usd: run.cost_usd,
    }
}

/// Each case's score on a run, in the cases' order. A case the run did not review, or reviewed
/// at another head, is left out.
pub fn scores(cases: &[Case], runs: &[CaseRun], settings: &ReviewSettings) -> Vec<Scored> {
    cases
        .iter()
        .filter_map(|c| {
            let run = runs.iter().find(|r| r.id == c.id && r.head == c.head)?;
            Some(score(c, run, settings))
        })
        .collect()
}

/// A run's scores, added up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub cases: usize,
    pub bugs: usize,
    pub caught: usize,
    pub clean: usize,
    /// Clean cases with at least one change sent back.
    pub clean_flagged: usize,
    pub false_positives: usize,
    pub unmatched: usize,
    pub failed: usize,
    pub cost_usd: f64,
}

impl Totals {
    pub fn of(scores: &[Scored]) -> Totals {
        let mut t = Totals::default();
        for s in scores {
            let clean = s.caught.is_empty();
            t.cases += 1;
            t.bugs += s.caught.len();
            t.caught += s.caught.iter().filter(|c| **c).count();
            t.clean += usize::from(clean);
            t.clean_flagged += usize::from(clean && !s.false_positives.is_empty());
            t.false_positives += s.false_positives.len();
            t.unmatched += s.unmatched.len();
            t.failed += usize::from(s.failed);
            t.cost_usd += s.cost_usd.unwrap_or(0.0);
        }
        t
    }

    /// The share of expected bugs caught, `None` with no bugs to catch.
    pub fn recall(&self) -> Option<f64> {
        (self.bugs > 0).then(|| self.caught as f64 / self.bugs as f64)
    }
}

/// Whether run B beats run A on the cases both reviewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It catches more and sends back no more false positives, or catches as much and sends
    /// back fewer.
    Keep,
    Drop,
}

/// Run B against run A, case by case.
#[derive(Clone, Debug, PartialEq)]
pub struct Comparison {
    pub a: Totals,
    pub b: Totals,
    /// `(case, bug index)` that B caught and A missed.
    pub gained: Vec<(String, usize)>,
    /// `(case, bug index)` that A caught and B missed.
    pub lost: Vec<(String, usize)>,
    /// Cases only one run scored, left out of both totals.
    pub left_out: Vec<String>,
    pub verdict: Verdict,
}

pub fn compare(a: &[Scored], b: &[Scored]) -> Comparison {
    let in_both = |s: &&Scored, other: &[Scored]| other.iter().any(|o| o.id == s.id);
    let left_out = a
        .iter()
        .filter(|s| !in_both(s, b))
        .chain(b.iter().filter(|s| !in_both(s, a)))
        .map(|s| s.id.clone())
        .collect();
    let both = |run: &[Scored], other: &[Scored]| -> Vec<Scored> {
        run.iter().filter(|s| in_both(s, other)).cloned().collect()
    };
    let (a, b) = (both(a, b), both(b, a));
    let (mut gained, mut lost) = (Vec::new(), Vec::new());
    for sa in &a {
        let sb = b.iter().find(|s| s.id == sa.id).expect("in both");
        for (i, (ca, cb)) in sa.caught.iter().zip(&sb.caught).enumerate() {
            match (ca, cb) {
                (false, true) => gained.push((sa.id.clone(), i)),
                (true, false) => lost.push((sa.id.clone(), i)),
                _ => {}
            }
        }
    }
    let (a, b) = (Totals::of(&a), Totals::of(&b));
    let fp = (a.false_positives, b.false_positives);
    let better = (b.caught > a.caught && fp.1 <= fp.0) || (b.caught == a.caught && fp.1 < fp.0);
    Comparison {
        verdict: if better { Verdict::Keep } else { Verdict::Drop },
        a,
        b,
        gained,
        lost,
        left_out,
    }
}

#[cfg(test)]
mod tests {
    use mergepatrol_core::{Evidence, Kind, Lean};

    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn case(expected: &str) -> Case {
        Case::parse(
            "c",
            &format!("repo: o/r\npr: 1\nbase: {SHA}\nhead: {SHA}\ntitle: t\nexpected:\n{expected}"),
        )
        .unwrap()
    }

    fn bugs(spans: &[(&str, &str)]) -> Case {
        let items: String = spans
            .iter()
            .map(|(file, lines)| {
                format!(
                    "    - at: [{{ file: {file}, lines: {lines} }}]\n      what: w\n      fixed_by: x\n"
                )
            })
            .collect();
        case(&format!("  bugs:\n{items}"))
    }

    fn bug(file: &str, lines: &str) -> Bug {
        let Expected::Bugs(b) = bugs(&[(file, lines)]).expected else {
            unreachable!()
        };
        b.iter().next().unwrap().clone()
    }

    /// A change MergeLoop sends back under the default settings: sure, and shown by evidence.
    fn change(file: &str, line: u32) -> Finding {
        Finding {
            id: String::new(),
            kind: Kind::Other,
            title: format!("{file}:{line}"),
            file: Some(file.into()),
            line: Some(line),
            confidence: 95,
            lean: Lean::Change,
            finding: String::new(),
            unsure: String::new(),
            checked: Vec::new(),
            would_clear: String::new(),
            suggestion: String::new(),
            evidence: Some(Evidence {
                command: "cargo test".into(),
                failed: true,
                ..Evidence::default()
            }),
            needs_human: false,
            code: Vec::new(),
            related: Vec::new(),
        }
    }

    fn run(findings: Vec<Finding>) -> CaseRun {
        CaseRun {
            id: "c".into(),
            head: SHA.to_owned().try_into().unwrap(),
            reviewed: Reviewed::Done {
                summary: String::new(),
                findings,
            },
            cost_usd: Some(0.5),
        }
    }

    fn scored(id: &str, caught: &[bool], false_positives: usize) -> Scored {
        Scored {
            id: id.into(),
            caught: caught.to_vec(),
            false_positives: vec!["fp".into(); false_positives],
            unmatched: Vec::new(),
            failed: false,
            cost_usd: Some(1.0),
        }
    }

    #[test]
    fn a_finding_is_about_a_bug_in_its_file_near_its_lines() {
        let b = bug("src/a.rs", "40-52");
        assert!(about(&b, &change("src/a.rs", 40)));
        assert!(about(&b, &change("src/a.rs", 52)));
        assert!(about(&b, &change("src/a.rs", 40 - NEAR)));
        assert!(about(&b, &change("src/a.rs", 52 + NEAR)));
        assert!(about(&b, &change("./src/a.rs", 45)));
        assert!(!about(&b, &change("src/a.rs", 40 - NEAR - 1)));
        assert!(!about(&b, &change("src/a.rs", 52 + NEAR + 1)));
        assert!(!about(&b, &change("src/b.rs", 45)));
        assert!(!about(&b, &change("a.rs", 45)));
    }

    #[test]
    fn a_finding_near_any_place_a_bug_lives_is_about_it() {
        let c = case(
            "  bugs:\n    - at:\n        - { file: a.rs, lines: 10 }\n        - { file: b.rs, lines: 90-95 }\n      what: w\n      fixed_by: x\n",
        );
        let Expected::Bugs(b) = c.expected else {
            unreachable!()
        };
        let b = b.iter().next().unwrap();
        assert!(about(b, &change("a.rs", 12)));
        assert!(about(b, &change("b.rs", 99)));
        assert!(!about(b, &change("b.rs", 10)));
        assert!(!about(b, &change("a.rs", 92)));
    }

    #[test]
    fn a_finding_with_no_line_or_no_file_is_about_no_bug() {
        let b = bug("src/a.rs", "3");
        let mut no_line = change("src/a.rs", 3);
        no_line.line = None;
        assert!(!about(&b, &no_line));
        let mut no_file = change("src/a.rs", 3);
        no_file.file = None;
        assert!(!about(&b, &no_file));
    }

    #[test]
    fn a_bug_counts_as_caught_only_by_a_change_mergeloop_sends_back() {
        let c = bugs(&[("a.rs", "10-12")]);
        let settings = ReviewSettings::default();
        let caught = |f: Finding| score(&c, &run(vec![f]), &settings).caught;
        assert_eq!(caught(change("a.rs", 11)), [true]);

        let mut fine = change("a.rs", 11);
        fine.lean = Lean::Fine;
        let mut unsure = change("a.rs", 11);
        unsure.confidence = 60;
        let mut unshown = change("a.rs", 11);
        unshown.evidence.as_mut().unwrap().failed = false;
        let mut human = change("a.rs", 11);
        human.needs_human = true;
        for f in [fine, unsure, unshown, human] {
            assert_eq!(caught(f), [false]);
        }
    }

    #[test]
    fn each_bug_is_caught_on_its_own_and_other_changes_are_unmatched() {
        let c = bugs(&[("a.rs", "10"), ("b.rs", "100"), ("c.rs", "1")]);
        let s = score(
            &c,
            &run(vec![
                change("a.rs", 12),
                change("b.rs", 100),
                change("z.rs", 1),
            ]),
            &ReviewSettings::default(),
        );
        assert_eq!(s.caught, [true, true, false]);
        assert_eq!(s.unmatched, ["z.rs:1"]);
        assert!(s.false_positives.is_empty());
        assert_eq!((s.failed, s.cost_usd), (false, Some(0.5)));
    }

    #[test]
    fn every_change_sent_back_on_a_clean_case_is_a_false_positive() {
        let mut cleared = change("a.rs", 2);
        cleared.lean = Lean::Fine;
        let s = score(
            &case("  clean\n"),
            &run(vec![change("a.rs", 1), cleared, change("b.rs", 9)]),
            &ReviewSettings::default(),
        );
        assert_eq!(s.false_positives, ["a.rs:1", "b.rs:9"]);
        assert!(s.caught.is_empty() && s.unmatched.is_empty());
    }

    #[test]
    fn a_failed_review_catches_nothing_and_flags_nothing() {
        let mut r = run(Vec::new());
        r.reviewed = Reviewed::Failed {
            reason: "no key".into(),
        };
        let s = score(
            &bugs(&[("a.rs", "1"), ("a.rs", "9")]),
            &r,
            &ReviewSettings::default(),
        );
        assert_eq!(s.caught, [false, false]);
        assert!(s.failed);
        let s = score(&case("  clean\n"), &r, &ReviewSettings::default());
        assert!(s.failed && s.false_positives.is_empty());
    }

    #[test]
    fn a_run_scores_only_the_cases_it_reviewed_at_their_head() {
        let mut moved = bugs(&[("a.rs", "1")]);
        moved.id = "moved".into();
        moved.head = "b".repeat(40).try_into().unwrap();
        let mut unrun = case("  clean\n");
        unrun.id = "unrun".into();
        let reviewed = bugs(&[("a.rs", "1")]);
        let mut moved_run = run(vec![change("a.rs", 1)]);
        moved_run.id = "moved".into();
        let mut gone = run(Vec::new());
        gone.id = "gone".into();
        let s = scores(
            &[reviewed, moved, unrun],
            &[gone, moved_run, run(vec![change("a.rs", 1)])],
            &ReviewSettings::default(),
        );
        let ids: Vec<_> = s
            .iter()
            .map(|s| (s.id.as_str(), s.caught.clone()))
            .collect();
        assert_eq!(ids, [("c", vec![true])]);
    }

    #[test]
    fn totals_add_up_the_cases() {
        let mut failed = scored("d", &[false], 0);
        failed.failed = true;
        failed.cost_usd = None;
        let mut unmatched = scored("e", &[true], 0);
        unmatched.unmatched = vec!["u".into()];
        let t = Totals::of(&[
            scored("a", &[true, false], 0),
            scored("b", &[], 2),
            scored("c", &[], 0),
            failed,
            unmatched,
        ]);
        assert_eq!(
            t,
            Totals {
                cases: 5,
                bugs: 4,
                caught: 2,
                clean: 2,
                clean_flagged: 1,
                false_positives: 2,
                unmatched: 1,
                failed: 1,
                cost_usd: 4.0,
            }
        );
        assert_eq!(t.recall(), Some(0.5));
        assert_eq!(Totals::of(&[scored("b", &[], 1)]).recall(), None);
    }

    #[test]
    fn a_run_that_catches_more_without_more_false_positives_is_kept() {
        let a = [scored("x", &[true, false], 1), scored("y", &[false], 0)];
        let b = [scored("x", &[true, true], 1), scored("y", &[false], 0)];
        let c = compare(&a, &b);
        assert_eq!(c.verdict, Verdict::Keep);
        assert_eq!(c.gained, [("x".to_owned(), 1)]);
        assert!(c.lost.is_empty());
        assert_eq!((c.a.caught, c.b.caught), (1, 2));
    }

    #[test]
    fn a_run_that_catches_as_much_with_fewer_false_positives_is_kept() {
        let a = [scored("x", &[true], 0), scored("y", &[], 2)];
        let b = [scored("x", &[true], 0), scored("y", &[], 1)];
        assert_eq!(compare(&a, &b).verdict, Verdict::Keep);
    }

    #[test]
    fn a_run_that_is_no_better_or_trades_one_for_the_other_is_dropped() {
        let a = [scored("x", &[true, false], 0), scored("y", &[], 0)];
        let same = compare(&a, &a);
        assert_eq!(same.verdict, Verdict::Drop);
        let traded = [scored("x", &[false, true], 0), scored("y", &[], 0)];
        let t = compare(&a, &traded);
        assert_eq!(t.verdict, Verdict::Drop);
        assert_eq!(t.gained, [("x".to_owned(), 1)]);
        assert_eq!(t.lost, [("x".to_owned(), 0)]);
        let noisier = [scored("x", &[true, true], 0), scored("y", &[], 1)];
        assert_eq!(compare(&a, &noisier).verdict, Verdict::Drop);
    }

    #[test]
    fn a_comparison_scores_only_the_cases_both_runs_reviewed() {
        let a = [scored("x", &[true], 0), scored("only-a", &[true, true], 0)];
        let b = [scored("x", &[true], 0), scored("only-b", &[], 3)];
        let c = compare(&a, &b);
        assert_eq!(c.left_out, ["only-a", "only-b"]);
        assert_eq!(
            (c.a.cases, c.a.caught, c.b.cases, c.b.false_positives),
            (1, 1, 1, 0)
        );
    }
}
