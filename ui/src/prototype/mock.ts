// PROTOTYPE, delete with the activity variants. A fetch stub so `pnpm prototype` runs the queue
// without a backend: one signed-in workspace, one lane, and PR #51's events from the screenshot.
import type { EntryView } from "../api/EntryView";
import type { EventView } from "../api/EventView";
import type { LaneView } from "../api/LaneView";
import type { SessionView } from "../api/SessionView";

const repo = "agent-labs-dev/mergepatrol";
const avatar = "https://github.com/jnsdls.png";
const ago = (s: number) => new Date(Date.now() - s * 1000).toISOString();

const session: SessionView = {
  status: "signed_in",
  setup_unlocked: true,
  user: { login: "jnsdls", avatar_url: avatar },
  app: { slug: "mergepatrol", html_url: "", install_url: "" },
  workspaces: [
    {
      installation_id: 1,
      account: "agent-labs-dev",
      account_type: "Organization",
      avatar_url: avatar,
    },
  ],
  analytics: null,
};

const entry = (number: number, title: string, extra: Partial<EntryView> = {}): EntryView => ({
  number,
  priority: false,
  state: "waiting",
  detail: null,
  title,
  url: `https://github.com/${repo}/pull/${number}`,
  author: { login: "jnsdls", avatar_url: avatar },
  created_at: ago(86400),
  head_ref: `feat/pr-${number}`,
  additions: 412,
  deletions: 96,
  changed_files: 9,
  review_decision: "approved",
  reviews: [],
  requested: [],
  checks: "pending",
  layers: [],
  ...extra,
});

const lanes: LaneView[] = [
  {
    repo,
    trunk: "main",
    batch: null,
    entries: [
      entry(51, "feat: queue and merge GitHub stacks as one, so stacked PRs land", {
        state: "active",
        head_ref: "feat/stacks",
      }),
      entry(53, "fix: show the fix cost in history", {
        additions: 12,
        deletions: 3,
        changed_files: 1,
      }),
    ],
  },
];

// Oldest first here; the API returns newest first.
const pr51: [number, string, string][] = [
  [250, "enqueued", "added to the queue"],
  [245, "rebasing", "onto main"],
  [240, "job", "agent-labs-dev-mergepatrol-51-9cd2e8e0"],
  [
    140,
    "conflict",
    "crates/mergepatrol/src/queue.rs, crates/mergepatrol/src/queue/loop_tests.rs, crates/web/src/api.rs",
  ],
  [
    125,
    "resolved",
    "crates/mergepatrol/src/queue.rs, crates/mergepatrol/src/queue/loop_tests.rs, crates/web/src/api.rs in 46 turns, $0.46",
  ],
  [122, "rebased", "0602dc0fabba7c31b98d2ba97321d602c5b968ff"],
  [120, "published", "11a5c89bdc1f7ba809e29f5fd343bb43da48ee31"],
  [75, "fixing", "check, attempt 1 of 2"],
  [70, "job", "agent-labs-dev-mergepatrol-51-fix-11a5c89b"],
  [50, "rebasing", "onto main"],
  [48, "published", "11a5c89bdc1f7ba809e29f5fd343bb43da48ee31"],
  [45, "fixing", "check, attempt 1 of 2"],
  [42, "job", "agent-labs-dev-mergepatrol-51-fix-11a5c89b"],
  [30, "fixing", "check"],
  [12, "fix", "5 turns, $0.03"],
  [10, "fixed", "2948a04f133c4869727b6a5a0118ce35719891d1"],
  [5, "published", "4167519724312ece50a1bc75b34f3efaece01b78"],
];

const pr53: [number, string, string][] = [[30, "enqueued", "added to the queue"]];

const events = (pr: number): EventView[] =>
  (pr === 51 ? pr51 : pr53)
    .map(([s, kind, message]) => ({ repo, pr, at: ago(s), kind, message }))
    .reverse();

const json = (body: unknown) =>
  new Response(JSON.stringify(body), { headers: { "content-type": "application/json" } });

const real = window.fetch.bind(window);
window.fetch = async (input, init) => {
  const url = new URL(typeof input === "string" ? input : (input as Request).url, location.href);
  if (url.pathname === "/api/session") return json(session);
  if (url.pathname.endsWith("/lanes")) return json(lanes);
  if (url.pathname.endsWith("/events")) return json(events(Number(url.searchParams.get("pr"))));
  if (url.pathname.startsWith("/api/")) return json([]);
  return real(input, init);
};
