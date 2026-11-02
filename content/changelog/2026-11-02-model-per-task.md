---
title: A default model, with an override per task
kind: improved
image: /changelog/model-per-task/after.png
---

A repo's settings list each provider's current models by name, so you pick a model instead of typing its id. Switching the provider switches the model to one of its own.

The agent has one default model, and its three tasks can each override it: resolving a rebase's conflicts, fixing failed checks, and reviewing. Run a fast model to resolve conflicts, say, and a stronger one to review. A task left on **Default** runs the default model, as every task did before.

In the config file, the `resolver:` section is now `agent:`, with each task's model under `agent.overrides`. A file that still says `resolver:` keeps working.
