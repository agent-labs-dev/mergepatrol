---
title: The agent's own fixes get reviewed before they merge
kind: improved
pr: 197
---

A fix for failed checks, or a conflict the agent resolved in a rebase, no longer merges on the approval of the code before it. The agent reviews that change alone, and the PR merges once it passes. A clean rebase keeps its approval.
