use serde::{Deserialize, Serialize};

use crate::PrNumber;

/// A labeled pull request, as GitHub shows it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub number: PrNumber,
    pub priority: bool,
    /// `Some(reason)` when GitHub would not merge the PR yet: a draft, or missing approvals.
    pub hold: Option<String>,
    pub summary: PrSummary,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaneChange {
    Enqueued(PrNumber),
    /// The label is gone, or the PR closed. `was_active` means a running job must stop.
    Dequeued {
        number: PrNumber,
        was_active: bool,
    },
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
    /// short: delete the branch, which closes its PR.
    #[serde(default)]
    pub batch_branch: Option<String>,
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
                    if e.state != EntryState::Active {
                        e.state = match &c.hold {
                            Some(reason) => EntryState::Held(reason.clone()),
                            None => EntryState::Waiting,
                        };
                    }
                }
                None => {
                    self.entries.push(Entry {
                        number: c.number,
                        priority: c.priority,
                        state: match &c.hold {
                            Some(reason) => EntryState::Held(reason.clone()),
                            None => EntryState::Waiting,
                        },
                        summary: c.summary.clone(),
                    });
                    changes.push(LaneChange::Enqueued(c.number));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn c(number: PrNumber) -> Candidate {
        Candidate {
            number,
            priority: false,
            hold: None,
            summary: PrSummary::default(),
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
        lane.sync(&[c(1), c(2)]);
        assert_eq!(lane.next(1), [1]);
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
