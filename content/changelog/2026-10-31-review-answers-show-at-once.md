---
title: Answering the agent's review moves the PR on at once
kind: fixed
---

After you answer the agent's review, the queue shows the PR as waiting, or as sent back, right away. Before, it kept saying "Needs a human" until the queue finished the PR it was merging, which in a repo with slow CI could take many minutes. The same goes for the agent finishing its review while another PR merges.
