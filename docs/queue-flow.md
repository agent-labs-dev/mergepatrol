# How a PR moves through the queue

A PR gets ready on its own, and a stack gets ready as one, so here a stack reads as a PR. Only the merge lane runs one at a time, so no PR's CI or agent holds up another. `MERGEPATROL_PREP=false` runs all of it in the lane instead.

```mermaid
flowchart TD
    queued(["PR queued"]) --> skip{"Fork?"}
    skip -- "no" --> rebase
    skip -- "yes" --> review

    subgraph ready ["Getting ready: each PR on its own, holding up no other"]
        rebase["Rebase, on a conflict"]
        ci["CI"]
        fixci["Fix"]
        patrols["bugpatrol / speedpatrol / sloppatrol"]:::later
        review["Code review"]
        fixreview["Fix"]
        human["Human answers: keeps its place"]

        rebase --> ci
        ci -- "failed" --> fixci
        fixci --> ci
        ci -.-> patrols
        patrols -.-> review
        ci -- "passed" --> review
        review -- "sent back" --> fixreview
        fixreview -- "new head: CI runs again" --> ci
        review -- "asks" --> human
        human -- "resumes" --> review
    end

    review -- "passed" --> gates["GitHub would merge it: required reviews, no changes requested"]

    subgraph lane ["Merge lane: one at a time"]
        batch["Batch on the current trunk, or alone"]
    end

    gates --> batch
    ready -- "cannot settle it" --> batch
    batch -- "conflict" --> rebase
    batch -- "failed, fixes left" --> fixci
    batch -- "failed" --> ejected{{"Ejected: back to a human"}}
    batch -- "passed" --> merged(["Merged"])

    classDef later stroke-dasharray: 5 5
```

Dashed is planned, and not built.

- **Rebase** runs only on a conflict with the trunk. A batch merges PRs as they are. A PR alone is rebased in the lane, unless GitHub can merge it as it is: not a stack, no conflict, a trunk that is not strict, checks passed on its head, and not a suspect of a failed batch.
- **Fixes** are one budget for a PR or a stack, shared by CI and review: `fix_ci.attempts`, 3 by default. They are counted from the `Fixed-by:` commits on top of the author's last commit, so a push from the author earns new ones.
- **A stack** rebases and publishes as one. A conflict on any layer rebases it all. A fix goes on the lowest layer that failed, or the layer the review read, and the layers above it are rebased onto the fix.
- **A fork** goes straight to the lane. No agent runs on it, so there is nothing to rebase past a conflict or to fix. Waiting for its checks here would save the lane little: a fork never joins a batch, so unless it sits on the current trunk, the lane rebases it and runs its checks again.
- **A title check** can't be fixed by a commit. The agent proposes a title, MergeLoop sets it, and an empty commit runs the checks once more.
- **A fix or an agent's conflict resolution** needs a review of that change alone. A clean rebase keeps its approval.
- **A review's fix** covers only what it sent back. A human keeps what the review asks about, and a PR with no fixes left waits for its author.
- **Getting ready never ejects.** What it cannot settle goes on to the lane, whose job ejects as it always did: a failed job, no fixes left, or a check that never finishes.
- **The lane's job** rebases a PR that runs alone when it must, and waits for its checks. A skipped rebase means the PR merges on checks that ran before the trunk's newest commits, so nothing tested the two together. If GitHub refuses that merge, the job rebases after all. A conflict or a failed check goes back to getting ready, and the PR keeps its place while the next one goes on. Only forks and heads getting ready could not settle stay in the lane.

The code is `crates/mergepatrol/src/queue/prep.rs` for getting ready, `run_job` in `crates/mergepatrol/src/queue.rs` for the lane, and `fix_sent_back` in `crates/mergepatrol/src/review.rs` for a review's fix.
