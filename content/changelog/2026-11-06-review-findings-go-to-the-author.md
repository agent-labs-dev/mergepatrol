---
title: What the review finds goes to the PR's author, as a review that requests changes
kind: improved
---

MergeLoop's agent no longer fixes what its review finds on its own. Each finding that must change goes back to the PR's author, or the author's agent, as a GitHub review that requests changes, with a thread on its line. The author decides what to make of each one: fix it and push, or reply with why it doesn't apply.

GitHub holds the PR too while the request stands, so nobody merges it by hand past the review. Once the author resolves every thread, or a later review finds nothing to send back, MergeLoop dismisses its request and the PR goes on. A review that only asks a human stays a comment. MergeLoop's agent still fixes failed checks.
