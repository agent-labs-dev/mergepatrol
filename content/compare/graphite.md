---
title: Mergepatrol vs Graphite's merge queue
description: Graphite's merge queue is stack-aware and part of Graphite's review tool. Mergepatrol is a standalone queue for gh stack and plain PRs, with an agent on conflicts and failed checks.
date: 2026-10-31
---

[Graphite](https://graphite.com) is a code review tool built around stacked pull requests, and its
[merge queue](https://graphite.com/docs/graphite-merge-queue) is one part of it. Mergepatrol is a
merge queue on its own: it works with GitHub's pull requests as they are, including stacks made
with `gh stack`. Checked against Graphite's docs and [pricing](https://graphite.com/pricing) on 31
October 2026.

## At a glance

| | Graphite merge queue | Mergepatrol |
| --- | --- | --- |
| Price | On the Team plan ($40 a seat a month, billed annually) as a "Basic" queue, "Advanced" on Enterprise; not listed on Hobby or Starter | Public repos free; private repos $15 a seat a month after a 14-day trial; self-hosted free |
| What you adopt | Graphite's review app, and its `gt` CLI for stacks | A GitHub App and a config file; review stays on GitHub |
| Stacks | Stack-aware; a queued stack is validated in parallel | A `gh stack` stack is one queue item, merged all at once with GitHub's asynchronous merge |
| Batching | In private beta; a failed batch is bisected or every PR isolated | On by default; a failed batch is split in half down to the PR that broke it |
| A PR that conflicts | Rebased automatically, but a real conflict goes back to you to `gt restack` and re-queue | A plain `git rebase` first, then an agent on a real conflict, checked by your `verify` commands |
| A required check that fails | Back to its author | An agent commits a fix on the PR, up to `fix_ci.attempts` times |
| Works alongside GitHub's merge queue | No: GitHub's queue must be off | No: one queue per branch |
| Source | Proprietary SaaS | Apache-2.0, self-hostable |

## Where Graphite is the better fit

- **Your team already reviews in Graphite and stacks with `gt`.** The queue is part of the same
  workflow, and restacking is handled by the same tool.
- **You want a review product, not only a queue.** Graphite's review and stacking UI come with the
  plan that includes the queue.

## Where Mergepatrol is the better fit

- **You review on GitHub and stack with `gh stack`,** or do not stack at all. Nothing changes about
  how people open and review pull requests: they add a label.
- **Conflicts that would send a stack back to its author.** The agent resolves them in place and
  the stack keeps its place. See [How to merge stacked pull
  requests](/blog/merge-stacked-pull-requests).
- **Batching you can use today on any plan,** with bisection by default.
- **Price per seat.** $15 a month for private repos, free for public ones, against a queue that
  starts at Graphite's $40 Team plan.

## FAQ

**Do I need the Graphite CLI to use its merge queue?**
You can queue from Graphite's web app, but its docs note that automatic restacking does not work
when merging from GitHub.

**Does Mergepatrol need `gh stack`?**
Only for stacks. A pull request stacked by hand on another PR's branch is held; a `gh stack` stack
queues as one item. Plain pull requests need nothing.
