# Mergepatrol

Mergepatrol is a merge queue for GitHub. Add a label to a pull request, and Mergepatrol rebases it onto its base branch, waits for the required checks, and merges it.

When a rebase has conflicts, an agent resolves them. Claude is the default. A plain `git rebase` runs first, so the agent runs only on real conflicts.

Status: early. Not ready for use yet.

## Run the spike

The spike is the thinnest end-to-end path. It polls GitHub (no webhooks yet), has one lane per repo on the default branch, and has no settings in the dashboard.

1. Create a GitHub App by hand (the manifest flow comes later). Give it these repository permissions:
   - Contents: read and write
   - Pull requests: read and write
   - Issues: read and write (for labels and comments)
   - Checks: read
   - Commit statuses: read
   - Administration: read (optional: without it, only rulesets count for required checks)
2. Install the App on the repos, and download its private key.
3. Run it:

```bash
export MERGEPATROL_GITHUB_APP_ID=123456
export MERGEPATROL_GITHUB_PRIVATE_KEY_PATH=./github-app.pem
export MERGEPATROL_REPOS=owner/repo,owner/other
export MERGEPATROL_SECRET_KEY=$(openssl rand -base64 32)   # keep it: it decrypts the stored secrets
(cd ui && pnpm install && pnpm build)
cargo run -p mergepatrol
```

Or with Docker: put the key in `./github-app.pem`, then run `docker compose up --build`.

The dashboard is at http://127.0.0.1:8080. Add your Anthropic API key there: Mergepatrol checks it with Anthropic, then stores it encrypted with `MERGEPATROL_SECRET_KEY`. Then add the `mergepatrol` label to a PR to queue it.

The dashboard has no login yet, so do not expose it beyond your machine.

| Variable | Default | What it does |
| --- | --- | --- |
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
