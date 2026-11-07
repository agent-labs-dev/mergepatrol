---
title: The review holds a PR to your repo's own instructions
kind: improved
---

The automatic review reads your repo's `AGENTS.md` and `CLAUDE.md`, and reports code that breaks a convention they state, and a change that leaves a line in them out of date.

Add a `REVIEW.md` to tell the reviewer what matters in your repo: what failure looks like, which paths to skip, how many small findings to report. Put one at the root, or in a directory for the code under it. The reviewer reads them as they are on the PR's base, so a PR cannot change how it is reviewed.
