---
title: A PR title check no longer ejects a PR
kind: fixed
pr: 213
---

When a check fails on a PR's title, the agent sets a title that meets it and runs the checks again, instead of ejecting the PR.
