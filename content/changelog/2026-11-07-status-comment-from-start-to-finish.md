---
title: The status comment shows when a PR joins and leaves the queue
kind: fixed
pr: 283
---

A PR gets its status comment as soon as it joins the queue, and while MergeLoop gets it ready, instead of only once it is held. When you remove the label, close the PR, or merge it yourself, the comment says it left the queue, so it no longer claims the PR keeps its place.
