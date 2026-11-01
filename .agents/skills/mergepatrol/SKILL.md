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
- **Under review**: with automatic review on, an agent reviews the PR before it takes its place in line. It clears what it is sure of, and asks a human about the rest, in a comment on the PR. The PR waits for their answers: all fine, and it goes on; a change asked for, and it waits for a push, then is reviewed again. `get_agent_review` over MCP reads the review. If you built the PR, work the review as "After you queue" says, but never answer it.
- **Ejected**: the rebase, the checks or the merge failed, and the comment says which. The trigger label is gone and `<label>:failed` is on. Fix the cause, push, then add `<label>` again.
- **Merged**: done. The queue labels are removed.

## After you queue: work the review

Queuing is not the end of your job. If you built the PR, stay with it until it merges, is ejected, or holds only on a human. Fix what reviewers find yourself; don't wait for the user to tell you to. Ask a human only for a call you can't make from the code, the repo and what the user told you.

### What counts as a comment

Each of these is one comment, and each gets a state:

- A finding the Mergepatrol review asks about (`asked: true` in `get_agent_review`, or numbered in its review on the PR).
- A change a human asked for in `answer_agent_review`'s answers (status `changes`), by finding id.
- An unresolved review thread on the PR, from a human or a bot.
- A review that requests changes, or a top-level comment that asks for one.

Findings the review cleared need nothing from you.

Each push gets a new review that numbers its findings from 1 again, and often words the same concern differently. So a finding's number is no name for it: name each comment by what it is about and where (its file, or the PR as a whole), and match each new finding against the comments you already have. A finding that matches one is that comment coming back, not a new one. Only work a review of the PR's current head: a review of an older head can land after you push, and is out of date.

### The states

| State | Means | What you do |
| --- | --- | --- |
| `open` | Not worked yet. | Read it, and the code it points at. Decide on one of the three below. |
| `fixed` | A pushed commit does what it asks. | Reply to it with the commit's short SHA and one line on what changed. Resolve a thread only after that push. |
| `declined` | You won't change it, for a reason the code shows. | Reply with the reason and the evidence: a file and line, a test, a doc. Leave a human's thread open for them; resolve a bot's. |
| `needs-human` | Only a human can decide it. | Say what decision is needed and the options, and tag the person. Leave it open. |

A comment is `needs-human` only for one of these: what the PR should do or include (scope), a product or design call, credentials or access you don't have, a change outside this repo, or a reviewer who still disagrees after you declined. "I'm not sure" is not one: read more code, run it, or ask the reviewer a question first.

### The loop

1. **Wait for the review of the current head.** Poll `pull_status` or `get_agent_review` (or the Mergepatrol comment) every few minutes while it is `pending` or `running`, and don't push meanwhile, so each head gets one whole review. Check for review threads and comments on the PR at the same time.
2. **List the comments** above. Match each against the ledger: a new one is `open`; one you marked `fixed` that came back is `open` again, and counts toward its tries.
3. **Work each one** to `fixed`, `declined` or `needs-human`. Batch the fixes in as few commits as you can: each push starts the PR over, and the new head gets a new review.
4. **Push once.** Fetch and rebase onto the remote branch first. Then reply in each review thread you worked, in that thread, with its state, and resolve the ones you fixed. The Mergepatrol review's findings have no threads: their replies are their rows in the ledger, not a new comment.
5. **Update the ledger** (below).
6. **Go back to step 1** for the new head. Stop when nothing is `open`, and every finding left is `declined` or `needs-human`, or the PR merged or was ejected. An ejected PR: fix the cause, push, and add `<label>` again.

### The Mergepatrol review's findings

Its asked findings are for a human to answer, and its review is adversarial on purpose, so:

- Never call `answer_agent_review` on a PR you built, and never approve your own PR with `review_pull`. That would answer your own review.
- `fixed`: push the fix. The new head's review shows whether the finding is gone.
- `declined`: ask the reviewer with `ask_agent_about_review`, giving your reason and evidence. Its answer shows in `questions`. A question never clears a finding, so a declined finding that is still asked is also `needs-human`: the human answers it, with your reason and the agent's answer in front of them.
- A finding that comes back after two fixes is `needs-human`. Don't try a third time.
- A finding about what wasn't checked, such as "never run on a device" or "no tests cover this", is yours to close while you can: run it, or write the test. Say what you ran in its row. Only when you can't, such as having no device, is it `needs-human`, and it says what a human would have to run.

### The ledger

Keep one comment on the PR that lists every comment and its state, and edit it each round rather than posting a new one. It is the only comment you post about the review: no "addressed in <commit>" comment per round, which leaves the reader to match its numbers to a review that has since been replaced. Start it with `<!-- mergepatrol-review-ledger -->` so you find it again, and keep each comment's row across rounds, with the heads whose reviews raised it:

```markdown
<!-- mergepatrol-review-ledger -->
**Review ledger** (head `def5678`)

| Comment | Raised on | State | Note |
| --- | --- | --- | --- |
| Review: retry loop has no cap, `src/retry.rs` | `abc1234` | fixed | `def5678` caps it at 5 |
| @alice: lock order, `src/queue.rs:88` | `abc1234` | declined | the caller holds the lock, see `src/lane.rs:40` |
| Review: never run on a device | `abc1234`, `def5678` | needs-human | no device here; @bob, please run the record-and-send flow on iOS |
| Review: drop the v1 endpoint? | `def5678` | needs-human | @bob: keep v1 for old clients, or drop it now? |
```

### What not to do

- Don't resolve a thread, or mark anything `fixed`, before the commit that fixes it is pushed.
- Don't resolve a thread without a reply that says why.
- Don't post a new comment for a reply that belongs in a thread or in the ledger.
- Don't leave a comment `open` when you stop. Each one ends `fixed`, `declined` or `needs-human`.
- Don't ping a human for something on this list you could do yourself.

When you stop, tell the user where the PR stands: merged, ejected and why, or what each `needs-human` comment needs from whom.

## Settings

`.mergepatrol/mergepatrol.yml` sets the label, the batch size, the conflict resolver, the `verify` commands, `fix_ci`, and `review`. Mergepatrol reads it from the default branch, so a change takes effect once its PR merges.
