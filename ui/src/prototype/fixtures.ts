/*
 * PROTOTYPE, throw away. Stub data for the PR detail sidebar variants: the backend has no
 * endpoint yet for a PR's body, timeline, check runs or patch. `fixture.patch` is a real commit
 * of this repo (df954aa), so the tree and the diffs render something believable.
 */

export type TimelineItem =
  | { kind: "commit"; at: string; login: string; sha: string; message: string }
  | {
      kind: "review";
      at: string;
      login: string;
      state: "approved" | "changes" | "commented";
      body?: string;
    }
  | { kind: "comment"; at: string; login: string; body: string }
  | { kind: "requested"; at: string; login: string; reviewer: string }
  | { kind: "label"; at: string; login: string; label: string; added: boolean }
  | { kind: "force-push"; at: string; login: string; from: string; to: string };

export type CheckRun = {
  name: string;
  status: "passed" | "failed" | "pending" | "skipped";
  took?: string;
};

export type Comment = {
  at: string;
  login: string;
  kind: "comment" | "review" | "bot";
  body: string;
};

export type PrExtras = {
  body: string;
  labels: string[];
  comments: Comment[];
  mergeable: "clean" | "conflicts" | "behind";
  timeline: TimelineItem[];
  checkRuns: CheckRun[];
};

const ago = (min: number) => new Date(Date.now() - min * 60_000).toISOString();

const body = `Every 5 minutes the queue lists the App's webhook deliveries since the last look and has GitHub send the failed ones again. Each read waits for its webhook, so a lost one left it stale until the queue read the whole repo again.

## Summary

- Redeliver failed webhooks, at most 3 times each.
- Every hour, delete the GitHub reads nothing has made for a day: repos turned off, commits long merged.

\`\`\`text
redeliver   every 5m   list deliveries since cursor → POST /attempts for status >= 400
prune       every 1h   DELETE github_read WHERE read_at < now() - 1 day
\`\`\`

## Test plan

- [x] \`cargo test -p mergepatrol redeliver\`
- [x] Killed the server for 2 minutes on staging, saw the 14 missed deliveries come back
- [ ] Watch the redelivery count on the dashboard for a day`;

const checkNames = [
  "ci / fmt",
  "ci / clippy",
  "ci / test (store)",
  "ci / test (github)",
  "ci / test (mergepatrol)",
  "ci / test (web)",
  "ci / ui build",
  "ci / ui check",
  "docker / build",
  "docker / scan",
  "e2e / queue",
  "e2e / batch",
];

export const extrasFor = (number: number): PrExtras => {
  const failing = number % 3 === 0;
  return {
    body,
    labels: number % 2 ? ["enhancement"] : [],
    // #215 and the like have none, for the empty state.
    comments:
      number % 3 === 2
        ? []
        : [
            {
              at: ago(200),
              login: "mpcode",
              kind: "review",
              body: "The prune deletes reads a running batch still needs. Keep anything younger than the oldest active batch?",
            },
            {
              at: ago(120),
              login: "jnsdls",
              kind: "comment",
              body: "Good catch. The prune now skips anything a batch in flight has read.",
            },
            {
              at: ago(90),
              login: "github-actions",
              kind: "bot",
              body: "Coverage: 81.4% (+0.6%) on crates/mergepatrol.",
            },
          ],
    mergeable: number % 5 === 0 ? "conflicts" : number % 4 === 0 ? "behind" : "clean",
    checkRuns: checkNames.map((name, i) => ({
      name,
      status:
        failing && (i === 4 || i === 10) ? "failed" : i === 11 && number % 2 ? "pending" : "passed",
      took: `${1 + ((i * 7 + number) % 9)}m ${(i * 13) % 60}s`,
    })),
    timeline: [
      {
        kind: "commit",
        at: ago(300),
        login: "jnsdls",
        sha: "b3e3877",
        message: "feat: read checks from the cache while CI runs",
      },
      { kind: "requested", at: ago(290), login: "jnsdls", reviewer: "mpcode" },
      { kind: "label", at: ago(289), login: "jnsdls", label: "enhancement", added: true },
      {
        kind: "review",
        at: ago(200),
        login: "mpcode",
        state: "changes",
        body: "The prune deletes reads a running batch still needs. Keep anything younger than the oldest active batch?",
      },
      {
        kind: "commit",
        at: ago(150),
        login: "jnsdls",
        sha: "e4fab7e",
        message: "fix: keep the reads an active batch needs",
      },
      { kind: "force-push", at: ago(149), login: "jnsdls", from: "e4fab7e", to: "df954aa" },
      {
        kind: "comment",
        at: ago(120),
        login: "jnsdls",
        body: "Good catch. The prune now skips anything a batch in flight has read.",
      },
      { kind: "review", at: ago(62), login: "mpcode", state: "approved" },
    ],
  };
};
