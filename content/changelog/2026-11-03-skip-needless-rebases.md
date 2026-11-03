---
title: A green PR merges without a needless rebase
kind: improved
pr: 219
---

A PR on its own that GitHub can already merge, with its checks passed, no longer gets rebased and tested again. It merges right away. A branch that requires PRs to be up to date still rebases every PR.

The merge lane now says what it is doing with a PR: **Rebasing**, **Running checks**, **Fixing checks** or **Merging**, in place of **Testing**.
