---
title: A merge closes the issues your commits close
kind: fixed
---

When a commit in a queued PR says `Fixes #12`, and the PR's description doesn't, the queue adds `Closes #12` to the description before it merges. GitHub then closes the issue, even when the repository squashes with a message that leaves the commit bodies out.
