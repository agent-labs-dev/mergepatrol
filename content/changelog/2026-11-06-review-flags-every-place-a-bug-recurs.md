---
title: A bug that recurs is one finding, with every place it is
kind: improved
pr: 276
image: /changelog/review-flags-every-place-a-bug-recurs/after.png
---

When the automatic review finds a bug that the PR repeats, it reports it once and lists every place the PR has it. The finding's thread on GitHub and the review on the dashboard show each place, and its test fails until every one is fixed, so one push settles it instead of a new finding each round.

Places in code the PR didn't bring go in their own note, like any bug that was already there, and hold nothing.
