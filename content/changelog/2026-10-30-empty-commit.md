---
title: PRs with an empty commit get through the queue
kind: fixed
pr: 114
---

A PR with a `git commit --allow-empty` in it, often pushed to rerun CI, was ejected on every try.
It now lands, with the empty commit kept.
