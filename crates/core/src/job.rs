use serde::{Deserialize, Serialize};

use crate::PrNumber;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Rebasing,
    /// `head` is the SHA we published. `base` is the trunk SHA it sits on.
    AwaitingChecks {
        head: String,
        base: String,
    },
    Merging {
        head: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EjectReason {
    RebaseFailed(String),
    ChecksFailed(Vec<String>),
    /// The trunk moved twice while the PR was in flight. Someone merges outside the queue.
    BaseMovedOutsideQueue,
    MergeRejected(String),
}

impl std::fmt::Display for EjectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RebaseFailed(r) => write!(f, "the rebase failed: {r}"),
            Self::ChecksFailed(names) => write!(f, "required checks failed: {}", names.join(", ")),
            Self::BaseMovedOutsideQueue => {
                write!(
                    f,
                    "the base branch moved outside the queue twice while this PR was in flight"
                )
            }
            Self::MergeRejected(r) => write!(f, "GitHub rejected the merge: {r}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Rebased {
        head: String,
        base: String,
    },
    RebaseFailed(String),
    ChecksPassed,
    ChecksFailed(Vec<String>),
    /// The PR head is no longer the SHA we published: the author pushed.
    HeadMoved,
    /// The trunk is no longer the SHA we rebased onto.
    BaseMoved,
    NeedsApproval,
    Merged,
    MergeRejected(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transition {
    Continue(Job),
    Merged,
    Ejected(EjectReason),
    /// Stop, keep the place in the lane, and wait until GitHub says the PR is eligible again.
    Held(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub number: PrNumber,
    pub phase: Phase,
    pub base_moves: u8,
}

impl Job {
    pub fn start(number: PrNumber) -> Self {
        Self {
            number,
            phase: Phase::Rebasing,
            base_moves: 0,
        }
    }

    pub fn step(self, event: Event) -> Transition {
        use Event as E;
        use Phase as P;
        match (&self.phase, event) {
            (P::Rebasing, E::Rebased { head, base }) => self.with(P::AwaitingChecks { head, base }),
            (P::Rebasing, E::RebaseFailed(r)) => Transition::Ejected(EjectReason::RebaseFailed(r)),

            (P::AwaitingChecks { head, .. }, E::ChecksPassed) => {
                let head = head.clone();
                self.with(P::Merging { head })
            }
            (P::AwaitingChecks { .. }, E::ChecksFailed(names)) => {
                Transition::Ejected(EjectReason::ChecksFailed(names))
            }

            (P::Merging { .. }, E::Merged) => Transition::Merged,
            (P::Merging { .. }, E::MergeRejected(r)) => {
                Transition::Ejected(EjectReason::MergeRejected(r))
            }

            (_, E::HeadMoved) => self.with(P::Rebasing),
            (_, E::BaseMoved) => {
                if self.base_moves >= 1 {
                    Transition::Ejected(EjectReason::BaseMovedOutsideQueue)
                } else {
                    Transition::Continue(Job {
                        phase: P::Rebasing,
                        base_moves: self.base_moves + 1,
                        ..self
                    })
                }
            }
            (_, E::NeedsApproval) => Transition::Held("waiting for an approval".into()),

            (phase, event) => panic!("event {event:?} is not valid in phase {phase:?}"),
        }
    }

    fn with(self, phase: Phase) -> Transition {
        Transition::Continue(Job { phase, ..self })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rebased(job: Job) -> Job {
        match job.step(Event::Rebased {
            head: "h1".into(),
            base: "b1".into(),
        }) {
            Transition::Continue(j) => j,
            t => panic!("{t:?}"),
        }
    }

    #[test]
    fn happy_path() {
        let job = rebased(Job::start(7));
        let Transition::Continue(job) = job.step(Event::ChecksPassed) else {
            panic!()
        };
        assert_eq!(job.phase, Phase::Merging { head: "h1".into() });
        assert_eq!(job.step(Event::Merged), Transition::Merged);
    }

    #[test]
    fn base_moves_once_then_ejects() {
        let job = rebased(Job::start(7));
        let Transition::Continue(job) = job.step(Event::BaseMoved) else {
            panic!()
        };
        assert_eq!(job.phase, Phase::Rebasing);
        let job = rebased(job);
        assert_eq!(
            job.step(Event::BaseMoved),
            Transition::Ejected(EjectReason::BaseMovedOutsideQueue)
        );
    }

    #[test]
    fn author_push_restarts_without_counting_as_churn() {
        let job = rebased(Job::start(7));
        let Transition::Continue(job) = job.step(Event::HeadMoved) else {
            panic!()
        };
        assert_eq!((job.phase.clone(), job.base_moves), (Phase::Rebasing, 0));
        let job = rebased(job);
        let Transition::Continue(_) = job.step(Event::BaseMoved) else {
            panic!()
        };
    }

    #[test]
    fn dismissed_approval_holds() {
        let job = rebased(Job::start(7));
        assert!(matches!(
            job.step(Event::NeedsApproval),
            Transition::Held(_)
        ));
    }
}
