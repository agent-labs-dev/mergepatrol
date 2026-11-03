---
title: A review cut off by a restart starts again right away
kind: fixed
pr: 212
---

When MergeLoop restarted during an agent's review, the PR showed "under review" for up to 45 minutes before the review ran again. It now runs again as soon as MergeLoop is back.
