---
title: The review goes back to the agent that built the PR, not to you
kind: improved
---

Automatic review is now on for every workspace with a model key. What the review finds to fix goes back to the PR's author, often the coding agent that built it: it's listed by id under "Sent back to the author", and the PR waits for a push. The next review checks each fix by its title. An author that thinks a finding is wrong can reply to it, and the review withdraws the finding when the reply holds up in the code. Once nothing is left, the PR goes on to the merge queue with no human approval.

You're asked only about what the author can't settle: a call on scope, product or design, access it lacks, or a change outside the repo. You're also asked when three rounds in a row still send findings back. Migrations still always go to a human.

Each kind's bar in **Settings** now sets how sure the review must be to send a change back to the author. A lower bar sends back more, and a change under it shows as a note that holds nothing. The defaults are 60% for security, 70% for dependencies and public APIs, and 80% for everything else.

The Mergepatrol skill that setup adds to a repo has coding agents work that review after they queue. Each comment is open, then addressed with a pushed fix or a reply, then resolved, and only its reviewer can resolve it. The agents keep one review ledger comment up to date on the PR, and never answer their own review.
