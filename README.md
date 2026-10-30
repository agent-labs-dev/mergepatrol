<img src="assets/banner.svg" alt="Mergepatrol: a merge queue for GitHub, with an agent on conflicts" width="100%">

Mergepatrol is a merge queue for GitHub. Add a label to a pull request, and Mergepatrol rebases it onto its base branch, waits for the required checks, and merges it.

Status: early. Expect rough edges, and [tell us](https://github.com/agent-labs-dev/mergepatrol/issues/new/choose) what breaks.

## Try it

The hosted Mergepatrol at https://mergepatrol.ai runs the public GitHub App `Mergepatrol`. You bring your own model API key.

1. Sign in with GitHub at https://mergepatrol.ai.
2. Install the App on your organization or account, and choose its repos. GitHub sends you back to the dashboard.
3. In **Repos**, **Set up** a repo. Mergepatrol opens a PR that adds its config file, [`.mergepatrol/mergepatrol.yml`](#repo-settings). Merge it, and the repo queues.
4. In **Settings**, add an Anthropic or OpenAI API key. Create one for Mergepatrol alone, with a spend limit.
5. Add the `mergepatrol` label to a PR, or queue it from the dashboard's **Queue** page, which lists the open PRs below the queue. The dashboard puts the label on as you, so it takes triage access to the repo.
6. In **You**, see your open PRs in every repo, by whose move it is, and the PRs that ask for your review or your team's, those the queue holds first. Approve, request changes or comment there, as you. A review waits 5 seconds before it goes to GitHub, so you can undo it.

Public repos are free. Private repos cost $15 a seat a month, after a free trial: see [Pricing](#pricing). To run your own, see [Run it](#run-it).

## How it works

When a rebase has conflicts, an agent resolves them. Claude is the default. A plain `git rebase` runs first, so the agent runs only on real conflicts.

When a PR's required checks fail, an agent fixes them too. It reads what GitHub shows of the failure, commits a fix on top of the PR, and the checks run again. After two fixes that do not make the checks pass, or when the agent gives up because the failure is not the PR's to fix (a flaky test, an outage), the PR is ejected.

Required checks that have not finished 3 hours after a rebase or a fix also eject the PR, with no fix: a required check that no workflow runs would hold the queue forever. In a batch, it counts as a failure.

An error outside the PR's job does not eject it. That covers GitHub being down, or slow to answer, as well as a rate limit. The PR keeps its place at the head of the queue, and its job runs again. Only after 30 minutes of such errors is it ejected.

### Branches

The default branch has a queue, and so does each branch that `branches` in [`.mergepatrol/mergepatrol.yml`](#repo-settings) lists, `release` say. A labeled PR joins the queue of the branch it targets, and is rebased onto that branch. A PR into any other branch is held, as is a PR stacked by hand on another PR's branch: a stack made with `gh stack` joins the queue of the branch the whole stack merges into. The queues of a repo take turns, one job each, so a busy branch does not hold another up; they share the repo's git cache, so two never run a job at once. A batch holds PRs into one branch only.

A PR retargeted to another branch leaves its queue, and joins the other branch's at the back. Deleting a branch closes the PRs into it, which leave its queue. A branch taken off the list holds its PRs. The settings in the file, on the default branch, apply to every branch's queue.

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

The You tab reads Mergepatrol's own database, never GitHub, when you open it. Webhooks tell Mergepatrol which PRs changed, and it reads those again from GitHub as the App. It also reads every open PR of each repo once a day, in case a webhook got lost. Without webhooks, as on a self-hosted http URL, it reads them every 10 minutes. Team review requests need the App's `members: read` permission, which each organization's owner accepts. An App created before this permission existed must be given it on GitHub, along with the `pull_request_review_comment`, `issue_comment`, `membership` and `team` events. Until then, only review requests made to you directly show.

## Pricing

The hosted Mergepatrol is free on public repos. Private repos cost $15 a month per seat, after a free trial of 14 days that needs no card. Either way, the agent runs on your own model API key.

- **Public repos** have a fair-use limit across a workspace: 100 PRs joining a queue, and 50 jobs (rebases and fixes), an hour. Over it, waiting PRs go on hold, keep their place, and go on once the last hour is under the limit. A PR already running finishes.
- **Private repos** need a subscription, started from the dashboard's Settings. Without one, their waiting PRs go on hold, with a comment that links there.
- **A seat** is a developer whose PR joined a private repo's queue. They take it with their first such PR, and keep it until removed in Settings. A seat added mid-month is charged for the rest of it; a removed one drops off the next bill. A PR authored by an App, such as Dependabot, takes no seat.

A self-hosted Mergepatrol has no limits, and bills no one.

## Run it

GitHub webhooks wake a queue as soon as something changes, but GitHub only delivers them to a public https URL. On localhost, Mergepatrol polls.

1. Generate the secret key. It encrypts the GitHub App and the model API keys in the database. Keep it: without it, you must set Mergepatrol up again.

   ```bash
   export MERGEPATROL_SECRET_KEY=$(openssl rand -base64 32)
   ```

2. Start Mergepatrol:

   ```bash
   docker compose up --build -d
   docker compose logs mergepatrol | grep setup_token
   ```

   Or without Docker for Mergepatrol itself, with Rust (`rust-toolchain.toml` picks the version) and Node 24 with pnpm:

   ```bash
   docker compose -f compose.dev.yml up -d   # or any Postgres 17
   (cd ui && pnpm install && pnpm build)     # the dashboard, which the binary embeds
   export MERGEPATROL_DATABASE_URL=postgres://mergepatrol:mergepatrol@127.0.0.1:54329/mergepatrol
   cargo run -p mergepatrol                  # prints the setup token
   ```

   Jobs then run as local processes, without a sandbox. If port 8080 is taken, set `MERGEPATROL_BIND=127.0.0.1:<port>` and `MERGEPATROL_PUBLIC_URL=http://localhost:<port>`.

3. Open http://localhost:8080 and do the setup in the dashboard:
   1. Enter the setup token from the logs.
   2. Create the GitHub App. GitHub creates it with the permissions Mergepatrol needs. If the button does nothing, the name is taken, even by an App you deleted: change it.
   3. Install the App on your organization or account, and choose its repos. GitHub sends you back to sign in.
   4. In **Repos**, set up the repos to queue. Each gets a PR that adds its config file (below). Merge it, and the repo queues.
   5. In **Settings**, add your Anthropic or OpenAI API key. Mergepatrol checks it with the provider, then stores it encrypted. Create a key for Mergepatrol alone, with a spend limit (for Anthropic, in a workspace with one), so a leaked key costs at most the limit. For OpenAI, you can **Sign in with ChatGPT** instead, and Codex runs on your ChatGPT plan. Mergepatrol signs in the way the Codex CLI does, which OpenAI does not support for other apps and could stop at any time: keep an API key to switch back to.

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
branches:          # queues besides the default branch's: exact names
  - release
```

The resolver runs Claude Code for Anthropic's models, and Codex for OpenAI's, with the workspace's key for the provider from **Settings**. A workspace signed in to ChatGPT runs Codex on its plan instead of the OpenAI key. A file that sets another provider than the default must set its model too.

The verify commands run after an agent resolution or fix, before the publish.

A fix runs in the sandbox, like a rebase, while the PR keeps its place at the head of the queue. It may change any file except those under `.github/` and `.mergepatrol/`. The agent gets each failed check's output and annotations, and for a GitHub Actions job, the end of its log. The log needs the App's `actions: read` permission: an App created before this permission existed must be given it on GitHub, or the agent works from the output alone.

A repo queues once its default branch has the file. Mergepatrol looks for it in every repo every 5 minutes, and at once when a push webhook adds or removes it. **Turn on** and **Turn off** in **Repos** override the file, until you choose **Follow the config file**.

The dashboard edits the file through PRs. **Set up** opens a PR that adds the file with the defaults, and the `mergepatrol` agent skill at `.agents/skills/mergepatrol/SKILL.md` and `.claude/skills/mergepatrol/SKILL.md`, so coding agents in the repo queue their PRs rather than merge them. **Settings** opens one that changes the file. Both come from the branch `mergepatrol/config`: a second change updates the open PR. The queue uses the new settings once the PR merges.

When the file does not parse, the queue pauses, and **Repos** shows why, until the file is fixed.

## From an agent, or the API

Mergepatrol serves an MCP server at `/mcp`, so a coding agent can do anything the dashboard does: queue a PR, take it out, read where it is, review it, read the queue's history, change a repo's settings. Every call acts as a GitHub user or bot: Mergepatrol puts the labels on, comments, and reviews with their token, so GitHub checks they may.

An MCP client that supports OAuth signs in with GitHub in the browser, as the dashboard does:

```sh
claude mcp add --transport http mergepatrol https://mergepatrol.ai/mcp
```

The client gets a Mergepatrol session, and the GitHub token stays on the server. Mergepatrol sends the sign-in back only to a loopback address, where native clients such as Claude Code, Codex and Cursor listen.

An agent without a browser, such as one in CI, passes a GitHub token instead: one from `gh auth token`, a personal access token, or a workflow's `GITHUB_TOKEN`. A fine-grained token also needs write access to issues or pull requests. Mergepatrol never stores the token. It remembers its hash, in memory for 5 minutes, so calls in a row do not each ask GitHub again.

```sh
claude mcp add --transport http mergepatrol https://mergepatrol.ai/mcp \
  --header "Authorization: Bearer $(gh auth token)"
```

A GitHub token reaches only the repos GitHub lets it read: Mergepatrol cannot tell which of its workspaces such a token belongs to. The tools marked **Sign-in** work across a workspace, so they need the sign-in. Each tool takes the repo as `repo`, `owner/name`, or the workspace as `workspace`, its installation ID, which `get_session` lists.

<!-- tools: the order of `ROUTES` in crates/web/src/routes.rs -->
| Tool | Does | Sign-in |
| --- | --- | --- |
| `get_changelog` | Mergepatrol's newest changelog entries | |
| `get_session` | Who you are signed in as, and your workspaces | |
| `get_inbox` | Your PRs, and the PRs that ask for your review, across workspaces | Yes |
| `list_queue` | The queues, in the order they take PRs: a repo's, or the workspace's | Without `repo` |
| `list_events` | A repo's queue events, newest first | |
| `get_fix` | The diff of a commit the fix agent pushed | |
| `list_history` | The PRs that left the queue: merged, ejected, or removed | Yes |
| `get_history_stats` | The queue's numbers over a window | Yes |
| `list_repos` | The workspace's repos, and whether each queues | Yes |
| `set_repo` | Turns a repo's queue on or off, or back to following its config file | Yes |
| `list_open_pulls` | A repo's open PRs that are not queued | |
| `get_pull` | A PR's description, labels, checks, and timeline | |
| `get_pull_diff` | A PR's changes, or one commit's | |
| `pull_status` | Where a PR is, as `/api/v1` below says, and its latest events | |
| `queue_pull` | Queues a PR, or its stack. `priority` puts it at the front | |
| `dequeue_pull` | Takes a PR, and its stack, out of the queue | |
| `close_pull` | Closes a PR | |
| `review_pull` | Approves a PR, asks for changes, or comments, then queues it if asked | |
| `mark_pull_seen` | Marks a PR read in your inbox | Yes |
| `snooze_pull` | Puts a PR out of your inbox for a while | Yes |
| `unsnooze_pull` | Puts a snoozed PR back in your inbox | Yes |
| `nudge_reviewers` | Mentions reviewers in a comment, asking them to review | |
| `get_repo_config` | A repo's queue settings | |
| `propose_repo_config` | Opens a PR that changes a repo's queue settings | Yes |
| `get_settings` | Which model API keys and ChatGPT login the workspace has | Yes |
| `set_api_key` | Sets a model provider's API key | Yes |
| `delete_api_key` | Removes a model provider's API key | Yes |
| `start_chatgpt_login` | Starts a ChatGPT sign-in: a code for the user to enter | Yes |
| `delete_chatgpt_login` | Removes the ChatGPT login | Yes |
| `poll_chatgpt_login` | Finishes the ChatGPT sign-in once the user entered the code | Yes |
| `get_billing` | The workspace's plan and seats | Yes |
| `start_checkout` | A Stripe page to subscribe | Yes |
| `open_billing_portal` | A Stripe page to manage the subscription | Yes |
| `free_seat` | Frees a seat | Yes |
<!-- /tools -->

Each tool calls a route of the JSON API under `/api`, which the dashboard uses. A script can call the API itself with the same `Authorization: Bearer` header:

```sh
curl -H "Authorization: Bearer $(gh auth token)" \
  https://mergepatrol.ai/api/w/<installation ID>/repos/<owner>/<name>/pulls/<number>/status
```

## Read a PR's state from another app

An app that shows PRs, such as a PR list in an editor, reads what Mergepatrol knows of them from `/api/v1`, instead of working it out from labels and the config file. It authenticates as the API above does: `Authorization: Bearer` with the viewer's GitHub token (OAuth, personal access token, or an App installation token) or a Mergepatrol session. GitHub decides what the token can read. A repo it cannot read is a 404, the same as a repo that does not exist. A repo it can read is a 200, also when Mergepatrol does not run there.

```sh
curl -H "Authorization: Bearer $(gh auth token)" \
  https://mergepatrol.ai/api/v1/repos/acme/widgets/pulls/42
curl -H "Authorization: Bearer $(gh auth token)" \
  "https://mergepatrol.ai/api/v1/repos/acme/widgets/pulls?numbers=42,43,44"
```

The second form reads up to 100 PRs of one repo, as `{ "pulls": [...] }` in the order asked. Each PR reads:

```json
{
  "repo": "acme/widgets",
  "number": 42,
  "url": "https://mergepatrol.ai/acme/widgets/pull/42",
  "mergepatrol": {
    "state": "on",
    "installed": true,
    "has_config": true,
    "override": null,
    "config_error": null
  },
  "queue": {
    "trunk": "main",
    "state": "held",
    "hold": {
      "reason": "review_required",
      "layer": null,
      "message": "waiting for a required review"
    },
    "priority": false,
    "position": 3,
    "stack": null,
    "batch_pr": null
  },
  "merged": null,
  "ejected": null
}
```

| Field | Says |
| --- | --- |
| `url` | The PR in the dashboard. It opens the PR's lane while queued, its history once merged or ejected, the repo's settings while Mergepatrol is off there, and the repo's open PRs otherwise |
| `mergepatrol.state` | `on`, `off` (no App on the repo, no config file, or turned off), or `paused` (the config file does not parse: `config_error` says why) |
| `mergepatrol.override` | `on` or `off` when turned so in the dashboard, whatever the config file says. `null` follows the file |
| `queue` | `null` unless queued. `state` is `waiting`, `running` or `held`. `position` counts from 1 in the order the queue takes PRs. `stack` is `{ number, layers }` for a stack. `batch_pr` is the draft PR testing it, while it runs in a batch |
| `queue.hold.reason` | `draft`, `review_required`, `changes_requested`, `workflow_approval` (a fork PR's runs wait for approval), `fork_edits_off` (a fork PR behind its trunk that does not allow edits by maintainers), `no_queue` (it targets a branch without a queue), `no_subscription` or `fair_use_limit`. `layer` is the stack's PR that holds it |
| `merged`, `ejected` | How it last left the queue: `{ at, sha }` or `{ at, reason }`. Both `null` while it is queued, and when it left otherwise |

Every response has an `ETag`: send it back as `If-None-Match` for a `304` when nothing changed. Each caller, by its token, has 5,000 calls an hour, a `304` included, which `x-ratelimit-limit`, `x-ratelimit-remaining`, `x-ratelimit-used` and `x-ratelimit-reset` (Unix seconds) report. Past the limit, a `429` has a `retry-after`, in seconds. So does a `503` when GitHub's own rate limit is used up.

## How jobs are isolated

A rebase job runs the repo's code (the verify commands) and an agent with a shell. Mergepatrol treats it as untrusted:

- The **orchestrator** holds the GitHub App key and the queue. It never runs repo code.
- Each **job** runs `mergepatrol job` in its own container, from the job image. It gets a GitHub token that can only read the one repo, and revokes it once it has fetched the repo. It returns a git bundle.
- The workspace's model API keys and ChatGPT login stay in the orchestrator, which renews the login. A job calls its provider's API through the orchestrator's proxy, at `/anthropic` for Claude Code or `/openai` for Codex, with a token that works only while the job runs, and for a set number of requests.
- The orchestrator checks the bundle before it publishes: the result must sit on the base branch, and change no file that the PR did not change.

In production the job containers use gVisor (`runsc`) on a network that cannot reach the cloud metadata server, private ranges, or the host. `deploy/sandbox-host.sh` sets that up on a Linux VM. Then set:

| Variable | Default | What it does |
| --- | --- | --- |
| `MERGEPATROL_EXECUTOR` | `local` | `docker` runs each job in a container. `agent-sandbox` runs it in a GKE sandbox (below). `local` runs it as a process, without isolation, for development |
| `MERGEPATROL_JOB_IMAGE` | `mergepatrol-job:latest` | The job image (`docker build --target job`) |
| `MERGEPATROL_JOB_RUNTIME` | Docker's default | `runsc` for gVisor |
| `MERGEPATROL_JOB_NETWORK` | Docker's default | `mergepatrol-jobs`, from `deploy/sandbox-host.sh` |
| `MERGEPATROL_JOB_RESOLV_CONF` | Docker's default | A file on the Docker host, mounted as the jobs' `/etc/resolv.conf`. `/etc/mergepatrol/job-resolv.conf` on the sandbox network |
| `MERGEPATROL_JOB_PROXY_URL` | `MERGEPATROL_PUBLIC_URL` | Where jobs reach the orchestrator: its model API proxy, and a GKE sandbox's link. `http://10.213.0.1:8081` on the sandbox network |
| `MERGEPATROL_JOB_PROXY_BIND` | None | A second listener that serves only what jobs use, for jobs that cannot reach the dashboard's. `0.0.0.0:8081` in `compose.yml` |
| `MERGEPATROL_JOB_MEMORY` | `4g` | Memory limit per job |
| `MERGEPATROL_JOB_CPUS` | `2` | CPU limit per job |

The job image has git, the Claude and Codex CLIs, Rust, Go, Python, and Node with pnpm, for the verify commands.

Each release publishes both images, tagged with its version and `latest`: `ghcr.io/agent-labs-dev/mergepatrol`, the orchestrator, and `ghcr.io/agent-labs-dev/mergepatrol-job`. To run a release instead of building, set `MERGEPATROL_JOB_IMAGE=ghcr.io/agent-labs-dev/mergepatrol-job:<version>`.

## On GCP

`deploy/gcp/bootstrap.sh` sets up one environment in a GCP project with billing linked: the GKE Autopilot cluster `mergepatrol`, Cloud SQL Postgres, and a load balancer for the public URL. It deploys with `deploy/gcp/cloudbuild.yaml`, which runs `deploy/gcp/deploy.sh`. The manifests are under `deploy/gcp/k8s`.

The orchestrator runs as the Deployment `mergepatrol`, one instance, with Cloud SQL through a proxy sidecar and its secrets from Secret Manager. Only the instance that holds a lock in Postgres runs the queues, so the new instance of a deploy waits until the old one has stopped. On SIGTERM, an instance lets go of the lock at once.

Each job runs in its own gVisor sandbox, through [Agent Sandbox](https://github.com/kubernetes-sigs/agent-sandbox): the orchestrator claims one from the warm pool `mergepatrol-job`, which keeps one sandbox ready, so a job starts in about a second. Jobs past the first start cold, on a new node, in 1 to 2.5 minutes. A sandbox has no service account and no DNS. Its network policy lets it reach two things: the orchestrator's job port, and the egress proxy `mergepatrol-egress`. It can't reach the metadata server, the rest of the cluster, the VPC, or the internet directly. The egress proxy is Squid, at `34.118.224.100:3128`, set as the sandbox's `HTTPS_PROXY`. It opens tunnels to port 443 of the hosts in `deploy/gcp/k8s/squid.conf`, and refuses every other host:

- GitHub, to fetch the repo and revoke the job's token.
- The package registries the verify commands use: npm and Yarn, PyPI, the Go module proxy and its bucket, crates.io, and rustup's toolchains.

A job that needs another host fails until the host is added there. The proxy logs every tunnel it opens or refuses (`kubectl -n mergepatrol-jobs logs -l app=mergepatrol-egress`). The allowlist goes by host name. So it doesn't stop a job that brings its own credentials from pushing to GitHub or publishing to a registry. It also doesn't stop a job from writing to a bucket that anyone can write to on `storage.googleapis.com`, where the Go proxy keeps large modules.

The orchestrator hands the sandbox its link, `http://<orchestrator pod IP>:8081/jobs/<token>`, over HTTP, since a claim that sets env would skip the warm pool. With that link, the job fetches its spec once, uploads its result in chunks, and reports that it is done.

This is the hosted Mergepatrol at `https://mergepatrol.ai`. It runs the public App `Mergepatrol`, which any account can install; each workspace brings its own Anthropic or OpenAI API key. `deploy/gcp/hosted-app.sh` creates the App once and stores its credentials in Secret Manager.

| Variable | What it is |
| --- | --- |
| `MERGEPATROL_SANDBOX_NAMESPACE` | Where the jobs' sandboxes run, with `MERGEPATROL_EXECUTOR=agent-sandbox`. `mergepatrol-jobs` by default |
| `MERGEPATROL_SANDBOX_WARM_POOL` | The `SandboxWarmPool` that sandboxes are claimed from. `mergepatrol-job` by default |
| `MERGEPATROL_POSTHOG_KEY` | The PostHog project key. Turns on product analytics (below) |
| `MERGEPATROL_POSTHOG_HOST` | The PostHog host. `https://us.i.posthog.com` by default |

### Billing

Billing runs through Stripe, and is off unless the three `MERGEPATROL_STRIPE_*` variables below are set. In Stripe, create:

1. A product, with a recurring price of $15 a month per unit: `MERGEPATROL_STRIPE_PRICE`. Mergepatrol keeps each subscription's quantity at the workspace's seats.
2. A webhook endpoint at `<public URL>/webhooks/stripe`, for `checkout.session.completed` and `customer.subscription.created`, `.updated` and `.deleted`: its signing secret is `MERGEPATROL_STRIPE_WEBHOOK_SECRET`.
3. The customer portal's settings, where a customer adds a card, sees invoices, or cancels.

A workspace's first subscription starts with Stripe's trial, without a card. One that has no card when the trial ends is cancelled.

| Variable | What it is |
| --- | --- |
| `MERGEPATROL_STRIPE_SECRET_KEY` | A secret or restricted key: Checkout sessions, customer portal sessions, and subscriptions (write) |
| `MERGEPATROL_STRIPE_WEBHOOK_SECRET` | The webhook endpoint's signing secret, `whsec_…` |
| `MERGEPATROL_STRIPE_PRICE` | The price, `price_…` |

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

### Analytics

With `MERGEPATROL_POSTHOG_KEY`, the hosted Mergepatrol sends product analytics to PostHog. Without it, as when self-hosted, it sends nothing, and the dashboard does not load PostHog.

- A person is a GitHub login. The dashboard identifies the signed-in user, and the server sends `signed in`.
- A `workspace` group is an installation of the App, keyed by its installation ID, with the account's name. The dashboard groups what the user does under the workspace in view.
- Each queue event goes to its PR's author and workspace as `pr <kind>`, such as `pr merged` or `pr ejected`, with the repo, trunk and PR number. When a PR leaves the queue, the event also has `queue_seconds`, the seconds in each step (`waiting_seconds`, `held_seconds`, `rebasing_seconds`, `checks_seconds`, `fixing_seconds`), and, for `pr ejected`, the `reason`: `fork`, `rebase-failed`, `checks-failed`, `fix-failed`, `base-moved`, `merge-rejected` or `checks-timed-out`.
- Installing the App sends `app installed`, for the person who installed it, and uninstalling it `app uninstalled`.
- The dashboard sends `api key set` and `api key removed`, with the `provider`, and `repo config proposed`.
- Before sign-in, the landing page and the `/blog` articles send PostHog's `$pageview` and autocaptured clicks under an anonymous id, with no person profile. The articles load PostHog's script themselves, as they are plain HTML outside the dashboard. A reader keeps the same id from an article to the landing page, and signing in joins it to their login.

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
