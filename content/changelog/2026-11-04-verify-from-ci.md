---
title: A repo with no verify commands is checked by its own CI
kind: improved
---

Most repos never set `verify` in `.mergeloop/mergeloop.yml`, so the agent had no check to run: a review could not show a change was needed, and a fix had nothing to pass before the publish.

MergeLoop now reads the workflows in `.github/workflows/` that run on a pull request, and hands the agent the commands they run. A review writes its test, runs one of those commands and sees it fail, and MergeLoop runs it again on the PR's head before the finding goes back to the author. The fix agent runs the same command after its change.

None of those commands can eject a PR. Only the ones your config file declares do that, so a repo that wants the queue gated by a command it names still sets `verify`; the rest get the checks their CI already runs.
