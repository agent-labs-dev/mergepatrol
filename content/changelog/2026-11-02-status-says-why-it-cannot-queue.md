---
title: The status says what will stop a PR, before you queue it
kind: improved
---

A PR's status, in the API and in MCP, now says before you queue it when the queue would hold it whatever the PR, as on a private repo without a subscription, and when the workspace has no model key for the agent to resolve conflicts, fix failed checks, or review. Queuing a PR from the dashboard or an agent says the same, instead of a bare "Done."

A PR ejected because its rebase conflicts and the workspace has no key now names the provider the repo's settings use, and links to the dashboard's settings, where you add the key.
