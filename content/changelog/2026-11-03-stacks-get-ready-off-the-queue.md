---
title: Stacks get ready without holding up the queue
kind: improved
pr: 216
---

A stack now gets ready the way a single PR does: its conflicts are resolved, its checks run and the agent fixes what fails, all off the queue, so no other PR waits for it.

The agent also fixes what the review sends back on a stack. It commits on the layer the review read and rebases the layers above onto the fix.
