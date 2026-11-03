---
title: A failed check no longer holds up the PRs behind it
kind: improved
---

When a PR's checks fail at the head of the queue, it goes back to **Getting ready** for the agent to fix, and keeps its place. The PRs behind it go on in the meantime.
