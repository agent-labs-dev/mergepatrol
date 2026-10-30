---
name: mergepatrol
description: This repo merges pull requests through Mergepatrol, a label-driven merge queue. Use when merging, landing or shipping a PR, queueing or dequeueing one, or reading a Mergepatrol comment or label on a PR.
---

# Mergepatrol

Pull requests in this repo land through Mergepatrol's merge queue. Queue a PR with a label and let Mergepatrol merge it; merging by hand or pushing to the default branch races the queue.

## The labels

The trigger label is `label` in `.mergepatrol/mergepatrol.yml` on the default branch, `mergepatrol` when the file leaves it out. Below, `<label>` stands for it.

- `<label>` queues the PR: `gh pr edit <number> --add-label <label>`. Removing it leaves the queue, and cancels the PR's run.
- `<label>:priority`, beside `<label>`, puts the PR at the front of the queue.
- `<label>:failed` is set by Mergepatrol when it ejects the PR.

## Over MCP

When the `mergepatrol` MCP server is connected, use its tools instead of the labels: `queue_pull` (with `priority` for the front of the queue), `dequeue_pull`, `pull_status` and `list_queue`. They change the same labels, as the GitHub user or bot the server was connected as, so everything below still holds. `pull_status` reads where a PR is without parsing Mergepatrol's comment. The server's other tools do what the Mergepatrol dashboard does, such as reviewing a PR or reading why the queue ejected one.

## What the queue does

Mergepatrol rebases the PR onto its base branch, and may test it in a batch with other queued PRs. It resolves conflicts with an agent, runs the repo's `verify` commands, waits for the required checks, then merges with the repo's default merge method. When a required check fails, an agent commits a fix to the PR branch and the checks run again, a few times at most, unless `fix_ci` turns it off. The default branch has a queue, and so does each branch listed under `branches` in `.mergepatrol/mergepatrol.yml`: a PR, or a stack of PRs (from `gh stack`), joins the queue of the branch it merges into. A PR into any other branch, such as one stacked by hand on another PR's branch, is held. Retargeting a queued PR moves it to the back of the other branch's queue.

A PR from a fork merges only as it was when the label went on: a push to it after that takes it out of the queue, until a maintainer adds `<label>` again. It never joins a batch, gets no agent, and is ejected on a conflict or a failed check. Mergepatrol can push its rebase only while the author allows edits by maintainers.

A stack is one item of the queue. The label on any of its PRs queues the whole stack, and the stack waits until every PR in it is ready. Mergepatrol rebases every layer when the stack is behind, and merges the stack all at once: every PR, or none. When it ejects a stack, every PR in it gets `<label>:failed`.

The rebase rewrites the PR branch, and a fix adds a commit to it. Before pushing more commits to a queued PR, fetch it and rebase your work onto the remote branch. A push to a queued PR makes Mergepatrol start the PR over.

## Reading its status

Mergepatrol keeps one comment on each queued PR up to date. It says one of:

- **On hold**: the PR is a draft, waits for a required review, or has changes requested. Mergepatrol comments the reason on the PR. It keeps its place, and runs once GitHub shows it ready.
- **Ejected**: the rebase, the checks or the merge failed, and the comment says which. The trigger label is gone and `<label>:failed` is on. Fix the cause, push, then add `<label>` again.
- **Merged**: done. The queue labels are removed.

## Settings

`.mergepatrol/mergepatrol.yml` sets the label, the batch size, the conflict resolver, the `verify` commands, and `fix_ci`. Mergepatrol reads it from the default branch, so a change takes effect once its PR merges.
