---
title: Your own agent can resolve conflicts and fix failed checks
kind: new
---

Set `agent.runs: local` in a repo's `.mergeloop/mergeloop.yml`, and MergeLoop leaves a queued PR's conflicts and failed checks to its author's agent, such as Claude Code or Codex, instead of running its own. The PR keeps its place while it waits, and the dashboard shows it as **Conflict** or **Checks failed**.

An agent that runs `mergeloop` on the PR is told what to rebase onto, or which checks failed and how to read why. Its push settles it, and the PR goes on. MergeLoop's agent still reviews the PR, and repos that leave the setting out work as before.
