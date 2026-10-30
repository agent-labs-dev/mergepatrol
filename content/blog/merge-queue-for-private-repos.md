---
title: A merge queue for private repos on GitHub Team
description: GitHub's merge queue needs Enterprise Cloud on private repos. What to use instead, what it costs, and what to check before you switch.
date: 2026-10-30
---

If your repositories are private and your organization is on GitHub Free, Pro or Team, GitHub's
merge queue is not a feature you have. That is not a configuration problem and no amount of
settings will change it. This page is about what to do instead, and how to tell whether it is
worth doing at all.

## Who can actually turn it on

GitHub's gated-features note is explicit: pull request merge queues are available in any public
repository owned by an organization, **or in private repositories owned by organizations using
GitHub Enterprise Cloud** ([github/docs, merge-queue gate](https://github.com/github/docs/blob/main/data/reusables/gated-features/merge-queue.md);
[general availability changelog, 12 July
2023](https://github.blog/changelog/2023-07-12-pull-request-merge-queue-is-now-generally-available/)).
GitHub Enterprise Server has it on any organization-owned repository.

Three consequences follow, and each one catches somebody:

- **Private on Team or Free: not available.** This is the common case for a startup's own product
  repositories.
- **A repository under a personal account: not available**, even in public, because the gate is on
  organization ownership.
- **Enabling it needs repository administrator rights**, because it is a branch protection or
  ruleset setting ("Require merge queue"). A contributor cannot switch it on for the team.

There is a wiring cost even when you are eligible. A queue waits for required checks reported on
`merge_group` events, so your CI has to gain a trigger and a job that reports on merge groups
before the queue will move ([GitHub docs, Managing a merge
queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)).
A queue also cannot be enabled on a branch protection rule whose pattern uses wildcards.

## What a queue is for, in case you are deciding whether you need one

A merge queue exists so that every pull request is tested against the code it will actually land
on. Without one, two pull requests that each pass their own checks can merge in an order that
leaves `main` broken, and neither author sees it.

If you merge a handful of pull requests a day, "Require branches to be up to date before merging"
covers most of that for free: the second pull request has to re-run its checks on top of the
first. It costs you a re-run and a wait, and it does nothing when the re-run conflicts.

The threshold where a queue earns its keep is roughly: several people or agents merging into the
same branch every day, CI that takes long enough that re-running it by hand is painful, and
conflicts frequent enough that somebody is spending their morning rebasing.

## What to use instead, on private repos

Mergepatrol is a merge queue that works on any repository you can install a GitHub App on:
private, personal-account, Free, Pro, Team, or Enterprise Cloud. It is Apache-2.0, and you can
self-host it.

The parts that matter when you are choosing:

- **No `merge_group` trigger to add.** Batches are tested on a draft pull request, so your
  existing `pull_request` workflows run on the combination unchanged. Moving onto the queue does
  not mean rewriting CI.
- **A conflict does not eject the pull request.** A plain `git rebase` runs first; only on a real
  conflict does an agent resolve it, your `verify` commands run on the result before anything is
  pushed, and the resolution may not touch a file the pull request did not already change.
- **Batching.** Queued pull requests that have each passed their own checks are tested together on
  a draft pull request, and a failed batch is split in half repeatedly down to the pull request
  that broke it.
- **A sandbox per job.** Each job runs in its own container with a token that can read only the
  one repository.
- **Your model key.** The agent runs on your own Anthropic or OpenAI key, so model spend is at
  cost rather than resold.

## What it costs

- **Public repositories: free.**
- **Private repositories: $15 per seat per month, after a 14-day trial that needs no card.** A
  seat is a developer whose pull request joined a private repository's queue; it is taken with
  their first such pull request. A pull request authored by an App, such as Dependabot, takes no
  seat.
- **Without a subscription**, waiting private-repo pull requests go on hold, with a comment that
  links to Settings.
- **Self-hosted: no licence fee**, and no per-seat limit; you pay for the host and the model calls.

The comparison to make is against your own rebasing time and against Enterprise Cloud, not
against a free plan you cannot use.

## Trying it without committing

1. Install the App on one repository, ideally a private one with steady merge traffic.
2. Merge the setup pull request that adds `.mergepatrol/mergepatrol.yml`, and set your `verify`
   commands. Leaving the model key out is a reasonable first week: you get the queue and the
   batching, with conflicts still ejected to a human.
3. Label a few pull requests `mergepatrol` and watch a batch form.

If the queue does not shorten your day, uninstall the App and nothing else changes: what it added
is the config file, an agent skill file and its queue labels, all of which you can delete.

## FAQ

**Does GitHub's merge queue work on private repositories on Team?**
No. It is gated to GitHub Enterprise Cloud on private repositories, and to public repositories
owned by an organization. Enterprise Server has it on any organization-owned repository.

**Can I use Mergepatrol and GitHub's native queue on the same branch?**
Technically you could install both, but do not: two queues on one branch will race each other for
merges. Pick one per branch.

**Do I have to be a repository admin?**
To install the App and merge the setup pull request, an admin has to act once. After that,
labelling pull requests needs only the permission to label them.

**Can we self-host it?**
Yes. It is Apache-2.0, `docker compose up` gets you started, and production jobs run in gVisor
sandboxes on a host you control. You supply the model key.
