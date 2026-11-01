---
name: mergepatrol
description: This repo merges pull requests through Mergepatrol, a label-driven merge queue. Use when merging, landing or shipping a PR, queueing or dequeueing one, reading a Mergepatrol comment or label on a PR, or working the review comments on a queued PR.
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
- **Under review**: with automatic review on, as it is by default, an agent reviews the PR before it takes its place in line. What it is sure should change it sends back to the PR's author, which is you if you built it, as review comments on their lines. The PR goes on once every review thread on it is resolved, with no human approval. It asks a human only about what the author can't settle: a call on scope or product, access, a change outside the repo. `get_agent_review` over MCP reads the review. If you built the PR, work the review as "After you queue" says, but never answer it.
- **Ejected**: the rebase, the checks or the merge failed, and the comment says which. The trigger label is gone and `<label>:failed` is on. Fix the cause, push, then add `<label>` again.
- **Merged**: done. The queue labels are removed.

## After you queue: work the review

Queuing is not the end of your job. If you built the PR, stay with it until it merges, is ejected, or holds only on a human. Go over every review comment yourself, without waiting for the user to tell you to: fix it, or dismiss it with your reasoning, and resolve it. The PR is held while any review thread is unresolved, and goes on to the queue once none is. Ask a human only for a call you can't make from the code, the repo and what the user told you.

### What counts as a comment

Every unresolved review thread on the PR, whoever started it, is one comment:

- Mergepatrol's review posts a thread on its line for each finding that holds the PR. "Sent back to the author" is yours to work. "For a human" is a finding it asked a human about: leave it to them, and resolve it once they have answered.
- A thread from a human reviewer or another bot.

Also count a review that requests changes, and a top-level comment that asks for one. Findings the review cleared, and its notes under the bar, need nothing from you.

Mergepatrol keeps one thread per finding across pushes. When the next review still finds it, it replies in that thread and opens it again, rather than posting a new one. When the review finds it fixed, or your reply holds up in the code, it resolves the thread itself. Only work a review of the PR's current head: a review of an older head can land after you push, and is out of date.

### The states

| State | Means | What you do |
| --- | --- | --- |
| `open` | Not worked yet. | Read it, and the code it points at. Then fix it, dismiss it, or mark it `needs-human`. |
| `fixed` | A pushed commit does what it asks. | Reply in the thread with the commit's short SHA and one line on what changed, then resolve it. Resolve only after the push. |
| `dismissed` | It doesn't apply, for a reason the code shows. | Reply in the thread with the reason and the evidence: a file and line, a test, a doc. Then resolve it. |
| `needs-human` | Only a human can decide it. | Reply with the decision needed and the options, tag the person, and leave it unresolved. |

A comment is `needs-human` only for one of these: what the PR should do or include (scope), a product or design call, credentials or access you don't have, a change outside this repo, or a reviewer who opened it again after you dismissed it. "I'm not sure" is not one: read more code, run it, or ask the reviewer first. The review stays adversarial: on your next push it reads your replies, and opens a dismissed thread again if your reason doesn't hold in the code.

### The loop

1. **Wait for the review of the current head.** Poll `pull_status` or `get_agent_review` (or the Mergepatrol comment) every few minutes while it is `pending` or `running`, and don't push meanwhile, so each head gets one whole review. Check the PR's review threads at the same time.
2. **List the unresolved threads**, and match each to the ledger. A thread that opened again is `open` again, and counts as a round.
3. **Work each `open` one** to `fixed`, `dismissed` or `needs-human`. Batch the fixes in as few commits as you can: each push gets a new review.
4. **Push once**, if you fixed anything. Fetch and rebase onto the remote branch first.
5. **Reply in each thread you worked, and resolve the `fixed` and `dismissed` ones.** Never resolve one without a reply that says why.
6. **Update the ledger** (below).
7. **Go back to step 1** after a push, for the new head. Stop when no thread is unresolved but the `needs-human` ones, or the PR merged or was ejected. An ejected PR: fix the cause, push, and add `<label>` again.

A PR with no unresolved thread and nothing asked of a human goes on to the queue by itself. You never need a human to approve it then.

### The Mergepatrol review's findings

- Never call `answer_agent_review` on a PR you built, and never approve your own PR with `review_pull`. That would answer your own review.
- After three reviews in a row that send findings back, the next one asks a human about what is still left. Don't try to beat it with a fourth fix: mark those `needs-human`.
- To have the reviewer settle a finding now rather than on the next push, reply to it with `ask_agent_about_review`, giving its `head_sha` and finding id. Its answer, in `questions`, ends `VERDICT: withdrawn` or `VERDICT: stands`.
- A finding about what wasn't checked, such as "never run on a device" or "no tests cover this", is yours to close while you can: run it, or write the test, and say what you ran in its reply. Only when you can't, such as having no device, is it `needs-human`, and it says what a human would have to run.

### The ledger

Keep one comment on the PR that lists every comment and its state, and edit it each round rather than posting a new one. It is the only top-level comment you post about the review: the replies go in the threads. Start it with `<!-- mergepatrol-review-ledger -->` so you find it again, and keep each comment's row across rounds, with the heads whose reviews raised it:

```markdown
<!-- mergepatrol-review-ledger -->
**Review ledger** (head `def5678`)

| Comment | Raised on | State | Note |
| --- | --- | --- | --- |
| Review: retry loop has no cap, `src/retry.rs` | `abc1234` | fixed | `def5678` caps it at 5 |
| Review: lock order, `src/queue.rs:88` | `abc1234` | dismissed | the caller holds the lock, `src/lane.rs:40` |
| @alice: rename `Lane::go` | `def5678` | fixed | renamed in `def5678` |
| Review: never run on a device | `abc1234`, `def5678` | needs-human | no device here; @bob, please run the record-and-send flow on iOS |
| Review: drop the v1 endpoint? | `def5678` | needs-human | asked a human; @bob: keep v1 for old clients, or drop it now? |
```

### What not to do

- Don't resolve a thread before the commit that fixes it is pushed, or without a reply that says why.
- Don't resolve a `needs-human` thread, or one Mergepatrol asked a human about, before the human has answered.
- Don't post a new comment for a reply that belongs in a thread or in the ledger.
- Don't leave a comment `open` when you stop. Each one ends `fixed`, `dismissed` or `needs-human`.
- Don't ping a human for something on this list you could do yourself.

When you stop, tell the user where the PR stands: merged, ejected and why, or what each `needs-human` comment needs from whom.

## Settings

`.mergepatrol/mergepatrol.yml` sets the label, the batch size, the conflict resolver, the `verify` commands, `fix_ci`, and `review`. Mergepatrol reads it from the default branch, so a change takes effect once its PR merges.
