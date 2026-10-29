use serde::{Deserialize, Serialize};

use crate::PrNumber;

/// A labeled pull request, or a stack with a labeled layer, as GitHub shows it now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Candidate {
    /// The PR's number, or the stack's: GitHub numbers stacks among PRs and issues.
    pub number: PrNumber,
    pub priority: bool,
    /// `Some(reason)` when GitHub would not merge the PR yet: a draft, or missing approvals.
    pub hold: Option<String>,
    pub summary: PrSummary,
    /// A stack's PRs, bottom to top. Empty for a PR.
    pub layers: Vec<PrNumber>,
}

/// What the dashboard shows of a PR, as of the last tick. The queue never reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrSummary {
    pub title: String,
    pub url: String,
    /// `None` for a deleted account.
    pub author: Option<Person>,
    pub created_at: String,
    pub head_ref: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    /// `APPROVED`, `REVIEW_REQUIRED`, `CHANGES_REQUESTED`, or `None` when no review is required.
    pub review_decision: Option<String>,
    /// Each reviewer's latest approval or change request.
    pub reviews: Vec<Review>,
    /// Users asked to review who have not yet.
    pub requested: Vec<Person>,
    /// The head commit's combined checks: `SUCCESS`, `FAILURE`, `ERROR`, `PENDING` or
    /// `EXPECTED`. `None` without checks.
    pub checks: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub login: String,
    pub avatar_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub reviewer: Person,
    /// `APPROVED` or `CHANGES_REQUESTED`.
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryState {
    Waiting,
    Held(String),
    Active,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub number: PrNumber,
    pub priority: bool,
    pub state: EntryState,
    /// Empty in a lane saved before the queue kept it, until the next tick.
    #[serde(default)]
    pub summary: PrSummary,
    /// A stack's PRs, bottom to top. Empty for a PR.
    #[serde(default)]
    pub layers: Vec<PrNumber>,
}

impl Entry {
    /// The PRs this entry merges: a stack's layers, or the PR itself.
    pub fn prs(&self) -> Vec<PrNumber> {
        if self.layers.is_empty() {
            vec![self.number]
        } else {
            self.layers.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaneChange {
    Enqueued(PrNumber),
    /// The label is gone, or the PR closed. `was_active` means a running job must stop.
    Dequeued {
        number: PrNumber,
        was_active: bool,
    },
    /// A waiting PR went on hold, or its reason changed.
    Held {
        number: PrNumber,
        reason: String,
    },
    /// A held PR can merge again, and waits in its place.
    Released(PrNumber),
}

/// The queue of one (repo, trunk). One job at a time: a single PR, or a batch of PRs tested
/// together, whose entries are all active.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub entries: Vec<Entry>,
    /// Set after a batch failed: the PR that broke it is among `suspects`.
    #[serde(default)]
    pub bisect: Option<Bisect>,
    /// The branch of the batch being tested. Still set at startup means a restart cut the batch
    /// short.
    #[serde(default)]
    pub batch_branch: Option<String>,
    /// The draft PR that tests the batch, once it is open.
    #[serde(default)]
    pub batch_pr: Option<PrNumber>,
    /// What the batch's branch holds, once built: what a restart needs to pick the batch up.
    #[serde(default)]
    pub batch_built: Option<BuiltBatch>,
}

/// A batch's branch: the trunk commit it starts from, then each PR merged onto it in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltBatch {
    pub trunk_sha: String,
    pub steps: Vec<BatchStep>,
}

/// One PR or stack of a batch: the head it merged (a stack's top), and the merge commit it made
/// on the branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchStep {
    pub number: PrNumber,
    pub head_sha: String,
    pub sha: String,
    pub tree: String,
    /// A stack's layers as tested, bottom to top. Empty for a PR.
    #[serde(default)]
    pub layers: Vec<LayerHead>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerHead {
    pub number: PrNumber,
    pub head_sha: String,
}

/// The search for the PR that broke a batch: the next batches take only suspects, and at most
/// half as many as the batch that failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bisect {
    pub suspects: Vec<PrNumber>,
    pub limit: usize,
}

impl Lane {
    /// Makes the lane match the labeled PRs on GitHub. New PRs go to the back.
    pub fn sync(&mut self, candidates: &[Candidate]) -> Vec<LaneChange> {
        let mut changes = Vec::new();

        self.entries.retain(|e| {
            let keep = candidates.iter().any(|c| c.number == e.number);
            if !keep {
                changes.push(LaneChange::Dequeued {
                    number: e.number,
                    was_active: e.state == EntryState::Active,
                });
            }
            keep
        });
        for change in &changes {
            if let LaneChange::Dequeued { number, .. } = change {
                self.clear_suspect(*number);
            }
        }

        for c in candidates {
            match self.entries.iter_mut().find(|e| e.number == c.number) {
                Some(e) => {
                    e.priority = c.priority;
                    e.summary = c.summary.clone();
                    e.layers = c.layers.clone();
                    if e.state == EntryState::Active || e.state == hold_state(c) {
                        continue;
                    }
                    e.state = hold_state(c);
                    changes.push(match &c.hold {
                        Some(reason) => LaneChange::Held {
                            number: c.number,
                            reason: reason.clone(),
                        },
                        None => LaneChange::Released(c.number),
                    });
                }
                None => {
                    self.entries.push(Entry {
                        number: c.number,
                        priority: c.priority,
                        state: hold_state(c),
                        summary: c.summary.clone(),
                        layers: c.layers.clone(),
                    });
                    changes.push(LaneChange::Enqueued(c.number));
                    if let Some(reason) = &c.hold {
                        changes.push(LaneChange::Held {
                            number: c.number,
                            reason: reason.clone(),
                        });
                    }
                }
            }
        }

        changes
    }

    pub fn active(&self) -> Vec<PrNumber> {
        self.entries
            .iter()
            .filter(|e| e.state == EntryState::Active)
            .map(|e| e.number)
            .collect()
    }

    /// The next PRs to start, at most `max`, in the order to merge them: waiting priority PRs,
    /// then waiting PRs, each by place. While bisecting, only suspects, at most the bisect's
    /// limit. Empty while a job is active.
    pub fn next(&self, max: usize) -> Vec<PrNumber> {
        if !self.active().is_empty() {
            return Vec::new();
        }
        let waiting = || {
            self.entries
                .iter()
                .filter(|e| e.state == EntryState::Waiting)
        };
        let order: Vec<PrNumber> = waiting()
            .filter(|e| e.priority)
            .chain(waiting().filter(|e| !e.priority))
            .map(|e| e.number)
            .collect();
        if let Some(b) = &self.bisect {
            let suspects: Vec<PrNumber> = order
                .iter()
                .copied()
                .filter(|n| b.suspects.contains(n))
                .take(b.limit)
                .collect();
            // Suspects that are all on hold must not stall the PRs behind them.
            if !suspects.is_empty() {
                return suspects;
            }
        }
        order.into_iter().take(max).collect()
    }

    /// A batch failed its checks: one of its PRs, or a mix of them, broke it. Test half of
    /// them next; a single PR runs alone, where a failure is its own.
    pub fn batch_failed(&mut self, members: &[PrNumber]) {
        self.bisect = Some(Bisect {
            suspects: members.to_vec(),
            limit: (members.len() / 2).max(1),
        });
    }

    pub fn activate(&mut self, number: PrNumber) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.number == number) {
            e.state = EntryState::Active;
        }
    }

    /// Ends the active job but keeps the PR in its place, for example to wait for an approval.
    pub fn hold(&mut self, number: PrNumber, reason: String) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.number == number) {
            e.state = EntryState::Held(reason);
        }
    }

    /// Ends the active job, and puts its PRs that are still active back in their places.
    pub fn stop(&mut self) {
        self.batch_branch = None;
        self.batch_pr = None;
        self.batch_built = None;
        for e in &mut self.entries {
            if e.state == EntryState::Active {
                e.state = EntryState::Waiting;
            }
        }
    }

    pub fn remove(&mut self, number: PrNumber) {
        self.entries.retain(|e| e.number != number);
        self.clear_suspect(number);
    }

    /// The PR was ejected. A suspect ejected is the one that broke the batch: the search is over.
    pub fn eject(&mut self, number: PrNumber) {
        self.entries.retain(|e| e.number != number);
        if self
            .bisect
            .as_ref()
            .is_some_and(|b| b.suspects.contains(&number))
        {
            self.bisect = None;
        }
    }

    fn clear_suspect(&mut self, number: PrNumber) {
        if let Some(b) = &mut self.bisect {
            b.suspects.retain(|n| *n != number);
            if b.suspects.is_empty() {
                self.bisect = None;
            }
        }
    }
}

/// Where a PR that is not active belongs: on hold, or waiting.
fn hold_state(c: &Candidate) -> EntryState {
    match &c.hold {
        Some(reason) => EntryState::Held(reason.clone()),
        None => EntryState::Waiting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(number: PrNumber) -> Candidate {
        Candidate {
            number,
            priority: false,
            hold: None,
            summary: PrSummary::default(),
            layers: Vec::new(),
        }
    }

    #[test]
    fn new_prs_go_to_the_back_and_keep_their_place() {
        let mut lane = Lane::default();
        lane.sync(&[c(2), c(1)]);
        lane.sync(&[c(1), c(2), c(3)]);
        let order: Vec<_> = lane.entries.iter().map(|e| e.number).collect();
        assert_eq!(order, [2, 1, 3]);
    }

    #[test]
    fn priority_goes_first() {
        let mut lane = Lane::default();
        lane.sync(&[
            c(1),
            Candidate {
                priority: true,
                ..c(2)
            },
        ]);
        assert_eq!(lane.next(1), [2]);
    }

    #[test]
    fn serial_while_active() {
        let mut lane = Lane::default();
        lane.sync(&[c(1), c(2)]);
        lane.activate(1);
        assert!(lane.next(8).is_empty());
    }

    #[test]
    fn held_prs_are_skipped_and_come_back_when_eligible() {
        let mut lane = Lane::default();
        lane.sync(&[
            Candidate {
                hold: Some("draft".into()),
                ..c(1)
            },
            c(2),
        ]);
        assert_eq!(lane.next(1), [2]);
        assert_eq!(lane.sync(&[c(1), c(2)]), [LaneChange::Released(1)]);
        assert_eq!(lane.next(1), [1]);
    }

    #[test]
    fn going_on_hold_is_reported_once_per_reason() {
        let mut lane = Lane::default();
        let held = |reason: &str| Candidate {
            hold: Some(reason.into()),
            ..c(1)
        };
        let report = |reason: &str| LaneChange::Held {
            number: 1,
            reason: reason.into(),
        };
        assert_eq!(
            lane.sync(&[held("draft")]),
            [LaneChange::Enqueued(1), report("draft")]
        );
        assert!(lane.sync(&[held("draft")]).is_empty());
        assert_eq!(lane.sync(&[held("review")]), [report("review")]);
    }

    #[test]
    fn unlabeling_the_active_pr_reports_it() {
        let mut lane = Lane::default();
        lane.sync(&[c(1), c(2)]);
        lane.activate(1);
        let changes = lane.sync(&[c(2)]);
        assert_eq!(
            changes,
            [LaneChange::Dequeued {
                number: 1,
                was_active: true
            }]
        );
        assert_eq!(lane.next(1), [2]);
    }

    #[test]
    fn sync_does_not_reset_the_active_pr() {
        let mut lane = Lane::default();
        lane.sync(&[c(1)]);
        lane.activate(1);
        lane.sync(&[Candidate {
            hold: Some("needs approval".into()),
            ..c(1)
        }]);
        assert_eq!(lane.active(), [1]);
    }

    fn lane(numbers: &[PrNumber]) -> Lane {
        let mut lane = Lane::default();
        lane.sync(&numbers.iter().map(|n| c(*n)).collect::<Vec<_>>());
        lane
    }

    #[test]
    fn a_batch_takes_priority_prs_first_then_by_place() {
        let mut lane = lane(&[1, 2, 3]);
        lane.sync(&[
            c(1),
            c(2),
            c(3),
            Candidate {
                priority: true,
                ..c(4)
            },
        ]);
        assert_eq!(lane.next(3), [4, 1, 2]);
    }

    #[test]
    fn a_failed_batch_bisects_down_to_one_pr() {
        let mut lane = lane(&[1, 2, 3, 4, 5, 6]);
        lane.batch_failed(&[1, 2, 3, 4]);
        assert_eq!(lane.next(8), [1, 2]);

        // The first half passed and merged: the rest of the suspects, still two at a time.
        lane.remove(1);
        lane.remove(2);
        assert_eq!(lane.next(8), [3, 4]);

        lane.batch_failed(&[3, 4]);
        assert_eq!(lane.next(8), [3]);
        lane.remove(3);
        assert_eq!(lane.next(8), [4]);

        // 4 alone failed: it broke the batch, and full batches resume.
        lane.eject(4);
        assert_eq!(lane.bisect, None);
        assert_eq!(lane.next(8), [5, 6]);
    }

    #[test]
    fn the_search_ends_when_the_culprit_is_ejected() {
        let mut lane = lane(&[1, 2, 3]);
        lane.batch_failed(&[1, 2]);
        lane.batch_failed(&[1]);
        lane.eject(1);
        assert_eq!(lane.next(8), [2, 3]);
    }

    #[test]
    fn suspects_on_hold_do_not_stall_the_lane() {
        let mut lane = lane(&[1, 2, 3]);
        lane.batch_failed(&[1, 2]);
        lane.sync(&[
            Candidate {
                hold: Some("draft".into()),
                ..c(1)
            },
            Candidate {
                hold: Some("draft".into()),
                ..c(2)
            },
            c(3),
        ]);
        assert_eq!(lane.next(8), [3]);
    }

    #[test]
    fn suspects_that_leave_the_queue_end_the_search() {
        let mut lane = lane(&[1, 2, 3]);
        lane.batch_failed(&[1, 2]);
        lane.sync(&[c(3)]);
        assert_eq!(lane.bisect, None);
    }

    #[test]
    fn stopping_a_batch_puts_its_prs_back_in_place() {
        let mut lane = lane(&[1, 2, 3]);
        lane.activate(1);
        lane.activate(2);
        lane.stop();
        assert_eq!(lane.next(8), [1, 2, 3]);
    }
}
