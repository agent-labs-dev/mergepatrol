---
title: A review's evidence is a test MergeLoop saw fail
kind: improved
---

The agent's review now runs your repo. To send a change back to the author, it writes a focused test or names a command that fails because of what it found. MergeLoop then runs that command itself on the PR's head, with only the files the finding names, and the finding goes back only if MergeLoop saw it fail. What the agent says it ran counts for nothing. The finding's thread shows the command, what it printed, and the test it wrote. The PR's review in MergeLoop shows the same, with the whole test to download, also when GitHub's comment had no room for it.

When the agent's fix takes the finding on, it starts from that test. The fix lands only once the command passes, and only if the test is left as the review wrote it. The test is committed with the fix, so your CI checks for that case from then on.
