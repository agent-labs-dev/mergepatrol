---
title: A change has to show its evidence, or it is only a note
kind: improved
---

A review finding that asks the author to change something now has to show why. Every finding leaning **change** of a kind other than security carries evidence: a failing test, a command with what it printed, or a repro. MergeLoop puts that evidence in the finding's thread, under **Shows it**, so the author knows what to run and what to see change.

Without one, the finding no longer goes back to the author. It shows under **What it cleared** as a note that holds nothing, so a reviewer's hunch about a bug it could not demonstrate does not stop a PR or send its author chasing it. **Security** findings still go back on the code alone: the code showing the hole is enough to close it. Migrations still always go to a human, and the per-kind confidence bars work as before.
