---
title: Mergepatrol vs Mergify
description: Mergify is a rules-driven merge queue with speculative checks. Mergepatrol is a label-driven queue whose agent resolves conflicts and fixes failed checks. How they compare.
date: 2026-10-31
---

[Mergify](https://mergify.com) is one of the longest-running merge queues for GitHub, configured
with rules in `.mergify.yml`. Mergepatrol is a newer, open-source queue that puts a coding agent
on the two things that eject pull requests most: rebase conflicts and failed checks. Checked
against Mergify's [docs](https://docs.mergify.com/merge-queue/),
[changelog](https://docs.mergify.com/changelog/) and [pricing](https://mergify.com/pricing) on
31 October 2026.

## At a glance

| | Mergify | Mergepatrol |
| --- | --- | --- |
| Price | Open Source $0; Free $0 for up to 5 users on private repos; Max $21 a seat a month; Enterprise custom | Public repos free; private repos $15 a seat a month after a 14-day trial; self-hosted free |
| Configuration | `.mergify.yml`: `queue_rules`, conditions, priorities, scopes | `.mergepatrol/mergepatrol.yml`: label, batch size, verify commands, agent settings, extra branches |
| Batching | `batch_size`, `batch_max_wait_time`; a failed batch is bisected | `batch_size`; a failed batch is split in half down to the PR that broke it |
| Parallel testing | Up to 128 speculative checks | No speculative builds: one batch of up to `batch_size` PRs (8 by default) |
| A PR that conflicts with its base | We found no conflict resolution in Mergify's docs | An agent resolves the conflict, your `verify` commands run, and the PR keeps its place |
| A required check that fails | Back to its author. Mergify can hand a quarantined flaky test to Claude Code or Cursor as a prompt | An agent commits a fix on the PR, up to `fix_ci.attempts` times |
| Stacked PRs | Queues GitHub stacked PRs; a stack enters the queue together | A `gh stack` stack queues and merges as one item, all or nothing |
| Agent code review | No | Optional, per workspace |
| Source | Proprietary SaaS | Apache-2.0, self-hostable |

## Where Mergify is the better fit

- **Large monorepos with many concurrent builds.** Speculative checks, scopes and two-step CI let
  Mergify test many queued pull requests in parallel, which a team landing hundreds of PRs a day
  may need.
- **Rule-heavy workflows.** If you want queue behaviour driven by conditions on labels, paths,
  authors and checks, Mergify's rules engine is built for it.
- **Small private teams that want to pay nothing.** Mergify's free plan covers up to 5 users on
  private repos; Mergepatrol's private plan is paid after the trial.

## Where Mergepatrol is the better fit

- **Pull requests keep getting ejected for conflicts.** Mergepatrol runs a plain `git rebase` first
  and an agent only on a real conflict, then your `verify` commands, before anything is pushed. See
  [Resolve rebase conflicts automatically](/blog/resolve-rebase-conflicts-automatically).
- **Failed checks that the PR could fix.** The fix agent reads the failure and commits a fix on the
  PR while it keeps its place. A failure that is not the PR's, such as a flaky test, ejects it.
- **You want a short config.** A label to queue, a batch size, and the commands that check an
  agent's work. Most repos run on the defaults the setup PR writes.
- **You want to run it yourself.** Mergepatrol is Apache-2.0 and starts with `docker compose up`.
- **Coding agents queue your PRs.** An MCP server lets an agent queue, read and review PRs with
  its own GitHub token.

## Switching

1. [Install Mergepatrol](/docs/getting-started) on the repositories and merge its setup PR.
2. Remove the repo's `queue_rules` from `.mergify.yml`, or uninstall Mergify there. Two queues on
   one branch race each other for merges.
3. Set `batch_size` in `.mergepatrol/mergepatrol.yml` to what you used in Mergify, and `verify` to
   your fastest checks.
4. Change whatever queued with Mergify, a label or a command, to add the `mergepatrol` label.
   `<label>:priority` jumps the queue.

## FAQ

**Does Mergify resolve merge conflicts?**
We found no conflict resolution in Mergify's documentation as of 31 October 2026. Its AI feature
for failing CI hands a quarantined test to Claude Code or Cursor as a prompt, outside the queue.

**Does Mergepatrol support priorities?**
Yes, with a second label, `mergepatrol:priority` by default, or `priority` on the `queue_pull` MCP
tool. It has no rules engine beyond that.
