---
title: Pick each task's model from a list
kind: improved
image: /changelog/model-per-task/after.png
---

A repo's settings list each provider's current models by name, so you pick a model instead of typing its id. Switching the provider switches the model to one of its own.

Rebase, Fix and Review can each run their own provider and model: a fast model to resolve conflicts, say, and a stronger one to review. A task you leave on **The resolver's** runs the resolver's model, as every task did before, so existing settings keep working unchanged.
