use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{PrNumber, REPO_CONFIG_PATH, ReviewStatus};

/// Why a queued PR keeps its place instead of merging. Its text is what the PR's comment and
/// event say.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Hold {
    Draft,
    /// A required review is missing.
    ReviewRequired,
    /// A reviewer whose approval is required requested changes.
    ChangesRequested,
    /// A fork PR's workflow runs wait for a maintainer to approve them.
    WorkflowApproval,
    /// A fork PR behind its trunk, whose author has not allowed edits by maintainers: the queue
    /// cannot push its rebase.
    ForkEditsOff {
        trunk: String,
    },
    /// It targets a branch without a queue.
    NoQueue {
        base: String,
        queues: Vec<String>,
    },
    /// A private repo, without a subscription.
    NoSubscription,
    /// The public repos of the account queued too much in the last hour.
    FairUseLimit,
    /// The agent is reviewing it, the queue's first gate.
    Reviewing,
    /// The agent's review asks a human about what it was not sure of.
    ReviewAsks,
    /// The agent sent findings back to the author, or a human answering its review asked for
    /// changes: it waits for a push, or for the author to resolve the review's threads.
    ReviewChanges,
    /// Review threads on it are unresolved: the review's gate waits for them all, as the
    /// workspace or the repo asks.
    UnresolvedThreads {
        count: u32,
    },
    /// A layer of the stack is not ready.
    Layer {
        number: PrNumber,
        hold: Box<Hold>,
    },
}

impl Hold {
    /// The hold itself, without the stack layer it is on.
    pub fn innermost(&self) -> &Hold {
        match self {
            Hold::Layer { hold, .. } => hold.innermost(),
            hold => hold,
        }
    }

    /// Held by the agent's review, the queue's first gate of a PR's own.
    pub fn of_review(&self) -> bool {
        match self.innermost() {
            Hold::Reviewing
            | Hold::ReviewAsks
            | Hold::ReviewChanges
            | Hold::UnresolvedThreads { .. } => true,
            Hold::Draft
            | Hold::ReviewRequired
            | Hold::ChangesRequested
            | Hold::WorkflowApproval
            | Hold::ForkEditsOff { .. }
            | Hold::NoQueue { .. }
            | Hold::NoSubscription
            | Hold::FairUseLimit => false,
            Hold::Layer { .. } => unreachable!("innermost is no layer"),
        }
    }

    /// What the agent's review holds a unit for: the first of its layers, bottom to top, whose
    /// newest review does not let it on, each layer with that review's status. A stack's hold
    /// names the layer; one PR's is the review's own.
    pub fn of_reviews(
        stacked: bool,
        layers: impl IntoIterator<Item = (PrNumber, ReviewStatus)>,
    ) -> Option<Hold> {
        layers.into_iter().find_map(|(number, status)| {
            let hold = status.hold()?;
            Some(match stacked {
                true => Hold::Layer {
                    number,
                    hold: Box::new(hold),
                },
                false => hold,
            })
        })
    }

    /// The stack layer it is on, if a layer holds the stack.
    pub fn layer(&self) -> Option<PrNumber> {
        match self {
            Hold::Layer { number, .. } => Some(*number),
            _ => None,
        }
    }
}

impl fmt::Display for Hold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Hold::Draft => f.write_str("the PR is a draft"),
            Hold::ReviewRequired => f.write_str("waiting for a required review"),
            Hold::ChangesRequested => {
                f.write_str("a reviewer requested changes, and their approval is required")
            }
            Hold::WorkflowApproval => {
                f.write_str("waiting for a maintainer to approve its workflow runs")
            }
            Hold::ForkEditsOff { trunk } => write!(
                f,
                "it is behind `{trunk}`, and MergeLoop can push the rebase to its fork only once its author allows edits by maintainers"
            ),
            Hold::NoQueue { base, queues } => {
                let queues: Vec<String> = queues.iter().map(|b| format!("`{b}`")).collect();
                write!(
                    f,
                    "targets `{base}`, which has no queue (queues: {}; `branches` in `{REPO_CONFIG_PATH}` adds one)",
                    queues.join(", ")
                )
            }
            Hold::NoSubscription => f.write_str("private repos need a MergeLoop subscription"),
            Hold::FairUseLimit => f.write_str("over the fair-use limit of public repos"),
            Hold::Reviewing => f.write_str("the agent is reviewing it"),
            Hold::ReviewAsks => {
                f.write_str("the agent's review asks a human about what it was not sure of")
            }
            Hold::ReviewChanges => f.write_str(
                "its review sent changes back to the author: it goes on once they resolve each comment, or push and it is reviewed again",
            ),
            Hold::UnresolvedThreads { count: 1 } => {
                f.write_str("1 review comment is unresolved: resolve it to go on")
            }
            Hold::UnresolvedThreads { count } => {
                write!(f, "{count} review comments are unresolved: resolve them to go on")
            }
            Hold::Layer { number, hold } => write!(f, "#{number} is not ready: {hold}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layer_says_which_and_why() {
        let hold = Hold::Layer {
            number: 7,
            hold: Box::new(Hold::Draft),
        };
        assert_eq!(hold.to_string(), "#7 is not ready: the PR is a draft");
        assert_eq!(hold.innermost(), &Hold::Draft);
        assert_eq!(hold.layer(), Some(7));
    }

    #[test]
    fn no_queue_names_the_queues() {
        let hold = Hold::NoQueue {
            base: "next".into(),
            queues: vec!["main".into(), "release".into()],
        };
        assert_eq!(
            hold.to_string(),
            "targets `next`, which has no queue (queues: `main`, `release`; `branches` in `.mergepatrol/mergepatrol.yml` adds one)"
        );
    }
}
