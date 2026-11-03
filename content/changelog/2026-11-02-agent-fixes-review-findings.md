---
title: The agent fixes what its review finds
kind: new
pr: 198
---

When the review sends a finding back, the agent now tries to fix it first: it pushes a commit, and the next review checks it. If it can't, the finding waits for the author as before. Findings a human asked about stay with humans.

A PR gets three fixes in all, for its checks and its review together, and a push from its author earns new ones. **Fix attempts** in a repo's settings sets the number, and turning fixes off leaves findings to the author.
