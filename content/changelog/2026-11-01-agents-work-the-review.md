---
title: Coding agents work their own review
kind: improved
---

The Mergepatrol skill that setup adds to a repo now has coding agents stay with a PR after they queue it. They fix what the review finds, reply to each comment as fixed, declined with a reason, or needing a human, and keep one review ledger comment up to date on the PR. You're only asked about calls they can't make themselves, such as scope, product decisions or access.

The agent never answers Mergepatrol's review of its own PR, so the review stays independent.
