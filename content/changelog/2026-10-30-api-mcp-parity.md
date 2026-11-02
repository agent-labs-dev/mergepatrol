---
title: Everything the dashboard does, over MCP and the API
kind: new
pr: 136
---

Coding agents and scripts can now do anything you can do in the dashboard: review a PR, read its
diff and checks, see the queue and its history, change a repo's settings, and more. Every
dashboard action is an MCP tool at `https://mergeloop.ai/mcp`, and a route in the JSON API.

A GitHub token (`gh auth token`, a personal access token, or `GITHUB_TOKEN` in Actions) works for
calls on one repo, where GitHub lets it read the repo. Calls across a workspace, such as history
and settings, need a sign-in.
