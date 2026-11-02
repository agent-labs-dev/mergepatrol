---
title: Fix failing CI in the merge queue, without sending the PR back
description: A required check that fails in the queue usually ejects the PR. How MergeLoop's agent commits a fix on the PR instead, what it may touch, and when it gives up.
date: 2026-10-31
---

A merge queue tests each pull request against the code it will land on. That is where a lot of
failures first show up: the PR passed on its own branch, then a test broke once it was rebased onto
what merged ahead of it. In most queues the PR is ejected, its author gets a notification, and it
goes to the back of the line once they have fixed it, often hours later.

Most of those failures are small. A renamed function, a changed fixture, a snapshot that needs
updating, a lint rule the new code on `main` turned on. This article is about fixing those in the
queue, on the PR, while it keeps its place.

## What the fix agent does

When a queued PR's required checks fail, MergeLoop runs a coding agent on it (Claude Code for
Anthropic's models, Codex for OpenAI's, on the workspace's own key):

1. **It reads the failure.** The agent gets each failed check's output and annotations, and for a
   GitHub Actions job, the end of its log.
2. **It commits a fix on top of the PR,** in the same sandbox a rebase runs in. It may change any
   file except those under `.github/` and `.mergepatrol/`, so it cannot edit CI or the queue's own
   settings to make a check pass.
3. **Your `verify` commands run** before anything is pushed.
4. **The checks run again.** The PR keeps its place at the head of the queue meanwhile.

The fix is a normal commit on the PR, so the author and reviewers see exactly what changed, and
the dashboard shows the diff of every commit the fix agent pushed.

## When it gives up

A queue that keeps "fixing" a PR forever is worse than one that ejects it. So:

- **It stops after `fix_ci.attempts` fixes** that do not make the checks pass, 2 by default and at
  most 5, and the PR is ejected.
- **It gives up at once when the failure is not the PR's to fix,** such as a flaky test or an
  outage. The PR is ejected with the reason, rather than patched around a problem it did not cause.
- **Required checks that have not finished 3 hours** after a rebase or fix eject the PR, with no
  fix. A required check that no workflow runs would otherwise hold the queue forever.
- **PRs from forks get no agent at all.** Their author is not trusted the way the repo's own are,
  so a failed check ejects a fork PR for its author to fix.

An error outside the PR's job, such as GitHub being slow or a rate limit, does not count as a
failure: the job runs again.

## Setting it up

The fix agent is on by default once a workspace has a model key. In
`.mergepatrol/mergepatrol.yml`:

```yaml
fix_ci:
  enabled: true    # an agent fixes failed required checks
  attempts: 2      # 1 to 5 fixes, before the PR is ejected
verify:
  - cargo check
```

Give the App `actions: read` so the agent can read GitHub Actions logs; without it, the agent works
from the check output alone. The full reference is in [Configuration](/docs/configuration).

## Is it safe?

The agent's commit goes through the same required checks as any other, and through your `verify`
commands first, so a fix that does not pass does not merge. It cannot touch `.github/` or
`.mergepatrol/`. It runs in a sandboxed job whose GitHub token can read only the one repo: see
[how jobs are isolated](/docs/self-hosting#how-jobs-are-isolated). If you would rather a human fix
every failure, set `fix_ci.enabled: false`: the queue then ejects on failure, as other queues do.

## Related

- [Resolve rebase conflicts automatically](/blog/resolve-rebase-conflicts-automatically), the
  same agent on the other common reason a PR is ejected.
- [What is a merge queue](/blog/what-is-a-merge-queue).
- [How MergeLoop compares](/compare) with GitHub's merge queue, Mergify, Graphite, Aviator and
  Trunk, none of which fix a failed check in the queue as of October 2026.
