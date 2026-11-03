---
title: The queue page shows what is in line and what is not yet
kind: improved
pr: 194
image: /changelog/queue-split-list/after.png
---

The **Queue** page now splits a repo's PRs by where they are. **Merge lane** has the PRs in line, numbered in merge order. **Getting ready** has the PRs that wait on the agent's review, the ones that need a human first.

**Held** has the PRs GitHub would not merge yet. **Ejected** lists the open PRs the queue ejected in the last week, with why, so you can queue them again.
