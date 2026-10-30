---
title: The agent reviews each queued PR, and asks you only what it isn't sure of
kind: new
image: /changelog/automatic-review/answer.png
---

Turn on **Automatic review** in **Settings**, and an agent reviews every queued PR before it takes
its place in line. It clears what it's sure of on its own. What it isn't sure of, it asks a person
about: the PR shows **Needs a human** in the queue, and the code owners get a review request.

**Answer them** shows only those findings, one at a time, each with its code, why the agent was
unsure, and what it already checked. Mark each fine as is, ask for a change on its line, or ask the
agent to dig further. Your answers go to GitHub as your review: all fine, and the PR takes its
place; a change, and it waits for the next push, then the agent looks again.

Set how sure is sure enough for each kind of change, against how the agent's confidence has held
up. Migrations always go to a person by default, and a repo can set its own bars in its config
file.
