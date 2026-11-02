---
title: PRs labeled while another runs show their place in the queue
kind: fixed
pr: 184
---

A PR labeled while another PR waits for its checks now joins the queue within a minute, as waiting or held, in the dashboard and the status API, instead of showing no queue at all until that PR is done. A PR whose required check never reports, as when it renames one, is ejected 30 minutes after its other checks finish, with a comment naming the check, instead of holding the queue for 3 hours.
