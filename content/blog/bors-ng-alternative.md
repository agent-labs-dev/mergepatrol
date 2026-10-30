---
title: bors-ng alternative: a maintained, label-driven merge queue
description: bors-ng is archived and its maintainers point to GitHub's queue. What to move to, what a migration involves, and what stays the same.
date: 2026-10-30
---

If you are still running bors-ng, you are running a merge bot whose repository has been archived.
`bors-ng/bors-ng` is marked archived on GitHub and its last push was 4 April 2024. Its README
opens with a deprecation notice: bug fixes will still be merged if someone provides them, new
features will not be accepted, and the advice is to use GitHub's built-in merge queue instead
([bors-ng README](https://github.com/bors-ng/bors-ng)).

That advice is fine if it applies to you. For a lot of bors users it does not, and this page is
about the gap.

## Why bors users are stuck in the middle

bors-ng did one thing well: it kept `main` green by testing the tree you were about to land, not
the tree you branched from. You commented `bors r+` and it took over.

GitHub's native queue covers the same idea, but it is gated: available in public repositories owned
by an organization, or in private repositories owned by organizations on **GitHub Enterprise
Cloud** ([github/docs, merge-queue
gate](https://github.com/github/docs/blob/main/data/reusables/gated-features/merge-queue.md)).
If your bors setup was on private repositories under a Free, Pro or Team organization, or under a
personal account, the recommended replacement is not something you can turn on.

There is a second cost even where it is available. GitHub's queue requires your CI to report checks
on `merge_group` events, so a workflow change comes with the migration, and the setting lives in
branch protection, so a repository admin has to act.

## What carries over

The useful way to read a migration is by what stays recognisable:

| bors-ng | Mergepatrol |
| --- | --- |
| `bors r+` in a comment | add the `mergepatrol` label to the pull request |
| `bors r+ p=1` sets a priority | `mergepatrol:priority` jumps the queue |
| Tests the combined tree before landing | queued pull requests are tested together on a draft pull request |
| A failed batch is split to find the culprit | the same: a failed batch splits in half repeatedly down to the pull request that broke it |
| Conflicts bounced back to the author | a plain `git rebase` first, an agent only on a real conflict, then your `verify` commands before anything is pushed |
| Self-hosted, one instance | Apache-2.0 and self-hostable, or hosted |
| Any CI | any CI: no `merge_group` trigger to add, checks come from your existing `pull_request` workflows |

Two differences are worth stating plainly rather than smoothing over:

- **Labels replace comment commands.** Nothing watches the comment thread, so `bors r+` does
  nothing here. Anyone who can label a pull request can queue it.
- **A conflict does not send the pull request back.** The queue resolves it in place when the
  workspace has a model key, and you read the resulting diff on the pull request. Leave the key
  out and you get the old behaviour: eject on conflict.

## What migration actually involves

1. **Install the App** on the repositories bors was serving, at
   [mergepatrol.ai](https://mergepatrol.ai).
2. **Merge the setup pull request** that adds `.mergepatrol/mergepatrol.yml` to the default branch.
   This is the switch: a pull request cannot change its own queue settings, so the config lands by
   review.
3. **Set `verify` and `batch_size`.** `verify` is the list of commands that must pass on a
   resolution before it is pushed. Start with the same commands your `pull_request` CI runs, or
   with the ones that take under a couple of minutes:

   ```yaml
   # .mergepatrol/mergepatrol.yml
   batch_size: 8
   verify:
     - cargo test --workspace
     - pnpm -C ui run lint
   ```

4. **Add a model key** in Settings if you want conflicts resolved. It runs on your own Anthropic or
   OpenAI key, so model spend is at cost.
5. **Retire bors** from the branch. Remove its required status check and its webhook, then turn off
   the instance. Run both queues on one branch and they will race each other for merges.
6. **Label pull requests** `mergepatrol`. If you have an existing workflow that comments `bors r+`,
   change it to add the label instead; `gh pr edit <n> --add-label mergepatrol` does it from a
   script.

Nothing about your CI has to change. The batch is tested on a draft pull request, so the
`pull_request` workflows you already have run on the combination.

## What it costs

Hosted Mergepatrol is free on public repositories. Private repositories are $15 per seat per month
after a 14-day trial that needs no card, where a seat is a developer whose pull request joined a
private repository's queue. If you were self-hosting bors, self-hosting Mergepatrol costs no
licence fee either: it is Apache-2.0, `docker compose up` starts it, and production jobs run in
gVisor sandboxes on a host you control.

## FAQ

**Is bors-ng really unmaintained?**
The repository is archived, the last push was April 2024, and the README says new features will not
be accepted and points readers at GitHub's merge queue. Bug fixes are still accepted.

**Can I just use GitHub's merge queue instead?**
If your repositories are public and owned by an organization, or private on Enterprise Cloud, yes,
and it is a reasonable choice. On private repositories outside Enterprise Cloud it is not available.

**Do I have to give the agent a model key?**
No. Without one you get the queue and batching, and a conflict or a failed check ejects the pull
request, with no model calls at all.

**Does it work with CI other than GitHub Actions?**
Yes. It waits for the required checks your branch protection already demands, whatever system
reports them.
