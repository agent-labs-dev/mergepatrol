use serde::{Deserialize, Serialize};

use crate::PrNumber;

/// A labeled pull request, as GitHub shows it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub number: PrNumber,
    pub priority: bool,
    /// `Some(reason)` when GitHub would not merge the PR yet: a draft, or missing approvals.
    pub hold: Option<String>,
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

/// The queue of one (repo, trunk). Serial: at most one entry is active.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub entries: Vec<Entry>,
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

        for c in candidates {
            match self.entries.iter_mut().find(|e| e.number == c.number) {
                Some(e) => {
                    e.priority = c.priority;
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
                    });
                    changes.push(LaneChange::Enqueued(c.number));
                }
            }
        }

        changes
    }

    pub fn active(&self) -> Option<PrNumber> {
        self.entries
            .iter()
            .find(|e| e.state == EntryState::Active)
            .map(|e| e.number)
    }

    /// The next PR to start: the first waiting priority PR, else the first waiting PR.
    /// `None` while a PR is active.
    pub fn next(&self) -> Option<PrNumber> {
        if self.active().is_some() {
            return None;
        }
        let waiting = || {
            self.entries
                .iter()
                .filter(|e| e.state == EntryState::Waiting)
        };
        waiting()
            .find(|e| e.priority)
            .or_else(|| waiting().next())
            .map(|e| e.number)
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

    pub fn remove(&mut self, number: PrNumber) {
        self.entries.retain(|e| e.number != number);
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
        assert_eq!(lane.next(), Some(2));
    }

    #[test]
    fn serial_while_active() {
        let mut lane = Lane::default();
        lane.sync(&[c(1), c(2)]);
        lane.activate(1);
        assert_eq!(lane.next(), None);
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
        assert_eq!(lane.next(), Some(2));
        lane.sync(&[c(1), c(2)]);
        assert_eq!(lane.next(), Some(1));
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
        assert_eq!(lane.next(), Some(2));
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
        assert_eq!(lane.active(), Some(1));
    }
}
