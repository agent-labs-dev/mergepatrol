use serde::{Deserialize, Serialize};

use crate::{Hold, PrNumber};

use Phase as P;

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
    /// The required checks `failed` on `head`, and GitHub Actions runs their failed jobs again
    /// once, in the workflow runs `runs`, before a fix: a check may fail on what its next run
    /// does not meet, as a dropped network request.
    Rerunning {
        head: String,
        base: String,
        failed: Vec<String>,
        runs: Vec<u64>,
    },
    /// The agent gave up on the checks `failed` on `head`, as not the PR's to fix: they run
    /// again, on a new commit of the same files. The failure may be a flaky test, or a check that
    /// read a PR title since changed.
    Rechecking {
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
    /// These required checks had not finished `CHECKS_TIMEOUT` after the publish.
    ChecksTimedOut(Vec<String>),
    /// These required checks never reported on the PR's head, `CHECKS_MISSING_FOR` after every
    /// check that did had finished: CI does not run them, as when the PR renames one.
    ChecksMissing(Vec<String>),
    /// What the queue would have to do to a fork PR, it may not.
    Fork(String),
}

/// How long a PR's required checks may take, after a publish. A check that never finishes would
/// otherwise hold the queue forever. One that never reports at all goes sooner, by
/// `CHECKS_MISSING_FOR`.
pub const CHECKS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3 * 60 * 60);

/// How long a required check may go without reporting at all on a PR's head while no other
/// check runs: long enough for a CI that creates its checks late, but not for the head of the
/// queue to wait out `CHECKS_TIMEOUT` for a check that no workflow runs anymore.
pub const CHECKS_MISSING_FOR: std::time::Duration = std::time::Duration::from_secs(30 * 60);

impl EjectReason {
    /// What kind of reason it is, for analytics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::RebaseFailed(_) => "rebase-failed",
            Self::ChecksFailed { .. } => "checks-failed",
            Self::FixFailed { .. } => "fix-failed",
            Self::BaseMovedOutsideQueue => "base-moved",
            Self::MergeRejected(_) => "merge-rejected",
            Self::ChecksTimedOut(_) => "checks-timed-out",
            Self::ChecksMissing(_) => "checks-missing",
            Self::Fork(_) => "fork",
        }
    }
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
            Self::ChecksTimedOut(names) => write!(
                f,
                "required checks did not finish within {} hours: {}",
                CHECKS_TIMEOUT.as_secs() / 3600,
                names.join(", ")
            ),
            Self::ChecksMissing(names) => write!(
                f,
                "required checks never reported, {} minutes after every other check finished: {}. Does a workflow still run them under that name?",
                CHECKS_MISSING_FOR.as_secs() / 60,
                names.join(", ")
            ),
            Self::Fork(r) => f.write_str(r),
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
    /// It conflicts with the trunk, and the lane left that for off the lane.
    RebaseConflict,
    ChecksPassed,
    ChecksFailed(Vec<String>),
    /// These required checks failed, and the workflow runs `runs` can run their failed jobs
    /// again.
    ChecksRerunnable {
        failed: Vec<String>,
        runs: Vec<u64>,
    },
    /// The failed jobs run again.
    ChecksRerun,
    /// GitHub would not run the failed jobs again, and said why.
    RerunFailed(String),
    /// These required checks are still pending, `CHECKS_TIMEOUT` after the publish.
    ChecksTimedOut(Vec<String>),
    /// These required checks never reported on the head, `CHECKS_MISSING_FOR` after the others
    /// finished.
    ChecksMissing(Vec<String>),
    /// The fix is published: `head` is the new PR head, on the same base.
    Fixed {
        head: String,
    },
    FixFailed(String),
    /// The agent found nothing in the code to fix, and said why.
    FixGaveUp(String),
    /// What failed was the PR's title, which the agent's new title now replaces.
    Retitled,
    /// The PR head is no longer the SHA we published: the author pushed.
    HeadMoved,
    /// The trunk is no longer the SHA we rebased onto.
    BaseMoved,
    /// GitHub would not merge the PR yet, for this reason: a draft, or a review is missing.
    OnHold(Hold),
    Merged,
    MergeRejected(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transition {
    Continue(Job),
    Merged,
    Ejected(EjectReason),
    /// Stop, keep the place in the lane, and wait until GitHub says the PR is eligible again.
    Held(Hold),
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
    /// The checks ran again since the author last pushed: a failure now is the PR's.
    pub rechecked: bool,
}

impl Job {
    pub fn start(number: PrNumber, fix_attempts: u32) -> Self {
        Self {
            number,
            phase: Phase::Rebasing,
            base_moves: 0,
            fix_attempts,
            fixes: 0,
            rechecked: false,
        }
    }

    /// Whether failed checks may still be fixed.
    pub fn can_fix(&self) -> bool {
        self.fixes < self.fix_attempts && !self.rechecked
    }

    pub fn step(self, event: Event) -> Transition {
        use Event as E;
        match (&self.phase, event) {
            (P::Rebasing, E::Rebased { head, base }) => self.with(P::AwaitingChecks { head, base }),
            (P::Rebasing, E::RebaseFailed(r)) => Transition::Ejected(EjectReason::RebaseFailed(r)),
            (P::Rebasing, E::RebaseConflict) => Transition::Held(Hold::PrepRebasing),

            (P::AwaitingChecks { head, .. }, E::ChecksPassed) => {
                let head = head.clone();
                self.with(P::Merging { head })
            }
            (P::AwaitingChecks { head, base }, E::ChecksRerunnable { failed, runs }) => {
                let phase = P::Rerunning {
                    head: head.clone(),
                    base: base.clone(),
                    failed,
                    runs,
                };
                self.with(phase)
            }
            (P::Rerunning { head, base, .. }, E::ChecksRerun) => {
                let phase = P::AwaitingChecks {
                    head: head.clone(),
                    base: base.clone(),
                };
                self.with(phase)
            }
            (P::Rerunning { failed, .. }, E::RerunFailed(_)) => {
                let failed = failed.clone();
                self.failed(failed)
            }
            (P::AwaitingChecks { .. }, E::ChecksFailed(failed)) => self.failed(failed),
            (P::AwaitingChecks { .. }, E::ChecksTimedOut(names)) => {
                Transition::Ejected(EjectReason::ChecksTimedOut(names))
            }
            (P::AwaitingChecks { .. }, E::ChecksMissing(names)) => {
                Transition::Ejected(EjectReason::ChecksMissing(names))
            }

            (P::Fixing { base, .. }, E::Fixed { head }) => {
                let base = base.clone();
                self.with(P::AwaitingChecks { head, base })
            }
            (P::Fixing { failed, .. }, E::FixFailed(reason))
            | (P::Rechecking { failed, .. }, E::FixFailed(reason)) => {
                Transition::Ejected(EjectReason::FixFailed {
                    checks: failed.clone(),
                    reason,
                })
            }
            (P::Fixing { head, base, failed }, E::FixGaveUp(_) | E::Retitled) => {
                let phase = P::Rechecking {
                    head: head.clone(),
                    base: base.clone(),
                    failed: failed.clone(),
                };
                Transition::Continue(Job {
                    phase,
                    rechecked: true,
                    ..self
                })
            }
            (P::Rechecking { base, .. }, E::Fixed { head }) => {
                let base = base.clone();
                self.with(P::AwaitingChecks { head, base })
            }

            (P::Merging { .. }, E::Merged) => Transition::Merged,
            (P::Merging { .. }, E::MergeRejected(r)) => {
                Transition::Ejected(EjectReason::MergeRejected(r))
            }

            // New code from the author earns new fixes.
            (_, E::HeadMoved) => Transition::Continue(Job {
                phase: P::Rebasing,
                fixes: 0,
                rechecked: false,
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
            (_, E::OnHold(reason)) => Transition::Held(reason),

            (phase, event) => panic!("event {event:?} is not valid in phase {phase:?}"),
        }
    }

    /// The required checks `failed` on the head the job waits for, or ran again: a fix while
    /// one is left, or else the PR is ejected.
    fn failed(self, failed: Vec<String>) -> Transition {
        let (P::AwaitingChecks { head, base } | P::Rerunning { head, base, .. }) = &self.phase
        else {
            unreachable!("only checks fail")
        };
        if !self.can_fix() {
            return Transition::Ejected(EjectReason::ChecksFailed {
                names: failed,
                fixes: self.fixes,
            });
        }
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
    fn a_conflict_left_for_off_the_lane_holds_the_pr() {
        assert_eq!(
            Job::start(7, 2).step(Event::RebaseConflict),
            Transition::Held(Hold::PrepRebasing)
        );
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
    fn a_fix_given_up_on_runs_the_checks_again_once_and_a_new_failure_ejects() {
        let Transition::Continue(fixing) = failed(rebased(Job::start(7, 2))) else {
            panic!()
        };
        let Transition::Continue(rechecking) = fixing.step(Event::FixGaveUp("flaky".into())) else {
            panic!()
        };
        assert!(matches!(rechecking.phase, Phase::Rechecking { .. }));
        let Transition::Continue(again) = rechecking.step(Event::Fixed { head: "c".into() }) else {
            panic!()
        };
        assert_eq!(
            again.phase,
            Phase::AwaitingChecks {
                head: "c".into(),
                base: "b1".into()
            }
        );
        assert_eq!(
            failed(again),
            Transition::Ejected(EjectReason::ChecksFailed {
                names: vec!["ci".into()],
                fixes: 1
            })
        );
    }

    #[test]
    fn a_new_title_runs_the_checks_again_once_and_a_new_failure_ejects() {
        let Transition::Continue(fixing) = failed(rebased(Job::start(7, 2))) else {
            panic!()
        };
        let Transition::Continue(rechecking) = fixing.step(Event::Retitled) else {
            panic!()
        };
        assert!(matches!(rechecking.phase, Phase::Rechecking { .. }));
        let Transition::Continue(again) = rechecking.step(Event::Fixed { head: "c".into() }) else {
            panic!()
        };
        assert!(matches!(failed(again), Transition::Ejected(_)));
    }

    fn rerunnable(job: Job) -> Transition {
        job.step(Event::ChecksRerunnable {
            failed: vec!["ci".into()],
            runs: vec![9],
        })
    }

    #[test]
    fn a_failed_check_runs_again_before_a_fix_and_waits_for_its_checks() {
        let Transition::Continue(rerunning) = rerunnable(rebased(Job::start(7, 2))) else {
            panic!()
        };
        assert_eq!(
            rerunning.phase,
            Phase::Rerunning {
                head: "h1".into(),
                base: "b1".into(),
                failed: vec!["ci".into()],
                runs: vec![9],
            }
        );
        assert_eq!(rerunning.fixes, 0, "a rerun is no fix");
        let Transition::Continue(again) = rerunning.step(Event::ChecksRerun) else {
            panic!()
        };
        assert_eq!(
            again.phase,
            Phase::AwaitingChecks {
                head: "h1".into(),
                base: "b1".into()
            }
        );
        let Transition::Continue(fixing) = failed(again) else {
            panic!()
        };
        assert!(matches!(fixing.phase, Phase::Fixing { .. }));
    }

    #[test]
    fn a_failed_check_with_no_fixes_runs_again_before_it_ejects() {
        let Transition::Continue(rerunning) = rerunnable(rebased(Job::start(7, 0))) else {
            panic!()
        };
        assert!(matches!(rerunning.phase, Phase::Rerunning { .. }));
        let Transition::Continue(again) = rerunning.step(Event::ChecksRerun) else {
            panic!()
        };
        assert!(matches!(failed(again), Transition::Ejected(_)));
    }

    #[test]
    fn a_rerun_github_refuses_goes_on_as_a_failure() {
        let Transition::Continue(rerunning) = rerunnable(rebased(Job::start(7, 2))) else {
            panic!()
        };
        let refused = Event::RerunFailed("403".into());
        let Transition::Continue(fixing) = rerunning.clone().step(refused.clone()) else {
            panic!()
        };
        assert!(matches!(&fixing.phase, Phase::Fixing { failed, .. } if failed == &["ci"]));
        let no_fixes = Job {
            fix_attempts: 0,
            ..rerunning
        };
        assert_eq!(
            no_fixes.step(refused),
            Transition::Ejected(EjectReason::ChecksFailed {
                names: vec!["ci".into()],
                fixes: 0
            })
        );
    }

    #[test]
    fn checks_that_never_finish_eject_without_a_fix() {
        let job = rebased(Job::start(7, 2));
        assert_eq!(
            job.step(Event::ChecksTimedOut(vec!["ci".into()])),
            Transition::Ejected(EjectReason::ChecksTimedOut(vec!["ci".into()]))
        );
    }

    #[test]
    fn checks_that_never_report_eject_without_a_fix_and_name_them() {
        let job = rebased(Job::start(7, 2));
        let reason = EjectReason::ChecksMissing(vec!["sift".into()]);
        assert_eq!(
            job.step(Event::ChecksMissing(vec!["sift".into()])),
            Transition::Ejected(reason.clone())
        );
        assert_eq!(reason.kind(), "checks-missing");
        assert!(reason.to_string().contains("30 minutes"), "{reason}");
        assert!(reason.to_string().contains(": sift."), "{reason}");
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
            job.step(Event::OnHold(Hold::ReviewRequired)),
            Transition::Held(Hold::ReviewRequired)
        ));
    }
}
