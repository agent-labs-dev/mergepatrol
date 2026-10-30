---
title: PRs with an empty commit get through the queue
date: 2026-10-30
kind: fixed
pr: 114
---

A PR with a `git commit --allow-empty` in it, often pushed to rerun CI, was ejected on every try.
It now lands, with the empty commit kept.
