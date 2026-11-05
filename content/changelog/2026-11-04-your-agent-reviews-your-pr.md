---
title: With agent.runs local, your own agent runs MergeLoop's review too
kind: new
---

In a repo that sets `agent.runs: local`, the review that gates a queued PR now runs on its author's agent as well. `mergeloop` hands the agent the same review MergeLoop's own agent would run, with what the last review sent back, to run in a fresh subagent on a checkout of the PR. The agent posts what it found, and MergeLoop holds it to your repo's bars as always: what must change comes back as review threads to work, and what needs a call goes to a human.

The queue page shows such a PR as **Author's agent reviews** until the review comes in. A repo on this setting no longer needs a model key in MergeLoop for its reviews.
