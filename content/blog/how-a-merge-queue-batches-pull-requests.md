---
title: How a merge queue batches pull requests, and finds the one that broke the build
description: Testing queued PRs one at a time merges a few an hour. Here is how Mergepatrol tests them together in one CI run, and bisects a failed batch.
date: 2026-10-30
---

A merge queue exists to keep `main` green. Every pull request is tested against the code it
will actually land on, not the `main` it branched from days ago. The simplest queue does that
one PR at a time: rebase onto `main`, run CI, merge, next.

That is correct, and slow. If CI takes 20 minutes, the queue merges three PRs an hour, however
many people are waiting. A busy team ends up with a queue longer than its day.

## Test them together

Mergepatrol's answer is batching. When several queued PRs have each passed their own checks, it
tests them as one:

1. It merges them, in queue order, onto a fresh branch at the base branch, named
   `mergepatrol/batch-<PRs>`.
2. It opens a draft PR from that branch, so your existing `pull_request` CI runs on the
   combination. Nothing about your CI changes.
3. When the required checks pass, it merges each PR as it is: no rebase, and no second CI run.
   Each merge must leave the base branch at exactly the tree the batch tested, or it stops.
4. It deletes the batch branch, which closes the draft.

Four PRs that would have taken four CI runs now take one.

## When a batch fails

A failed batch says that one of its PRs broke something. It does not say which. So Mergepatrol
splits it: it tests half of the batch next, and keeps halving down to a single PR. That PR runs
alone, rebased with its own checks, and is ejected. The rest merge.

Halving keeps the cost of a failure small. In a batch of eight, finding the culprit takes about
three more runs, not eight.

## When a PR runs alone

Batching is not always safe, so some PRs skip it:

- A PR whose own checks have not passed yet.
- A PR that conflicts with the PRs ahead of it.
- Any PR into a base branch that requires PRs to be up to date before they merge. Mergepatrol
  reads that from your rulesets and branch protection.
- A PR from a fork. A batch's draft PR runs CI with the repo's secrets, which a fork's author
  should not reach.

## And when a rebase conflicts

A PR that runs alone is rebased onto its base branch first. A plain `git rebase` runs, and only
on a real conflict does an agent resolve it. Your verify commands then run before anything is
pushed, and the result must change no file the PR did not already change.

## Try it

Mergepatrol is free for public repos. [Sign in with GitHub](/auth/login), install the App, and
add the `mergepatrol` label to a pull request.
