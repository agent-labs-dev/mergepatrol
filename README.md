# Mergepatrol

Mergepatrol is a merge queue for GitHub. Add a label to a pull request, and Mergepatrol rebases it onto its base branch, waits for the required checks, and merges it.

When a rebase has conflicts, an agent resolves them. Claude is the default. A plain `git rebase` runs first, so the agent runs only on real conflicts.

## Batches

A queue that runs CI once per PR merges a few PRs an hour. So when several queued PRs have passed their own checks, Mergepatrol tests them together:

1. It merges them, in queue order, onto a branch `mergepatrol/batch-<PRs>` at the base branch, and opens a draft PR from it, so the repo's `pull_request` CI runs on the combination. The draft's title is the first PR's title, plus a note, so a PR title check passes.
2. When the required checks pass, it merges the PRs one by one, as they are: no rebase, and no second CI run. Each merge must leave the base branch at the tree that the batch tested, else it stops.
3. It deletes the branch, which closes the draft PR.

When a batch fails, Mergepatrol tests half of it next, and so on down to one PR, which runs alone: rebased, with its own checks. That PR is ejected; the rest merge.

A PR runs alone when its own checks have not passed, when it conflicts with the PRs ahead of it, and on a base branch that requires PRs to be up to date before they merge. Mergepatrol reads that setting from the rulesets, and from classic branch protection with the App's `administration: read` permission. Without the permission, every base branch counts as requiring it. An App created before this permission existed must be given it on GitHub.

Status: early. Not ready for use yet.

## Run it

The spike has one queue per repo, on the default branch. GitHub webhooks wake a queue as soon as something changes, but GitHub only delivers them to a public https URL. On localhost, Mergepatrol polls.

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
   2. Create the GitHub App. GitHub creates it with the permissions Mergepatrol needs. If the button does nothing, the name is taken, even by an App you deleted: change it.
   3. Install the App on your organization or account, and choose its repos. GitHub sends you back to sign in.
   4. In **Repos**, set up the repos to queue. Each gets a PR that adds its config file (below). Merge it, and the repo queues.
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
| `MERGEPATROL_GITHUB_WEBHOOK_SECRET` | The webhook secret. Optional: without it, Mergepatrol only polls |

The App needs the permissions in `crates/github/src/setup.rs` (`manifest`), the callback URL `<public URL>/auth/callback`, the setup URL `<public URL>/setup/installed`, and the webhook URL `<public URL>/webhooks/github`, subscribed to the events in `WEBHOOK_EVENTS` in the same file.

If you reach Mergepatrol at another address than http://localhost:8080, set `MERGEPATROL_PUBLIC_URL` before you create the App: GitHub sends users back to it, and delivers webhooks there when it is https. To get webhooks on your machine, run a tunnel such as `cloudflared tunnel --url http://localhost:8080` and use its URL.

| Variable | Default | What it does |
| --- | --- | --- |
| `MERGEPATROL_DATABASE_URL` | (required) | Postgres. Compose runs one |
| `MERGEPATROL_SECRET_KEY` | (required) | Encrypts the stored secrets. 32 bytes, base64 |
| `MERGEPATROL_PUBLIC_URL` | `http://localhost:8080` | Where users reach the dashboard |
| `MERGEPATROL_LABEL` | `mergepatrol` | The default trigger label. `<label>:priority` jumps the queue, and ejected PRs get `<label>:failed` |
| `MERGEPATROL_POLL_SECS` | `60` | How often to poll GitHub, in case a webhook is lost |
| `MERGEPATROL_BATCH_SIZE` | `8` | The default batch size: the most PRs tested together. `1` merges one PR at a time |
| `MERGEPATROL_DATA_DIR` | `./data` | The repo caches and the job outputs, with their transcripts |
| `MERGEPATROL_BIND` | `127.0.0.1:8080` | The dashboard address |
| `MERGEPATROL_MODEL` | `claude-opus-5-5` | The resolver's default model |
| `MERGEPATROL_EFFORT` | `medium` | The resolver's default effort |

## Repo settings

Each repo sets its queue in `.mergepatrol/mergepatrol.yml` on its default branch. Mergepatrol reads the file from there, so a PR cannot change its own settings. A setting the file leaves out takes the default from the table above.

```yaml
version: 1
label: mergepatrol
batch_size: 8
resolver:
  model: claude-opus-5-5
  effort: medium   # low, medium, high, xhigh or max
verify:
  - cargo check
```

The verify commands run after an agent resolution, before the publish.

A repo queues once its default branch has the file. Mergepatrol looks for it in every repo every 5 minutes, and at once when a push webhook adds or removes it. **Turn on** and **Turn off** in **Repos** override the file, until you choose **Follow the config file**.

The dashboard edits the file through PRs. **Set up** opens a PR that adds the file with the defaults, and **Settings** opens one that changes it. Both come from the branch `mergepatrol/config`: a second change updates the open PR. The queue uses the new settings once the PR merges.

When the file does not parse, the queue pauses, and **Repos** shows why, until the file is fixed.

## How jobs are isolated

A rebase job runs the repo's code (the verify commands) and an agent with a shell. Mergepatrol treats it as untrusted:

- The **orchestrator** holds the GitHub App key and the queue. It never runs repo code.
- Each **job** runs `mergepatrol job` in its own container, from the job image. It gets a token that can only read the one repo, and the workspace's Anthropic key. It returns a git bundle.
- The orchestrator checks the bundle before it publishes: the result must sit on the base branch, and change no file that the PR did not change.

In production the job containers use gVisor (`runsc`) on a network that cannot reach the cloud metadata server, private ranges, or the host. `deploy/sandbox-host.sh` sets that up on a Linux VM. Then set:

| Variable | Default | What it does |
| --- | --- | --- |
| `MERGEPATROL_EXECUTOR` | `local` | `docker` runs each job in a container. `cloud-run` runs it as a Cloud Run Job execution (below). `local` runs it as a process, without isolation, for development |
| `MERGEPATROL_JOB_IMAGE` | `mergepatrol-job:latest` | The job image (`docker build --target job`) |
| `MERGEPATROL_JOB_RUNTIME` | Docker's default | `runsc` for gVisor |
| `MERGEPATROL_JOB_NETWORK` | Docker's default | `mergepatrol-jobs`, from `deploy/sandbox-host.sh` |
| `MERGEPATROL_JOB_RESOLV_CONF` | Docker's default | A file on the Docker host, mounted as the jobs' `/etc/resolv.conf`. `/etc/mergepatrol/job-resolv.conf` on the sandbox network |
| `MERGEPATROL_JOB_MEMORY` | `4g` | Memory limit per job |
| `MERGEPATROL_JOB_CPUS` | `2` | CPU limit per job |

The job image has git, the Claude CLI, Rust, Go, Python, and Node with pnpm, for the verify commands.

## On GCP

`deploy/gcp/bootstrap.sh` sets up one environment in a GCP project with billing linked, and deploys it with `deploy/gcp/cloudbuild.yaml`. The orchestrator runs as the Cloud Run service `mergepatrol`, as one instance that is always on, with Cloud SQL Postgres. Each job runs as an execution of the Cloud Run Job `mergepatrol-job`, which Cloud Run sandboxes with gVisor. The job's service account has no roles, and the job has no way into the orchestrator but its link: `<MERGEPATROL_PUBLIC_URL>/jobs/<token>`. With that link, the job fetches its spec once, uploads its result in chunks, and reports that it is done. The token is in the execution's arguments, so anyone who can read the project's Cloud Run executions can see it.

This is the hosted Mergepatrol at `https://mergepatrol.ai`. It runs the public App `Mergepatrol`, which any account can install; each workspace brings its own Anthropic API key. `deploy/gcp/hosted-app.sh` creates the App once and stores its credentials in Secret Manager.

| Variable | What it is |
| --- | --- |
| `MERGEPATROL_CLOUD_RUN_JOB` | `projects/<p>/locations/<r>/jobs/<name>`, with `MERGEPATROL_EXECUTOR=cloud-run` |

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
