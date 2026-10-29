//! Where a PR's time in the queue went, read from its events.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A stretch of a PR's time in the queue. The event that begins one names it; the events
/// within a step, such as a job's, leave it as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// In line, behind other PRs or a batch that stopped.
    Waiting,
    /// GitHub would not merge it yet.
    Held,
    Rebasing,
    /// Its checks, or its batch's, running. Ends with the merge.
    Checks,
    /// The agent mending failed checks.
    Fixing,
}

impl Step {
    pub const ALL: [Step; 5] = [
        Step::Waiting,
        Step::Held,
        Step::Rebasing,
        Step::Checks,
        Step::Fixing,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Step::Waiting => "waiting",
            Step::Held => "held",
            Step::Rebasing => "rebasing",
            Step::Checks => "checks",
            Step::Fixing => "fixing",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    /// The step an event kind begins, if it begins one.
    fn begun_by(kind: &str) -> Option<Self> {
        Some(match kind {
            "enqueued" | "released" | "batch-stopped" | "batch-failed" => Step::Waiting,
            "held" => Step::Held,
            "rebasing" => Step::Rebasing,
            "published" | "batch" | "batch-resumed" => Step::Checks,
            "fixing" => Step::Fixing,
            _ => return None,
        })
    }
}

/// Seconds in each step, every step present.
pub type StepTimes = BTreeMap<Step, f64>;

/// `events` are one PR's, as (Unix seconds, kind) in order, from the time it joined the queue
/// to the time it left.
pub fn step_times(events: &[(f64, &str)]) -> StepTimes {
    let mut times: StepTimes = Step::ALL.into_iter().map(|s| (s, 0.0)).collect();
    let mut current = None;
    let mut since = 0.0;
    for &(at, kind) in events {
        if let Some(step) = current {
            *times.get_mut(&step).expect("every step is present") += at - since;
        }
        since = at;
        current = Step::begun_by(kind).or(current);
    }
    times
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_stretch_counts_for_the_step_that_began_it() {
        let times = step_times(&[
            (0.0, "enqueued"),
            (10.0, "held"),
            (15.0, "released"),
            (20.0, "rebasing"),
            (21.0, "job"),
            (30.0, "published"),
            (90.0, "fixing"),
            (150.0, "published"),
            (200.0, "merged"),
        ]);
        let secs: Vec<_> = Step::ALL.iter().map(|s| times[s]).collect();
        // Waiting 10 + 5, held 5, rebasing 10, checks 60 + 50, fixing 60.
        assert_eq!(secs, [15.0, 5.0, 10.0, 110.0, 60.0]);
    }

    #[test]
    fn a_failed_batch_sends_its_prs_back_to_wait() {
        let times = step_times(&[
            (0.0, "enqueued"),
            (5.0, "batch"),
            (65.0, "batch-failed"),
            (70.0, "batch"),
            (100.0, "merged"),
        ]);
        assert_eq!(times[&Step::Waiting], 10.0);
        assert_eq!(times[&Step::Checks], 90.0);
    }

    #[test]
    fn names_round_trip() {
        for s in Step::ALL {
            assert_eq!(Step::parse(s.name()), Some(s));
            assert_eq!(serde_json::to_value(s).unwrap(), s.name());
        }
    }
}
