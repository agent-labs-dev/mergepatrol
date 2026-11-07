---
title: See how the agent's review is doing
kind: new
pr: 271
image: /changelog/review-numbers/review.png
---

**History** now shows how the agent's review did over the window, for all repos or the one you pick. Each change it asked for is sorted by what became of it: fixed, wrong, intended, dismissed, or still open. Beside that, you see how many PRs passed on their first review or within one round of fixes, how many took five reviews or more, and how long PRs waited on a person.

A change the author replied to and the agent then withdrew says whether the finding was wrong or the code does what was meant. Agents read the same numbers with the `get_review_stats` tool, and each finding's outcome shows in `get_agent_review`.
