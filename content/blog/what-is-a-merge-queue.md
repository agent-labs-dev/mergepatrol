---
title: What is a merge queue, and when does a team need one?
description: A merge queue tests each pull request against the code it will land on, so main stays green. How one works, what it costs, and the signs you need one.
date: 2026-10-31
---

A merge queue is a bot that merges pull requests for you, one place in line at a time, and only
after testing each one against the code it will actually land on. You mark a pull request as
ready, the queue takes it from there, and `main` never gets a commit that nobody tested.

That last part is the whole point, and it is easy to miss why it is needed.

## The problem it solves: two green PRs, one red main

Branch protection says a pull request may merge once its checks pass. But the checks ran on the
pull request's branch, against the `main` it was branched from or last rebased onto. If `main`
moved since, nothing tested the combination.

A concrete case:

1. Ana's PR renames `parse_config` to `load_config` everywhere. CI passes.
2. Ben's PR, branched the day before, adds a new call to `parse_config`. CI passes.
3. Both merge. Neither conflicts in Git, because they touch different lines.
4. `main` no longer compiles, and the next person to branch from it inherits the breakage.

GitHub's "require branches to be up to date before merging" setting closes the gap by hand: every
author rebases and re-runs CI before merging, and whoever merges first sends everyone else back to
do it again. On a team of five that is an annoyance. On a team of thirty, people spend their
afternoon rebasing.

A merge queue does that rebase-and-test step for you, in order, so the up-to-date rule holds
without anyone babysitting it.

## How a merge queue works

Every merge queue does some version of these steps:

1. **Join the queue.** A developer, or a coding agent, marks a pull request ready: a label, a
   button, or a comment command, depending on the tool.
2. **Wait for what GitHub requires.** Reviews, no draft status, any other rule the branch has. A
   pull request that is not mergeable yet keeps its place but waits.
3. **Rebase onto the base branch** as it is right now, including everything merged ahead of it.
4. **Run the required checks** on the rebased result.
5. **Merge if they pass, eject if they fail,** and tell the author why.

Doing that one pull request at a time is correct and slow: with a 20-minute CI, the queue lands
three pull requests an hour. So most queues test several queued pull requests together in one CI
run, and when that run fails, split the group to find the one that broke it. We wrote up how
batching and bisection work in [How a merge queue batches pull
requests](/blog/how-a-merge-queue-batches-pull-requests).

## What a queue costs you

- **Latency per pull request.** A queued pull request merges after its CI run on the rebased
  code, not the moment it is approved. Batching hides most of this once the queue is busy.
- **CI minutes.** Each pull request, or each batch, runs CI again. With batching it is roughly one
  extra run per batch, not per pull request.
- **Ejections.** A pull request that conflicts with the code ahead of it, or fails on the combined
  tree, leaves the queue and goes back to its author. On a fast-moving `main` this is the cost
  people feel most. Mergepatrol's answer is to run a coding agent on the conflict or the failed
  check, so the pull request keeps its place: see [Resolve rebase conflicts
  automatically](/blog/resolve-rebase-conflicts-automatically).

## Signs you need one

- `main` breaks with no single commit to blame, and the fix is "rebase and it went away".
- You have turned on "require branches to be up to date", and authors complain about rebasing.
- Pull requests sit approved for hours because nobody wants to be the one to rebase and watch CI.
- Coding agents open pull requests faster than people merge them. An agent that can queue its own
  pull request, rather than merging it, keeps a human in the review and the tests in the loop.

If none of these sound familiar, you probably do not need a queue yet. A team of two on a slow
`main` can rebase by hand.

## Your options on GitHub

- **GitHub's built-in merge queue.** Available on public repositories owned by an organization,
  and on private repositories only with GitHub Enterprise Cloud. CI needs a `merge_group` trigger.
  See [Mergepatrol vs GitHub's merge queue](/compare/github-merge-queue).
- **A hosted merge queue as a GitHub App**, which works on any plan. Mergepatrol is one; the
  [Compare](/compare) pages put it next to the others, and [Get started](/docs/getting-started)
  sets it up in five steps.
- **A self-hosted bot.** bors-ng was the classic one and is now archived: see [bors-ng
  alternative](/blog/bors-ng-alternative). Mergepatrol is Apache-2.0 and self-hostable too.

## FAQ

**Is a merge queue the same as auto-merge?**
No. GitHub's auto-merge merges a pull request once its own checks pass, against whatever `main`
was when they ran. A merge queue tests it against the `main` it will land on.

**Does a merge queue replace code review?**
No. It waits for the reviews your branch already requires. Mergepatrol can also run an agent
review on each queued pull request, which asks a human only about what it is unsure of.

**Does it work with any CI?**
A queue waits on the checks your branch protection requires, so any CI that reports checks to
GitHub works. Mergepatrol runs them through your existing `pull_request` workflows.

**What does a queue do with a PR whose checks fail?**
Most eject it. Mergepatrol can commit a fix first: see [Fix failing CI in the merge
queue](/blog/fix-failing-ci-in-the-merge-queue).
