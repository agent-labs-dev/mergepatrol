---
title: A PR that keeps up by merging gets a merge, not a rebase
kind: fixed
---

When your branch keeps up with `main` by merging it in, MergeLoop now brings it up to date the same way, unless the repo merges PRs by rebasing them: it merges `main` into the branch and pushes that merge commit, never a force-push. Before, it rebased the branch, replaying each commit, so it hit conflicts your merges had already resolved, or took so long that the PR was ejected with "the job did not finish". A rebase that still runs out of time now merges instead, and no longer ejects the PR.
