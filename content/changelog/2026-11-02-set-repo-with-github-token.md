---
title: An agent with a GitHub token can turn a repo's queue on
kind: fixed
pr: 181
---

`set_repo` takes a GitHub token from anyone with admin access to the repo, so an agent can turn
on the queue for a repo without a config file yet, in one call, without a MergeLoop sign-in.
