/*
 * PROTOTYPE, throw away: a fake API for the fix-diff prototype, so the real dashboard renders a
 * PR mid-fix with no server. Installed by main.tsx only in dev with `?variant=`.
 */
import type { EventView } from "../api/EventView";
import type { LaneView } from "../api/LaneView";
import type { SessionView } from "../api/SessionView";
import attempt1 from "./attempt-1.diff?raw";
import attempt2 from "./attempt-2.diff?raw";

export const REPO = "agent-labs-dev/mergepatrol";
export const PR = 71;

const REBASED = "1f0e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c";
export const FIX_1 = "9c2e41ab7f03d5e6a1b2c3d4e5f60718293a4b5c";
export const FIX_2 = "4be7a90c1d2e3f405162738495a6b7c8d9e0f1a2";

/** What the API would serve per fix: the patch of the commit the queue published. */
export const fixPatches: Record<string, string> = { [FIX_1]: attempt1, [FIX_2]: attempt2 };
/** The patch the agent is writing right now, for the live variant. */
export const inProgressPatch = attempt2;

export type Phase = "fixing" | "fixed";
export const phase = (): Phase =>
  localStorage.getItem("proto-fix-phase") === "fixed" ? "fixed" : "fixing";
export const setPhase = (p: Phase) => {
  localStorage.setItem("proto-fix-phase", p);
  location.reload();
};

const ago = (min: number) => new Date(Date.now() - min * 60_000).toISOString();

function events(): EventView[] {
  const e = (min: number, kind: string, message: string): EventView => ({
    repo: REPO,
    pr: PR,
    at: ago(min),
    kind,
    message,
  });
  const run = [
    e(14, "enqueued", "by jnsdls"),
    e(13.9, "job", "mp-71-a81f"),
    e(13.8, "rebasing", "onto main"),
    e(12, "published", REBASED),
    e(6, "fixing", "clippy, test (ubuntu-latest), attempt 1 of 2"),
    e(5.9, "fixing", "clippy, test (ubuntu-latest)"),
    e(4.4, "verify", "cargo clippy --all-targets -- -D warnings"),
    e(4.2, "verify", "cargo test"),
    e(4, "fix", "7 turns, $0.42"),
    e(3.9, "published", FIX_1),
    e(1.2, "fixing", "test (ubuntu-latest), attempt 2 of 2"),
    e(1.1, "fixing", "test (ubuntu-latest)"),
  ];
  if (phase() === "fixed")
    run.push(
      e(0.4, "verify", "cargo test"),
      e(0.3, "fix", "3 turns, $0.11"),
      e(0.2, "published", FIX_2),
    );
  return run.reverse();
}

const person = (login: string) => ({
  login,
  avatar_url: `https://github.com/${login}.png`,
});

const lane: LaneView = {
  repo: REPO,
  trunk: "main",
  batch: null,
  entries: [
    {
      number: PR,
      priority: false,
      state: "active",
      detail: null,
      title: "fix: page the history with a real cursor",
      url: `https://github.com/${REPO}/pull/${PR}`,
      author: person("jnsdls"),
      created_at: ago(60 * 26),
      head_ref: "fix/history-cursor",
      fork: false,
      additions: 64,
      deletions: 22,
      changed_files: 5,
      review_decision: "approved",
      reviews: [{ reviewer: person("octocat"), approved: true }],
      requested: [],
      checks: "pending",
      layers: [],
    },
    {
      number: 72,
      priority: false,
      state: "waiting",
      detail: null,
      title: "feat: show the trunk in the lane header",
      url: `https://github.com/${REPO}/pull/72`,
      author: person("octocat"),
      created_at: ago(60 * 5),
      head_ref: "feat/lane-trunk",
      fork: false,
      additions: 12,
      deletions: 3,
      changed_files: 2,
      review_decision: "approved",
      reviews: [],
      requested: [],
      checks: "passed",
      layers: [],
    },
  ],
};

const session: SessionView = {
  status: "signed_in",
  setup_unlocked: true,
  user: person("jnsdls"),
  app: { slug: "merge-patrol", html_url: "https://github.com/apps/merge-patrol", install_url: "#" },
  workspaces: [
    {
      installation_id: 1,
      account: "agent-labs-dev",
      account_type: "Organization",
      avatar_url: "https://github.com/agent-labs-dev.png",
    },
  ],
  analytics: null,
};

export function installMockApi() {
  const real = window.fetch;
  window.fetch = async (input, init) => {
    const url = new URL(typeof input === "string" ? input : (input as Request).url, location.href);
    if (!url.pathname.startsWith("/api/")) return real(input, init);
    const json = (body: unknown) =>
      new Response(JSON.stringify(body), { headers: { "content-type": "application/json" } });
    const p = url.pathname;
    if (p === "/api/session") return json(session);
    if (p.endsWith("/lanes")) return json([lane]);
    if (p.endsWith("/events"))
      return json(url.searchParams.get("pr") === String(PR) ? events() : []);
    if (p.endsWith("/settings"))
      return json({
        api_keys: [
          {
            provider: { id: "anthropic", name: "Anthropic" },
            key: { hint: "beef", updated_at: ago(9000) },
          },
        ],
      });
    return json([]);
  };
}
