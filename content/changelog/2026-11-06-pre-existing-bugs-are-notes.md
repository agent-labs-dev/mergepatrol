---
title: A bug your PR didn't bring is a note, not a hold
kind: improved
pr: 267
image: /changelog/pre-existing-bugs/after.png
---

When the review finds a bug in code your PR didn't add, it says so: the finding is marked as already there before this PR. It never goes back to the author and never holds the PR, whatever its kind or how sure the agent is.

These bugs are listed under **Already there before this PR**, in the review's comment on GitHub and on the PR's review in the queue. Each one has a **File an issue** link that opens a new issue already filled in with the finding.
