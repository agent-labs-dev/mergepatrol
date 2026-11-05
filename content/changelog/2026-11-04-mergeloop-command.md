---
title: Agents land a PR with the mergeloop command
kind: new
---

Tell your coding agent to run `mergeloop` on a PR. It queues the PR, then waits, and returns the moment the PR needs the agent: a review thread to work, a draft to publish, an ejection to fix. Each time it says what to do, and which command to run once that's done, until the PR merges.

The agent no longer has to poll the PR to find out it needs attention. When the PR waits on a human, `mergeloop` says what for, so the agent can tell you. Install it with `cargo install --locked --git https://github.com/agent-labs-dev/mergeloop mergeloop-cli`. An agent connected over MCP gets the same loop from the `next_need` tool.
