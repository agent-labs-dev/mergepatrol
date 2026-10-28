# Mergepatrol

Mergepatrol is a merge queue for GitHub. Add a label to a pull request, and Mergepatrol rebases it onto its base branch, waits for the required checks, and merges it.

When a rebase has conflicts, an agent resolves them. Claude is the default. A plain `git rebase` runs first, so the agent runs only on real conflicts.

Status: early. Not ready for use yet.

## Run it

The spike polls GitHub (no webhooks yet) and has one queue per repo, on the default branch.

1. Generate the secret key. It encrypts the GitHub App and the Anthropic API key in the database. Keep it: without it, you must set Mergepatrol up again.

   ```bash
   export MERGEPATROL_SECRET_KEY=$(openssl rand -base64 32)
   ```

2. Start Mergepatrol:

   ```bash
   docker compose up --build -d
   docker compose logs mergepatrol | grep setup_token
   ```

   Or without Docker: `(cd ui && pnpm install && pnpm build) && cargo run -p mergepatrol`.

3. Open http://localhost:8080 and do the setup in the dashboard:
   1. Enter the setup token from the logs.
   2. Create the GitHub App. GitHub creates it with the permissions Mergepatrol needs.
   3. Install the App on your organization or account, and choose its repos. GitHub sends you back to sign in.
   4. In **Repos**, turn on the repos to queue.
   5. In **Settings**, add your Anthropic API key. Mergepatrol checks it with Anthropic, then stores it encrypted.

4. Add the `mergepatrol` label to a PR to queue it.

Anyone who can access the App's installation on its owner account can sign in.

If you reach Mergepatrol at another address than http://localhost:8080, set `MERGEPATROL_PUBLIC_URL` before you create the App: GitHub sends users back to it.

| Variable | Default | What it does |
| --- | --- | --- |
| `MERGEPATROL_SECRET_KEY` | (required) | Encrypts the stored secrets. 32 bytes, base64 |
| `MERGEPATROL_PUBLIC_URL` | `http://localhost:8080` | Where users reach the dashboard |
| `MERGEPATROL_LABEL` | `mergepatrol` | The trigger label. `<label>:priority` jumps the queue, and ejected PRs get `<label>:failed` |
| `MERGEPATROL_POLL_SECS` | `60` | How often to poll GitHub |
| `MERGEPATROL_DATA_DIR` | `./data` | SQLite, the repo caches, the worktrees, and the transcripts |
| `MERGEPATROL_BIND` | `127.0.0.1:8080` | The dashboard address |
| `MERGEPATROL_MODEL` | `claude-opus-5-5` | The resolver's model |
| `MERGEPATROL_EFFORT` | `medium` | The resolver's effort |

A repo can set verify commands in `.mergepatrol/mergepatrol.yml`. They run after an agent resolution, before the publish. Mergepatrol reads this file from the base branch, so a PR cannot change its own checks.

```yaml
version: 1
verify:
  - cargo check
```

## Crates

| Crate | What it does |
| --- | --- |
| `core` | The queue and its state machine. Pure logic, no I/O |
| `github` | GitHub App auth, the REST and GraphQL calls, and publishing commits through the Git Data API |
| `git` | Local git: fetch, worktrees, rebase |
| `resolver` | The `Resolver` trait, and the Claude implementation |
| `executor` | Where a job's workspace and commands run |
| `store` | The `Store` trait, and SQLite |
| `web` | The HTTP server: JSON API and the dashboard |
| `mergepatrol` | The binary |

## License

Apache-2.0
