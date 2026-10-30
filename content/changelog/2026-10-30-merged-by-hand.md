---
title: A PR merged by hand mid-job leaves the queue cleanly
date: 2026-10-30
kind: fixed
pr: 133
---

If you merge or close a queued PR yourself while its checks run, the queue now lets it go instead
of trying to rebase a branch that's gone. Hosted jobs also start warm about 95% of the time, up
from about 60%.
