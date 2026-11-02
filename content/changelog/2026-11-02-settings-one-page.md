---
title: Repos and Settings are one page, with defaults for every repo
kind: new
pr: 192
image: /changelog/settings-one-page/after.png
---

**Repos** and **Settings** are now one page. **Workspace** sits at the top of the list, with your plan, model access and review settings, and each repo below it says what it sets of its own.

**Repo defaults** are new: set the trigger label, batch size, model, effort and CI fixes once, and every repo follows them, at once and with no PR. A repo's config file now holds only what differs, so an empty file is valid and inherits everything. **Update all** trims existing files down to that.

PRs that change a repo's settings or update its skill are now queued as they open, when the repo queues.
