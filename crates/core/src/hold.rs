use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{PrNumber, REPO_CONFIG_PATH};

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
                "it is behind `{trunk}`, and Mergepatrol can push the rebase to its fork only once its author allows edits by maintainers"
            ),
            Hold::NoQueue { base, queues } => {
                let queues: Vec<String> = queues.iter().map(|b| format!("`{b}`")).collect();
                write!(
                    f,
                    "targets `{base}`, which has no queue (queues: {}; `branches` in `{REPO_CONFIG_PATH}` adds one)",
                    queues.join(", ")
                )
            }
            Hold::NoSubscription => f.write_str("private repos need a Mergepatrol subscription"),
            Hold::FairUseLimit => f.write_str("over the fair-use limit of public repos"),
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
