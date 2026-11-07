---
title: Security findings go back to you only when fresh agents agree
kind: improved
pr: 266
image: /changelog/security-findings-are-voted-on/after.png
---

A security finding the review can't show with a failing test no longer comes back to you on one agent's word. Three fresh agents each check it on their own, seeing only the finding and the code, and it goes back only when two of them say it holds. Otherwise it's a note that says how the vote went.

Each vote, and why the agent cast it, shows on the finding in the review and in its thread on GitHub. When your own agent reviews (`agent.runs: local`), it runs the same check with its subagents, and the review says those votes are its word.
