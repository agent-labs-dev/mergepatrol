# Review eval

Scores MergeLoop's review on pull requests whose bugs we already know. Run it before you keep a
change to the review prompt, its tools, or its model: if the score went up, keep the change; if
not, drop it.

## Terms

- **Case**: a merged PR, as it was when it merged, in `cases/<id>.yml`. It names the repo, the
  PR, its `base` (where it left the trunk) and `head` SHAs, its title and body, and what it was
  expected to get: `clean`, or `bugs`.
- **Expected bug**: a bug the PR shipped, found because a later PR fixed it or reverted it. It
  gives each place it lives `at` (a `file` and its `lines` on the PR's head), `what` it is, and
  what it was `fixed_by`. Someone who knows the code confirmed each one.
- **Sent back**: a finding MergeLoop routes to the author under the default review settings: a
  `change`, over its kind's bar, not for a human, and shown by evidence that MergeLoop ran and saw
  fail (or `security`). Only these count, since only these make anyone change the code.
- **Caught**: an expected bug with a finding sent back in a file it lives in, within 5 lines of
  it there.
- **False positive**: a finding sent back on a clean case. It would have led the author to change
  code that was fine.
- **Unmatched**: a finding sent back on a case with bugs that is about none of them. Maybe a bug
  nobody knew of, maybe noise: read them.
- **Run**: one review of each case, by one agent, kept in a JSON file with the findings. A run
  is scored against the cases as they are now, so a corrected case rescores old runs. A case whose
  head changed since the run is left out of it.

## Run it

The review runs as production runs it: `review-eval` writes the queue's review job for each case
and runs it with `mergepatrol job`, calling the model through MergeLoop's own API proxy. So the
prompt, the agent's tools, and how its evidence runs are the binary's. The job clones the case's
repo from GitHub and runs the evidence's commands, so it needs what those commands need (here,
Rust and the dev Postgres from `compose.dev.yml`), and the agent's CLI (`claude` or `codex`).

```bash
(cd ui && pnpm install && pnpm build)            # mergepatrol embeds the dashboard
cargo build --release -p mergepatrol -p mergepatrol-eval
ANTHROPIC_API_KEY=$(security find-generic-password -s <key-name> -w) \
  target/release/review-eval run --label main --model claude-opus-5-5 --effort medium
```

It prints each case and the totals, and writes the run to `target/review-eval/<label>.json`.
`--case <id>` runs only some cases, `--jobs` how many at once (2), `--provider` picks `openai` or
`openrouter` (with `OPENAI_API_KEY` or `OPENROUTER_API_KEY`).

## Compare two runs

A prompt change lives in the binary, so build each side and point the run at it:

```bash
git worktree add ../try my-prompt-branch && (cd ../try && cargo build --release -p mergepatrol)
target/release/review-eval run --label try --model claude-opus-5-5 --job-bin ../try/target/release/mergepatrol
target/release/review-eval compare target/review-eval/main.json target/review-eval/try.json
```

For a model change, run the same binary with another `--model`. `compare` scores both runs on the
cases both reviewed, lists each bug one caught and the other missed, and says `keep B` only when
B catches more bugs with no more false positives, or as many with fewer. `report <run.json>`
scores one run again, against the cases as they are now.

A run costs real money: the totals say how much. An agent is not deterministic, so one bug won or
lost may be noise; run both sides twice before you trust a close call.

## Add a case

A PR that shipped a bug: find what fixed it, and the lines on the PR's head that had to change.
Copy a case in `cases/`, and set `base` to `git merge-base <merge>^1 <merge>^2` and `head` to
`<merge>^2`. Have someone who knows the code confirm the bug and its lines. A clean PR is one that
no later fix touched.
