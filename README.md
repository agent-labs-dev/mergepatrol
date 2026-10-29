<img src="assets/banner.svg" alt="Mergepatrol: a merge queue for GitHub, with an agent on conflicts" width="100%">

Mergepatrol is a merge queue for GitHub. Add a label to a pull request, and Mergepatrol rebases it onto its base branch, waits for the required checks, and merges it.

Status: early. Expect rough edges, and [tell us](https://github.com/agent-labs-dev/mergepatrol/issues/new/choose) what breaks.

## Try it

The hosted Mergepatrol at https://mergepatrol.ai runs the public GitHub App `Mergepatrol`. You bring your own model API key.

1. Sign in with GitHub at https://mergepatrol.ai.
2. Install the App on your organization or account, and choose its repos. GitHub sends you back to the dashboard.
3. In **Repos**, **Set up** a repo. Mergepatrol opens a PR that adds its config file, [`.mergepatrol/mergepatrol.yml`](#repo-settings). Merge it, and the repo queues.
4. In **Settings**, add an Anthropic or OpenAI API key. Create one for Mergepatrol alone, with a spend limit.
5. Add the `mergepatrol` label to a PR.

To run your own, see [Run it](#run-it).

## How it works

When a rebase has conflicts, an agent resolves them. Claude is the default. A plain `git rebase` runs first, so the agent runs only on real conflicts.

When a PR's required checks fail, an agent fixes them too. It reads what GitHub shows of the failure, commits a fix on top of the PR, and the checks run again. After two fixes that do not make the checks pass, or when the agent gives up because the failure is not the PR's to fix (a flaky test, an outage), the PR is ejected.

Required checks that have not finished 3 hours after a rebase or a fix also eject the PR, with no fix: a required check that no workflow runs would hold the queue forever. In a batch, it counts as a failure.

An error outside the PR's job does not eject it. That covers GitHub being down, or slow to answer, as well as a rate limit. The PR keeps its place at the head of the queue, and its job runs again. Only after 30 minutes of such errors is it ejected.

### Batches

A queue that runs CI once per PR merges a few PRs an hour. So when several queued PRs have passed their own checks, Mergepatrol tests them together:

1. It merges them, in queue order, onto a branch `mergepatrol/batch-<PRs>` at the base branch, and opens a draft PR from it, so the repo's `pull_request` CI runs on the combination. The draft's title is the first PR's title, plus a note, so a PR title check passes.
2. When the required checks pass, it merges the PRs one by one, as they are: no rebase, and no second CI run. Each merge must leave the base branch at the tree that the batch tested, else it stops.
3. It deletes the branch, which closes the draft PR.

When a batch fails, Mergepatrol tests half of it next, and so on down to one PR, which runs alone: rebased, with its own checks. That PR is ejected; the rest merge.

A PR runs alone when its own checks have not passed, when it conflicts with the PRs ahead of it, and on a base branch that requires PRs to be up to date before they merge. Mergepatrol reads that setting from the rulesets, and from classic branch protection with the App's `administration: read` permission. Without the permission, every base branch counts as requiring it. An App created before this permission existed must be given it on GitHub.

### Stacks

A stack of PRs, made with `gh stack`, is one item of the queue: the label on any of its PRs queues the whole stack, and Mergepatrol merges it all at once, with GitHub's asynchronous merge. Either every PR of the stack merges, or none does. GitHub merges a stack only when the required checks of each of its PRs passed.

- The stack waits while any of its PRs is a draft or waits for a review.
- In a batch, the stack is one step: its top PR holds every layer below it.
- Alone, Mergepatrol rebases each layer onto the one below it, the bottom onto the base branch, and waits for each layer's checks. A failed check is fixed on the layer it failed on, and the layers above it are rebased onto the fix.
- A failed stack is ejected whole.

Every merge goes through the asynchronous merge API, stacked or not. A merge still pending when Mergepatrol restarts is waited for, not requested again.

### PRs from forks

A maintainer queues a PR from a fork with the label, as any other. Its author is not trusted as the repo's own are, so the queue treats it differently:

- It merges the PR only as it was when the label went on. Mergepatrol reads that head from the `labeled` webhook, or, without webhooks, from when it first sees the PR. If the author pushes after that, the PR leaves the queue, and a maintainer adds the label again to queue the new commits.
- It never tests the PR in a batch, whose draft PR runs CI with the repo's secrets. The PR runs alone, with the fork's own CI.
- It runs no agent on the PR: a plain `git rebase` only. A conflict, or a failed required check, ejects the PR, for its author to fix.
- It pushes the rebase to the fork, which GitHub allows while the author allows edits by maintainers. Without that, a PR that needs a rebase waits on hold until they allow it. GitHub does not let the App push to a fork that an organization owns, nor push changes to workflow files to a fork: such a PR that needs a rebase is ejected, for its author to rebase.
- While GitHub waits for a maintainer to approve the fork's workflow runs, the PR waits on hold.

A stack with a PR from a fork is not supported yet.

## Run it

The spike has one queue per repo, on the default branch. GitHub webhooks wake a queue as soon as something changes, but GitHub only delivers them to a public https URL. On localhost, Mergepatrol polls.

1. Generate the secret key. It encrypts the GitHub App and the model API keys in the database. Keep it: without it, you must set Mergepatrol up again.

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
   5. In **Settings**, add your Anthropic or OpenAI API key. Mergepatrol checks it with the provider, then stores it encrypted. Create a key for Mergepatrol alone, with a spend limit (for Anthropic, in a workspace with one), so a leaked key costs at most the limit.

4. Add the `mergepatrol` label to a PR to queue it. Mergepatrol creates the queue labels on each repo it queues.

Anyone with a GitHub account can sign in. Each installation of the App is a workspace, with its own repos and its own model API keys. A user sees the workspaces that GitHub lets them access. A session reads them from GitHub again every hour, so a new installation shows up, and a lost one goes, within the hour. A session lasts 30 days from its last use, and ends when GitHub stops taking the user's token: when they revoke it, or after 6 months, when GitHub's refresh token expires.

## Run it with an App you made yourself

The hosted Mergepatrol uses one public GitHub App, which we create by hand on GitHub, rather than with the setup in the dashboard. A self-hosted Mergepatrol can do the same. Set the App's credentials, and the setup token and the App creation are skipped:

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
| `MERGEPATROL_SECRET_KEY` | (required) | Encrypts the stored secrets. 32 bytes, base64. To rotate it, see below |
| `MERGEPATROL_SECRET_KEY_FILE` | | A file that holds the secret key, instead of `MERGEPATROL_SECRET_KEY`. Compose uses one |
| `MERGEPATROL_PUBLIC_URL` | `http://localhost:8080` | Where users reach the dashboard |
| `MERGEPATROL_LABEL` | `mergepatrol` | The default trigger label. `<label>:priority` jumps the queue, and ejected PRs get `<label>:failed` |
| `MERGEPATROL_POLL_SECS` | `60` | How often to poll GitHub, in case a webhook is lost |
| `MERGEPATROL_BATCH_SIZE` | `8` | The default batch size: the most PRs tested together. `1` merges one PR at a time |
| `MERGEPATROL_MAX_JOBS` | `4` | The most rebase and fix jobs that run at once, across every queue |
| `MERGEPATROL_DATA_DIR` | `./data` | The repo caches and the job outputs, with their transcripts. A job's output is kept 7 days |
| `MERGEPATROL_BIND` | `127.0.0.1:8080` | The dashboard address |
| `MERGEPATROL_PROVIDER` | `anthropic` | The resolver's default provider: `anthropic` or `openai` |
| `MERGEPATROL_MODEL` | `claude-opus-5-5` | The resolver's default model. Required when the default provider is not `anthropic` |
| `MERGEPATROL_EFFORT` | `medium` | The resolver's default effort |

### Rotate the secret key

Generate a new key, and set it in front of the old one, separated by a comma: `MERGEPATROL_SECRET_KEY=<new>,<old>`. Restart Mergepatrol. At startup, it re-encrypts every stored secret with the new key, and logs how many. Then set only the new key, and restart again. Each session's GitHub token is re-encrypted at its next hourly check; a session idle through the whole rotation signs out.

On GCP, a deploy runs the old revision beside the new one for a while, and each must read what the other writes. Add each step as a version of the `mergepatrol-secret-key` secret, and deploy it:

1. `<old>,<new>`: every revision can read both keys, and still encrypts with the old one.
2. `<new>,<old>`: the new revision re-encrypts with the new key.
3. `<new>,<old>` again, once the revision from step 1 has stopped: it re-encrypts what that one wrote with the old key meanwhile.
4. `<new>`.

## Repo settings

Each repo sets its queue in `.mergepatrol/mergepatrol.yml` on its default branch. Mergepatrol reads the file from there, so a PR cannot change its own settings. A setting the file leaves out takes the default from the table above.

```yaml
version: 1
label: mergepatrol
batch_size: 8
resolver:
  provider: anthropic   # or openai
  model: claude-opus-5-5
  effort: medium        # low, medium, high, xhigh or max
verify:
  - cargo check
fix_ci:
  enabled: true    # an agent fixes failed required checks
  attempts: 2      # 1 to 5 fixes, before the PR is ejected
```

The resolver runs Claude Code for Anthropic's models, and Codex for OpenAI's, with the workspace's key for the provider from **Settings**. A file that sets another provider than the default must set its model too.

The verify commands run after an agent resolution or fix, before the publish.

A fix runs in the sandbox, like a rebase, while the PR keeps its place at the head of the queue. It may change any file except those under `.github/` and `.mergepatrol/`. The agent gets each failed check's output and annotations, and for a GitHub Actions job, the end of its log. The log needs the App's `actions: read` permission: an App created before this permission existed must be given it on GitHub, or the agent works from the output alone.

A repo queues once its default branch has the file. Mergepatrol looks for it in every repo every 5 minutes, and at once when a push webhook adds or removes it. **Turn on** and **Turn off** in **Repos** override the file, until you choose **Follow the config file**.

The dashboard edits the file through PRs. **Set up** opens a PR that adds the file with the defaults, and the `mergepatrol` agent skill at `.agents/skills/mergepatrol/SKILL.md` and `.claude/skills/mergepatrol/SKILL.md`, so coding agents in the repo queue their PRs rather than merge them. **Settings** opens one that changes the file. Both come from the branch `mergepatrol/config`: a second change updates the open PR. The queue uses the new settings once the PR merges.

When the file does not parse, the queue pauses, and **Repos** shows why, until the file is fixed.

## How jobs are isolated

A rebase job runs the repo's code (the verify commands) and an agent with a shell. Mergepatrol treats it as untrusted:

- The **orchestrator** holds the GitHub App key and the queue. It never runs repo code.
- Each **job** runs `mergepatrol job` in its own container, from the job image. It gets a GitHub token that can only read the one repo, and revokes it once it has fetched the repo. It returns a git bundle.
- The workspace's model API keys stay in the orchestrator. A job calls its provider's API through the orchestrator's proxy, at `/anthropic` for Claude Code or `/openai` for Codex, with a token that works only while the job runs, and for a set number of requests.
- The orchestrator checks the bundle before it publishes: the result must sit on the base branch, and change no file that the PR did not change.

In production the job containers use gVisor (`runsc`) on a network that cannot reach the cloud metadata server, private ranges, or the host. `deploy/sandbox-host.sh` sets that up on a Linux VM. Then set:

| Variable | Default | What it does |
| --- | --- | --- |
| `MERGEPATROL_EXECUTOR` | `local` | `docker` runs each job in a container. `cloud-run` runs it as a Cloud Run Job execution (below). `local` runs it as a process, without isolation, for development |
| `MERGEPATROL_JOB_IMAGE` | `mergepatrol-job:latest` | The job image (`docker build --target job`) |
| `MERGEPATROL_JOB_RUNTIME` | Docker's default | `runsc` for gVisor |
| `MERGEPATROL_JOB_NETWORK` | Docker's default | `mergepatrol-jobs`, from `deploy/sandbox-host.sh` |
| `MERGEPATROL_JOB_RESOLV_CONF` | Docker's default | A file on the Docker host, mounted as the jobs' `/etc/resolv.conf`. `/etc/mergepatrol/job-resolv.conf` on the sandbox network |
| `MERGEPATROL_JOB_PROXY_URL` | `MERGEPATROL_PUBLIC_URL` | Where jobs reach the orchestrator's model API proxy. `http://10.213.0.1:8081` on the sandbox network |
| `MERGEPATROL_JOB_PROXY_BIND` | None | A second listener that serves only the proxy, for jobs that cannot reach the dashboard's. `0.0.0.0:8081` in `compose.yml` |
| `MERGEPATROL_JOB_MEMORY` | `4g` | Memory limit per job |
| `MERGEPATROL_JOB_CPUS` | `2` | CPU limit per job |

The job image has git, the Claude and Codex CLIs, Rust, Go, Python, and Node with pnpm, for the verify commands.

Each release publishes both images, tagged with its version and `latest`: `ghcr.io/agent-labs-dev/mergepatrol`, the orchestrator, and `ghcr.io/agent-labs-dev/mergepatrol-job`. To run a release instead of building, set `MERGEPATROL_JOB_IMAGE=ghcr.io/agent-labs-dev/mergepatrol-job:<version>`.

## On GCP

`deploy/gcp/bootstrap.sh` sets up one environment in a GCP project with billing linked, and deploys it with `deploy/gcp/cloudbuild.yaml`. The orchestrator runs as the Cloud Run service `mergepatrol`, as one instance that is always on, with Cloud SQL Postgres. Only the instance that holds a lock in Postgres runs the queues, so the new instance of a deploy waits until the old one has stopped. On SIGTERM, an instance lets go of the lock at once. Each job runs as an execution of the Cloud Run Job `mergepatrol-job`, which Cloud Run sandboxes with gVisor. The job's service account has no roles, and the job has no way into the orchestrator but its link: `<MERGEPATROL_PUBLIC_URL>/jobs/<token>`. With that link, the job fetches its spec once, uploads its result in chunks, and reports that it is done. The token is in the execution's arguments, so anyone who can read the project's Cloud Run executions can see it.

This is the hosted Mergepatrol at `https://mergepatrol.ai`. It runs the public App `Mergepatrol`, which any account can install; each workspace brings its own Anthropic or OpenAI API key. `deploy/gcp/hosted-app.sh` creates the App once and stores its credentials in Secret Manager.

| Variable | What it is |
| --- | --- |
| `MERGEPATROL_CLOUD_RUN_JOB` | `projects/<p>/locations/<r>/jobs/<name>`, with `MERGEPATROL_EXECUTOR=cloud-run` |
| `MERGEPATROL_POSTHOG_KEY` | The PostHog project key. Turns on product analytics (below) |
| `MERGEPATROL_POSTHOG_HOST` | The PostHog host. `https://us.i.posthog.com` by default |

### Analytics

With `MERGEPATROL_POSTHOG_KEY`, the hosted Mergepatrol sends product analytics to PostHog. Without it, as when self-hosted, it sends nothing, and the dashboard does not load PostHog.

- A person is a GitHub login. The dashboard identifies the signed-in user, and the server sends `signed in`.
- A `workspace` group is an installation of the App, keyed by its installation ID, with the account's name. The dashboard groups what the user does under the workspace in view.
- Each queue event goes to its PR's author and workspace as `pr <kind>`, such as `pr merged` or `pr ejected`, with the repo, trunk and PR number. When a PR leaves the queue, the event also has `queue_seconds`, the seconds in each step (`waiting_seconds`, `held_seconds`, `rebasing_seconds`, `checks_seconds`, `fixing_seconds`), and, for `pr ejected`, the `reason`: `fork`, `rebase-failed`, `checks-failed`, `fix-failed`, `base-moved`, `merge-rejected` or `checks-timed-out`.
- Installing the App sends `app installed`, for the person who installed it, and uninstalling it `app uninstalled`.
- The dashboard sends `api key set` and `api key removed`, with the `provider`, and `repo config proposed`.

## Crates

| Crate | What it does |
| --- | --- |
| `core` | The queue and its state machine. Pure logic, no I/O |
| `github` | GitHub App auth, the REST and GraphQL calls, and publishing commits through the Git Data API |
| `git` | Local git: fetch, worktrees, rebase |
| `resolver` | The `Resolver` trait, and its implementations: Claude Code for Anthropic, Codex for OpenAI |
| `executor` | Runs a rebase job: as a local process, or in a container. The job protocol |
| `store` | The `Store` trait, Postgres, and the encrypted secrets. Tests need the dev Postgres from `compose.dev.yml` |
| `web` | The HTTP server: JSON API and the dashboard |
| `analytics` | Product analytics in PostHog, for the hosted Mergepatrol |
| `mergepatrol` | The binary: the orchestrator, and `mergepatrol job` for the sandbox |

## Compared to GitHub's merge queue

GitHub's [merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue) also tests PRs together before they merge. What Mergepatrol adds:

| | Mergepatrol |
| --- | --- |
| A PR that conflicts with its base | An agent resolves the conflict, and the PR stays in the queue |
| A required check that fails | An agent commits a fix on the PR, up to `fix_ci.attempts` times, before the PR is ejected |
| A batch that fails | Half of it is tested next, and so on down to the one PR that fails. The rest merge |
| Stacked PRs | A `gh stack` stack queues and merges as one item |
| CI | Runs on the repo's `pull_request` workflows, on a draft PR per batch. No `merge_group` trigger to add |
| Queueing a PR | A label, so people and coding agents queue the same way |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Report vulnerabilities privately, as [SECURITY.md](SECURITY.md) describes.

## License

Apache-2.0
