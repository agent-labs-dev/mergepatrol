---
title: Your agent's review now runs your CI's checks and names who ran it
kind: improved
---

In a repo that sets `agent.runs: local` and no `verify` commands, the review `mergeloop` hands your agent now lists the commands your CI runs, read from `.github/workflows/` at the PR's head, as MergeLoop's own review always did. Its instructions also say plainly that MergeLoop takes your agent's word for a failed check and runs nothing again.

A review your agent posts now names whose agent ran it, since anyone who can write to the repo may review for a PR's author. `post_review` answers with how many findings MergeLoop took and what comes next, in place of a bare "Done.", and `mergeloop` says what it waits on without telling you to run a command it is already running.
