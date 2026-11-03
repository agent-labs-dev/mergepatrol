---
title: Fewer PRs get ejected for reasons that aren't theirs
kind: fixed
pr: 195
---

MergeLoop now uses a merge method your repo's settings allow, instead of a merge commit that GitHub rejects. When the agent finds nothing to fix in the code, the checks run once more, so a flaky test no longer ejects a PR. Rebase conflicts get twice as many agent turns, and a PR whose changes are already on its base branch says so.
