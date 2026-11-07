---
title: A check that fails once runs again before a fix
kind: fixed
pr: 284
---

When a queued PR's required GitHub Actions check fails, MergeLoop now runs its failed jobs again once before the agent tries a fix, so a failure like a dropped network request no longer costs a fix or ejects the PR. Without a model API key in your settings, a check that still fails ejects the PR with a comment saying to add one, rather than a failed agent run. Running jobs again needs the App's new `actions: write` permission, which your account's owner accepts on GitHub.
