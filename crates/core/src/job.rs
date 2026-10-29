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
    /// The required checks `failed` on `head`: the agent commits a fix on top of it.
    Fixing {
        head: String,
        base: String,
        failed: Vec<String>,
    },
    Merging {
        head: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EjectReason {
    RebaseFailed(String),
    /// `fixes` is how many fixes were tried before.
    ChecksFailed {
        names: Vec<String>,
        fixes: u32,
    },
    FixFailed {
        checks: Vec<String>,
        reason: String,
    },
    /// The trunk moved twice while the PR was in flight. Someone merges outside the queue.
    BaseMovedOutsideQueue,
    MergeRejected(String),
}

impl std::fmt::Display for EjectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RebaseFailed(r) => write!(f, "the rebase failed: {r}"),
            Self::ChecksFailed { names, fixes } => {
                write!(f, "required checks failed: {}", names.join(", "))?;
                match fixes {
                    0 => Ok(()),
                    1 => write!(f, ", after a fix"),
                    n => write!(f, ", after {n} fixes"),
                }
            }
            Self::FixFailed { checks, reason } => write!(
                f,
                "required checks failed: {}, and the fix failed: {reason}",
                checks.join(", ")
            ),
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
    /// The fix is published: `head` is the new PR head, on the same base.
    Fixed {
        head: String,
    },
    FixFailed(String),
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
    /// How many times failed checks may be fixed. 0 ejects on the first failure.
    pub fix_attempts: u32,
    /// The fixes tried since the author last pushed.
    pub fixes: u32,
}

impl Job {
    pub fn start(number: PrNumber, fix_attempts: u32) -> Self {
        Self {
            number,
            phase: Phase::Rebasing,
            base_moves: 0,
            fix_attempts,
            fixes: 0,
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
            (P::AwaitingChecks { head, base }, E::ChecksFailed(failed))
                if self.fixes < self.fix_attempts =>
            {
                let phase = P::Fixing {
                    head: head.clone(),
                    base: base.clone(),
                    failed,
                };
                Transition::Continue(Job {
                    phase,
                    fixes: self.fixes + 1,
                    ..self
                })
            }
            (P::AwaitingChecks { .. }, E::ChecksFailed(names)) => {
                Transition::Ejected(EjectReason::ChecksFailed {
                    names,
                    fixes: self.fixes,
                })
            }

            (P::Fixing { base, .. }, E::Fixed { head }) => {
                let base = base.clone();
                self.with(P::AwaitingChecks { head, base })
            }
            (P::Fixing { failed, .. }, E::FixFailed(reason)) => {
                Transition::Ejected(EjectReason::FixFailed {
                    checks: failed.clone(),
                    reason,
                })
            }

            (P::Merging { .. }, E::Merged) => Transition::Merged,
            (P::Merging { .. }, E::MergeRejected(r)) => {
                Transition::Ejected(EjectReason::MergeRejected(r))
            }

            // New code from the author earns new fixes.
            (_, E::HeadMoved) => Transition::Continue(Job {
                phase: P::Rebasing,
                fixes: 0,
                ..self
            }),
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
        let job = rebased(Job::start(7, 0));
        let Transition::Continue(job) = job.step(Event::ChecksPassed) else {
            panic!()
        };
        assert_eq!(job.phase, Phase::Merging { head: "h1".into() });
        assert_eq!(job.step(Event::Merged), Transition::Merged);
    }

    #[test]
    fn base_moves_once_then_ejects() {
        let job = rebased(Job::start(7, 0));
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
        let job = rebased(Job::start(7, 0));
        let Transition::Continue(job) = job.step(Event::HeadMoved) else {
            panic!()
        };
        assert_eq!((job.phase.clone(), job.base_moves), (Phase::Rebasing, 0));
        let job = rebased(job);
        let Transition::Continue(_) = job.step(Event::BaseMoved) else {
            panic!()
        };
    }

    fn failed(job: Job) -> Transition {
        job.step(Event::ChecksFailed(vec!["ci".into()]))
    }

    #[test]
    fn without_fixes_a_failed_check_ejects() {
        assert_eq!(
            failed(rebased(Job::start(7, 0))),
            Transition::Ejected(EjectReason::ChecksFailed {
                names: vec!["ci".into()],
                fixes: 0
            })
        );
    }

    #[test]
    fn failed_checks_are_fixed_until_the_attempts_run_out() {
        let mut job = rebased(Job::start(7, 2));
        for n in 1..=2 {
            let Transition::Continue(fixing) = failed(job) else {
                panic!()
            };
            assert!(matches!(&fixing.phase, Phase::Fixing { base, .. } if base == "b1"));
            let Transition::Continue(fixed) = fixing.step(Event::Fixed {
                head: format!("f{n}"),
            }) else {
                panic!()
            };
            assert_eq!(
                fixed.phase,
                Phase::AwaitingChecks {
                    head: format!("f{n}"),
                    base: "b1".into()
                }
            );
            job = fixed;
        }
        assert_eq!(
            failed(job),
            Transition::Ejected(EjectReason::ChecksFailed {
                names: vec!["ci".into()],
                fixes: 2
            })
        );
    }

    #[test]
    fn a_fix_that_fails_ejects() {
        let Transition::Continue(job) = failed(rebased(Job::start(7, 2))) else {
            panic!()
        };
        assert_eq!(
            job.step(Event::FixFailed("flaky".into())),
            Transition::Ejected(EjectReason::FixFailed {
                checks: vec!["ci".into()],
                reason: "flaky".into()
            })
        );
    }

    #[test]
    fn an_author_push_resets_the_fixes() {
        let Transition::Continue(job) = failed(rebased(Job::start(7, 1))) else {
            panic!()
        };
        let Transition::Continue(job) = job.step(Event::HeadMoved) else {
            panic!()
        };
        assert_eq!(job.fixes, 0);
        assert!(matches!(failed(rebased(job)), Transition::Continue(_)));
    }

    #[test]
    fn dismissed_approval_holds() {
        let job = rebased(Job::start(7, 0));
        assert!(matches!(
            job.step(Event::NeedsApproval),
            Transition::Held(_)
        ));
    }
}
