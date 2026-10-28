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

   Or without Docker: start the dev Postgres with `docker compose -f compose.dev.yml up -d`, then `(cd ui && pnpm install && pnpm build) && MERGEPATROL_DATABASE_URL=$DATABASE_URL cargo run -p mergepatrol`, where `DATABASE_URL` is the one in `.cargo/config.toml`.

3. Open http://localhost:8080 and do the setup in the dashboard:
   1. Enter the setup token from the logs.
   2. Create the GitHub App. GitHub creates it with the permissions Mergepatrol needs.
   3. Install the App on your organization or account, and choose its repos. GitHub sends you back to sign in.
   4. In **Repos**, turn on the repos to queue.
   5. In **Settings**, add your Anthropic API key. Mergepatrol checks it with Anthropic, then stores it encrypted.

4. Add the `mergepatrol` label to a PR to queue it.

Anyone with a GitHub account can sign in. Each installation of the App is a workspace, with its own repos and its own Anthropic API key. A user sees the workspaces that GitHub lets them access, as of their sign-in. Sign in again after you install the App somewhere new.

## Hosted

The hosted Mergepatrol uses one public GitHub App, which we create by hand on GitHub, rather than with the setup in the dashboard. Set its credentials, and the setup token and the App creation are skipped:

| Variable | What it is |
| --- | --- |
| `MERGEPATROL_GITHUB_APP_ID` | The App ID |
| `MERGEPATROL_GITHUB_APP_PRIVATE_KEY` | The private key, PEM |
| `MERGEPATROL_GITHUB_CLIENT_ID` | The OAuth client ID |
| `MERGEPATROL_GITHUB_CLIENT_SECRET` | The OAuth client secret |

The App needs the permissions in `crates/github/src/setup.rs` (`manifest`), the callback URL `<public URL>/auth/callback`, and the setup URL `<public URL>/setup/installed`.

If you reach Mergepatrol at another address than http://localhost:8080, set `MERGEPATROL_PUBLIC_URL` before you create the App: GitHub sends users back to it.

| Variable | Default | What it does |
| --- | --- | --- |
| `MERGEPATROL_DATABASE_URL` | (required) | Postgres. Compose runs one |
| `MERGEPATROL_SECRET_KEY` | (required) | Encrypts the stored secrets. 32 bytes, base64 |
| `MERGEPATROL_PUBLIC_URL` | `http://localhost:8080` | Where users reach the dashboard |
| `MERGEPATROL_LABEL` | `mergepatrol` | The trigger label. `<label>:priority` jumps the queue, and ejected PRs get `<label>:failed` |
| `MERGEPATROL_POLL_SECS` | `60` | How often to poll GitHub |
| `MERGEPATROL_DATA_DIR` | `./data` | The repo caches and the job outputs, with their transcripts |
| `MERGEPATROL_BIND` | `127.0.0.1:8080` | The dashboard address |
| `MERGEPATROL_MODEL` | `claude-opus-5-5` | The resolver's model |
| `MERGEPATROL_EFFORT` | `medium` | The resolver's effort |

A repo can set verify commands in `.mergepatrol/mergepatrol.yml`. They run after an agent resolution, before the publish. Mergepatrol reads this file from the base branch, so a PR cannot change its own checks.

```yaml
version: 1
verify:
  - cargo check
```

## How jobs are isolated

A rebase job runs the repo's code (the verify commands) and an agent with a shell. Mergepatrol treats it as untrusted:

- The **orchestrator** holds the GitHub App key and the queue. It never runs repo code.
- Each **job** runs `mergepatrol job` in its own container, from the job image. It gets a token that can only read the one repo, and the workspace's Anthropic key. It returns a git bundle.
- The orchestrator checks the bundle before it publishes: the result must sit on the base branch, and change no file that the PR did not change.

In production the job containers use gVisor (`runsc`) on a network that cannot reach the cloud metadata server, private ranges, or the host. `deploy/sandbox-host.sh` sets that up on a Linux VM. Then set:

| Variable | Default | What it does |
| --- | --- | --- |
| `MERGEPATROL_EXECUTOR` | `local` | `docker` runs each job in a container. `local` runs it as a process, without isolation, for development |
| `MERGEPATROL_JOB_IMAGE` | `mergepatrol-job:latest` | The job image (`docker build --target job`) |
| `MERGEPATROL_JOB_RUNTIME` | Docker's default | `runsc` for gVisor |
| `MERGEPATROL_JOB_NETWORK` | Docker's default | `mergepatrol-jobs`, from `deploy/sandbox-host.sh` |
| `MERGEPATROL_JOB_DNS` | Docker's default | DNS servers for jobs, comma-separated. `1.1.1.1,8.8.8.8` on the sandbox network |
| `MERGEPATROL_JOB_MEMORY` | `4g` | Memory limit per job |
| `MERGEPATROL_JOB_CPUS` | `2` | CPU limit per job |

The job image has git, the Claude CLI, Rust, Go, Python, and Node with pnpm, for the verify commands.

## Crates

| Crate | What it does |
| --- | --- |
| `core` | The queue and its state machine. Pure logic, no I/O |
| `github` | GitHub App auth, the REST and GraphQL calls, and publishing commits through the Git Data API |
| `git` | Local git: fetch, worktrees, rebase |
| `resolver` | The `Resolver` trait, and the Claude implementation |
| `executor` | Runs a rebase job: as a local process, or in a container. The job protocol |
| `store` | The `Store` trait, Postgres, and the encrypted secrets. Tests need the dev Postgres from `compose.dev.yml` |
| `web` | The HTTP server: JSON API and the dashboard |
| `mergepatrol` | The binary: the orchestrator, and `mergepatrol job` for the sandbox |

## License

Apache-2.0
