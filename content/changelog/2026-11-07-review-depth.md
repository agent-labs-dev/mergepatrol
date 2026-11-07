---
title: Choose how hard the review looks, from Quick to Deep
kind: new
pr: 279
image: /changelog/review-depth/after.png
---

**Settings → Automatic review** has a **Review depth**: Quick, Standard or Deep. Standard is the review you have today, and stays the default. Quick reports only what could be wrong, without the notes on what the agent checked and found sound.

Deep is for code where one missed bug costs more than the extra review. Three reviewers look at once, each on its own, and what they find is merged so you see each issue once. Every change no failing test shows, of any kind, then goes to three fresh agents, and comes back to you only when two of them say it holds. A deep review spends several times what a standard one does, and each run's spend shows on the dashboard.

A repo can set its own depth, over the workspace's, from its settings or in its config file as `review.depth`.
