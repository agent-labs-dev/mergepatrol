---
title: How to resolve git rebase conflicts automatically in CI
description: A rebase conflict stops the queue and waits for a human. Here is the safe way to have an agent resolve it, verify it, and merge.
date: 2026-10-30
---

A rebase conflict is the one failure in a merge queue that a machine currently cannot finish on
its own. The queue stops, the pull request sits there, and someone has to fetch the branch,
resolve the conflict by hand, force-push, and re-queue. On a busy repository that is a few times
a day; when most of your pull requests are written by coding agents, it is most of the day.

This guide covers the three ways teams automate it, what each one actually resolves, and the
checks a good automated resolution has to pass before it is allowed to merge. The short version:
a plain rebase should do all the mechanical work, a model should be given only the conflicts that
are genuinely conflicts, and the result must be verified before it is pushed.

## Why a merge queue does not resolve this for you

Worth knowing, because it explains where the human actually enters the loop.

GitHub's merge queue tests each pull request against the current target branch plus everything
queued ahead of it. If that combination fails, or if the pull request conflicts with the base
branch, **GitHub removes the pull request from the queue** ([GitHub docs, Managing a merge
queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)).
The entry is out, the queue moves on, and the author is left to rebase on their own and enqueue
again.

So the queue preserves your default branch; it does not remove the rebase work from your team.
The more pull requests in flight, the more of that work there is, and the cost lands on whoever
owns the conflicted files.

## Three ways to automate it

### 1. Require branches to be up to date, and let a bot rebase

The oldest approach, and still the cheapest. Turn on "Require branches to be up to date before
merging", then run something on a schedule or on push-to-main that rebases each open pull request
and force-pushes.

```yaml
# Runs on the PR, so no extra workflow has to be maintained per repo
- uses: actions/checkout@v4
  with:
    ref: ${{ github.head_ref }}
- run: |
    git fetch origin ${{ github.base_ref }}
    git rebase origin/${{ github.base_ref }}
```

It handles the common case well: your branch is simply behind, and nothing conflicts. Where it
stops is the case you care about. `git rebase` exits non-zero, the job fails, and the pull request
is back to waiting for a person. No amount of retrying helps, because the conflict is not
transient.

### 2. `git rerere`, Git's recorded resolutions

`rerere` ("reuse recorded resolution") records how you resolved a conflict once and replays that
resolution the next time the identical conflict appears. For long-lived branches or a nightly
integration branch that collects many topics, it can remove most of the repeated work with no
model involved.

Two honest limits:

- It only replays conflicts that match exactly. If the surrounding lines changed, `rerere` does
  nothing.
- The replay is not a decision. It applies a resolution that was correct in a different context,
  and with `rerere.autoUpdate` it stages that resolution without asking anyone. Safe on a throwaway
  integration branch, questionable on a branch that deploys.

`rerere` is worth enabling either way. It cuts the noise before an agent is asked anything.

### 3. A model, given only real conflicts, with the result verified

This is the approach that closes the loop, and it has a particular shape if you want to trust it:

1. Run the plain `git rebase` first. If it succeeds, the model is never called.
2. If it stops, hand the model the conflicted files only, plus the intent behind both sides (the
   pull request description, the commits that touched those files).
3. Run your verification commands on the result. If they fail, throw the resolution away.
4. Push only if the diff changes no file the pull request did not already change.

Step 4 is the one that matters and the one most implementations skip. A language model resolving
a conflict is editing code nobody reviewed, on a branch that is about to merge. The cheapest
guardrail is to make that edit provably scoped: the resolution may only touch the files the pull
request already touches, so it cannot quietly reach into a part of the codebase the author never
opened.

[Factory's write-up of agent conflict
resolution](https://factory.com/articles/coding-agent-merge-conflict-resolution) makes the same
point from the other end: a clean Git index only proves the conflict markers are gone. The file
still has to preserve the intent of both changes, which is why the check has to run on behaviour,
not on the absence of `<<<<<<<`.

## What a safe automated resolution has to prove

Treat these as requirements you can test against any tool, including ours:

- **The mechanical case never reaches the model.** A stale branch with no conflict should be a
  fast-forward or a clean rebase, with no tokens spent and no variance.
- **The model sees only the conflict.** Not the whole repository, and not a free hand to "tidy"
  nearby code.
- **The change is scoped.** Nothing outside the files the pull request already changed.
- **Your checks run on the resolution, before the push.** Type check, tests, lint, whatever your
  `verify` is. A resolution that compiles but breaks the test is not a resolution.
- **There is a limit on attempts.** A model that cannot resolve should give up and eject the pull
  request, not loop.
- **Someone can read what happened.** The resolution should be visible as a diff on the pull
  request, not buried in a merge commit, so a reviewer can see the new code that a machine wrote.

The last one is the objection practitioners raise most often, and fairly. Several published
guides argue you should never ask a model to resolve conflicts at all, because models drop
adjacent changes and invent logic (see, for example, [raylabs' multi-agent worktree
notes](https://raylabs.app/articles/handling-git-merge-conflicts-in-multi-agent-worktrees/)). That
criticism is correct about an unverified prompt-and-pray setup. It is an argument for the
guardrails above, not against the approach.

## Setting it up with MergeLoop

MergeLoop is a label-driven merge queue for GitHub, and conflict resolution is the part it was
built around. The behaviour above is the default rather than a configuration exercise.

1. **Sign in and install the App** on the repositories you want queued, at
   [mergeloop.ai](https://mergeloop.ai).
2. **Set up a repo.** MergeLoop opens a pull request that adds `.mergepatrol/mergepatrol.yml` on
   the default branch. Merging it is what turns the queue on; a pull request cannot change its own
   queue settings.
3. **Add your model key** in Settings. Anthropic or OpenAI, your account, your spend limit. The
   agent runs on your key, so model cost is at cost, not marked up.
4. **Set your `verify` commands.** These run after an agent resolution, and after a fix, before
   anything is pushed:

   ```yaml
   # .mergepatrol/mergepatrol.yml
   batch_size: 8
   verify:
     - cargo test --workspace
     - pnpm -C ui run lint
   ```

5. **Label a pull request** with `mergepatrol`. `mergepatrol:priority` puts it at the front of the
   queue.

From there the queue does what the section above describes: a plain rebase first, the agent only
on a real conflict, the verify commands on the result, then the required checks, then the merge.
On a conflict the pull request **stays in the queue**; it is not ejected back to you, which is the
difference that saves the round trip.

Each job runs in its own sandbox with a token that can read only the one repository, and the
resolution may not change a file the pull request did not change. If a required check then fails,
the agent reads the failure, commits a fix on top of the pull request, and the checks run again.
After two fixes that do not pass, or when the failure is not the pull request's to fix (a flaky
test, an outage), the pull request is ejected rather than looped on.

Public repositories are free; private ones are $15 a seat a month after a 14-day trial that needs
no card. It is Apache-2.0 if you would rather run it yourself.

## When you should not let an agent resolve it

Being clear about this is the difference between a useful tool and a liability.

- **Semantic conflicts.** Two changes that merge cleanly but break each other are not visible to
  `git` at all, so there is nothing for a resolver to see. A merge queue and combined CI catch
  those; conflict resolution does not.
- **Lockfiles and generated files.** The resolution is a regeneration, not a merge. If your
  tooling cannot regenerate and verify, a human should, because a hand-edited lockfile is how you
  get a build nobody can reproduce.
- **Files where intent is not in the diff.** Schema migrations, public API changes, anything where
  the correct resolution depends on a decision that was never written down. A model will pick one
  side plausibly, and plausible is the failure mode.

For those, ejection is the right outcome. A queue that escalates honestly is worth more than one
that merges confidently.

## FAQ

**Does this replace the merge queue, or work with it?**
It works inside one. The queue provides the ordering and the combined test; the agent only handles
the rebase conflict the queue would otherwise hand back to you.

**Can I keep conflicts away from the model entirely?**
Yes. Without a model key MergeLoop runs the plain `git rebase` only, ejecting on a real
conflict. Some teams start there and turn the agent on for one repository at a time.

**What does it cost to run?**
The agent calls your model provider on your own API key, so you pay the provider directly. Public
repositories are free. Private repositories are $15 per seat per month after a 14-day trial with
no card.

**Does it work with any CI provider?**
Yes. There is no `merge_group` trigger to add: batches run on the `pull_request` workflows your
repository already has, and the queue waits for your existing required checks.

Sign in with GitHub and queue your first pull request. If a conflict resolution is ever wrong, the
diff is on the pull request for you to read and revert.
