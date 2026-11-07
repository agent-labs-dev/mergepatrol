---
title: MergeLoop's own agent does all of a PR's agent work again
kind: improved
---

The `agent.runs: local` setting is gone. MergeLoop's agent resolves conflicts, fixes failed checks and reviews every queued PR again, with the workspace's model key. Remove `runs` from `agent` in `.mergeloop/mergeloop.yml`: a config file that still sets it no longer reads, and its repo's queue stops until it does.

The `mergeloop` command still lands a PR. It tells your agent when review threads, a reviewer's changes, a draft or an ejection need it, and `mergeloop call` still runs any tool.
