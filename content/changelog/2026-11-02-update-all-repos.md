---
title: Update every repo from one button
kind: new
pr: 191
image: /changelog/update-all-repos/after.png
---

**Settings** has an **Update all** button. It brings every queuing repo up to the latest settings and agent skill, opens the PRs each repo needs, and queues them, so you no longer go through your repos one by one.

It keeps what each repo sets of its own and only opens PRs for repos that are behind, so you can run it whenever you like. When it finishes, it lists what it did to each repo: the PRs it queued, the repos already up to date, and any it skipped, and why.
