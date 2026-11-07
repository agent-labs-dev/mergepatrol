---
title: One queue per base branch
kind: new
pr: 102
---

List branches such as `release` under `branches` in `.mergeloop/mergeloop.yml`, or in a repo's
settings, and PRs into them get a queue of their own. They no longer wait behind, or batch with,
PRs into `main`.
