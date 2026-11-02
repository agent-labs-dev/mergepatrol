---
title: MergeLoop vs GitHub's merge queue
description: GitHub's merge queue needs Enterprise Cloud on private repos and a merge_group trigger in CI. Where MergeLoop differs: any plan, agent-resolved conflicts, bisected batches.
date: 2026-10-31
---

GitHub's built-in [merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)
and MergeLoop do the same core job: test each pull request against the code it will land on,
then merge it. They differ in who can use them, what your CI has to change, and what happens when
a pull request does not go through cleanly. Checked against GitHub's docs on 31 October 2026.

## At a glance

| | GitHub merge queue | MergeLoop |
| --- | --- | --- |
| Private repositories | Only for organizations on GitHub Enterprise Cloud | Any GitHub plan, $15 per seat a month after a 14-day trial |
| Public repositories | Organization-owned repositories | Any public repository, free within a fair-use limit |
| CI changes | GitHub Actions workflows must run on `merge_group`; other CI on pushes to `gh-readonly-queue/*` branches | None: batches run your existing `pull_request` workflows on a draft PR |
| How to queue | "Merge when ready", `gh pr merge`, or the GraphQL `enqueuePullRequest` mutation | A label, the dashboard, or the MCP tool `queue_pull`, so people and coding agents queue the same way |
| A PR that conflicts with its base | Removed from the queue | An agent resolves the conflict, your `verify` commands run, and the PR keeps its place |
| A required check that fails | The PR is removed | An agent commits a fix on the PR, up to `fix_ci.attempts` times, before ejecting it |
| A failed group of PRs | The failing PR is removed and the group is rebuilt without it | The batch is split in half repeatedly down to the PR that broke it; the rest merge |
| Stacked PRs | Not described in the merge queue docs | A `gh stack` stack queues and merges as one item, all or nothing |
| Agent code review | No | Optional: an agent reviews each queued PR and asks code owners about what it is unsure of |
| Self-hosting | GitHub Enterprise Server | Apache-2.0, `docker compose up` |

## Where GitHub's merge queue is the better fit

- **You are on Enterprise Cloud, or your repositories are public and organization-owned,** and you
  want nothing outside GitHub. It is built in and has no extra bill.
- **You want a large number of concurrent builds.** GitHub lets you run up to 100 `merge_group`
  builds at once and set minimum and maximum group sizes.
- **Your CI already runs on `merge_group`.** Then the wiring cost is paid.

## Where MergeLoop is the better fit

- **Private repositories on GitHub Free, Pro or Team.** GitHub's queue is not available there; see
  [A merge queue for private repos](/blog/merge-queue-for-private-repos).
- **A busy `main` where conflicts eject pull requests.** A plain `git rebase` runs first; an agent
  (Claude Code or Codex, on your own model key) runs only on a real conflict, and the result is
  checked by your `verify` commands before it is pushed. See [Resolve rebase conflicts
  automatically](/blog/resolve-rebase-conflicts-automatically).
- **Failed checks the PR could fix itself.** The fix agent reads the failed check's
  output and log, and commits a fix on top. When the failure is not the PR's to fix, such as a
  flaky test or an outage, it gives up and the PR is ejected.
- **You do not want to touch CI.** Batches open a draft PR, so the `pull_request` workflows you
  have run on the combination.
- **Coding agents open your pull requests.** The MCP server lets an agent queue its own PR, read
  why it is held, and answer review questions, with the same permissions GitHub gives its token.

## Switching

MergeLoop and GitHub's queue should not both run on one branch: they would race for merges.

1. [Install MergeLoop](/docs/getting-started) and merge the setup PR that adds
   `.mergepatrol/mergepatrol.yml` ([reference](/docs/configuration)).
2. Turn off "Require merge queue" on the branch's ruleset or branch protection.
3. Keep your required checks. MergeLoop waits for the same ones.
4. Label pull requests `mergepatrol` instead of pressing "Merge when ready".

You can leave the `merge_group` trigger in your workflows; nothing calls it.

## FAQ

**Is GitHub's merge queue available on GitHub Team?**
GitHub's documentation lists public repositories owned by an organization, and private
repositories owned by organizations on GitHub Enterprise Cloud. Team is not on that list.

**Does GitHub's merge queue bisect a failed group?**
Its documentation describes removing the failing pull request and rebuilding the group for the
rest. MergeLoop splits a failed batch in half until it finds the one that broke it: see [How a
merge queue batches pull requests](/blog/how-a-merge-queue-batches-pull-requests).

**Does MergeLoop need a model key?**
Only for the agent features. Without one, a conflict or a failed check ejects the PR, as GitHub's
queue does.
