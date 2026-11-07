---
title: Fix commits follow your repo's commit convention, and leave stray files out
kind: fixed
pr: 281
---

When MergeLoop fixes a failed check, its commit subject follows your repo's convention, such as `fix(auth): ...`, so a commit-msg hook no longer rejects it and ejects the PR. Its commits also leave out files your checks leave behind, such as coverage output, so a resolved conflict is no longer ejected for changing files outside the PR.
