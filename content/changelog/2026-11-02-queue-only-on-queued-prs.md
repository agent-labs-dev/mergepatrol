---
title: The queue acts only on PRs you queued
kind: fixed
---

MergeLoop looked at comments, reviews and checks on any PR whose review was open, even one you never queued. Now its review, and its answers to people on the PR, stay quiet until the PR carries the queue label and has a place in the queue, and stop again if the PR leaves it.
