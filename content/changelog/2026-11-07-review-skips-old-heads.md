---
title: A review never posts about a commit you already replaced
kind: fixed
pr: 265
---

When you push while MergeLoop's review is running, the review of the older commit is dropped instead of posted, so you no longer get "still holds" about a fix you just pushed. The new commit gets its own review.
