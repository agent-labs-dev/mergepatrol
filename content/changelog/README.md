# Changelog entries (PROTOTYPE contract)

Draft of the rule agents follow. Each `.md` file here is one entry on `/changelog`, and a
permalink at `/changelog/<slug>`.

## When to write one

A PR whose change a Mergepatrol user can see or feel adds exactly one entry, in the same PR:
a new page or control, a changed behavior, a fix to something users hit, a price change.
Deploy, CI, refactors, internal logging and docs-only PRs don't. If unsure, ask: would a user
who reads the PR title care? If not, skip it.

## The file

`content/changelog/<YYYY-MM-DD>-<slug>.md`, dated the day the PR is opened (the merge date
replaces it if they differ: TBD).

```markdown
---
title: Close a PR from the queue page
date: 2026-10-30
kind: improved        # new | improved | fixed
pr: 125
image: /changelog/close-pr/after.png   # optional, the one picture that shows it
---

One to three short paragraphs, to the user, in the present tense. What they can do now and
where to find it. No file names, no internals, no "we refactored".
```

- `title`: what the user gets, as a sentence fragment. Not the PR's Conventional Commit title.
- `kind: fixed` entries are one or two sentences and usually have no picture.

## Pictures

The PR already takes screenshots for its body (`pr-shots/`, dropped before merge). Copy the one
or two that show the change best to `ui/public/changelog/<slug>/` and keep those. Use the
"after" picture; a before/after table only when the difference is small (a fix). Same
viewport and theme as the PR shots; no real customer repos or names.
