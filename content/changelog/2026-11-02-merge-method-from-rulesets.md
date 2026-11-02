---
title: Merges follow the base branch's rulesets
kind: fixed
---

The queue merges with a method the base branch's rulesets allow, so a repo whose ruleset allows only squash no longer has its PRs ejected for a merge commit. Private repos on GitHub Free, which have no rulesets, queue again instead of failing at every step.
