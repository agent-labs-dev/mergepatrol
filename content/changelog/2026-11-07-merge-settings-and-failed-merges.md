---
title: A PR GitHub merged is no longer ejected, and a merge-settings change no longer ejects the next PR
kind: fixed
pr: 282
---

When GitHub merges a PR but answers that the merge could not be completed, MergeLoop now sees the PR merged and reports it so, instead of ejecting it. When you turn a merge method off in the repo's settings, the next PR merges with a method still allowed, instead of being ejected.
