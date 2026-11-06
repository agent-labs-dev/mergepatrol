---
title: A review's test goes where your tests live
kind: fixed
---

The test a review writes to show a finding is committed with the fix, as a regression test. The agent now writes it where your repo keeps its tests, named and written like them, so your own test command runs it. Before, it could land in a folder of its own that your CI never ran.
