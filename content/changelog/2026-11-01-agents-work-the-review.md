---
title: Review comments go to the agent that built the PR, and resolving them is the gate
kind: improved
---

Automatic review is now on for every workspace with a model key. What it finds to fix goes back to the PR's author, often the coding agent that built it, as a review comment on the line in question: one thread per finding. The author fixes each one and pushes, or replies with why it doesn't apply, then resolves the thread. Once every thread is resolved, the PR goes on to the merge queue with no human approval.

The review stays adversarial. On the next push, it reads the author's replies. A finding still there gets a reply in its own thread, which opens again, never a repeat comment. A thread whose finding is fixed, or whose reply holds up, is resolved. You're asked only about what the author can't settle, such as a call on scope or product, access it lacks, or a change outside the repo. You're also asked when three rounds in a row still send findings back. Migrations still always go to a human.

**Require resolved comments**, on by default in **Settings**, holds a queued PR while any review thread on it is unresolved, whoever started it. Each kind's bar now sets how sure the review must be to send a change back to the author: a lower bar sends back more, and a change under it is a note that holds nothing. The defaults are 60% for security, 70% for dependencies and public APIs, and 80% for everything else.

The Mergepatrol skill that setup adds to a repo has coding agents work every comment after they queue: fixed, dismissed with a reason, or needing a human. They resolve each one as they go, and keep one review ledger comment up to date.
