---
title: A re-review reads only what you changed since
kind: improved
pr: 275
---

Once the agent's review sends findings back, the next review of the PR checks those findings and reads only the commits pushed since, not the whole PR again. It no longer turns up new findings about code it already saw on the first pass, unless your new commits break it.

After a rebase, or a merge of your base branch into the PR, the review reads the whole PR again. This holds for reviews your own agent runs too.
