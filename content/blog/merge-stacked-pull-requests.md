---
title: How to merge stacked pull requests without breaking main
description: Stacked PRs make review easier and merging harder. How a merge queue lands a gh stack stack all at once, what happens when one layer fails, and what to set up.
date: 2026-10-31
---

Stacked pull requests split one large change into a chain of small ones: the first targets
`main`, the second targets the first's branch, and so on. Reviewers get changes they can read in
ten minutes. The cost lands at merge time.

## Why stacks are hard to merge

Merging a stack by hand goes like this: merge the bottom pull request, retarget the next one onto
`main`, rebase it, wait for CI, merge it, and repeat for each layer. Every step is a chance for
someone else's merge to land in between and send a layer back to CI, and every layer merged alone
leaves `main` holding half a feature.

Two things make it worse with a merge queue that does not know about stacks:

- **It sees layers as unrelated pull requests.** A layer above the bottom targets another
  pull request's branch, not `main`, so the queue either refuses it or merges it into the wrong
  branch.
- **A failure leaves a half-merged stack.** If layer three fails after layers one and two
  merged, `main` has an incomplete change until someone fixes it.

## What a stack-aware queue does instead

Mergepatrol treats a stack made with `gh stack` as one item in the queue:

- **One label queues the whole stack.** Put `mergepatrol` on any of its pull requests. The stack
  joins the queue of the branch the whole stack merges into.
- **It waits for every layer.** While any layer is a draft or waits for a review, the stack waits,
  and keeps its place.
- **It merges all at once, or not at all.** The merge goes through GitHub's asynchronous merge, so
  either every pull request in the stack merges or none does. `main` never holds half of it.
- **It rebases layer by layer.** Run alone, the bottom layer is rebased onto the base branch and
  each layer onto the one below it, and each layer's checks must pass.
- **It fixes the layer that failed.** When a required check fails, the agent commits a fix on the
  layer it failed on, and the layers above are rebased onto the fix. A stack that still fails is
  ejected whole.
- **It batches with everything else.** In a batch, the stack is one step, tested together with the
  other queued pull requests.

A pull request stacked by hand, targeting another pull request's branch without `gh stack`, is
held: the queue cannot tell it is part of a stack.

## Setting it up

1. Make the stack with [`gh stack`](https://github.com/github/gh-stack): `gh stack add` per layer,
   then `gh stack submit`.
2. [Install Mergepatrol](https://mergepatrol.ai) on the repository and merge its setup pull
   request, which adds `.mergepatrol/mergepatrol.yml`.
3. Get the layers reviewed as your branch requires.
4. Label any layer `mergepatrol`, or from a script:

   ```sh
   gh pr edit <number> --add-label mergepatrol
   ```

   A coding agent connected to Mergepatrol's MCP server can call `queue_pull` on any layer instead,
   which queues the stack.

To take the stack out, use the dashboard's Queue page, or call `dequeue_pull` on any layer: it takes the
whole stack out.

## FAQ

**Can a stack include a pull request from a fork?**
Not yet. Queue fork pull requests on their own.

