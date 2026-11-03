---
title: PRs get ready on their own, without holding up the queue
kind: improved
pr: 199
---

A queued PR no longer needs the head of the line to get its checks green. The agent resolves a conflict and fixes failed checks while the PR waits under **Getting ready** on the Queue page, and other PRs pass it once they are ready. The agent's review now comes after the checks pass, so it reads the code that will merge.
