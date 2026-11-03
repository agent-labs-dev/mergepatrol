---
title: The review holds a PR only for what must change
kind: improved
pr: 211
---

The review now weighs how much a finding matters, not just how sure it is. Style, rare paths that do little harm and small costs show up as notes and no longer hold the PR.

When the agent fixes a finding, it checks the finding against the code first, and keeps its fix small.
