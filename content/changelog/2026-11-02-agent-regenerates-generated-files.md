---
title: The agent regenerates generated files when a rebase conflicts in them
kind: fixed
---

A rebase that conflicted in a generated file, such as a contract or the digest of one, could eject the PR, because the Claude agent had no shell to run the repo's generator. It now has one, as the Codex agent does, and the job image comes with uv and Bun. The agent finds the generator in the repo's `AGENTS.md`, `CLAUDE.md` or docs, and the result must still touch only the PR's own files.
