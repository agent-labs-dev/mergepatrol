---
title: MergeLoop vs Aviator MergeQueue
description: Aviator MergeQueue offers parallel mode and affected targets for monorepos. MergeLoop adds an agent that resolves conflicts and fixes failed checks. How the two compare.
date: 2026-10-31
---

[Aviator](https://www.aviator.co) sells MergeQueue alongside stacked-PR tooling (the open-source
`av` CLI) and other developer workflow products. MergeLoop is a merge queue only, with a coding
agent for conflicts and failed checks. Checked against Aviator's
[docs](https://docs.aviator.co/mergequeue/concepts/parallel-mode) and
[pricing](https://www.aviator.co/pricing) on 31 October 2026.

## At a glance

| | Aviator MergeQueue | MergeLoop |
| --- | --- | --- |
| Price | Team $20 a developer a month (parallel queues, batching); Scale $50 (affected targets); Enterprise custom. A free plan covers up to 3 repos; the pricing page does not say it includes MergeQueue | Public repos free; private repos $15 a seat a month after a 14-day trial; self-hosted free |
| Parallel testing | Parallel mode: a draft PR per queued PR, stacked on the ones ahead | No speculative builds: one batch of up to `batch_size` PRs (8 by default) |
| Batching | A failed batch is re-queued as two halves | A failed batch is split in half down to the PR that broke it |
| Monorepos | Affected targets, on Scale | One queue per branch; no per-target queues |
| A PR that conflicts | Back to its author: we found no conflict agent in its docs | An agent resolves the conflict, checked by your `verify` commands |
| A required check that fails | Back to its author | An agent commits a fix on the PR, up to `fix_ci.attempts` times |
| Stacked PRs | With the `av` CLI; a stack is validated as one PR | With `gh stack`; a stack merges all at once |
| Source | Proprietary SaaS; `av` CLI open source | Apache-2.0, self-hostable |

## Where Aviator is the better fit

- **Large monorepos.** Affected targets let unrelated changes skip each other's queue, and parallel
  mode runs a build per queued PR.
- **You already stack with `av`.**

## Where MergeLoop is the better fit

- **Conflicts and failed checks are what slow your queue,** not CI throughput. MergeLoop's agent
  keeps those pull requests in line instead of sending them back.
- **You want no CI changes.** Batches run on a draft PR with your existing `pull_request`
  workflows.
- **You want to self-host,** or pay nothing on public repositories.

## FAQ

**Does Aviator resolve conflicts with AI?**
We found no conflict or CI-fix agent in Aviator's MergeQueue docs as of 31 October 2026. Its AI
feature, Verify, checks that a change matches its intent.
