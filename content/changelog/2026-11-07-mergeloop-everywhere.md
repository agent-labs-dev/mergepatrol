---
title: New repos queue with the mergeloop label
kind: improved
---

A repo you set up now gets `.mergeloop/mergeloop.yml` and queues PRs labeled `mergeloop`, with
`mergeloop:priority` and `mergeloop:failed` beside it. MergeLoop's branches, commit trailers and
agent skill say `mergeloop` too, and the API reports a repo's status as `mergeloop`.

A repo set up before keeps its file, its labels and its queued PRs as they are. **Update skill** on a
repo moves its agent skill to `skills/mergeloop`.
