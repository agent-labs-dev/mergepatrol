---
title: One queue per base branch
date: 2026-10-30
kind: new
pr: 102
---

PRs into `release/*` or any other branch get their own queue, so they don't wait behind, or batch
with, PRs into `main`.
